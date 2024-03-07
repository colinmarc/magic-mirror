// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use smithay::wayland::xwayland_shell;
use smithay::{reexports::wayland_server::Resource, xwayland};
use tracing::{instrument, trace};

use crate::compositor::{window, State};

impl xwayland_shell::XWaylandShellHandler for State {
    fn xwayland_shell_state(&mut self) -> &mut xwayland_shell::XWaylandShellState {
        &mut self.xwayland_shell_state
    }

    fn surface_associated(
        &mut self,
        surface: smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        serial: u64,
    ) {
        trace!(
            surface = surface.id().protocol_id(),
            serial,
            "surface associated"
        );
    }
}

smithay::delegate_xwayland_shell!(State);

impl xwayland::XwmHandler for State {
    fn xwm_state(&mut self, _xwm: xwayland::xwm::XwmId) -> &mut xwayland::X11Wm {
        self.xwm.as_mut().unwrap()
    }

    fn new_window(&mut self, _xwm: xwayland::xwm::XwmId, window: xwayland::X11Surface) {
        trace!("new x11 window {:?}", window);
    }

    fn new_override_redirect_window(
        &mut self,
        _xwm: xwayland::xwm::XwmId,
        window: xwayland::X11Surface,
    ) {
        trace!("new override redirect window {:?}", window);
    }

    fn map_window_request(&mut self, _xwm: xwayland::xwm::XwmId, xwindow: xwayland::X11Surface) {
        map_window(self, xwindow).unwrap();
    }

    fn mapped_override_redirect_window(
        &mut self,
        _xwm: xwayland::xwm::XwmId,
        xwindow: xwayland::X11Surface,
    ) {
        map_window(self, xwindow).unwrap();
    }

    fn unmapped_window(&mut self, _xwm: xwayland::xwm::XwmId, xwindow: xwayland::X11Surface) {
        trace!("unmapped window {:?}", xwindow);
        if !xwindow.is_override_redirect() {
            xwindow.set_mapped(false).unwrap();
        }

        self.unmap_x11_window(&xwindow).unwrap();
    }

    fn destroyed_window(&mut self, _xwm: xwayland::xwm::XwmId, window: xwayland::X11Surface) {
        trace!("destroyed window: {:?}", window);
    }

    fn configure_request(
        &mut self,
        _xwm: xwayland::xwm::XwmId,
        window: xwayland::X11Surface,
        x: Option<i32>,
        y: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
        reorder: Option<xwayland::xwm::Reorder>,
    ) {
        trace!(?window, w, h, x, y, ?reorder, "configure request",);
        // Ignore.
    }

    fn configure_notify(
        &mut self,
        _xwm: xwayland::xwm::XwmId,
        window: xwayland::X11Surface,
        geometry: smithay::utils::Rectangle<i32, smithay::utils::Logical>,
        _above: Option<smithay::reexports::x11rb::protocol::xproto::Window>,
    ) {
        if let Some(window) = self.window_stack.iter_mut().find(|w| match w.ty {
            window::SurfaceType::X11Popup(ref x, _) => x == &window,
            _ => false,
        }) {
            window.bounds_changed(geometry);
        }
    }

    fn resize_request(
        &mut self,
        _xwm: xwayland::xwm::XwmId,
        _window: xwayland::X11Surface,
        _button: u32,
        _resize_edge: xwayland::xwm::ResizeEdge,
    ) {
        // Ignore.
    }

    fn move_request(
        &mut self,
        _xwm: xwayland::xwm::XwmId,
        _window: xwayland::X11Surface,
        _button: u32,
    ) {
        // Ignore.
    }
}

impl State {
    #[instrument(level = "trace", skip(self))]
    pub fn map_delayed_xwindows(&mut self) -> anyhow::Result<()> {
        let pending = self
            .xwindows_pending_map_on_surface
            .drain(..)
            .collect::<Vec<_>>();

        let mut remaining = Vec::new();
        for xwindow in pending {
            let surface = xwindow.wl_surface();

            if surface.is_some() {
                map_window(self, xwindow)?;
            } else {
                remaining.push(xwindow);
            }
        }

        self.xwindows_pending_map_on_surface = remaining;
        Ok(())
    }
}

#[instrument(level = "trace", skip_all)]
fn map_window(state: &mut State, xwindow: xwayland::X11Surface) -> anyhow::Result<()> {
    let surface = xwindow.wl_surface();

    // We can't map until a surface is attached, but we still have to call
    // set_mapped.
    if !xwindow.is_override_redirect() {
        xwindow.set_mapped(true)?;
    }

    if surface.is_none() {
        if !state.xwindows_pending_map_on_surface.contains(&xwindow) {
            state.xwindows_pending_map_on_surface.push(xwindow);
        }

        return Ok(());
    }

    trace!(
        wl_surface = surface.as_ref().map(|s| s.id().protocol_id()),
        is_override_redirect = xwindow.is_override_redirect(),
        "map x11surface"
    );

    state.map_x11(xwindow)
}
