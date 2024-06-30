// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use tracing::trace;
use wayland_protocols::xwayland::shell::v1::server::{xwayland_shell_v1, xwayland_surface_v1};
use wayland_server::Resource as _;

use crate::compositor::{
    surface::{SurfaceKey, SurfaceRole},
    ClientState, State,
};

impl wayland_server::GlobalDispatch<xwayland_shell_v1::XwaylandShellV1, ()> for State {
    fn bind(
        _state: &mut Self,
        _handle: &wayland_server::DisplayHandle,
        _client: &wayland_server::Client,
        resource: wayland_server::New<xwayland_shell_v1::XwaylandShellV1>,
        _global_data: &(),
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: wayland_server::Client, _global_data: &()) -> bool {
        client
            .get_data::<ClientState>()
            .map(|data| data.xwayland)
            .unwrap_or_default()
    }
}

impl wayland_server::Dispatch<xwayland_shell_v1::XwaylandShellV1, ()> for State {
    fn request(
        _state: &mut Self,
        _client: &wayland_server::Client,
        _resource: &xwayland_shell_v1::XwaylandShellV1,
        request: xwayland_shell_v1::Request,
        _data: &(),
        _dhandle: &wayland_server::DisplayHandle,
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        match request {
            xwayland_shell_v1::Request::GetXwaylandSurface { id, surface } => {
                let surface_id = surface
                    .data::<SurfaceKey>()
                    .expect("surface has no userdata");

                data_init.init(id, *surface_id);
            }
            xwayland_shell_v1::Request::Destroy => (),
            _ => unreachable!(),
        }
    }
}

impl wayland_server::Dispatch<xwayland_surface_v1::XwaylandSurfaceV1, SurfaceKey> for State {
    fn request(
        state: &mut Self,
        _client: &wayland_server::Client,
        resource: &xwayland_surface_v1::XwaylandSurfaceV1,
        request: xwayland_surface_v1::Request,
        data: &SurfaceKey,
        _dhandle: &wayland_server::DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        match request {
            xwayland_surface_v1::Request::SetSerial {
                serial_lo,
                serial_hi,
            } => {
                let serial = super::make_u64(serial_hi, serial_lo);
                trace!(serial, "associating xwindow with surface");

                if !state.set_surface_role(*data, SurfaceRole::XWayland { serial }) {
                    resource.post_error(
                        xwayland_shell_v1::Error::Role,
                        "Surface already has a role.",
                    );
                }
            }
            xwayland_surface_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}
