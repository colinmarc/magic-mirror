// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use wayland_server::{
    protocol::{wl_keyboard, wl_pointer, wl_seat},
    Resource as _,
};

use crate::compositor::{seat::Cursor, State};

impl wayland_server::GlobalDispatch<wl_seat::WlSeat, ()> for State {
    fn bind(
        _state: &mut Self,
        _handle: &wayland_server::DisplayHandle,
        _client: &wayland_server::Client,
        resource: wayland_server::New<wl_seat::WlSeat>,
        _global_data: &(),
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        let wl_seat = data_init.init(resource, ());
        wl_seat.capabilities(wl_seat::Capability::Keyboard | wl_seat::Capability::Pointer);
    }
}

impl wayland_server::Dispatch<wl_seat::WlSeat, ()> for State {
    fn request(
        state: &mut Self,
        _client: &wayland_server::Client,
        resource: &wl_seat::WlSeat,
        request: wl_seat::Request,
        _data: &(),
        _dhandle: &wayland_server::DisplayHandle,
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        match request {
            wl_seat::Request::GetPointer { id } => {
                let wl_pointer = data_init.init(id, ());
                state.default_seat.get_pointer(wl_pointer);
            }
            wl_seat::Request::GetKeyboard { id } => {
                let wl_keyboard = data_init.init(id, ());
                state.default_seat.get_keyboard(wl_keyboard);
            }
            wl_seat::Request::GetTouch { .. } => {
                resource.post_error(
                    wl_seat::Error::MissingCapability,
                    "No touch capability advertized.",
                );
            }
            _ => (),
        }
    }
}

impl wayland_server::Dispatch<wl_pointer::WlPointer, ()> for State {
    fn request(
        state: &mut Self,
        _client: &wayland_server::Client,
        resource: &wl_pointer::WlPointer,
        request: wl_pointer::Request,
        _data: &(),
        _dhandle: &wayland_server::DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        match request {
            wl_pointer::Request::SetCursor {
                surface,
                hotspot_x,
                hotspot_y,
                ..
            } => {
                let hotspot_x = hotspot_x.max(0) as u32;
                let hotspot_y = hotspot_y.max(0) as u32;

                let cursor = if let Some(wl_surface) = surface {
                    Cursor::Surface {
                        surface: *wl_surface.data().unwrap(),
                        hotspot: (hotspot_x, hotspot_y).into(),
                        needs_render: true,
                        rendered: None,
                    }
                } else {
                    Cursor::Hidden
                };

                state.set_cursor(resource, cursor);
            }
            wl_pointer::Request::Release => (),
            _ => (),
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        resource: &wl_pointer::WlPointer,
        _data: &(),
    ) {
        state.default_seat.destroy_pointer(resource);
    }
}

impl wayland_server::Dispatch<wl_keyboard::WlKeyboard, ()> for State {
    fn request(
        _state: &mut Self,
        _client: &wayland_server::Client,
        _resource: &wl_keyboard::WlKeyboard,
        _request: wl_keyboard::Request,
        _data: &(),
        _dhandle: &wayland_server::DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        resource: &wl_keyboard::WlKeyboard,
        _data: &(),
    ) {
        state.default_seat.destroy_keyboard(resource);
    }
}
