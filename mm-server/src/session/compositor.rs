// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{collections::BTreeMap, sync::Arc};

use protocols::*;
use slotmap::SlotMap;
use tracing::{debug, instrument, trace};
use wayland_protocols::{
    wp::{
        fractional_scale::v1::server::wp_fractional_scale_manager_v1,
        linux_dmabuf::zv1::server::zwp_linux_dmabuf_v1,
        linux_drm_syncobj::v1::server::wp_linux_drm_syncobj_manager_v1,
        pointer_constraints::zv1::server::zwp_pointer_constraints_v1,
        presentation_time::server::wp_presentation,
        relative_pointer::zv1::server::zwp_relative_pointer_manager_v1,
        text_input::zv3::server::zwp_text_input_manager_v3,
    },
    xdg::shell::server::xdg_wm_base,
    xwayland::shell::v1::server::xwayland_shell_v1,
};
use wayland_server::{
    protocol::{self, wl_output, wl_shm},
    Resource as _,
};

use crate::{
    session::{
        control::*,
        video::{self, TextureSync},
        SessionHandle,
    },
    vulkan::VkContext,
};

pub mod buffers;
mod dispatch;
mod oneshot_render;
mod output;
mod protocols;
mod sealed;
mod seat;
mod serial;
mod shm;
mod stack;
pub mod surface;
pub mod xwayland;

pub use seat::{ButtonState, KeyState};

use super::EPOCH;

pub struct Compositor {
    serial: serial::Serial,

    surfaces: SlotMap<surface::SurfaceKey, surface::Surface>,
    buffers: SlotMap<buffers::BufferKey, buffers::Buffer>,
    shm_pools: SlotMap<shm::ShmPoolKey, shm::ShmPool>,
    cached_dmabuf_feedback: buffers::CachedDmabufFeedback,
    imported_syncobj_timelines: SlotMap<buffers::SyncobjTimelineKey, buffers::SyncobjTimeline>,
    in_flight_buffers: Vec<surface::ContentUpdate>,
    pending_presentation_feedback: Vec<surface::PendingPresentationFeedback>,

    surface_stack: Vec<surface::SurfaceKey>,
    active_surface: Option<surface::SurfaceKey>,

    output_proxies: Vec<wl_output::WlOutput>,

    // TODO: one seat per operator
    pub default_seat: seat::Seat,

    display_params: DisplayParams,
    session_handle: SessionHandle,

    xwm: Option<xwayland::Xwm>,
    xwayland_surface_lookup: BTreeMap<u64, surface::SurfaceKey>,

    // At the bottom for drop order.
    vk: Arc<VkContext>,
}

impl Compositor {
    pub fn new(
        vk: Arc<VkContext>,
        handle: SessionHandle,
        display_params: DisplayParams,
    ) -> anyhow::Result<Self> {
        let cached_dmabuf_feedback = buffers::CachedDmabufFeedback::new(vk.clone())?;

        Ok(Self {
            serial: serial::Serial::new(),

            surfaces: SlotMap::default(),
            buffers: SlotMap::default(),
            shm_pools: SlotMap::default(),
            cached_dmabuf_feedback,
            imported_syncobj_timelines: SlotMap::default(),
            in_flight_buffers: Vec::new(),
            pending_presentation_feedback: Vec::new(),

            surface_stack: Vec::new(),
            active_surface: None,

            output_proxies: Vec::new(),

            default_seat: seat::Seat::default(),

            display_params,
            session_handle: handle.clone(),

            xwm: None,
            xwayland_surface_lookup: BTreeMap::default(),

            vk,
        })
    }

    pub fn update_display_params(
        &mut self,
        display_params: DisplayParams,
        active: bool,
    ) -> anyhow::Result<()> {
        let now = EPOCH.elapsed().as_millis() as u32;

        // Reconfigure all surfaces to be the right size.
        for surface in &self.surface_stack {
            let surf = &mut self.surfaces[*surface];

            let xwin = surf.role.current.as_ref().and_then(|role| {
                if let surface::SurfaceRole::XWayland { serial } = role {
                    self.xwm.as_ref().unwrap().xwindow_for_serial(*serial)
                } else {
                    None
                }
            });

            surf.reconfigure(display_params, xwin);

            if display_params.width != self.display_params.width
                || display_params.height != self.display_params.height
                || display_params.ui_scale != self.display_params.ui_scale
            {
                // Try to trick the surface into thinking it's moving to a
                // different monitor. This helps some games adjust to mode
                // changes.
                for wl_output in &self.output_proxies {
                    if wl_output.client() == surf.wl_surface.client() {
                        surf.wl_surface.leave(wl_output);
                        surf.wl_surface.enter(wl_output);
                    }
                }

                // Discharge any pending frame callbacks, since we won't
                // render the current content, and some clients get stuck
                // otherwise.
                if let Some(cb) = surf.frame_callback.current.take() {
                    cb.done(now);
                }
            }
        }

        self.update_focus_and_visibility(active)?;
        self.display_params = display_params;
        self.emit_output_params();

        Ok(())
    }

    #[instrument(skip_all)]
    pub fn composite_frame(
        &mut self,
        video_pipeline: &mut video::EncodePipeline,
    ) -> anyhow::Result<()> {
        let now = EPOCH.elapsed().as_millis() as u32;
        let ready = unsafe { video_pipeline.begin()? };
        if !ready {
            debug!("dropped frame because of backpressure");
            return Ok(());
        }

        // Iterate backwards to find the first fullscreen window.
        let first_visible_idx = self
            .surface_stack
            .iter()
            .rposition(|id| {
                self.surfaces[*id]
                    .configuration
                    .map_or(true, |conf| conf.fullscreen)
            })
            .unwrap_or_default();

        let num_surfaces = self.surface_stack.len() - first_visible_idx;
        let mut presentation_feedback = Vec::with_capacity(num_surfaces);

        for id in self.surface_stack[first_visible_idx..].iter() {
            let surface = &mut self.surfaces[*id];

            let conf = surface
                .configuration
                .expect("mapped surface has no configuration");

            let content = surface
                .content
                .as_mut()
                .expect("mapped surface has no content");

            let buffer = &mut self.buffers[content.buffer];

            let sync = match &mut buffer.backing {
                buffers::BufferBacking::Dmabuf { .. } => {
                    if let Some((acquire, _)) = content.explicit_sync.as_ref() {
                        Some(TextureSync::Explicit(acquire.clone()))
                    } else {
                        Some(TextureSync::ImplicitInterop)
                    }
                }
                _ => None,
            };

            unsafe { content.tp_done = video_pipeline.composite_surface(buffer, sync, conf)? };
            if let Some(callback) = surface.frame_callback.current.take().as_mut() {
                callback.done(now);
            }

            if let Some(fb) = content.wp_presentation_feedback.take() {
                presentation_feedback.push(fb);
            }

            trace!(?surface, ?conf, "compositing surface");
        }

        let tp_render = unsafe { video_pipeline.end_and_submit()? };
        for fb in presentation_feedback.drain(..) {
            self.pending_presentation_feedback
                .push(surface::PendingPresentationFeedback(fb, tp_render.clone()));
        }

        Ok(())
    }

    pub fn idle(&mut self, active: bool) -> anyhow::Result<()> {
        // Update the window stack, if it changed.
        self.update_focus_and_visibility(active)?;

        // Send any pending surface configures.
        self.configure_surfaces()?;

        // Check if the pointer is locked.
        self.update_pointer_lock();

        // Send pending pointer frames.
        self.default_seat.pointer_frame();

        // Release any unused buffers.
        self.release_buffers()?;

        // Send presentation feedback.
        self.send_presentation_feedback()?;

        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct ClientState {
    xwayland: bool,
}

impl wayland_server::backend::ClientData for ClientState {
    fn initialized(&self, _client_id: wayland_server::backend::ClientId) {}
    fn disconnected(
        &self,
        _client_id: wayland_server::backend::ClientId,
        _reason: wayland_server::backend::DisconnectReason,
    ) {
    }
}

pub fn create_globals(dh: &wayland_server::DisplayHandle) {
    create_global::<protocol::wl_compositor::WlCompositor>(dh, 6);
    create_global::<protocol::wl_output::WlOutput>(dh, 4);
    create_global::<xdg_wm_base::XdgWmBase>(dh, 6);
    create_global::<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1>(dh, 1);

    create_global::<protocol::wl_seat::WlSeat>(dh, 9);
    create_global::<protocol::wl_data_device_manager::WlDataDeviceManager>(dh, 3);
    create_global::<zwp_pointer_constraints_v1::ZwpPointerConstraintsV1>(dh, 1);
    create_global::<zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1>(dh, 1);
    create_global::<zwp_text_input_manager_v3::ZwpTextInputManagerV3>(dh, 1);

    create_global::<wl_shm::WlShm>(dh, 1);
    create_global::<zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1>(dh, 5);
    create_global::<wp_presentation::WpPresentation>(dh, 1);
    create_global::<wp_linux_drm_syncobj_manager_v1::WpLinuxDrmSyncobjManagerV1>(dh, 1);

    create_global::<xwayland_shell_v1::XwaylandShellV1>(dh, 1);
    create_global::<wl_drm::WlDrm>(dh, 2);
}

fn create_global<G: wayland_server::Resource + 'static>(
    dh: &wayland_server::DisplayHandle,
    version: u32,
) where
    Compositor: wayland_server::GlobalDispatch<G, ()>,
{
    let _ = dh.create_global::<Compositor, G, ()>(version, ());
}
