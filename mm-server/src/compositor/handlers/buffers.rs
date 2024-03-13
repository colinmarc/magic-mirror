// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use smithay::{
    reexports::wayland_server::{
        protocol::{wl_buffer, wl_surface},
        Resource,
    },
    wayland::{buffer, dmabuf, shm},
};
use tracing::{error, trace};

use super::State;

impl buffer::BufferHandler for State {
    fn buffer_destroyed(&mut self, buffer: &wl_buffer::WlBuffer) {
        trace!(buffer = buffer.id().protocol_id(), "destroying buffer");

        match dmabuf::get_dmabuf(buffer) {
            Ok(dmabuf) => self.video_pipeline.remove_dmabuf(&dmabuf).unwrap(),
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

        if let Err(e) = self.video_pipeline.import_dma_buffer(global, dmabuf) {
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
    buffer: &wl_buffer::WlBuffer,
) -> anyhow::Result<()> {
    trace!(
        surface = surface.id().protocol_id(),
        buffer = buffer.id().protocol_id(),
        "committing buffer"
    );

    tracy_client::frame_mark();

    match dmabuf::get_dmabuf(buffer) {
        Ok(dmabuf) => {
            return state
                .video_pipeline
                .attach_dma_buffer(surface, buffer, dmabuf);
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
            .video_pipeline
            .import_and_attach_shm_buffer(surface, buffer, contents, &metadata)
    })??;

    Ok(())
}

smithay::delegate_shm!(State);
smithay::delegate_dmabuf!(State);
