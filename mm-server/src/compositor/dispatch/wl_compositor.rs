// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use tracing::warn;
use wayland_server::{
    protocol::{wl_callback, wl_compositor, wl_output, wl_region, wl_surface},
    Resource as _,
};

use crate::{
    compositor::{
        surface::{CommitError, PendingBuffer, Surface, SurfaceKey},
        State,
    },
    pixel_scale::PixelScale,
};

impl wayland_server::GlobalDispatch<wl_compositor::WlCompositor, ()> for State {
    fn bind(
        _state: &mut Self,
        _handle: &wayland_server::DisplayHandle,
        _client: &wayland_server::Client,
        resource: wayland_server::New<wl_compositor::WlCompositor>,
        _global_data: &(),
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl wayland_server::Dispatch<wl_compositor::WlCompositor, ()> for State {
    fn request(
        state: &mut Self,
        _client: &wayland_server::Client,
        _resource: &wl_compositor::WlCompositor,
        request: wl_compositor::Request,
        _data: &(),
        _dhandle: &wayland_server::DisplayHandle,
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        match request {
            wl_compositor::Request::CreateSurface { id } => {
                state
                    .surfaces
                    .insert_with_key(|k| Surface::new(data_init.init(id, k)));
            }
            wl_compositor::Request::CreateRegion { id } => {
                // We don't do anything with regions.
                data_init.init(id, ());
            }
            _ => unreachable!(),
        }
    }
}

impl wayland_server::Dispatch<wl_surface::WlSurface, SurfaceKey> for State {
    fn request(
        state: &mut Self,
        _client: &wayland_server::Client,
        resource: &wl_surface::WlSurface,
        request: wl_surface::Request,
        data: &SurfaceKey,
        _dhandle: &wayland_server::DisplayHandle,
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        match request {
            wl_surface::Request::Attach { buffer, x, y } => {
                if x != 0 || y != 0 {
                    warn!(x, y, "ignoring nonzero buffer x/y offset")
                }

                state
                    .surfaces
                    .get_mut(*data)
                    .expect("surface has no entry")
                    .pending_buffer = match buffer {
                    Some(buf) => {
                        let buffer_id = *buf.data().expect("buffer has no userdata");
                        Some(PendingBuffer::Attach(buffer_id))
                    }
                    None => Some(PendingBuffer::Detach),
                };
            }
            wl_surface::Request::Frame { callback } => {
                let callback = data_init.init(callback, *data);
                state
                    .surfaces
                    .get_mut(*data)
                    .expect("surface has no entry")
                    .frame_callback
                    .pending = Some(callback);
            }
            wl_surface::Request::Commit => {
                if let Err(CommitError(code, msg)) = state.surface_commit(*data) {
                    resource.post_error(code, msg);
                }
            }

            wl_surface::Request::SetBufferTransform { transform } => {
                if !matches!(transform.into_result(), Ok(wl_output::Transform::Normal)) {
                    warn!(?transform, "ignoring nonzero buffer rotation");
                }
            }
            wl_surface::Request::SetBufferScale { scale } => {
                if scale < 1 {
                    resource.post_error(wl_surface::Error::InvalidScale, "Scale must be >= 1");
                    return;
                }

                state
                    .surfaces
                    .get_mut(*data)
                    .expect("surface has no entry")
                    .buffer_scale
                    .pending = Some(PixelScale(scale as u32, 1));
            }
            wl_surface::Request::Offset { x, y } => {
                if x != 0 || y != 0 {
                    warn!(x, y, "ignoring nonzero buffer offset");
                }
            }
            // We ignore damage and don't do any related optimizations.
            wl_surface::Request::DamageBuffer { .. } => (),
            wl_surface::Request::Damage { .. } => (),
            // We ignore input and opaque regions, because we don't support subcompositing.
            wl_surface::Request::SetOpaqueRegion { .. } => (),
            wl_surface::Request::SetInputRegion { .. } => (),
            wl_surface::Request::Destroy => (),
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        _resource: &wl_surface::WlSurface,
        data: &SurfaceKey,
    ) {
        state.surface_destroyed(*data);
    }
}

impl wayland_server::Dispatch<wl_callback::WlCallback, SurfaceKey> for State {
    fn request(
        _state: &mut Self,
        _client: &wayland_server::Client,
        _resource: &wl_callback::WlCallback,
        _request: wl_callback::Request,
        _data: &SurfaceKey,
        _dhandle: &wayland_server::DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
    }
}

impl wayland_server::Dispatch<wl_region::WlRegion, ()> for State {
    fn request(
        _state: &mut Self,
        _client: &wayland_server::Client,
        _resource: &wl_region::WlRegion,
        _request: wl_region::Request,
        _data: &(),
        _dhandle: &wayland_server::DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
    }
}
