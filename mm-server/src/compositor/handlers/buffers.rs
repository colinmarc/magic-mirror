// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use smithay::{
    reexports::wayland_server::{
        protocol::{wl_buffer, wl_surface},
        Resource,
    },
    wayland::{buffer, compositor, dmabuf, shm},
};
use tracing::{debug, error, trace};

use crate::{
    color::ColorSpace, compositor::handlers::color_management::ColorManagementCachedState,
};

use super::State;

impl buffer::BufferHandler for State {
    fn buffer_destroyed(&mut self, buffer: &wl_buffer::WlBuffer) {
        trace!(buffer = buffer.id().protocol_id(), "destroying buffer");

        match dmabuf::get_dmabuf(buffer) {
            Ok(dmabuf) => self.texture_manager.remove_dmabuf(&dmabuf).unwrap(),
            Err(smithay::utils::UnmanagedResource) => (),
        }

        buffer.release();
    }
}

impl shm::ShmHandler for State {
    fn shm_state(&self) -> &shm::ShmState {
        &self.shm_state
    }
}

impl dmabuf::DmabufHandler for State {
    fn dmabuf_state(&mut self) -> &mut dmabuf::DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        global: &dmabuf::DmabufGlobal,
        dmabuf: smithay::backend::allocator::dmabuf::Dmabuf,
        notifier: dmabuf::ImportNotifier,
    ) {
        if dmabuf.num_planes() != 1 {
            notifier.invalid_format();
            return;
        }

        if let Err(e) = self.texture_manager.import_dma_buffer(global, dmabuf) {
            error!("dmabuf import failed: {:#}", e);
            notifier.failed();
        } else {
            notifier.successful::<Self>().unwrap();
        }
    }
}

pub fn buffer_commit(
    state: &mut State,
    surface: &wl_surface::WlSurface,
    surface_data: &compositor::SurfaceData,
    buffer: &wl_buffer::WlBuffer,
) -> anyhow::Result<()> {
    trace!(
        surface = surface.id().protocol_id(),
        buffer = buffer.id().protocol_id(),
        is_cursor = state.cursor_surface.as_ref() == Some(surface),
        "committing buffer"
    );

    if state.cursor_surface.as_ref() == Some(surface) {
        debug!(surface = surface.id().protocol_id(), "marking cursor dirty");
        state.cursor_dirty = true;
    } else {
        tracy_client::frame_mark();
    }

    match dmabuf::get_dmabuf(buffer) {
        Ok(dmabuf) => {
            let colorspace = surface_data
                .cached_state
                .current::<ColorManagementCachedState>()
                .colorspace
                .unwrap_or(ColorSpace::Srgb);

            return state
                .texture_manager
                .attach_dma_buffer(surface, buffer, dmabuf, colorspace);
        }
        Err(smithay::utils::UnmanagedResource) => (), // Fall through to shm handler.
    }

    shm::with_buffer_contents(buffer, |ptr, _, metadata| {
        let contents = unsafe {
            std::slice::from_raw_parts(
                ptr.offset(metadata.offset as isize),
                (metadata.height * metadata.stride) as usize,
            )
        };

        state
            .texture_manager
            .import_and_attach_shm_buffer(surface, buffer, contents, &metadata)
    })??;

    Ok(())
}

smithay::delegate_shm!(State);
smithay::delegate_dmabuf!(State);
