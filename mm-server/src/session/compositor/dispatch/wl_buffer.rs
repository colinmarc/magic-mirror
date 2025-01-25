// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use wayland_server::protocol::wl_buffer;

use crate::session::compositor::{buffers::BufferKey, Compositor};

impl wayland_server::Dispatch<wl_buffer::WlBuffer, BufferKey> for Compositor {
    fn request(
        _state: &mut Self,
        _client: &wayland_server::Client,
        _resource: &wl_buffer::WlBuffer,
        request: wl_buffer::Request,
        _data: &BufferKey,
        _dhandle: &wayland_server::DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        match request {
            wl_buffer::Request::Destroy => (),
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        _resource: &wl_buffer::WlBuffer,
        data: &BufferKey,
    ) {
        // We can't destroy the buffer until it's released. This marks it for
        // destruction later.
        if let Some(buffer) = state.buffers.get_mut(*data) {
            buffer.needs_destruction = true;
        }
    }
}
