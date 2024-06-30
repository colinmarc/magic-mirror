// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use wayland_protocols::wp::presentation_time::server::{wp_presentation, wp_presentation_feedback};
use wayland_server::Resource as _;

use crate::compositor::{surface::SurfaceKey, State};

impl wayland_server::GlobalDispatch<wp_presentation::WpPresentation, ()> for State {
    fn bind(
        _state: &mut Self,
        _handle: &wayland_server::DisplayHandle,
        _client: &wayland_server::Client,
        resource: wayland_server::New<wp_presentation::WpPresentation>,
        _global_data: &(),
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        let wp_presentation = data_init.init(resource, ());
        wp_presentation.clock_id(nix::time::ClockId::CLOCK_MONOTONIC.as_raw() as u32)
    }
}

impl wayland_server::Dispatch<wp_presentation::WpPresentation, ()> for State {
    fn request(
        state: &mut Self,
        _client: &wayland_server::Client,
        _resource: &wp_presentation::WpPresentation,
        request: wp_presentation::Request,
        _data: &(),
        _dhandle: &wayland_server::DisplayHandle,
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        match request {
            wp_presentation::Request::Feedback {
                surface,
                callback: id,
            } => {
                if let Some(surface_key) = surface.data::<SurfaceKey>() {
                    let wp_presentation_feedback = data_init.init(id, *surface_key);

                    // for wl_output in state.output_proxies.iter().filter(|wl_output| wl_output.id().same_client_as(surface.id())) {
                    //     wp_presentation_feedback.sync_output()
                    // }

                    let surface = state
                        .surfaces
                        .get_mut(*surface_key)
                        .expect("surface has no entry");

                    surface.pending_feedback = Some(wp_presentation_feedback);
                }
            }
            wp_presentation::Request::Destroy => (),
            _ => (),
        }
    }
}

impl wayland_server::Dispatch<wp_presentation_feedback::WpPresentationFeedback, SurfaceKey>
    for State
{
    fn request(
        _state: &mut Self,
        _client: &wayland_server::Client,
        _resource: &wp_presentation_feedback::WpPresentationFeedback,
        _request: wp_presentation_feedback::Request,
        _data: &SurfaceKey,
        _dhandle: &wayland_server::DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
    }
}
