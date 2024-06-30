// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use cstr::cstr;
use hashbrown::HashMap;
use wayland_server::{
    backend::ClientId,
    protocol::{wl_keyboard, wl_pointer, wl_surface},
    Resource as _,
};

use crate::compositor::{sealed::SealedFile, serial::Serial, EPOCH};

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

pub struct Seat {
    pointers: HashMap<wl_pointer::WlPointer, Pointer>,
    pointer_focus: Option<(wl_surface::WlSurface, glam::DVec2)>,

    keyboards: HashMap<wl_keyboard::WlKeyboard, ClientId>,
    keyboard_focus: Option<wl_surface::WlSurface>,
    keymap: SealedFile,
}

impl Default for Seat {
    fn default() -> Self {
        Self {
            pointers: HashMap::default(),
            pointer_focus: None,

            keyboards: HashMap::default(),
            keyboard_focus: None,
            keymap: SealedFile::new(
                cstr!("mm-keymap"),
                include_bytes!(concat!(env!("OUT_DIR"), "/keymaps/iso_us.txt")),
            )
            .expect("failed to create keymap sealed fd"),
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

    pub fn get_keyboard(&mut self, wl_keyboard: wl_keyboard::WlKeyboard) {
        let client_id = wl_keyboard.client().expect("pointer has no client").id();

        use std::os::fd::AsFd as _;
        wl_keyboard.keymap(
            wl_keyboard::KeymapFormat::XkbV1,
            self.keymap.as_fd(),
            self.keymap.size() as u32,
        );

        // We disable client-side key repeat handling, and instead
        // simulate it.
        wl_keyboard.repeat_info(0, i32::MAX);

        self.keyboards.insert(wl_keyboard, client_id);
    }

    pub fn destroy_pointer(&mut self, wl_pointer: wl_pointer::WlPointer) {
        self.pointers.remove(&wl_pointer);
    }

    pub fn destroy_keyboard(&mut self, wl_keyboard: wl_keyboard::WlKeyboard) {
        self.keyboards.remove(&wl_keyboard);
    }

    pub fn lift_pointer(&mut self, serial: &Serial) {
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

    pub fn set_pointer(
        &mut self,
        serial: &Serial,
        focus: wl_surface::WlSurface,
        pointer_surface_coords: impl Into<glam::DVec2>,
    ) {
        let new_coords = pointer_surface_coords.into();

        match self.pointer_focus.as_mut() {
            Some((surf, coords)) if surf == &focus && surf.is_alive() => {
                // Round before checking for location equality.
                if coords.round().as_ivec2() != new_coords.round().as_ivec2() {
                    let client = surf.client().unwrap();

                    for (wl_pointer, p) in self
                        .pointers
                        .iter_mut()
                        .filter(|(_, p)| p.client_id == client.id())
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

        if let Some(client) = focus.client() {
            for (wl_pointer, p) in self
                .pointers
                .iter_mut()
                .filter(|(_, p)| p.client_id == client.id())
            {
                p.pending_frame = true;
                wl_pointer.enter(serial.next(), &focus, new_coords.x, new_coords.y);
            }
        }

        self.pointer_focus = Some((focus, new_coords));
    }

    pub fn relative_pointer_motion(
        &mut self,
        serial: &Serial,
        surface_vector: impl Into<glam::DVec2>,
    ) {
        todo!()
    }

    pub fn pointer_axis(&mut self, serial: &Serial, surface_vector: impl Into<glam::DVec2>) {
        todo!()
    }

    pub fn pointer_axis_discrete(
        &mut self,
        serial: &Serial,
        surface_vector: impl Into<glam::DVec2>,
    ) {
        todo!()
    }

    pub fn pointer_input(
        &mut self,
        serial: &Serial,
        surface: wl_surface::WlSurface,
        surface_coords: impl Into<glam::DVec2>,
        button_code: u32,
        state: ButtonState,
    ) {
        let coords = surface_coords.into();
        self.set_pointer(serial, surface.clone(), coords);

        if let Some(client) = surface.client() {
            for (wl_pointer, p) in self
                .pointers
                .iter_mut()
                .filter(|(_, p)| p.client_id == client.id())
            {
                p.pending_frame = true;
                wl_pointer.button(
                    serial.next(),
                    EPOCH.elapsed().as_millis() as u32,
                    button_code,
                    state.into(),
                );
            }
        }
    }

    pub fn pointer_frame(&mut self) {
        for (wl_pointer, p) in self.pointers.iter_mut() {
            if p.pending_frame {
                wl_pointer.frame();
                p.pending_frame = false;
            }
        }
    }

    pub fn set_keyboard_focus(&mut self, serial: &Serial, surface: Option<wl_surface::WlSurface>) {
        if self.keyboard_focus == surface {
            return;
        }

        if let Some(old_surf) = self.keyboard_focus.take() {
            if let Some(client) = old_surf.client() {
                for (wl_keyboard, _) in self.keyboards.iter().filter(|(_, c)| **c == client.id()) {
                    wl_keyboard.leave(serial.next(), &old_surf);
                }
            }
        }

        if let Some(new_surf) = surface.as_ref() {
            if let Some(client) = new_surf.client() {
                for (wl_keyboard, _) in self.keyboards.iter().filter(|(_, c)| **c == client.id()) {
                    wl_keyboard.enter(serial.next(), &new_surf, Vec::new());
                    // todo track pressed keys + modifiers
                    // wl_keyboard.modifiers()
                }
            }
        }

        self.keyboard_focus = None;
    }

    pub fn keyboard_input(&mut self, serial: &Serial, scancode: u32, state: KeyState) {
        let state = match state {
            KeyState::Pressed => wl_keyboard::KeyState::Pressed,
            KeyState::Released => wl_keyboard::KeyState::Released,
            KeyState::Repeat => return, // TODO: simulate repeat
        };

        if let Some(surf) = self.keyboard_focus.as_ref() {
            if let Some(client) = surf.client() {
                for (wl_keyboard, _) in self
                    .keyboards
                    .iter_mut()
                    .filter(|(_, c)| **c == client.id())
                {
                    wl_keyboard.key(
                        serial.next(),
                        EPOCH.elapsed().as_millis() as u32,
                        scancode,
                        state,
                    );
                }
            }
        }
    }
}
