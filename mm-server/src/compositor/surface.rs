// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::time;

use tracing::{debug, trace, warn};
use wayland_protocols::{
    wp::presentation_time::server::wp_presentation_feedback,
    xdg::shell::server::{xdg_surface, xdg_toplevel},
};
use wayland_server::{
    protocol::{wl_callback, wl_surface},
    Resource as _,
};

use crate::{
    compositor::{
        buffers::{BufferBacking, BufferKey},
        xwayland, DisplayParams, State,
    },
    pixel_scale::PixelScale,
};

slotmap::new_key_type! { pub struct SurfaceKey; }

#[derive(Clone, Eq, PartialEq)]
pub struct Surface {
    pub wl_surface: wl_surface::WlSurface,

    pub pending_buffer: Option<PendingBuffer>,
    pub pending_feedback: Option<wp_presentation_feedback::WpPresentationFeedback>,
    pub frame_callback: DoubleBuffered<wl_callback::WlCallback>,
    pub buffer_scale: DoubleBuffered<PixelScale>,
    pub content: Option<ContentUpdate>,

    pub role: DoubleBuffered<SurfaceRole>,
    pub sent_configuration: Option<SurfaceConfiguration>,
    pub configuration: Option<SurfaceConfiguration>,
    pub pending_configure: Option<u32>,

    pub title: Option<String>,
    pub app_id: Option<String>,
}

impl Surface {
    pub fn new(wl_surface: wl_surface::WlSurface) -> Self {
        Self {
            wl_surface,

            pending_buffer: None,
            pending_feedback: None,
            frame_callback: DoubleBuffered::default(),
            buffer_scale: DoubleBuffered::default(),
            content: None,

            role: DoubleBuffered::default(),
            sent_configuration: None,
            configuration: None,
            pending_configure: None,

            title: None,
            app_id: None,
        }
    }

    pub fn reconfigure(&mut self, params: DisplayParams, xwin: Option<&xwayland::XWindow>) {
        // Keep current visibility, or start new windows visible.
        let visibility = self
            .configuration
            .map_or(Visibility::Visible, |c| c.visibility);

        let conf = match self.role.current {
            None | Some(SurfaceRole::Cursor) => None,
            Some(SurfaceRole::XdgToplevel { .. }) => Some(SurfaceConfiguration {
                topleft: glam::UVec2::ZERO,
                size: (params.width, params.height).into(),
                scale: params.ui_scale,
                visibility,
                fullscreen: true,
            }),
            Some(SurfaceRole::XWayland { .. }) => {
                match xwin {
                    None => None,
                    Some(xwayland::XWindow {
                        x,
                        y,
                        width,
                        height,
                        override_redirect,
                        ..
                    }) if *override_redirect => Some(SurfaceConfiguration {
                        topleft: (*x, *y).into(),
                        size: (*width, *height).into(),
                        scale: PixelScale::ONE,
                        visibility,
                        fullscreen: false,
                    }),
                    Some(_) => {
                        Some(SurfaceConfiguration {
                            topleft: glam::UVec2::ZERO,
                            size: (params.width, params.height).into(),
                            scale: PixelScale::ONE, // XWayland always uses scale one.
                            visibility,
                            fullscreen: true,
                        })
                    }
                }
            }
        };

        self.configuration = conf;
    }

    pub fn contains(&self, coords: impl Into<glam::UVec2>) -> bool {
        if let Some(conf) = self.configuration {
            let coords = coords.into();
            let bottomright = conf.topleft + conf.size;

            coords.x >= conf.topleft.x
                && coords.y >= conf.topleft.y
                && coords.x < bottomright.x
                && coords.y < bottomright.y
        } else {
            false
        }
    }

    pub fn effective_scale(&self) -> PixelScale {
        self.buffer_scale.current.unwrap_or_default()
    }
}

impl std::fmt::Debug for Surface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = self
            .title
            .as_ref()
            .or(self.app_id.as_ref())
            .map(|s| s.as_str())
            .unwrap_or("Untitled");

        let (role, id, extra) = match &self.role.current {
            None => ("wl_surface", self.wl_surface.id().protocol_id() as u64, ""),
            Some(SurfaceRole::Cursor) => (
                "wl_surface",
                self.wl_surface.id().protocol_id() as u64,
                " [CURSOR]",
            ),
            Some(SurfaceRole::XdgToplevel { xdg_toplevel, .. }) => {
                ("xdg_toplevel", xdg_toplevel.id().protocol_id() as u64, "")
            }
            Some(SurfaceRole::XWayland { serial }) => ("xwayland", *serial, ""),
        };

        write!(f, "<{:?} {}@{}{}>", name, role, id, extra)?;

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
    XWayland {
        serial: u64,
    },
    Cursor,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Visibility {
    Occluded,
    Visible,
    Active,
}

/// The configuration to be sent to the surface.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct SurfaceConfiguration {
    // x, y, width, and height are in the "physical" coordinate space. x and y
    // are not relevant to xdg_shell surfaces.
    pub topleft: glam::UVec2,
    pub size: glam::UVec2,
    pub scale: PixelScale,
    pub fullscreen: bool,
    pub visibility: Visibility,
}

impl SurfaceConfiguration {}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum PendingBuffer {
    Attach(BufferKey),
    Detach,
}

#[derive(Clone, Eq, PartialEq)]
pub struct ContentUpdate {
    pub buffer: BufferKey,
    pub wp_presentation_feedback: Option<wp_presentation_feedback::WpPresentationFeedback>,
}

impl Drop for ContentUpdate {
    fn drop(&mut self) {
        if let Some(feedback) = self.wp_presentation_feedback.take() {
            feedback.discarded();
        }
    }
}

pub struct CommitError(pub xdg_surface::Error, pub String);

impl State {
    /// Handles wl_surface.commit.
    pub fn surface_commit(&mut self, id: SurfaceKey) -> Result<(), CommitError> {
        let display_params = self.effective_display_params();
        let surface = &mut self.surfaces[id];

        // Buffer swap happens first. We handle it a bit differently because
        // buffers can be removed, not just overwritten.
        let mut feedback = surface.pending_feedback.take();
        match surface.pending_buffer.take() {
            Some(PendingBuffer::Detach) => {
                self.unmap_surface(id);
                return Ok(());
            }
            Some(PendingBuffer::Attach(buffer_id)) => {
                // Creates a content update.
                let buffer = &mut self.buffers[buffer_id];

                // If we haven't yet sent a configure, it's an error to
                // manipulate a buffer.
                if (matches!(surface.role.current, Some(SurfaceRole::XdgToplevel { .. }))
                    && surface.sent_configuration.is_none())
                    || surface.role.pending.is_some()
                {
                    return Err(CommitError(
                        xdg_surface::Error::UnconfiguredBuffer,
                        "The buffer must be configured prior to attaching a buffer.".to_string(),
                    ));
                }

                // If we're waiting on an ack_configure, poke the client again.
                if surface.pending_configure.is_some() {
                    debug!(pending_configure = ?surface.pending_configure, "pending configure, resending frame callback");
                    if let Some(fb) = feedback.take() {
                        fb.discarded();
                    }

                    if let Some(cb) = surface.frame_callback.pending.take() {
                        cb.done(self.serial.next());
                    }
                }

                buffer.needs_release = true;
                surface.content = Some(ContentUpdate {
                    buffer: buffer_id,
                    wp_presentation_feedback: feedback,
                });

                // In the case of shm buffer, we do a copy and immediately release it.
                if let BufferBacking::Shm {
                    staging_buffer,
                    format,
                    pool,
                    dirty,
                    ..
                } = &mut buffer.backing
                {
                    let len = (format.stride * format.height) as usize;
                    let pool = pool.read().unwrap();
                    let contents = pool.data(format.offset as usize, len);

                    staging_buffer.copy_from_slice(contents);
                    *dirty = true;
                    buffer.needs_release = false;
                    buffer.wl_buffer.release();
                }
            }
            None => (),
        }

        // Configure surfaces which have a newly applied role.
        match surface.role.promote() {
            CommitResult::Replaced(_, _) => panic!("surface already has a role"),
            CommitResult::Added(role) => {
                let xwin = if let SurfaceRole::XWayland { serial } = role {
                    self.xwayland_surface_lookup.insert(serial, id);
                    self.xwm.as_ref().unwrap().xwindow_for_serial(serial)
                } else {
                    None
                };

                surface.reconfigure(display_params, xwin);
            }
            _ => (),
        }

        surface.buffer_scale.promote();
        surface.frame_callback.promote();

        trace!(?surface, "surface commit");

        // Map the surface, if we've fulfilled all requirements.
        let is_mappable = match surface.role.current {
            None | Some(SurfaceRole::Cursor) => false,
            Some(SurfaceRole::XdgToplevel { .. }) => {
                surface.pending_configure.is_none() && surface.content.is_some()
            }
            Some(SurfaceRole::XWayland { serial }) => {
                if surface.content.is_none() {
                    false
                } else if let Some(xwin) = self.xwm.as_mut().unwrap().xwindow_for_serial(serial) {
                    // Copy over title and app_id.
                    surface.title = xwin.title.clone();
                    surface.app_id = xwin.app_id.clone();

                    xwin.mapped
                } else {
                    false
                }
            }
        };

        if is_mappable {
            if let Some(ContentUpdate { buffer, .. }) = surface.content {
                self.map_surface(id, buffer);
            }
        }

        Ok(())
    }

    /// Cleans up for a surface destroyed by the client.
    pub fn surface_destroyed(&mut self, id: SurfaceKey) {
        self.unmap_surface(id);

        let surf = self.surfaces.remove(id);
        if let Some(SurfaceRole::XWayland { serial }) = surf.and_then(|s| s.role.current) {
            self.xwayland_surface_lookup.remove(&serial);
        }
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

    /// Checks if any surfaces have outdated configuration, and sends a
    /// configure event.
    pub fn configure_surfaces(&mut self) -> anyhow::Result<()> {
        for (_id, surface) in self.surfaces.iter_mut() {
            if surface.configuration.is_none()
                || surface.configuration == surface.sent_configuration
            {
                continue;
            }

            trace!(?surface, conf = ?surface.configuration, "configuring surface");

            let conf = surface.configuration.unwrap();
            match &surface.role.current {
                None => panic!("surface configured without role"),
                Some(SurfaceRole::XdgToplevel {
                    xdg_surface,
                    xdg_toplevel,
                }) => {
                    if conf.scale.is_fractional() {
                        warn!(
                            scale = ?conf.scale,
                            "fractional scale not supported, using next integer"
                        )
                    }

                    let scale = conf.scale.ceil();
                    if surface.wl_surface.version() >= 6 {
                        let scale: f64 = scale.into();
                        surface.wl_surface.preferred_buffer_scale(scale as i32);
                    }

                    let mut states = match conf.visibility {
                        Visibility::Occluded if xdg_toplevel.version() >= 6 => {
                            vec![xdg_toplevel::State::Suspended]
                        }
                        Visibility::Occluded => vec![],
                        Visibility::Visible => vec![],
                        Visibility::Active => vec![xdg_toplevel::State::Activated],
                    };

                    if conf.fullscreen {
                        states.push(xdg_toplevel::State::Fullscreen);
                    }

                    let raw_states = states
                        .into_iter()
                        .flat_map(|st| {
                            let v: u32 = st.into();
                            v.to_ne_bytes()
                        })
                        .collect::<Vec<u8>>();

                    // Wayland wants the "logical" width and height to be
                    // pre-scaling. That means if we want a 1200x600 buffer
                    // at 2x ui scale, we need to configure it for 600x300.
                    let scaled: glam::IVec2 = buffer_vector_to_surface(conf.size, scale).as_ivec2();

                    let serial = self.serial.next();
                    xdg_toplevel.configure(scaled.x, scaled.y, raw_states);
                    xdg_surface.configure(serial);

                    surface.sent_configuration = Some(conf);
                    surface.pending_configure = Some(serial);
                }
                Some(SurfaceRole::XWayland { serial }) => {
                    let xwm = self.xwm.as_mut().unwrap();
                    match xwm.xwindow_for_serial(*serial) {
                        Some(xwayland::XWindow {
                            id,
                            override_redirect,
                            ..
                        }) if !override_redirect => {
                            xwm.configure_window(*id, conf)?;
                        }
                        _ => (),
                    }

                    surface.sent_configuration = Some(conf);
                }
                Some(SurfaceRole::Cursor) => unreachable!(),
            }
        }

        Ok(())
    }

    /// Sends complete presentation feedback. Note that since this is called as
    /// an idle operation, the timestamps are only accurate if the compositor
    /// thread is woken within a reasonable timeframe.
    pub fn send_presentation_feedback(&mut self) -> anyhow::Result<()> {
        let time = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
        let tv_sec_hi = (time.tv_sec >> 32) as u32;
        let tv_sec_lo = (time.tv_sec & 0xFFFFFFFF) as u32;
        let tv_nsec = time.tv_nsec as u32;

        let framerate = self.effective_display_params().framerate;
        let refresh = time::Duration::from_secs_f64(1.0 / framerate as f64).as_nanos() as u32;

        let mut still_pending = Vec::with_capacity(self.pending_presentation_feedback.len());
        for (fb, tp) in self.pending_presentation_feedback.drain(..) {
            if unsafe { !tp.poll()? } {
                still_pending.push((fb, tp));
                continue;
            }

            for wl_output in self
                .output_proxies
                .iter()
                .filter(|wl_output| wl_output.id().same_client_as(&fb.id()))
            {
                fb.sync_output(wl_output);
            }

            fb.presented(
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
                refresh,
                0, // seq_hi
                0, // seq_lo
                wp_presentation_feedback::Kind::empty(),
            );
        }

        self.pending_presentation_feedback = still_pending;
        Ok(())
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
