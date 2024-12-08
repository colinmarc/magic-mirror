// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use tracing::error;
use wayland_protocols::wp::linux_drm_syncobj::v1::server::{
    wp_linux_drm_syncobj_manager_v1, wp_linux_drm_syncobj_surface_v1,
    wp_linux_drm_syncobj_timeline_v1,
};
use wayland_server::Resource as _;

use crate::{
    compositor::{
        buffers::{BufferTimeline, BufferTimelineKey},
        surface::SurfaceKey,
        State,
    },
    vulkan::VkTimelineSemaphore,
};

impl wayland_server::GlobalDispatch<wp_linux_drm_syncobj_manager_v1::WpLinuxDrmSyncobjManagerV1, ()>
    for State
{
    fn bind(
        _state: &mut Self,
        _handle: &wayland_server::DisplayHandle,
        _client: &wayland_server::Client,
        resource: wayland_server::New<wp_linux_drm_syncobj_manager_v1::WpLinuxDrmSyncobjManagerV1>,
        _global_data: &(),
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl wayland_server::Dispatch<wp_linux_drm_syncobj_manager_v1::WpLinuxDrmSyncobjManagerV1, ()>
    for State
{
    fn request(
        state: &mut Self,
        _client: &wayland_server::Client,
        resource: &wp_linux_drm_syncobj_manager_v1::WpLinuxDrmSyncobjManagerV1,
        request: wp_linux_drm_syncobj_manager_v1::Request,
        _data: &(),
        _dhandle: &wayland_server::DisplayHandle,
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        match request {
            wp_linux_drm_syncobj_manager_v1::Request::GetSurface { id, surface } => {
                if let Some(surface_key) = surface.data::<SurfaceKey>() {
                    let wp_syncobj_surface = data_init.init(id, *surface_key);

                    let surface = state
                        .surfaces
                        .get_mut(*surface_key)
                        .expect("surface has no entry");

                    if surface.wp_syncobj_surface.is_some() {
                        resource.post_error(
                            wp_linux_drm_syncobj_manager_v1::Error::SurfaceExists,
                            "A syncobj surface already exists for that wl_surface.",
                        );
                        return;
                    }

                    surface.wp_syncobj_surface = Some(wp_syncobj_surface);
                }
            }
            wp_linux_drm_syncobj_manager_v1::Request::ImportTimeline { id, fd } => {
                let sema = match VkTimelineSemaphore::from_syncobj_fd(state.vk.clone(), fd) {
                    Ok(t) => t,
                    Err(e) => {
                        error!("failed to import syncobj timeline: {e:#}");
                        resource.post_error(
                            wp_linux_drm_syncobj_manager_v1::Error::InvalidTimeline,
                            "Failed to import timeline.",
                        );
                        return;
                    }
                };

                state
                    .imported_buffer_timelines
                    .insert_with_key(|k| BufferTimeline {
                        _wp_syncobj_timeline: data_init.init(id, k),
                        sema,
                    });
            }
            wp_linux_drm_syncobj_manager_v1::Request::Destroy => (),
            _ => unreachable!(),
        }
    }
}

impl
    wayland_server::Dispatch<
        wp_linux_drm_syncobj_surface_v1::WpLinuxDrmSyncobjSurfaceV1,
        SurfaceKey,
    > for State
{
    fn request(
        state: &mut Self,
        _client: &wayland_server::Client,
        _resource: &wp_linux_drm_syncobj_surface_v1::WpLinuxDrmSyncobjSurfaceV1,
        request: wp_linux_drm_syncobj_surface_v1::Request,
        surface_key: &SurfaceKey,
        _dhandle: &wayland_server::DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        match request {
            wp_linux_drm_syncobj_surface_v1::Request::SetAcquirePoint {
                timeline,
                point_hi,
                point_lo,
            } => {
                let timeline = timeline
                    .data::<BufferTimelineKey>()
                    .and_then(|key| state.imported_buffer_timelines.get(*key))
                    .expect("timeline has no entry");

                let surface = state
                    .surfaces
                    .get_mut(*surface_key)
                    .expect("surface has no entry");

                surface.pending_acquire_point =
                    Some(timeline.sema.new_point(super::make_u64(point_hi, point_lo)))
            }
            wp_linux_drm_syncobj_surface_v1::Request::SetReleasePoint {
                timeline,
                point_hi,
                point_lo,
            } => {
                let timeline = timeline
                    .data::<BufferTimelineKey>()
                    .and_then(|key| state.imported_buffer_timelines.get(*key))
                    .expect("timeline has no entry");

                let surface = state
                    .surfaces
                    .get_mut(*surface_key)
                    .expect("surface has no entry");

                surface.pending_release_point =
                    Some(timeline.sema.new_point(super::make_u64(point_hi, point_lo)))
            }
            wp_linux_drm_syncobj_surface_v1::Request::Destroy => (),
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        _resource: &wp_linux_drm_syncobj_surface_v1::WpLinuxDrmSyncobjSurfaceV1,
        surface_key: &SurfaceKey,
    ) {
        if let Some(surface) = state.surfaces.get_mut(*surface_key) {
            surface.pending_acquire_point = None;
            surface.pending_release_point = None;
        }
    }
}

impl
    wayland_server::Dispatch<
        wp_linux_drm_syncobj_timeline_v1::WpLinuxDrmSyncobjTimelineV1,
        BufferTimelineKey,
    > for State
{
    fn request(
        _state: &mut Self,
        _client: &wayland_server::Client,
        _resource: &wp_linux_drm_syncobj_timeline_v1::WpLinuxDrmSyncobjTimelineV1,
        _request: wp_linux_drm_syncobj_timeline_v1::Request,
        _data: &BufferTimelineKey,
        _dhandle: &wayland_server::DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
    }
}
