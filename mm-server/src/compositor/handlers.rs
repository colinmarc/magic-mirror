// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

mod buffers;
pub mod color_management;
mod input;
mod x11;
mod xdg_shell;

use super::{ClientState, State};
use smithay::{
    reexports::wayland_server::{self, protocol::wl_surface, Resource},
    wayland::{compositor, output},
    xwayland::{self, X11Wm},
};
use tracing::debug;

impl compositor::CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut compositor::CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(
        &self,
        client: &'a wayland_server::Client,
    ) -> &'a compositor::CompositorClientState {
        if let Some(state) = client.get_data::<xwayland::XWaylandClientData>() {
            return &state.compositor_state;
        }

        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &wl_surface::WlSurface) {
        X11Wm::commit_hook(self, surface);

        if compositor::is_sync_subsurface(surface) {
            debug!("skipping commit for sync subsurface: {:?}", surface.id());
            return;
        }

        let mut removed_surfaces = Vec::new();
        compositor::with_surface_tree_upward(
            surface,
            (),
            |_, _, _| compositor::TraversalAction::DoChildren(()),
            |surf, data, _| {
                let mut attrs = data.cached_state.current::<compositor::SurfaceAttributes>();

                match attrs.buffer.take() {
                    Some(compositor::BufferAssignment::Removed) => {
                        debug!(surface = surf.id().protocol_id(), "buffer removed");
                        removed_surfaces.push(surf.clone());
                    }
                    Some(compositor::BufferAssignment::NewBuffer(buffer)) => {
                        buffers::buffer_commit(self, surf, data, &buffer).unwrap();
                    }
                    None => (),
                };
            },
            |_, _, _| true,
        );

        for surf in removed_surfaces {
            self.texture_manager.remove_surface(&surf).unwrap();
            if !compositor::is_sync_subsurface(&surf) {
                self.unmap_window_for_surface(&surf).unwrap();
            }
        }
    }

    fn new_surface(&mut self, _surface: &wl_surface::WlSurface) {}

    fn destroyed(&mut self, surface: &wl_surface::WlSurface) {
        debug!("surface destroyed: {:?}", surface.id());
        self.texture_manager.remove_surface(surface).unwrap();
        if self.cursor_surface == Some(surface.clone()) {
            self.cursor_surface = None;
        }
    }
}

impl output::OutputHandler for State {}

smithay::delegate_compositor!(State);
smithay::delegate_output!(State);
