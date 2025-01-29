// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use tracing::error;
use wayland_protocols::wp::linux_drm_syncobj::v1::server::{
    wp_linux_drm_syncobj_manager_v1, wp_linux_drm_syncobj_surface_v1,
    wp_linux_drm_syncobj_timeline_v1,
};
use wayland_server::Resource as _;

use crate::session::compositor::{
    buffers::{SyncobjTimeline, SyncobjTimelineKey},
    surface::SurfaceKey,
    Compositor,
};

impl wayland_server::GlobalDispatch<wp_linux_drm_syncobj_manager_v1::WpLinuxDrmSyncobjManagerV1, ()>
    for Compositor
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
    for Compositor
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
                if let Err(err) = state.imported_syncobj_timelines.try_insert_with_key(|k| {
                    SyncobjTimeline::import(state.vk.clone(), data_init.init(id, k), fd)
                }) {
                    error!("failed to import syncobj timeline: {err:#}");
                    resource.post_error(
                        wp_linux_drm_syncobj_manager_v1::Error::InvalidTimeline,
                        "Failed to import timeline.",
                    );
                }
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
    > for Compositor
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
                    .data::<SyncobjTimelineKey>()
                    .and_then(|key| state.imported_syncobj_timelines.get(*key))
                    .expect("timeline has no entry");

                let surface = state
                    .surfaces
                    .get_mut(*surface_key)
                    .expect("surface has no entry");

                surface.pending_acquire_point =
                    Some(timeline.new_timeline_point(super::make_u64(point_hi, point_lo)))
            }
            wp_linux_drm_syncobj_surface_v1::Request::SetReleasePoint {
                timeline,
                point_hi,
                point_lo,
            } => {
                let timeline = timeline
                    .data::<SyncobjTimelineKey>()
                    .and_then(|key| state.imported_syncobj_timelines.get(*key))
                    .expect("timeline has no entry");

                let surface = state
                    .surfaces
                    .get_mut(*surface_key)
                    .expect("surface has no entry");

                surface.pending_release_point =
                    Some(timeline.new_timeline_point(super::make_u64(point_hi, point_lo)))
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
            surface.wp_syncobj_surface = None;
            surface.pending_acquire_point = None;
            surface.pending_release_point = None;
        }
    }
}

impl
    wayland_server::Dispatch<
        wp_linux_drm_syncobj_timeline_v1::WpLinuxDrmSyncobjTimelineV1,
        SyncobjTimelineKey,
    > for Compositor
{
    fn request(
        _state: &mut Self,
        _client: &wayland_server::Client,
        _resource: &wp_linux_drm_syncobj_timeline_v1::WpLinuxDrmSyncobjTimelineV1,
        _request: wp_linux_drm_syncobj_timeline_v1::Request,
        _data: &SyncobjTimelineKey,
        _dhandle: &wayland_server::DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
    }
}
