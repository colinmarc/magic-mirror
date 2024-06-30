// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use tracing::trace;

use crate::compositor::{
    buffers::BufferKey,
    surface::{buffer_vector_to_surface, SurfaceKey},
    DisplayParams, State,
};

#[derive(Default, Debug, Copy, Clone, Eq, PartialEq)]
pub struct Position {
    pub topleft: glam::UVec2,
    pub size: glam::UVec2,
}
impl Position {
    fn new(buffer_size: impl Into<glam::UVec2>, params: &DisplayParams) -> Self {
        let buffer_size: glam::UVec2 = buffer_size.into();
        let display_size: glam::UVec2 = (params.width, params.height).into();

        let size = buffer_size.min(display_size);
        let topleft = (display_size - size) / 2;

        Self { topleft, size }
    }

    pub fn is_fullscreen(&self, params: &DisplayParams) -> bool {
        self.topleft == glam::UVec2::ZERO && self.size == (params.width, params.height).into()
    }

    pub fn contains(&self, coords: impl Into<glam::UVec2>) -> bool {
        let coords = coords.into();
        let bottomright = self.topleft + self.size;

        coords.x >= self.topleft.x
            && coords.y >= self.topleft.y
            && coords.x < bottomright.x
            && coords.y < bottomright.y
    }
}

impl State {
    /// Displays the surface, if it has not yet been displayed.
    pub fn map_surface(&mut self, id: SurfaceKey, buffer_id: BufferKey) {
        let surface = self.surfaces.get(id).expect("surface has no entry");
        let buffer = self.buffers.get(buffer_id).expect("buffer has no entry");
        let buffer_size = buffer.dimensions();

        trace!(?surface, ?buffer_size, "mapping surface");

        match self.surface_positioning.get_mut(id) {
            Some(Position { topleft, size }) if *size != buffer_size => {
                let new_position = Position::new(buffer_size, &self.display_params);
                trace!(
                    old_position = ?topleft,
                    old_size = ?size,
                    new_position = ?new_position.topleft,
                    new_size = ?new_position.size,
                    "recentering surface"
                );

                *topleft = new_position.topleft;
                *size = new_position.size;
            }
            Some(_) => return,
            None => (), // New window, we need to map it.
        }

        trace!(?surface, ?buffer_size, "mapping surface");
        let position = Position::new(buffer_size, &self.display_params);

        self.surface_stack.push(id);
        self.surface_positioning.insert(id, position);
    }

    /// Removes any configuration and attached buffer from a surface. This happens if a nil buffer
    /// is committed or the role object is destroyed by the client.
    pub fn unmap_surface(&mut self, id: SurfaceKey) {
        let surface = self.surfaces.get_mut(id).expect("surface has no entry");
        trace!(?surface, "surface unmapped");

        surface.content = None;
        surface.pending_configure = None;
        surface.configuration = None;
        surface.sent_configuration = None;

        if self.surface_positioning.remove(id).is_some() {
            self.surface_stack.retain(|v| *v != id);
        }
    }

    /// Updates focus and surface configurations based on any changes made to
    /// the stack order, mapping and unmapping of surfaces, etc.
    pub fn update_focus_and_visibility(&mut self, active: bool) {
        let top_surface = if active {
            self.surface_stack.last().cloned()
        } else {
            None
        };

        if top_surface == self.focused_surface {
            return;
        }

        trace!(active, ?top_surface, "setting focus");

        if let Some(conf) = self
            .focused_surface
            .take()
            .and_then(|id| self.surfaces.get_mut(id))
            .and_then(|surf| surf.configuration.as_mut())
        {
            conf.active = false;
        }

        if let Some(focus) = top_surface {
            let surf = self.surfaces.get_mut(focus).expect("surface has no entry");

            surf.configuration
                .as_mut()
                .expect("mapped surface with no configuration")
                .active = true;

            // todo if surf.pointer_constraint...
            self.focused_surface = Some(focus);
            self.default_seat
                .set_keyboard_focus(&self.serial, Some(surf.wl_surface.clone()));

            // The surface under the cursor could be different from the top one.
            if let Some((pointer_focus, coords)) = self
                .pointer_coords
                .and_then(|coords| self.surface_under(coords))
            {
                let wl_surface = self
                    .surfaces
                    .get(pointer_focus)
                    .expect("surface has no entry")
                    .wl_surface
                    .clone();

                self.default_seat
                    .set_pointer(&self.serial, wl_surface, coords);
            }
        } else {
            self.default_seat.set_keyboard_focus(&self.serial, None);
            self.default_seat.lift_pointer(&self.serial);
        }
    }

    pub fn surface_under(
        &mut self,
        coords: impl Into<glam::DVec2>,
    ) -> Option<(SurfaceKey, glam::DVec2)> {
        let coords = coords.into();

        for id in self.surface_stack.iter().rev() {
            let position = self
                .surface_positioning
                .get(*id)
                .expect("surface in stack has no position");

            if position.is_fullscreen(&self.display_params)
                || position.contains(coords.round().as_uvec2())
            {
                let coords =
                    buffer_vector_to_surface(coords - position.topleft.as_dvec2(), self.ui_scale);

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
            self.surface_positioning
                .get(*id)
                .expect("surface in stack has no position")
                .is_fullscreen(&self.display_params)
        });

        for id in self.surface_stack[first_visible_idx.unwrap_or_default()..].iter() {
            let surf = self.surfaces.get(*id).expect("surface has no entry");

            if surf.content.is_none() || surf.pending_configure.is_some() {
                return false;
            }
        }

        true
    }
}
