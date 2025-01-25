// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use wayland_protocols::wp::fractional_scale::v1::server::{
    wp_fractional_scale_manager_v1, wp_fractional_scale_v1,
};
use wayland_server::Resource as _;

use crate::session::compositor::{surface::SurfaceKey, Compositor};

impl wayland_server::GlobalDispatch<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1, ()>
    for Compositor
{
    fn bind(
        _state: &mut Self,
        _handle: &wayland_server::DisplayHandle,
        _client: &wayland_server::Client,
        resource: wayland_server::New<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1>,
        _global_data: &(),
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl wayland_server::Dispatch<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1, ()>
    for Compositor
{
    fn request(
        state: &mut Self,
        _client: &wayland_server::Client,
        resource: &wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
        request: wp_fractional_scale_manager_v1::Request,
        _data: &(),
        _dhandle: &wayland_server::DisplayHandle,
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        match request {
            wp_fractional_scale_manager_v1::Request::GetFractionalScale { id, surface } => {
                if let Some(surface_key) = surface.data::<SurfaceKey>() {
                    let wp_fractional_scale = data_init.init(id, *surface_key);

                    let surface = state
                        .surfaces
                        .get_mut(*surface_key)
                        .expect("surface has no entry");

                    if surface.wp_fractional_scale.is_some() {
                        resource.post_error(
                            wp_fractional_scale_manager_v1::Error::FractionalScaleExists,
                            "wp_fractional_scale object already exists for surface.",
                        )
                    }

                    surface.wp_fractional_scale = Some(wp_fractional_scale);
                }
            }
            wp_fractional_scale_manager_v1::Request::Destroy => (),
            _ => unreachable!(),
        }
    }
}

impl wayland_server::Dispatch<wp_fractional_scale_v1::WpFractionalScaleV1, SurfaceKey>
    for Compositor
{
    fn request(
        _state: &mut Self,
        _client: &wayland_server::Client,
        _resource: &wp_fractional_scale_v1::WpFractionalScaleV1,
        _request: wp_fractional_scale_v1::Request,
        _data: &SurfaceKey,
        _dhandle: &wayland_server::DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
    }
}
