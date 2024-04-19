// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

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

smithay::delegate_seat!(State);
smithay::delegate_text_input_manager!(State);
