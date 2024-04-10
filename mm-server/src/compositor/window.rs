// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use anyhow::Context;
use smithay::{
    input::{keyboard::KeyboardTarget, pointer::PointerTarget, touch::TouchTarget},
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::{protocol::wl_surface, Resource},
    },
    utils::IsAlive,
    wayland::{seat::WaylandFocus, shell::xdg},
    xwayland,
};
use tracing::trace;

use crate::pixel_scale::PixelScale;

use super::State;

pub const TODO_X11_SCALE: i32 = 1;

#[derive(Debug, PartialEq, Clone)]
pub enum SurfaceType {
    X11Window(xwayland::X11Surface),
    X11Popup(
        xwayland::X11Surface,
        smithay::utils::Rectangle<i32, smithay::utils::Physical>,
    ),
    XdgToplevel(xdg::ToplevelSurface),
}

#[derive(Debug, Clone)]
pub struct Window {
    pub ty: SurfaceType,
    pub surface: wl_surface::WlSurface,
}

impl Window {
    pub fn popup_bounds(&self) -> Option<smithay::utils::Rectangle<i32, smithay::utils::Physical>> {
        match &self.ty {
            SurfaceType::X11Popup(_, bounds) => Some(*bounds),
            _ => None,
        }
    }

    pub fn bounds_changed(
        &mut self,
        new_bounds: smithay::utils::Rectangle<i32, smithay::utils::Logical>,
    ) {
        if let SurfaceType::X11Popup(_, bounds) = &mut self.ty {
            *bounds = new_bounds.to_physical(TODO_X11_SCALE)
        }
    }

    pub fn recenter(&mut self, output_width: u32, output_height: u32) {
        match self.ty {
            SurfaceType::X11Popup(ref xwin, ref mut bounds) if !xwin.is_override_redirect() => {
                bounds.loc = (
                    std::cmp::max(0, (output_width as i32 - bounds.size.w) / 2),
                    std::cmp::max(0, (output_height as i32 - bounds.size.h) / 2),
                )
                    .into();
            }
            _ => (),
        }
    }

    pub fn configure_activated(
        &mut self,
        output_width: u32,
        output_height: u32,
        ui_scale: PixelScale,
    ) -> anyhow::Result<()> {
        let loc: smithay::utils::Point<i32, smithay::utils::Physical> = (0, 0).into();
        let size: smithay::utils::Size<i32, smithay::utils::Physical> =
            (output_width as i32, output_height as i32).into();
        let fullscreen_bbox = smithay::utils::Rectangle::from_loc_and_size(loc, size);

        match &mut self.ty {
            SurfaceType::X11Popup(_, _) => {
                self.recenter(output_width, output_height);
            }
            SurfaceType::X11Window(window) => {
                if window.is_override_redirect() {
                    return Ok(());
                }

                window.configure(fullscreen_bbox.to_logical(1))?;
                window.set_minimized(false)?;
                window.set_activated(true)?;
                window.set_fullscreen(true)?;
            }
            SurfaceType::XdgToplevel(toplevel) => {
                let scale: smithay::output::Scale = ui_scale.into();
                let bbox = fullscreen_bbox
                    .to_f64()
                    .to_logical(scale.fractional_scale())
                    .to_i32_round();

                toplevel.with_pending_state(|tl| {
                    tl.states.unset(xdg_toplevel::State::Suspended);
                    tl.states.set(xdg_toplevel::State::Activated);
                    tl.states.set(xdg_toplevel::State::Fullscreen);
                    tl.size = Some(bbox.size);
                });

                toplevel.send_pending_configure();
            }
        }

        Ok(())
    }

    pub fn configure_suspended(&mut self) -> anyhow::Result<()> {
        match &mut self.ty {
            SurfaceType::X11Window(window) | SurfaceType::X11Popup(window, _) => {
                window.set_activated(false)?;
                window.set_minimized(true)?;
            }
            SurfaceType::XdgToplevel(toplevel) => {
                toplevel.with_pending_state(|tl| {
                    tl.states.unset(xdg_toplevel::State::Activated);
                    tl.states.set(xdg_toplevel::State::Suspended);
                });

                toplevel.send_pending_configure();
            }
        }

        Ok(())
    }

    pub fn send_frame_callbacks(&mut self, ts: u32) {
        smithay::wayland::compositor::with_surface_tree_downward(
            &self.surface,
            (),
            |_, _, &()| smithay::wayland::compositor::TraversalAction::DoChildren(()),
            |_surf, states, &()| {
                let mut attrs = states
                    .cached_state
                    .current::<smithay::wayland::compositor::SurfaceAttributes>();

                for callback in attrs.frame_callbacks.drain(..) {
                    callback.done(ts);
                }
            },
            |_, _, &()| true,
        );
    }
}

impl PartialEq for Window {
    fn eq(&self, other: &Self) -> bool {
        match (&self.ty, &other.ty) {
            (SurfaceType::X11Window(a), SurfaceType::X11Window(b)) => a == b,
            (SurfaceType::XdgToplevel(a), SurfaceType::XdgToplevel(b)) => a == b,
            _ => false,
        }
    }
}

impl IsAlive for Window {
    fn alive(&self) -> bool {
        match &self.ty {
            SurfaceType::X11Window(window) | SurfaceType::X11Popup(window, _) => window.alive(),
            SurfaceType::XdgToplevel(window) => window.alive(),
        }
    }
}

impl KeyboardTarget<State> for Window {
    fn enter(
        &self,
        seat: &smithay::input::Seat<State>,
        data: &mut State,
        keys: Vec<smithay::input::keyboard::KeysymHandle<'_>>,
        serial: smithay::utils::Serial,
    ) {
        if let SurfaceType::X11Window(window) | SurfaceType::X11Popup(window, _) = &self.ty {
            KeyboardTarget::enter(window, seat, data, keys, serial);
        } else {
            KeyboardTarget::enter(&self.surface, seat, data, keys, serial);
        }
    }

    fn leave(
        &self,
        seat: &smithay::input::Seat<State>,
        data: &mut State,
        serial: smithay::utils::Serial,
    ) {
        if let SurfaceType::X11Window(window) | SurfaceType::X11Popup(window, _) = &self.ty {
            KeyboardTarget::leave(window, seat, data, serial);
        } else {
            KeyboardTarget::leave(&self.surface, seat, data, serial);
        }
    }

    fn key(
        &self,
        seat: &smithay::input::Seat<State>,
        data: &mut State,
        key: smithay::input::keyboard::KeysymHandle<'_>,
        state: smithay::backend::input::KeyState,
        serial: smithay::utils::Serial,
        time: u32,
    ) {
        if let SurfaceType::X11Window(window) | SurfaceType::X11Popup(window, _) = &self.ty {
            KeyboardTarget::key(window, seat, data, key, state, serial, time);
        } else {
            KeyboardTarget::key(&self.surface, seat, data, key, state, serial, time);
        }
    }

    fn modifiers(
        &self,
        seat: &smithay::input::Seat<State>,
        data: &mut State,
        modifiers: smithay::input::keyboard::ModifiersState,
        serial: smithay::utils::Serial,
    ) {
        if let SurfaceType::X11Window(window) | SurfaceType::X11Popup(window, _) = &self.ty {
            KeyboardTarget::modifiers(window, seat, data, modifiers, serial);
        } else {
            KeyboardTarget::modifiers(&self.surface, seat, data, modifiers, serial);
        }
    }
}

impl PointerTarget<State> for Window {
    fn enter(
        &self,
        seat: &smithay::input::Seat<State>,
        data: &mut State,
        event: &smithay::input::pointer::MotionEvent,
    ) {
        if let SurfaceType::X11Window(window) | SurfaceType::X11Popup(window, _) = &self.ty {
            PointerTarget::enter(window, seat, data, event);
        } else {
            PointerTarget::enter(&self.surface, seat, data, event);
        }
    }

    fn motion(
        &self,
        seat: &smithay::input::Seat<State>,
        data: &mut State,
        event: &smithay::input::pointer::MotionEvent,
    ) {
        if let SurfaceType::X11Window(window) | SurfaceType::X11Popup(window, _) = &self.ty {
            PointerTarget::motion(window, seat, data, event);
        } else {
            PointerTarget::motion(&self.surface, seat, data, event);
        }
    }

    fn relative_motion(
        &self,
        seat: &smithay::input::Seat<State>,
        data: &mut State,
        event: &smithay::input::pointer::RelativeMotionEvent,
    ) {
        if let SurfaceType::X11Window(window) | SurfaceType::X11Popup(window, _) = &self.ty {
            PointerTarget::relative_motion(window, seat, data, event);
        } else {
            PointerTarget::relative_motion(&self.surface, seat, data, event);
        }
    }

    fn button(
        &self,
        seat: &smithay::input::Seat<State>,
        data: &mut State,
        event: &smithay::input::pointer::ButtonEvent,
    ) {
        if let SurfaceType::X11Window(window) | SurfaceType::X11Popup(window, _) = &self.ty {
            PointerTarget::button(window, seat, data, event);
        } else {
            PointerTarget::button(&self.surface, seat, data, event);
        }
    }

    fn axis(
        &self,
        seat: &smithay::input::Seat<State>,
        data: &mut State,
        frame: smithay::input::pointer::AxisFrame,
    ) {
        if let SurfaceType::X11Window(window) | SurfaceType::X11Popup(window, _) = &self.ty {
            PointerTarget::axis(window, seat, data, frame);
        } else {
            PointerTarget::axis(&self.surface, seat, data, frame);
        }
    }

    fn frame(&self, seat: &smithay::input::Seat<State>, data: &mut State) {
        if let SurfaceType::X11Window(window) | SurfaceType::X11Popup(window, _) = &self.ty {
            PointerTarget::frame(window, seat, data);
        } else {
            PointerTarget::frame(&self.surface, seat, data);
        }
    }

    fn gesture_swipe_begin(
        &self,
        seat: &smithay::input::Seat<State>,
        data: &mut State,
        event: &smithay::input::pointer::GestureSwipeBeginEvent,
    ) {
        if let SurfaceType::X11Window(window) | SurfaceType::X11Popup(window, _) = &self.ty {
            PointerTarget::gesture_swipe_begin(window, seat, data, event);
        } else {
            PointerTarget::gesture_swipe_begin(&self.surface, seat, data, event);
        }
    }

    fn gesture_swipe_update(
        &self,
        seat: &smithay::input::Seat<State>,
        data: &mut State,
        event: &smithay::input::pointer::GestureSwipeUpdateEvent,
    ) {
        if let SurfaceType::X11Window(window) | SurfaceType::X11Popup(window, _) = &self.ty {
            PointerTarget::gesture_swipe_update(window, seat, data, event);
        } else {
            PointerTarget::gesture_swipe_update(&self.surface, seat, data, event);
        }
    }

    fn gesture_swipe_end(
        &self,
        seat: &smithay::input::Seat<State>,
        data: &mut State,
        event: &smithay::input::pointer::GestureSwipeEndEvent,
    ) {
        if let SurfaceType::X11Window(window) | SurfaceType::X11Popup(window, _) = &self.ty {
            PointerTarget::gesture_swipe_end(window, seat, data, event);
        } else {
            PointerTarget::gesture_swipe_end(&self.surface, seat, data, event);
        }
    }

    fn gesture_pinch_begin(
        &self,
        seat: &smithay::input::Seat<State>,
        data: &mut State,
        event: &smithay::input::pointer::GesturePinchBeginEvent,
    ) {
        if let SurfaceType::X11Window(window) | SurfaceType::X11Popup(window, _) = &self.ty {
            PointerTarget::gesture_pinch_begin(window, seat, data, event);
        } else {
            PointerTarget::gesture_pinch_begin(&self.surface, seat, data, event);
        }
    }

    fn gesture_pinch_update(
        &self,
        seat: &smithay::input::Seat<State>,
        data: &mut State,
        event: &smithay::input::pointer::GesturePinchUpdateEvent,
    ) {
        if let SurfaceType::X11Window(window) | SurfaceType::X11Popup(window, _) = &self.ty {
            PointerTarget::gesture_pinch_update(window, seat, data, event);
        } else {
            PointerTarget::gesture_pinch_update(&self.surface, seat, data, event);
        }
    }

    fn gesture_pinch_end(
        &self,
        seat: &smithay::input::Seat<State>,
        data: &mut State,
        event: &smithay::input::pointer::GesturePinchEndEvent,
    ) {
        if let SurfaceType::X11Window(window) | SurfaceType::X11Popup(window, _) = &self.ty {
            PointerTarget::gesture_pinch_end(window, seat, data, event);
        } else {
            PointerTarget::gesture_pinch_end(&self.surface, seat, data, event);
        }
    }

    fn gesture_hold_begin(
        &self,
        seat: &smithay::input::Seat<State>,
        data: &mut State,
        event: &smithay::input::pointer::GestureHoldBeginEvent,
    ) {
        if let SurfaceType::X11Window(window) | SurfaceType::X11Popup(window, _) = &self.ty {
            PointerTarget::gesture_hold_begin(window, seat, data, event);
        } else {
            PointerTarget::gesture_hold_begin(&self.surface, seat, data, event);
        }
    }

    fn gesture_hold_end(
        &self,
        seat: &smithay::input::Seat<State>,
        data: &mut State,
        event: &smithay::input::pointer::GestureHoldEndEvent,
    ) {
        if let SurfaceType::X11Window(window) | SurfaceType::X11Popup(window, _) = &self.ty {
            PointerTarget::gesture_hold_end(window, seat, data, event);
        } else {
            PointerTarget::gesture_hold_end(&self.surface, seat, data, event);
        }
    }

    fn leave(
        &self,
        seat: &smithay::input::Seat<State>,
        data: &mut State,
        serial: smithay::utils::Serial,
        time: u32,
    ) {
        if let SurfaceType::X11Window(window) | SurfaceType::X11Popup(window, _) = &self.ty {
            PointerTarget::leave(window, seat, data, serial, time);
        } else {
            PointerTarget::leave(&self.surface, seat, data, serial, time);
        }
    }
}

impl TouchTarget<State> for Window {
    fn down(
        &self,
        seat: &smithay::input::Seat<State>,
        data: &mut State,
        event: &smithay::input::touch::DownEvent,
        seq: smithay::utils::Serial,
    ) {
        if let SurfaceType::X11Window(window) | SurfaceType::X11Popup(window, _) = &self.ty {
            TouchTarget::down(window, seat, data, event, seq);
        } else {
            TouchTarget::down(&self.surface, seat, data, event, seq);
        }
    }

    fn up(
        &self,
        seat: &smithay::input::Seat<State>,
        data: &mut State,
        event: &smithay::input::touch::UpEvent,
        seq: smithay::utils::Serial,
    ) {
        if let SurfaceType::X11Window(window) | SurfaceType::X11Popup(window, _) = &self.ty {
            TouchTarget::up(window, seat, data, event, seq);
        } else {
            TouchTarget::up(&self.surface, seat, data, event, seq);
        }
    }

    fn motion(
        &self,
        seat: &smithay::input::Seat<State>,
        data: &mut State,
        event: &smithay::input::touch::MotionEvent,
        seq: smithay::utils::Serial,
    ) {
        if let SurfaceType::X11Window(window) | SurfaceType::X11Popup(window, _) = &self.ty {
            TouchTarget::motion(window, seat, data, event, seq);
        } else {
            TouchTarget::motion(&self.surface, seat, data, event, seq);
        }
    }

    fn frame(
        &self,
        seat: &smithay::input::Seat<State>,
        data: &mut State,
        seq: smithay::utils::Serial,
    ) {
        if let SurfaceType::X11Window(window) | SurfaceType::X11Popup(window, _) = &self.ty {
            TouchTarget::frame(window, seat, data, seq);
        } else {
            TouchTarget::frame(&self.surface, seat, data, seq);
        }
    }

    fn cancel(
        &self,
        seat: &smithay::input::Seat<State>,
        data: &mut State,
        seq: smithay::utils::Serial,
    ) {
        if let SurfaceType::X11Window(window) | SurfaceType::X11Popup(window, _) = &self.ty {
            TouchTarget::cancel(window, seat, data, seq);
        } else {
            TouchTarget::cancel(&self.surface, seat, data, seq);
        }
    }

    fn shape(
        &self,
        seat: &smithay::input::Seat<State>,
        data: &mut State,
        event: &smithay::input::touch::ShapeEvent,
        seq: smithay::utils::Serial,
    ) {
        if let SurfaceType::X11Window(window) | SurfaceType::X11Popup(window, _) = &self.ty {
            TouchTarget::shape(window, seat, data, event, seq);
        } else {
            TouchTarget::shape(&self.surface, seat, data, event, seq);
        }
    }

    fn orientation(
        &self,
        seat: &smithay::input::Seat<State>,
        data: &mut State,
        event: &smithay::input::touch::OrientationEvent,
        seq: smithay::utils::Serial,
    ) {
        if let SurfaceType::X11Window(window) | SurfaceType::X11Popup(window, _) = &self.ty {
            TouchTarget::orientation(window, seat, data, event, seq);
        } else {
            TouchTarget::orientation(&self.surface, seat, data, event, seq);
        }
    }
}

impl WaylandFocus for Window {
    fn wl_surface(&self) -> Option<wl_surface::WlSurface> {
        Some(self.surface.clone())
    }
}

impl State {
    pub fn map_x11(&mut self, window: xwayland::X11Surface) -> anyhow::Result<()> {
        // The xwindow must already have an attached surface.
        let surface = window.wl_surface().as_ref().cloned().unwrap();

        let is_normal = window.window_type().is_none()
            || window.window_type() == Some(xwayland::xwm::WmWindowType::Normal);
        let window = if window.is_override_redirect() || !is_normal {
            let mut bounds = window.geometry().to_physical(TODO_X11_SCALE);
            if !window.is_override_redirect() {
                // Center the window.
                bounds.loc = (
                    std::cmp::max(0, (self.display_params.width as i32 - bounds.size.w) / 2),
                    std::cmp::max(0, (self.display_params.height as i32 - bounds.size.h) / 2),
                )
                    .into();
            }

            bounds.size = (
                std::cmp::min(bounds.size.w, self.display_params.width as i32),
                std::cmp::min(bounds.size.h, self.display_params.height as i32),
            )
                .into();

            trace!("placing popup at {:?}", bounds);

            Window {
                ty: SurfaceType::X11Popup(window, bounds),
                surface,
            }
        } else {
            Window {
                ty: SurfaceType::X11Window(window),
                surface,
            }
        };

        self.push_window(window)?;
        trace!("window stack: {:#?}", self.window_stack);
        Ok(())
    }

    pub fn map_xdg(&mut self, window: xdg::ToplevelSurface) -> anyhow::Result<()> {
        let surface = window.wl_surface().clone();
        let window = Window {
            ty: SurfaceType::XdgToplevel(window),
            surface,
        };

        self.push_window(window)
    }

    fn push_window(&mut self, mut window: Window) -> anyhow::Result<()> {
        trace!("pushing window: {:?}", window);

        if window.popup_bounds().is_some() {
            // The window just gets pushed on top - no change to visibility.
        } else {
            for current in self.window_stack.iter_mut() {
                current.configure_suspended()?;
                self.output.leave(&current.surface);
            }
        }

        window
            .configure_activated(
                self.display_params.width,
                self.display_params.height,
                self.display_params.ui_scale,
            )
            .context("failed to configure x11 window")?;
        self.output.enter(&window.surface);

        let kb = self.seat.get_keyboard().unwrap();
        kb.set_focus(
            self,
            Some(window.clone()),
            smithay::utils::SERIAL_COUNTER.next_serial(),
        );

        trace!("current focus: {:?}", kb.is_focused());

        let text_input = smithay::wayland::text_input::TextInputSeat::text_input(&self.seat);

        text_input.leave();
        text_input.set_focus(Some(window.surface.clone()));
        text_input.enter();

        self.window_stack.push(window);
        Ok(())
    }

    pub fn unmap_x11_window(&mut self, xwindow: &xwayland::X11Surface) -> anyhow::Result<()> {
        let idx = self.window_stack.iter().position(|w| match w.ty {
            SurfaceType::X11Window(ref w) | SurfaceType::X11Popup(ref w, _) => w == xwindow,
            _ => false,
        });

        if let Some(idx) = idx {
            self.unmap_window(idx)?;
        } else {
            trace!("no window found to unmap")
        }

        Ok(())
    }

    pub fn unmap_window_for_surface(
        &mut self,
        surface: &wl_surface::WlSurface,
    ) -> anyhow::Result<()> {
        let idx = self.window_stack.iter().position(|w| w.surface == *surface);
        if let Some(idx) = idx {
            self.unmap_window(idx)?;
        } else {
            trace!("no window found for surface: {:?}", surface.id())
        }

        Ok(())
    }

    fn unmap_window(&mut self, idx: usize) -> anyhow::Result<()> {
        trace!("unmapping window at position {}", idx);

        let window = self.window_stack.remove(idx);
        self.output.leave(&window.surface);

        // Uncover visible windows.
        self.activate_top_window()?;

        trace!("window stack: {:#?}", self.window_stack);
        Ok(())
    }

    pub fn suspend_all_windows(&mut self) -> anyhow::Result<()> {
        for window in self.window_stack.iter_mut() {
            window.configure_suspended()?;
            self.output.leave(&window.surface);
        }

        Ok(())
    }

    pub fn activate_top_window(&mut self) -> anyhow::Result<()> {
        for window in self.window_stack.iter_mut().rev() {
            trace!("activating window: {:?}", window);

            window.configure_activated(
                self.display_params.width,
                self.display_params.height,
                self.display_params.ui_scale,
            )?;
            self.output.enter(&window.surface);

            // If the window is fullscreen, everything under it is invisible.
            if window.popup_bounds().is_none() {
                break;
            }
        }

        if let Some(top_window) = self.window_stack.last() {
            let text_input = smithay::wayland::text_input::TextInputSeat::text_input(&self.seat);

            text_input.leave();
            text_input.set_focus(Some(top_window.surface.clone()));
            text_input.enter();

            let kb = self.seat.get_keyboard().unwrap();
            kb.set_focus(
                self,
                Some(top_window.clone()),
                smithay::utils::SERIAL_COUNTER.next_serial(),
            );
        }

        Ok(())
    }

    pub fn window_at(
        &self,
        point: smithay::utils::Point<f64, smithay::utils::Physical>,
    ) -> Option<Window> {
        for window in self.window_stack.iter().rev() {
            if let Some(bounds) = window.popup_bounds() {
                if bounds.contains(point.to_i32_round()) {
                    return Some(window.clone());
                }
            } else {
                return Some(window.clone());
            }
        }

        None
    }

    // Returns an iterator over windows with any visible content, from bottom to top.
    pub fn visible_windows(&self) -> impl Iterator<Item = &Window> {
        let first_visible_idx = self
            .window_stack
            .iter()
            .rposition(|w| w.popup_bounds().is_none());

        if let Some(idx) = first_visible_idx {
            self.window_stack[idx..].iter()
        } else {
            self.window_stack.iter()
        }
    }
}
