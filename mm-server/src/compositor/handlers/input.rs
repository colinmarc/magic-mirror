// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use smithay::{
    input::pointer::PointerHandle, reexports::wayland_server::protocol::wl_surface,
    wayland::pointer_constraints,
};
use tracing::debug;

use crate::compositor::window;

use super::State;

impl smithay::input::SeatHandler for State {
    type KeyboardFocus = window::Window;
    type PointerFocus = window::Window;
    type TouchFocus = window::Window;

    fn seat_state(&mut self) -> &mut smithay::input::SeatState<State> {
        &mut self.seat_state
    }

    fn cursor_image(
        &mut self,
        _seat: &smithay::input::Seat<Self>,
        status: smithay::input::pointer::CursorImageStatus,
    ) {
        debug!(?status, "cursor image changed");
        self.new_cursor_status = Some(status);
    }

    fn focus_changed(
        &mut self,
        _seat: &smithay::input::Seat<Self>,
        _focused: Option<&Self::KeyboardFocus>,
    ) {
    }
}

impl pointer_constraints::PointerConstraintsHandler for State {
    fn new_constraint(&mut self, surface: &wl_surface::WlSurface, pointer: &PointerHandle<Self>) {
        pointer_constraints::with_pointer_constraint(surface, pointer, |constraint| {
            if let Some(constraint) = constraint {
                if let pointer_constraints::PointerConstraint::Locked(_) = *constraint {
                    if self.window_under_cursor().as_ref().map(|w| &w.surface) == Some(surface) {
                        constraint.activate();
                    }
                }
            }
        });
    }
}

smithay::delegate_seat!(State);
smithay::delegate_text_input_manager!(State);
smithay::delegate_relative_pointer!(State);
smithay::delegate_pointer_constraints!(State);
