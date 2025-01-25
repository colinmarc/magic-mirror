// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use wayland_protocols::xdg::shell::server::{
    xdg_popup, xdg_positioner, xdg_surface, xdg_toplevel, xdg_wm_base,
};
use wayland_server::Resource as _;

use crate::session::compositor::{
    surface::{SurfaceKey, SurfaceRole},
    Compositor,
};

impl wayland_server::GlobalDispatch<xdg_wm_base::XdgWmBase, ()> for Compositor {
    fn bind(
        _state: &mut Self,
        _handle: &wayland_server::DisplayHandle,
        _client: &wayland_server::Client,
        resource: wayland_server::New<xdg_wm_base::XdgWmBase>,
        _global_data: &(),
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl wayland_server::Dispatch<xdg_wm_base::XdgWmBase, ()> for Compositor {
    fn request(
        state: &mut Self,
        _client: &wayland_server::Client,
        resource: &xdg_wm_base::XdgWmBase,
        request: xdg_wm_base::Request,
        _data: &(),
        _dhandle: &wayland_server::DisplayHandle,
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        match request {
            xdg_wm_base::Request::CreatePositioner { id } => {
                // Not used yet.
                data_init.init(id, ());
            }
            xdg_wm_base::Request::GetXdgSurface { id, surface } => {
                let surface_id = surface
                    .data::<SurfaceKey>()
                    .expect("surface has no userdata");

                let surface = state
                    .surfaces
                    .get(*surface_id)
                    .expect("surface has no entry");

                if surface.content.is_some() {
                    resource.post_error(
                        xdg_surface::Error::AlreadyConstructed,
                        "The surface already has a buffer attached.",
                    );
                }

                data_init.init(id, *surface_id);
            }
            xdg_wm_base::Request::Pong { .. } => (),
            xdg_wm_base::Request::Destroy => (),
            _ => unreachable!(),
        }
    }
}

impl wayland_server::Dispatch<xdg_surface::XdgSurface, SurfaceKey> for Compositor {
    fn request(
        state: &mut Self,
        _client: &wayland_server::Client,
        resource: &xdg_surface::XdgSurface,
        request: xdg_surface::Request,
        data: &SurfaceKey,
        _dhandle: &wayland_server::DisplayHandle,
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        match request {
            xdg_surface::Request::GetToplevel { id } => {
                let xdg_toplevel = data_init.init(id, *data);

                if !state.set_surface_role(
                    *data,
                    SurfaceRole::XdgToplevel {
                        xdg_surface: resource.clone(),
                        xdg_toplevel,
                    },
                ) {
                    resource.post_error(xdg_wm_base::Error::Role, "Surface already has a role.");
                }
            }
            xdg_surface::Request::GetPopup { id, .. } => {
                data_init.init(id, ());
            }
            xdg_surface::Request::AckConfigure { serial } => {
                let surface = state.surfaces.get_mut(*data).expect("surface has no entry");

                match surface.pending_configure {
                    Some(s) if serial == s => {
                        surface.pending_configure = None;
                    }
                    Some(s) if serial < s => (),
                    _ => resource.post_error(xdg_surface::Error::InvalidSerial, "Invalid serial."),
                }
            }
            xdg_surface::Request::SetWindowGeometry { .. } => (),
            xdg_surface::Request::Destroy => (),
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        resource: &xdg_surface::XdgSurface,
        data: &SurfaceKey,
    ) {
        // Check that there isn't a surface role created from this object.
        match state
            .surfaces
            .get(*data)
            .and_then(|s| s.role.current.as_ref())
        {
            Some(SurfaceRole::XdgToplevel { xdg_surface, .. }) if xdg_surface == resource => {
                resource.post_error(
                    xdg_surface::Error::DefunctRoleObject,
                    "The role created from this object must be destroyed first.",
                );
            }
            _ => (),
        }
    }
}

impl wayland_server::Dispatch<xdg_positioner::XdgPositioner, ()> for Compositor {
    fn request(
        _state: &mut Self,
        _client: &wayland_server::Client,
        _resource: &xdg_positioner::XdgPositioner,
        _request: xdg_positioner::Request,
        _data: &(),
        _dhandle: &wayland_server::DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        // TODO we don't support popups at present.
    }
}

impl wayland_server::Dispatch<xdg_popup::XdgPopup, ()> for Compositor {
    fn request(
        _state: &mut Self,
        _client: &wayland_server::Client,
        resource: &xdg_popup::XdgPopup,
        request: xdg_popup::Request,
        _data: &(),
        _dhandle: &wayland_server::DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        match request {
            xdg_popup::Request::Grab { .. } => {
                // Immediately dismiss the popup, because we don't support popups.
                // resource.post_error(xdg_popup::Error::InvalidGrab, "Popups are not
                // supported.");
                resource.popup_done();
            }
            xdg_popup::Request::Reposition { .. } => (),
            xdg_popup::Request::Destroy => (),
            _ => unreachable!(),
        }
        // TODO we don't support popups at present.
    }
}

impl wayland_server::Dispatch<xdg_toplevel::XdgToplevel, SurfaceKey> for Compositor {
    fn request(
        state: &mut Self,
        _client: &wayland_server::Client,
        _resource: &xdg_toplevel::XdgToplevel,
        request: xdg_toplevel::Request,
        data: &SurfaceKey,
        _dhandle: &wayland_server::DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        match request {
            xdg_toplevel::Request::SetParent { .. } => (),
            xdg_toplevel::Request::SetTitle { title } => {
                state
                    .surfaces
                    .get_mut(*data)
                    .expect("surface has no entry")
                    .title = Some(title);
            }
            xdg_toplevel::Request::SetAppId { app_id } => {
                state
                    .surfaces
                    .get_mut(*data)
                    .expect("surface has no entry")
                    .app_id = Some(app_id);
            }
            xdg_toplevel::Request::ShowWindowMenu { .. } => (),
            xdg_toplevel::Request::Move { .. } => (),
            xdg_toplevel::Request::Resize { .. } => (),
            xdg_toplevel::Request::SetMaxSize { .. } => (),
            xdg_toplevel::Request::SetMinSize { .. } => (),
            xdg_toplevel::Request::SetMaximized => (),
            xdg_toplevel::Request::UnsetMaximized => (),
            xdg_toplevel::Request::SetFullscreen { .. } => (),
            xdg_toplevel::Request::UnsetFullscreen => (),
            xdg_toplevel::Request::SetMinimized => (),
            xdg_toplevel::Request::Destroy => (),
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        resource: &xdg_toplevel::XdgToplevel,
        data: &SurfaceKey,
    ) {
        let surface = state.surfaces.get_mut(*data);
        match surface.as_ref().and_then(|s| s.role.current.as_ref()) {
            Some(SurfaceRole::XdgToplevel { xdg_toplevel, .. }) if xdg_toplevel == resource => {
                surface.unwrap().role.current = None;
                state.unmap_surface(*data);
            }
            _ => (),
        }
    }
}
