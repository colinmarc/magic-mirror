// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use tracing::{debug, trace, warn};
use wayland_server::Resource as _;

use crate::compositor::{
    buffers::BufferKey,
    surface::{self, buffer_vector_to_surface, SurfaceKey, SurfaceRole},
    State,
};

impl State {
    /// Displays the surface, if it has not yet been displayed.
    pub fn map_surface(&mut self, id: SurfaceKey, buffer_id: BufferKey) {
        if self.surface_stack.contains(&id) {
            return;
        }

        let surface = &self.surfaces[id];
        let config = surface.configuration.expect("mapping unconfigured surface");

        let buffer = &self.buffers[buffer_id];
        let buffer_size = buffer.dimensions();

        trace!(?surface, ?buffer_size, "mapping surface");
        if buffer_size != config.size {
            warn!(
                expected = ?(config.size.x, config.size.y),
                actual = ?(buffer_size.x, buffer_size.y),
                "unexpected buffer dimensions"
            );
        }

        for wl_output in self
            .output_proxies
            .iter()
            .filter(|wl_output| wl_output.id().same_client_as(&surface.wl_surface.id()))
        {
            surface.wl_surface.enter(wl_output);
        }

        trace!(?surface, "surface mapped");
        self.surface_stack.push(id);
    }

    /// Removes any configuration and attached buffer from a surface. This
    /// happens if a nil buffer is committed or the role object is destroyed
    /// by the client.
    pub fn unmap_surface(&mut self, id: SurfaceKey) {
        let surface = &mut self.surfaces[id];
        trace!(?surface, "surface unmapped");

        surface.content = None;
        surface.pending_configure = None;
        surface.configuration = None;
        surface.sent_configuration = None;

        self.surface_stack.retain(|v| *v != id);
    }

    /// Raises an X11 window to the top.
    pub fn raise_x11_surface(&mut self, serial: u64) {
        let stack_position = self
            .xwayland_surface_lookup
            .get(&serial)
            .and_then(|surface_id| self.surface_stack.iter().rposition(|id| surface_id == id));

        if let Some(pos) = stack_position {
            self.raise_surface_at(pos);
        }
    }

    fn raise_surface_at(&mut self, position: usize) {
        let id = self.surface_stack.remove(position);

        if tracing::event_enabled!(tracing::Level::TRACE) {
            trace!(surf = ?&self.surfaces[id], "raising surface");
        }

        self.surface_stack.push(id);
    }

    /// Updates focus and surface configurations based on any changes made to
    /// the stack order, mapping and unmapping of surfaces, etc.
    pub fn update_focus_and_visibility(&mut self, active: bool) -> anyhow::Result<()> {
        let top_surface = if active {
            self.surface_stack.last().cloned()
        } else {
            None
        };

        if top_surface == self.active_surface {
            return Ok(());
        }

        // Mark the old active surface as occluded.
        if let Some(conf) = self
            .active_surface
            .take()
            .and_then(|id| self.surfaces.get_mut(id))
            .and_then(|surf| surf.configuration.as_mut())
        {
            conf.visibility = surface::Visibility::Occluded;
        }

        if let Some(focus) = top_surface {
            let surf = &mut self.surfaces[focus];
            trace!(active, focus = ?surf, "setting focus");

            let conf = surf
                .configuration
                .as_mut()
                .expect("mapped surface with no configuration");
            let is_fullscreen = conf.fullscreen;
            conf.visibility = surface::Visibility::Active;

            self.active_surface = Some(focus);
            self.default_seat
                .set_keyboard_focus(&self.serial, Some(surf.wl_surface.clone()));

            // Xwayland maintains its own focus.
            if let Some(SurfaceRole::XWayland { serial }) = &surf.role.current {
                let xwm = self.xwm.as_mut().unwrap();
                let id = xwm.xwindow_for_serial(*serial).map(|xwin| xwin.id);
                xwm.set_focus(id)?;
            } else if let Some(xwm) = &mut self.xwm {
                // The xwayland window is occluded by a wayland window.
                xwm.set_focus(None)?;
            }

            trace!(?surf, depth = self.surface_stack.len(), "focus changed");

            // The surface under the cursor could be different from the top one.
            if let Some(coords) = self.default_seat.pointer_coords() {
                if let Some((pointer_focus, surface_coords)) = self.surface_under(coords) {
                    let wl_surface = self.surfaces[pointer_focus].wl_surface.clone();

                    self.default_seat.update_pointer(
                        &self.serial,
                        wl_surface,
                        surface_coords,
                        coords,
                    );
                }
            }

            // If the top window isn't covering the entire output, make sure we
            // uncover the windows below.
            if !is_fullscreen {
                for surface_id in self.surface_stack.iter().rev().skip(1) {
                    let conf = self.surfaces[*surface_id]
                        .configuration
                        .as_mut()
                        .expect("mapped surface with no configuration");

                    conf.visibility = surface::Visibility::Visible;
                    if conf.fullscreen {
                        break;
                    }
                }
            }
        } else {
            self.default_seat.set_keyboard_focus(&self.serial, None);
            self.default_seat.lift_pointer(&self.serial);

            if let Some(xwm) = &mut self.xwm {
                xwm.set_focus(None)?;
            }
        }

        Ok(())
    }

    pub fn surface_under(
        &mut self,
        coords: impl Into<glam::DVec2>,
    ) -> Option<(SurfaceKey, glam::DVec2)> {
        let coords = coords.into();

        for id in self.surface_stack.iter().rev() {
            let surf = &self.surfaces[*id];
            let fullscreen = surf.configuration.map_or(true, |conf| conf.fullscreen);

            if fullscreen || surf.contains(coords.round().as_uvec2()) {
                let conf = surf.configuration.unwrap();
                let coords = buffer_vector_to_surface(
                    coords - conf.topleft.as_dvec2(),
                    surf.effective_scale(),
                );

                return Some((*id, coords));
            }
        }

        None
    }

    /// Returns true if all visible surfaces is settled (with no configure
    /// pending) and has content.
    pub fn surfaces_ready(&self) -> bool {
        if self.surface_stack.is_empty() {
            return false;
        }

        // Iterate backwards to find the first fullscreen window.
        let first_visible_idx = self.surface_stack.iter().rposition(|id| {
            self.surfaces[*id]
                .configuration
                .map_or(false, |conf| conf.fullscreen)
        });

        for id in &self.surface_stack[first_visible_idx.unwrap_or_default()..] {
            let surf = &self.surfaces[*id];
            if surf.content.is_none() || surf.pending_configure.is_some() {
                debug!(
                    pending_attachments = self.pending_attachments.len(),
                    content_is_some = surf.content.is_some(),
                    pending_configure = ?surf.pending_configure,
                    "surface not ready!"
                );
                return false;
            }
        }

        true
    }
}
