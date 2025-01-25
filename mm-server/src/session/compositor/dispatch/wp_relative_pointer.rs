// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use wayland_protocols::wp::relative_pointer::zv1::server::{
    zwp_relative_pointer_manager_v1, zwp_relative_pointer_v1,
};

use crate::session::compositor::Compositor;

impl
    wayland_server::GlobalDispatch<zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1, ()>
    for Compositor
{
    fn bind(
        _state: &mut Self,
        _handle: &wayland_server::DisplayHandle,
        _client: &wayland_server::Client,
        resource: wayland_server::New<zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1>,
        _global_data: &(),
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl wayland_server::Dispatch<zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1, ()>
    for Compositor
{
    fn request(
        state: &mut Self,
        _client: &wayland_server::Client,
        _resource: &zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1,
        request: zwp_relative_pointer_manager_v1::Request,
        _data: &(),
        _dhandle: &wayland_server::DisplayHandle,
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        match request {
            zwp_relative_pointer_manager_v1::Request::GetRelativePointer { id, pointer } => {
                let wp_relative_pointer = data_init.init(id, ());

                state
                    .default_seat
                    .get_relative_pointer(wp_relative_pointer, pointer);
            }
            zwp_relative_pointer_manager_v1::Request::Destroy => (),
            _ => (),
        }
    }
}

impl wayland_server::Dispatch<zwp_relative_pointer_v1::ZwpRelativePointerV1, ()> for Compositor {
    fn request(
        _state: &mut Self,
        _client: &wayland_server::Client,
        _resource: &zwp_relative_pointer_v1::ZwpRelativePointerV1,
        _request: zwp_relative_pointer_v1::Request,
        _data: &(),
        _dhandle: &wayland_server::DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        resource: &zwp_relative_pointer_v1::ZwpRelativePointerV1,
        _data: &(),
    ) {
        state.default_seat.destroy_relative_pointer(resource);
    }
}
