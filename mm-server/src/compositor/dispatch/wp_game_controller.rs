// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use crate::compositor::{wp_game_controller::wp_game_controller_v1, State};

impl wayland_server::GlobalDispatch<wp_game_controller_v1::WpGameControllerV1, ()> for State {
    fn bind(
        state: &mut Self,
        _handle: &wayland_server::DisplayHandle,
        _client: &wayland_server::Client,
        resource: wayland_server::New<wp_game_controller_v1::WpGameControllerV1>,
        _global_data: &(),
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        let global = data_init.init(resource, ());
        state.default_seat.add_gamepad(global);
    }
}

impl wayland_server::Dispatch<wp_game_controller_v1::WpGameControllerV1, ()> for State {
    fn request(
        state: &mut Self,
        _client: &wayland_server::Client,
        resource: &wp_game_controller_v1::WpGameControllerV1,
        request: wp_game_controller_v1::Request,
        _data: &(),
        _dhandle: &wayland_server::DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        match request {
            wp_game_controller_v1::Request::Destroy => {
                state.default_seat.destroy_gamepad(resource);
            }
        }
    }
}
