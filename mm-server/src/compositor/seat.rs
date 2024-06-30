// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use bytes::Bytes;
use cstr::cstr;
use hashbrown::{HashMap, HashSet};
use tracing::{debug, warn};
use wayland_protocols::wp::{
    pointer_constraints::zv1::server::zwp_locked_pointer_v1,
    relative_pointer::zv1::server::zwp_relative_pointer_v1,
    text_input::zv3::server::zwp_text_input_v3,
};
use wayland_server::{
    protocol::{wl_keyboard, wl_pointer, wl_surface},
    Resource as _,
};

use crate::compositor::{
    buffers::BufferBacking,
    oneshot_render::shm_to_png,
    sealed::SealedFile,
    serial::Serial,
    surface::{surface_vector_to_buffer, SurfaceKey, SurfaceRole},
    CompositorEvent, State, EPOCH,
};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum KeyState {
    Pressed,
    Released,
    Repeat,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ButtonState {
    Pressed,
    Released,
}

impl From<ButtonState> for wl_pointer::ButtonState {
    fn from(value: ButtonState) -> Self {
        match value {
            ButtonState::Pressed => wl_pointer::ButtonState::Pressed,
            ButtonState::Released => wl_pointer::ButtonState::Released,
        }
    }
}

#[derive(Debug)]
struct Pointer {
    client_id: wayland_server::backend::ClientId,
    pending_frame: bool,
}

struct PointerLock {
    wl_pointer: wl_pointer::WlPointer,
    wp_locked_pointer: zwp_locked_pointer_v1::ZwpLockedPointerV1,
    oneshot: bool,
    defunct: bool,
}

#[derive(Default, Debug, PartialEq, Eq)]
pub enum Cursor {
    #[default]
    Unset,
    Hidden,
    Surface {
        surface: SurfaceKey,
        needs_render: bool,
        // Contains the hotspot in physical coords.
        rendered: Option<(Bytes, glam::UVec2)>,
        // In surface coords, supplied by the client.
        hotspot: glam::UVec2,
    },
}

pub struct Seat {
    pointers: HashMap<wl_pointer::WlPointer, Pointer>,
    relative_pointers:
        HashMap<zwp_relative_pointer_v1::ZwpRelativePointerV1, wl_pointer::WlPointer>,
    pointer_focus: Option<(wl_surface::WlSurface, glam::DVec2)>,
    pointer_coords: Option<glam::DVec2>, // Global coords.

    keyboards: HashSet<wl_keyboard::WlKeyboard>,
    text_inputs: HashSet<zwp_text_input_v3::ZwpTextInputV3>,
    keyboard_focus: Option<wl_surface::WlSurface>,
    keymap: SealedFile,

    inactive_pointer_locks: HashMap<wl_surface::WlSurface, PointerLock>,
    pointer_lock: Option<(wl_surface::WlSurface, PointerLock)>,

    cursor: Cursor,
}

impl Default for Seat {
    fn default() -> Self {
        let keymap = SealedFile::new(
            cstr!("mm-keymap"),
            include_bytes!(concat!(env!("OUT_DIR"), "/keymaps/iso_us.txt")),
        )
        .expect("failed to create keymap sealed fd");

        Self {
            pointers: HashMap::default(),
            relative_pointers: HashMap::default(),
            pointer_focus: None,
            pointer_coords: None,

            keyboards: HashSet::default(),
            text_inputs: HashSet::default(),
            keyboard_focus: None,
            keymap,

            inactive_pointer_locks: HashMap::default(),
            pointer_lock: None,

            cursor: Cursor::default(),
        }
    }
}

impl Seat {
    pub fn get_pointer(&mut self, wl_pointer: wl_pointer::WlPointer) {
        let client_id = wl_pointer.client().expect("pointer has no client").id();

        self.pointers.insert(
            wl_pointer,
            Pointer {
                client_id,
                pending_frame: false,
            },
        );
    }

    pub fn get_relative_pointer(
        &mut self,
        wp_relative_pointer: zwp_relative_pointer_v1::ZwpRelativePointerV1,
        wl_pointer: wl_pointer::WlPointer,
    ) {
        self.relative_pointers
            .insert(wp_relative_pointer, wl_pointer);
    }

    pub fn get_keyboard(&mut self, wl_keyboard: wl_keyboard::WlKeyboard) {
        use std::os::fd::AsFd as _;
        wl_keyboard.keymap(
            wl_keyboard::KeymapFormat::XkbV1,
            self.keymap.as_fd(),
            self.keymap.size() as u32,
        );

        // We disable client-side key repeat handling, and instead
        // simulate it.
        if wl_keyboard.version() >= 4 {
            wl_keyboard.repeat_info(0, i32::MAX);
        }

        self.keyboards.insert(wl_keyboard);
    }

    pub fn get_text_input(&mut self, wp_text_input: zwp_text_input_v3::ZwpTextInputV3) {
        self.text_inputs.insert(wp_text_input);
    }

    pub fn destroy_pointer(&mut self, wl_pointer: &wl_pointer::WlPointer) {
        self.pointers.remove(wl_pointer);
        self.inactive_pointer_locks
            .retain(|_, lock| &lock.wl_pointer != wl_pointer);

        match &mut self.pointer_lock {
            Some((
                _,
                PointerLock {
                    wl_pointer: p,
                    defunct,
                    ..
                },
            )) if p == wl_pointer => {
                *defunct = true;
            }
            _ => (),
        }
    }

    pub fn destroy_relative_pointer(
        &mut self,
        wp_relative_pointer: &zwp_relative_pointer_v1::ZwpRelativePointerV1,
    ) {
        self.relative_pointers.remove(wp_relative_pointer);
    }

    pub fn destroy_keyboard(&mut self, wl_keyboard: &wl_keyboard::WlKeyboard) {
        self.keyboards.remove(wl_keyboard);
    }

    pub fn destroy_text_input(&mut self, wp_text_input: &zwp_text_input_v3::ZwpTextInputV3) {
        self.text_inputs.remove(wp_text_input);
    }

    pub fn lift_pointer(&mut self, serial: &Serial) {
        self.pointer_coords = None;

        if let Some((surf, _)) = self.pointer_focus.take() {
            if let Some(client) = surf.client() {
                for (wl_pointer, p) in self
                    .pointers
                    .iter_mut()
                    .filter(|(_, p)| p.client_id == client.id())
                {
                    p.pending_frame = true;
                    wl_pointer.leave(serial.next(), &surf);
                }
            }
        }
    }

    // Moves the pointer to a location, returning the active pointer lock.
    pub fn update_pointer(
        &mut self,
        serial: &Serial,
        focus: wl_surface::WlSurface,
        surface_coords: impl Into<glam::DVec2>,
        global_coords: impl Into<glam::DVec2>,
    ) {
        if self.pointer_lock.is_some() {
            return;
        }

        self.pointer_coords = Some(global_coords.into());
        let new_coords = surface_coords.into();
        match self.pointer_focus.as_mut() {
            Some((surf, coords)) if surf == &focus => {
                // Round before checking for location equality.
                if coords.round().as_ivec2() != new_coords.round().as_ivec2() {
                    for (wl_pointer, p) in self
                        .pointers
                        .iter_mut()
                        .filter(|(p, _)| p.is_alive() && p.id().same_client_as(&surf.id()))
                    {
                        p.pending_frame = true;
                        wl_pointer.motion(
                            EPOCH.elapsed().as_millis() as u32,
                            new_coords.x,
                            new_coords.y,
                        );
                    }
                }

                return;
            }
            _ => (),
        }

        if let Some((surf, _)) = self.pointer_focus.take() {
            for (wl_pointer, p) in self
                .pointers
                .iter_mut()
                .filter(|(p, _)| p.is_alive() && p.id().same_client_as(&surf.id()))
            {
                p.pending_frame = true;
                wl_pointer.leave(serial.next(), &surf);
            }
        }

        for (wl_pointer, p) in self
            .pointers
            .iter_mut()
            .filter(|(p, _)| p.is_alive() && p.id().same_client_as(&focus.id()))
        {
            p.pending_frame = true;
            wl_pointer.enter(serial.next(), &focus, new_coords.x, new_coords.y);
        }

        self.pointer_focus = Some((focus, new_coords));
    }

    pub fn relative_pointer_motion(&mut self, surface_vector: impl Into<glam::DVec2>) {
        if self.pointer_lock.is_none() {
            return;
        }

        let Some((focus, _)) = self.pointer_focus.as_ref() else {
            return;
        };

        let vector = surface_vector.into();

        let now = EPOCH.elapsed().as_micros() as u64;
        let utime_hi = (now >> 32) as u32;
        let utime_lo = (now & 0xffffffff) as u32;

        for (wp_relative_pointer, wl_pointer) in self
            .relative_pointers
            .iter()
            .filter(|(p, _)| p.id().same_client_as(&focus.id()))
        {
            wp_relative_pointer
                .relative_motion(utime_hi, utime_lo, vector.x, vector.y, vector.x, vector.y);

            if let Some(p) = self.pointers.get_mut(wl_pointer) {
                p.pending_frame = true;
            }
        }
    }

    pub fn pointer_axis(&mut self, surface_vector: impl Into<glam::DVec2>) {
        let vector = surface_vector.into();
        let now = EPOCH.elapsed().as_millis() as u32;
        for (wl_pointer, p) in self.focused_pointers() {
            if vector.x != 0.0 {
                wl_pointer.axis(now, wl_pointer::Axis::HorizontalScroll, vector.x);
                p.pending_frame = true;
            }

            if vector.y != 0.0 {
                wl_pointer.axis(now, wl_pointer::Axis::HorizontalScroll, vector.x);
                p.pending_frame = true;
            }
        }
    }

    pub fn pointer_axis_discrete(&mut self, vector: impl Into<glam::DVec2>) {
        let vector = vector.into();
        for (wl_pointer, p) in self.focused_pointers() {
            if vector.x != 0.0 {
                send_axis_discrete(wl_pointer, wl_pointer::Axis::HorizontalScroll, vector.x);
                p.pending_frame = true;
            }

            if vector.y != 0.0 {
                send_axis_discrete(wl_pointer, wl_pointer::Axis::VerticalScroll, vector.y);
                p.pending_frame = true;
            }
        }
    }

    pub fn pointer_input(
        &mut self,
        serial: &Serial,
        surface: wl_surface::WlSurface,
        surface_coords: impl Into<glam::DVec2>,
        global_coords: impl Into<glam::DVec2>,
        button_code: u32,
        state: ButtonState,
    ) {
        let coords = surface_coords.into();
        self.update_pointer(serial, surface.clone(), coords, global_coords);

        for (wl_pointer, p) in self.focused_pointers() {
            p.pending_frame = true;
            wl_pointer.button(
                serial.next(),
                EPOCH.elapsed().as_millis() as u32,
                button_code,
                state.into(),
            );
        }
    }

    pub fn pointer_frame(&mut self) {
        for (wl_pointer, p) in self.pointers.iter_mut() {
            if p.pending_frame {
                if wl_pointer.version() >= 5 {
                    wl_pointer.frame();
                }

                p.pending_frame = false;
            }
        }
    }

    fn focused_pointers(&mut self) -> impl Iterator<Item = (&wl_pointer::WlPointer, &mut Pointer)> {
        let client_id = self
            .pointer_focus
            .as_ref()
            .and_then(|(focus, _)| focus.client())
            .map(|c| c.id());

        self.pointers
            .iter_mut()
            .filter(move |(p, _)| p.is_alive() && p.client().map(|c| c.id()) == client_id)
    }

    pub fn set_keyboard_focus(&mut self, serial: &Serial, surface: Option<wl_surface::WlSurface>) {
        if self.keyboard_focus == surface {
            return;
        }

        if let Some(old_surf) = self.keyboard_focus.take() {
            for wl_keyboard in self
                .keyboards
                .iter()
                .filter(|k| k.id().same_client_as(&old_surf.id()))
            {
                wl_keyboard.leave(serial.next(), &old_surf);
            }

            for wp_text_input in self
                .text_inputs
                .iter()
                .filter(|ti| ti.id().same_client_as(&old_surf.id()))
            {
                wp_text_input.leave(&old_surf);
            }
        }

        if let Some(new_surf) = surface.as_ref() {
            for wl_keyboard in self
                .keyboards
                .iter()
                .filter(|k| k.id().same_client_as(&new_surf.id()))
            {
                wl_keyboard.enter(serial.next(), new_surf, Vec::new());
                // TODO we're responsible for sending the list of depressed
                // modifiers. For our use case, this isn't very important.
                wl_keyboard.modifiers(serial.next(), 0, 0, 0, 0);
            }

            for wp_text_input in self
                .text_inputs
                .iter()
                .filter(|ti| ti.id().same_client_as(&new_surf.id()))
            {
                wp_text_input.enter(new_surf);
            }
        }

        self.keyboard_focus = surface;
    }

    pub fn keyboard_input(&mut self, serial: &Serial, scancode: u32, state: KeyState) {
        let state = match state {
            KeyState::Pressed => wl_keyboard::KeyState::Pressed,
            KeyState::Released => wl_keyboard::KeyState::Released,
            KeyState::Repeat => unreachable!(),
        };

        for wl_keyboard in self.focused_keyboards() {
            wl_keyboard.key(
                serial.next(),
                EPOCH.elapsed().as_millis() as u32,
                scancode,
                state,
            );
        }
    }

    pub fn focused_keyboards(&self) -> impl Iterator<Item = &wl_keyboard::WlKeyboard> {
        let client_id = self
            .keyboard_focus
            .as_ref()
            .and_then(|focus| focus.client())
            .map(|c| c.id());

        self.keyboards
            .iter()
            .filter(move |k| k.is_alive() && k.client().map(|c| c.id()) == client_id)
    }

    pub fn has_text_input(&mut self) -> bool {
        self.focused_text_inputs().count() > 0
    }

    pub fn text_input_char(&mut self, serial: &Serial, ch: char) {
        if let Some(focus) = self.keyboard_focus.as_ref() {
            for wp_text_input in self
                .text_inputs
                .iter()
                .filter(|ti| ti.id().same_client_as(&focus.id()))
            {
                wp_text_input.commit_string(Some(ch.into()));
                wp_text_input.done(serial.next())
            }
        }
    }

    fn focused_text_inputs(&mut self) -> impl Iterator<Item = &zwp_text_input_v3::ZwpTextInputV3> {
        let client_id = self
            .keyboard_focus
            .as_ref()
            .and_then(|focus| focus.client())
            .map(|c| c.id());

        self.text_inputs
            .iter()
            .filter(move |ti| ti.is_alive() && ti.client().map(|c| c.id()) == client_id)
    }

    pub fn pointer_focus(&self) -> Option<wl_surface::WlSurface> {
        self.pointer_focus.as_ref().map(|(surf, _)| surf).cloned()
    }

    #[allow(dead_code)]
    pub fn keyboard_focus(&self) -> Option<wl_surface::WlSurface> {
        self.keyboard_focus.clone()
    }

    pub fn pointer_coords(&self) -> Option<glam::DVec2> {
        self.pointer_coords
    }

    pub fn pointer_locked(&self) -> Option<glam::DVec2> {
        if self.pointer_lock.is_some() {
            Some(self.pointer_coords.unwrap_or_default())
        } else {
            None
        }
    }

    pub fn has_lock(&self, wl_surface: &wl_surface::WlSurface) -> bool {
        self.inactive_pointer_locks.contains_key(wl_surface)
            || self.pointer_lock.as_ref().map(|(surf, _)| surf) == Some(wl_surface)
    }

    pub fn create_lock(
        &mut self,
        wl_pointer: wl_pointer::WlPointer,
        wl_surface: wl_surface::WlSurface,
        wp_locked_pointer: zwp_locked_pointer_v1::ZwpLockedPointerV1,
        oneshot: bool,
    ) {
        if self
            .inactive_pointer_locks
            .insert(
                wl_surface,
                PointerLock {
                    wp_locked_pointer,
                    wl_pointer,
                    oneshot,
                    defunct: false,
                },
            )
            .is_some()
        {
            panic!("constraint already exists for surface");
        }
    }

    pub fn destroy_lock(&mut self, wp_locked_pointer: &zwp_locked_pointer_v1::ZwpLockedPointerV1) {
        self.inactive_pointer_locks
            .retain(|_, lock| &lock.wp_locked_pointer != wp_locked_pointer);

        match &mut self.pointer_lock {
            Some((
                _,
                PointerLock {
                    wp_locked_pointer: lock,
                    defunct,
                    ..
                },
            )) if lock == wp_locked_pointer => {
                // Cleared in update_pointer_lock.
                *defunct = true;
            }
            _ => (),
        }
    }
}

impl State {
    pub fn update_pointer_lock(&mut self) {
        let seat = &mut self.default_seat;
        let active = self.active_surface.and_then(|id| self.surfaces.get(id));

        if let Some((wl_surface, lock)) = &seat.pointer_lock {
            if Some(wl_surface) == active.map(|s| &s.wl_surface)
                && lock.wp_locked_pointer.is_alive()
            {
                // Same surface, active lock, nothing to do.
                return;
            }
        }

        let prev_lock = if let Some((surf, lock)) = seat.pointer_lock.take() {
            lock.wp_locked_pointer.unlocked();

            let lock_clone = lock.wp_locked_pointer.clone();
            if lock.wp_locked_pointer.is_alive() && !lock.oneshot {
                seat.inactive_pointer_locks.insert(surf, lock);
            }

            Some(lock_clone)
        } else {
            None
        };

        if let Some((wl_surface, lock)) =
            active.and_then(|s| seat.inactive_pointer_locks.remove_entry(&s.wl_surface))
        {
            lock.wp_locked_pointer.locked();
            seat.pointer_lock = Some((wl_surface, lock));
            let (x, y) = seat.pointer_coords().unwrap_or_default().into();

            debug!(surface = ?active, x, y, "activating pointer lock");
            self.handle.dispatch(CompositorEvent::PointerLocked(x, y));
        } else if let Some(wp_locked_pointer) = prev_lock {
            wp_locked_pointer.unlocked();

            debug!("pointer lock released");
            self.handle.dispatch(CompositorEvent::PointerReleased);
        }
    }

    pub fn set_cursor(&mut self, wl_pointer: &wl_pointer::WlPointer, cursor: Cursor) {
        if !self
            .default_seat
            .pointer_focus
            .as_ref()
            .is_some_and(|(wl_surface, _)| wl_surface.id().same_client_as(&wl_pointer.id()))
        {
            return;
        }

        match cursor {
            Cursor::Unset => unreachable!(),
            Cursor::Surface { surface: id, .. } => {
                let Some(surface) = self.surfaces.get_mut(id) else {
                    return;
                };

                if surface.role.current.is_some()
                    && surface.role.current != Some(SurfaceRole::Cursor)
                {
                    debug!(
                        ?surface,
                        "ignoring cursor role for surface with preexisting role"
                    );

                    return;
                }

                surface.role.current = Some(SurfaceRole::Cursor);
            }
            _ => (),
        }

        let old_cursor = std::mem::replace(&mut self.default_seat.cursor, cursor);
        if let Cursor::Surface { surface: id, .. } = old_cursor {
            if let Some(surface) = self.surfaces.get_mut(id) {
                surface.role.current = None;
                self.unmap_surface(id);
            }
        }

        self.dispatch_cursor();
    }

    pub fn dispatch_cursor(&mut self) {
        match &mut self.default_seat.cursor {
            Cursor::Unset => (),
            Cursor::Surface {
                needs_render,
                rendered: Some((img, hotspot)),
                ..
            } if !*needs_render => {
                self.handle.dispatch(CompositorEvent::CursorUpdate {
                    image: Some(img.clone()),
                    icon: None,
                    hotspot_x: hotspot.x,
                    hotspot_y: hotspot.y,
                });
            }
            Cursor::Surface { .. } => {
                // The cursor will be dispatched after it's rendered during the next frame.
            }
            Cursor::Hidden => self.handle.dispatch(CompositorEvent::CursorUpdate {
                image: None,
                icon: None,
                hotspot_x: 0,
                hotspot_y: 0,
            }),
        }
    }

    pub fn render_cursor(&mut self) -> anyhow::Result<()> {
        let Cursor::Surface {
            surface,
            hotspot,
            needs_render,
            rendered,
        } = &mut self.default_seat.cursor
        else {
            return Ok(());
        };

        if !*needs_render {
            return Ok(());
        }

        let surface = &mut self.surfaces[*surface];
        let buffer = surface.content.as_ref().map(|c| &self.buffers[c.buffer]);

        let image = match buffer.map(|b| &b.backing) {
            None => return Ok(()), // No content yet, try again later.
            Some(BufferBacking::Dmabuf { .. }) => {
                warn!("ignoring dmabuf cursor texture");

                // TODO: for now, we set the cursor to the default.
                *needs_render = false;
                self.handle.dispatch(CompositorEvent::CursorUpdate {
                    image: None,
                    icon: Some(cursor_icon::CursorIcon::Default),
                    hotspot_x: 0,
                    hotspot_y: 0,
                });

                return Ok(());
            }
            Some(BufferBacking::Shm {
                format,
                staging_buffer,
                ..
            }) => {
                debug!("rendering cursor to png");
                shm_to_png(staging_buffer, *format)?
            }
        };

        let scale = surface.effective_scale();
        let hotspot = surface_vector_to_buffer(*hotspot, scale).as_uvec2();

        self.handle.dispatch(CompositorEvent::CursorUpdate {
            image: Some(image.clone()),
            icon: None,
            hotspot_x: hotspot.x,
            hotspot_y: hotspot.y,
        });

        *rendered = Some((image, hotspot));
        *needs_render = false;
        if let Some(cb) = surface.frame_callback.current.take() {
            cb.done(EPOCH.elapsed().as_millis() as u32);
        }

        Ok(())
    }
}

fn send_axis_discrete(pointer: &wl_pointer::WlPointer, axis: wl_pointer::Axis, value: f64) {
    let version = pointer.version();
    if (5..8).contains(&version) {
        pointer.axis_discrete(axis, value.trunc() as i32);
    } else if version >= 8 {
        pointer.axis_value120(axis, (value * 120.0).round() as i32);
    }
}
