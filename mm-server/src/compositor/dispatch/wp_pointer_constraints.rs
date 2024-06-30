// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use wayland_protocols::wp::pointer_constraints::zv1::server::{
    zwp_confined_pointer_v1, zwp_locked_pointer_v1, zwp_pointer_constraints_v1,
};
use wayland_server::Resource as _;

use crate::compositor::State;

impl wayland_server::GlobalDispatch<zwp_pointer_constraints_v1::ZwpPointerConstraintsV1, ()>
    for State
{
    fn bind(
        _state: &mut Self,
        _handle: &wayland_server::DisplayHandle,
        _client: &wayland_server::Client,
        resource: wayland_server::New<zwp_pointer_constraints_v1::ZwpPointerConstraintsV1>,
        _global_data: &(),
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl wayland_server::Dispatch<zwp_pointer_constraints_v1::ZwpPointerConstraintsV1, ()> for State {
    fn request(
        state: &mut Self,
        _client: &wayland_server::Client,
        resource: &zwp_pointer_constraints_v1::ZwpPointerConstraintsV1,
        request: zwp_pointer_constraints_v1::Request,
        _data: &(),
        _dhandle: &wayland_server::DisplayHandle,
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        match request {
            zwp_pointer_constraints_v1::Request::LockPointer {
                id,
                surface,
                pointer,
                lifetime,
                ..
            } => {
                if state.default_seat.has_lock(&surface) {
                    resource.post_error(
                        zwp_pointer_constraints_v1::Error::AlreadyConstrained,
                        "There already exists a pointer constraint for that surface on this seat.",
                    );
                    return;
                }

                let wp_locked_pointer = data_init.init(id, ());
                let oneshot = lifetime.into_result().ok()
                    == Some(zwp_pointer_constraints_v1::Lifetime::Oneshot);

                state
                    .default_seat
                    .create_lock(pointer, surface, wp_locked_pointer, oneshot);
            }
            zwp_pointer_constraints_v1::Request::ConfinePointer { id, .. } => {
                // We don't support confined pointers.
                data_init.init(id, ());
            }
            zwp_pointer_constraints_v1::Request::Destroy => (),
            _ => (),
        }
    }
}

impl wayland_server::Dispatch<zwp_locked_pointer_v1::ZwpLockedPointerV1, ()> for State {
    fn request(
        _state: &mut Self,
        _client: &wayland_server::Client,
        _resource: &zwp_locked_pointer_v1::ZwpLockedPointerV1,
        _request: zwp_locked_pointer_v1::Request,
        _data: &(),
        _dhandle: &wayland_server::DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        resource: &zwp_locked_pointer_v1::ZwpLockedPointerV1,
        _data: &(),
    ) {
        state.default_seat.destroy_lock(resource);
    }
}

impl wayland_server::Dispatch<zwp_confined_pointer_v1::ZwpConfinedPointerV1, ()> for State {
    fn request(
        _state: &mut Self,
        _client: &wayland_server::Client,
        _resource: &zwp_confined_pointer_v1::ZwpConfinedPointerV1,
        _request: zwp_confined_pointer_v1::Request,
        _data: &(),
        _dhandle: &wayland_server::DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
    }
}
