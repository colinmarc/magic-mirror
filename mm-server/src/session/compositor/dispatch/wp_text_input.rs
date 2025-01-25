// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use wayland_protocols::wp::text_input::zv3::server::{
    zwp_text_input_manager_v3, zwp_text_input_v3,
};

use crate::session::compositor::Compositor;

impl wayland_server::GlobalDispatch<zwp_text_input_manager_v3::ZwpTextInputManagerV3, ()>
    for Compositor
{
    fn bind(
        _state: &mut Self,
        _handle: &wayland_server::DisplayHandle,
        _client: &wayland_server::Client,
        resource: wayland_server::New<zwp_text_input_manager_v3::ZwpTextInputManagerV3>,
        _global_data: &(),
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl wayland_server::Dispatch<zwp_text_input_manager_v3::ZwpTextInputManagerV3, ()> for Compositor {
    fn request(
        state: &mut Self,
        _client: &wayland_server::Client,
        _resource: &zwp_text_input_manager_v3::ZwpTextInputManagerV3,
        request: zwp_text_input_manager_v3::Request,
        _data: &(),
        _dhandle: &wayland_server::DisplayHandle,
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        match request {
            zwp_text_input_manager_v3::Request::GetTextInput { id, .. } => {
                let wp_text_input = data_init.init(id, ());

                state.default_seat.get_text_input(wp_text_input);
            }
            zwp_text_input_manager_v3::Request::Destroy => (),
            _ => (),
        }
    }
}

impl wayland_server::Dispatch<zwp_text_input_v3::ZwpTextInputV3, ()> for Compositor {
    fn request(
        _state: &mut Self,
        _client: &wayland_server::Client,
        _resource: &zwp_text_input_v3::ZwpTextInputV3,
        _request: zwp_text_input_v3::Request,
        _data: &(),
        _dhandle: &wayland_server::DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        resource: &zwp_text_input_v3::ZwpTextInputV3,
        _data: &(),
    ) {
        state.default_seat.destroy_text_input(resource);
    }
}
