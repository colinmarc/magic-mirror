// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use smithay::reexports::wayland_server::protocol::wl_seat;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::Serial;
use smithay::wayland::shell::xdg;
use tracing::trace;

use super::State;

impl xdg::XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut xdg::XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: xdg::ToplevelSurface) {
        trace!(
            wl_surface = surface.wl_surface().id().protocol_id(),
            toplevel = surface.xdg_toplevel().id().protocol_id(),
            "new toplevel xdg surface"
        );

        self.map_xdg(surface).unwrap();
    }

    fn new_popup(&mut self, _surface: xdg::PopupSurface, _positioner: xdg::PositionerState) {
        // TODO
    }

    fn grab(&mut self, _surface: xdg::PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {
        // Ignored
    }

    fn reposition_request(
        &mut self,
        _surface: xdg::PopupSurface,
        _positioner: xdg::PositionerState,
        _token: u32,
    ) {
        // Ignored
    }
}

// Xdg Shell
smithay::delegate_xdg_shell!(State);
