// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use wayland_server::protocol::wl_output;

use crate::compositor::{output::configure_output, State};

impl wayland_server::GlobalDispatch<wl_output::WlOutput, ()> for State {
    fn bind(
        state: &mut Self,
        _handle: &wayland_server::DisplayHandle,
        _client: &wayland_server::Client,
        resource: wayland_server::New<wl_output::WlOutput>,
        _global_data: &(),
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        let wl_output = data_init.init(resource, ());
        configure_output(&wl_output, state.effective_display_params());

        state.output_proxies.push(wl_output);
    }
}

impl wayland_server::Dispatch<wl_output::WlOutput, ()> for State {
    fn request(
        _state: &mut Self,
        _client: &wayland_server::Client,
        _resource: &wl_output::WlOutput,
        _request: wl_output::Request,
        _data: &(),
        _dhandle: &wayland_server::DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        resource: &wl_output::WlOutput,
        _data: &(),
    ) {
        state.output_proxies.retain(|o| o == resource);
    }
}
