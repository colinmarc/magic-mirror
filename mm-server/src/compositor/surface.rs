// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use tracing::trace;
use wayland_protocols::xdg::shell::server::{xdg_surface, xdg_toplevel};
use wayland_server::{
    protocol::{wl_callback, wl_surface},
    Resource as _,
};

use crate::{
    compositor::buffers::{import_buffer, BufferKey},
    compositor::State,
    pixel_scale::PixelScale,
};

slotmap::new_key_type! { pub struct SurfaceKey; }

#[derive(Clone, Eq, PartialEq)]
pub struct Surface {
    pub wl_surface: wl_surface::WlSurface,

    pub pending_buffer: Option<PendingBuffer>,
    pub pending_frame_callback: Option<wl_callback::WlCallback>,
    pub content: Option<ContentUpdate>,

    pub role: DoubleBuffered<SurfaceRole>,
    pub sent_configuration: Option<SurfaceConfiguration>,
    pub configuration: Option<SurfaceConfiguration>,
    pub pending_configure: Option<u32>,

    pub xdg_title: Option<String>,
    pub xdg_app_id: Option<String>,
}

impl Surface {
    pub fn new(wl_surface: wl_surface::WlSurface) -> Self {
        Self {
            wl_surface,

            pending_buffer: None,
            pending_frame_callback: None,
            content: None,

            role: DoubleBuffered::default(),
            sent_configuration: None,
            configuration: None,
            pending_configure: None,

            xdg_title: None,
            xdg_app_id: None,
        }
    }

    fn is_mappable(&self) -> bool {
        match self.role.current {
            None => false,
            Some(SurfaceRole::XdgToplevel { .. }) => {
                self.pending_configure.is_none() && self.content.is_some()
            }
        }
    }
}

impl std::fmt::Debug for Surface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = self
            .xdg_title
            .as_ref()
            .or(self.xdg_app_id.as_ref())
            .map(|s| s.as_str())
            .unwrap_or("Untitled");

        let (role, wayland_id) = match &self.role.current {
            None => ("wl_surface", self.wl_surface.id().protocol_id()),
            Some(SurfaceRole::XdgToplevel { xdg_toplevel, .. }) => {
                ("xdg_toplevel", xdg_toplevel.id().protocol_id())
            }
        };

        write!(f, "<{:?} {}@{}>", name, role, wayland_id)?;

        Ok(())
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DoubleBuffered<T: Clone + Eq + PartialEq> {
    pub pending: Option<T>,
    pub current: Option<T>,
}

impl<T: Clone + Eq + PartialEq> Default for DoubleBuffered<T> {
    fn default() -> Self {
        Self {
            pending: None,
            current: None,
        }
    }
}

#[derive(Debug)]
pub enum CommitResult<T> {
    NoChange,
    Added(T),
    Replaced(T, T),
}

impl<T: Clone + Eq + PartialEq> DoubleBuffered<T> {
    pub fn promote(&mut self) -> CommitResult<T> {
        if self.pending.is_none() || self.pending == self.current {
            self.pending = None;
            return CommitResult::NoChange;
        }

        match (self.pending.take(), self.current.take()) {
            (Some(v), None) => {
                self.current = Some(v.clone());
                CommitResult::Added(v)
            }
            (Some(new), Some(old)) if new != old => {
                self.current = Some(new.clone());
                CommitResult::Replaced(old, new)
            }
            _ => unreachable!(),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum SurfaceRole {
    XdgToplevel {
        xdg_surface: xdg_surface::XdgSurface,
        xdg_toplevel: xdg_toplevel::XdgToplevel,
    },
}

/// The configuration to be sent to the surface.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct SurfaceConfiguration {
    // width and height are in the "physical" coordinate space.
    pub width: u32,
    pub height: u32,
    pub scale: PixelScale,
    pub active: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum PendingBuffer {
    Attach(BufferKey),
    Detach,
}

#[derive(Clone, Eq, PartialEq)]
pub struct ContentUpdate {
    pub buffer: BufferKey,
    pub frame_callback: Option<wl_callback::WlCallback>,
}

pub struct CommitError(pub xdg_surface::Error, pub String);

impl State {
    /// Handles wl_surface.commit.
    pub fn surface_commit(&mut self, id: SurfaceKey) -> Result<(), CommitError> {
        let surface = self.surfaces.get_mut(id).expect("surface has no entry");

        // Buffer swap happens first. We handle it a bit differently because
        // buffers can be removed, not just overwritten.
        match surface.pending_buffer.take() {
            Some(PendingBuffer::Detach) => {
                self.unmap_surface(id);
                return Ok(());
            }
            Some(PendingBuffer::Attach(buffer_id)) => {
                // Creates a content update.
                let buffer = self
                    .buffers
                    .get_mut(buffer_id)
                    .expect("buffer has no entry");

                // If we haven't yet sent a configure, it's an error to
                // manipulate a buffer.
                if (matches!(surface.role.current, Some(SurfaceRole::XdgToplevel { .. }))
                    && surface.sent_configuration.is_none())
                    || surface.role.pending.is_some()
                {
                    trace!(?surface.sent_configuration, ?surface.configuration, ?surface.role, "test");
                    return Err(CommitError(
                        xdg_surface::Error::UnconfiguredBuffer,
                        "The buffer must be configured prior to attaching a buffer.".to_string(),
                    ));
                }

                // let conf = surface
                //     .configuration
                //     .expect("buffer attached to non-configured surface");
                //
                // let (buffer_width, buffer_height) = buffer.dimensions();
                // if buffer_width != conf.width || buffer_height != conf.height {
                //     warn!(
                //         buffer_width,
                //         buffer_height,
                //         ?conf,
                //         "buffer dimensions don't match surface configuration"
                //     );
                // }

                buffer.needs_release = true;
                surface.content = Some(ContentUpdate {
                    buffer: buffer_id,
                    frame_callback: surface.pending_frame_callback.take(),
                });

                import_buffer(self.vk.clone(), buffer_id, buffer, &mut self.textures)
                    .expect("importing buffer failed");
            }
            None => (),
        }

        // Configure surfaces which have a newly applied role.
        match surface.role.promote() {
            CommitResult::Replaced(_, _) => panic!("surface already has a role"),
            CommitResult::Added(new_role) => match new_role {
                SurfaceRole::XdgToplevel { .. } => {
                    let config = SurfaceConfiguration {
                        width: self.display_params.width,
                        height: self.display_params.height,
                        scale: self.ui_scale,
                        active: !self.attachments.is_empty(),
                    };

                    surface.configuration = Some(config);
                }
            },
            _ => (),
        }

        trace!(?surface, "surface commit");

        // Map the surface, if we've fulfilled all requirements.
        if surface.is_mappable() {
            if let Some(ContentUpdate { buffer, .. }) = surface.content {
                self.map_surface(id, buffer);
            }
        }

        Ok(())
    }

    /// Cleans up for a surface destroyed by the client.
    pub fn surface_destroyed(&mut self, id: SurfaceKey) {
        self.unmap_surface(id);
        self.surfaces.remove(id);
    }

    /// Sets a pending role for the surface. Returns false if the surface
    /// already has a role or no longer exists.
    pub fn set_surface_role(&mut self, id: SurfaceKey, role: SurfaceRole) -> bool {
        match self.surfaces.get_mut(id) {
            Some(ref mut surf) if surf.role.current.is_none() => {
                surf.role.pending = Some(role);
                true
            }
            _ => false,
        }
    }

    /// Checks if any surfaces have outdated configuration, and sends a configure event.
    pub fn configure_surfaces(&mut self) {
        for (_id, surface) in self.surfaces.iter_mut() {
            if surface.configuration.is_none()
                || surface.configuration == surface.sent_configuration
            {
                continue;
            }

            trace!(?surface.configuration, "reconfiguring surface");

            let conf = surface.configuration.unwrap();
            match &surface.role.current {
                None => panic!("surface configured without role"),
                Some(SurfaceRole::XdgToplevel {
                    xdg_surface,
                    xdg_toplevel,
                }) => {
                    if conf.scale.is_fractional() {
                        todo!()
                    } else if surface.wl_surface.version() >= 6 {
                        let scale: u32 = conf.scale.try_into().unwrap();
                        surface.wl_surface.preferred_buffer_scale(scale as i32);
                    }

                    let states = if conf.active {
                        vec![
                            xdg_toplevel::State::Activated,
                            xdg_toplevel::State::Fullscreen,
                        ]
                    } else if xdg_toplevel.version() >= 6 {
                        vec![xdg_toplevel::State::Suspended]
                    } else {
                        vec![]
                    };

                    let raw_states = states
                        .into_iter()
                        .flat_map(|st| {
                            let v: u32 = st.into();
                            v.to_ne_bytes()
                        })
                        .collect::<Vec<u8>>();

                    let conf_size: glam::UVec2 = (conf.width, conf.height).into();
                    let scaled: glam::IVec2 =
                        buffer_vector_to_surface(conf_size, conf.scale).as_ivec2();

                    let serial = self.serial.next();
                    xdg_toplevel.configure(scaled.x, scaled.y, raw_states);
                    xdg_surface.configure(serial);

                    surface.sent_configuration = Some(conf);
                    surface.pending_configure = Some(serial);
                }
            }
        }
    }
}

/// Converts a vector of pixels into surface-local or "logical" coordinates
/// as wayland expects them.
pub fn buffer_vector_to_surface(coords: impl Into<glam::DVec2>, scale: PixelScale) -> glam::DVec2 {
    let scale: f64 = scale.into();
    coords.into() / scale
}

/// Converts a surface-local vector (sometimes called "logical" coordinates)
/// into pixels.
pub fn surface_vector_to_buffer(coords: impl Into<glam::DVec2>, scale: PixelScale) -> glam::DVec2 {
    let scale: f64 = scale.into();
    coords.into() * scale
}
