// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{cell::RefCell, os::fd::AsRawFd as _, rc::Rc};

use shipyard::{AddComponent as _, Component, EntityId, Get as _, NonSendSync, ViewMut};
use tracing::error;
use wayland_server::{
    protocol::{wl_shm, wl_shm_pool},
    Resource as _,
};

use crate::compositor::{
    buffers::{validate_buffer_parameters, Buffer, PlaneMetadata},
    shm::Pool,
    State,
};

#[derive(Component, Debug)]
pub struct ShmPool {
    wl_shm: wl_shm::WlShm,
    wl_shm_pool: wl_shm_pool::WlShmPool,
    pool: Rc<RefCell<Pool>>,
}

impl wayland_server::GlobalDispatch<wl_shm::WlShm, ()> for State {
    fn bind(
        _state: &mut Self,
        _handle: &wayland_server::DisplayHandle,
        _client: &wayland_server::Client,
        resource: wayland_server::New<wl_shm::WlShm>,
        _global_data: &(),
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl wayland_server::Dispatch<wl_shm::WlShm, ()> for State {
    fn request(
        state: &mut Self,
        _client: &wayland_server::Client,
        wl_shm: &wl_shm::WlShm,
        request: wl_shm::Request,
        _data: &(),
        _dhandle: &wayland_server::DisplayHandle,
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        match request {
            wl_shm::Request::CreatePool { id, fd, size } => {
                if size <= 0 {
                    wl_shm.post_error(
                        wl_shm::Error::InvalidStride,
                        "Negative or zero size provided.",
                    );
                }

                let fd_debug = fd.as_raw_fd();
                let pool = match Pool::new(fd, size as usize) {
                    Ok(p) => p,
                    Err(err) => {
                        error!(?err, fd = fd_debug, size, "failed to map client shm");
                        wl_shm.post_error(wl_shm::Error::InvalidFd, "mmap failed.");
                        return;
                    }
                };

                let entity_id = state.world.add_entity(());
                let wl_shm_pool = data_init.init(id, entity_id);

                // Required because Pool is not send or sync.
                let mut vm = state
                    .world
                    .borrow::<NonSendSync<ViewMut<ShmPool>>>()
                    .expect("borrow failed");

                // The pool shouldn't be unmapped until all buffers referencing it have been
                // destroyed. We represent this with an Rc.
                vm.add_component_unchecked(
                    entity_id,
                    ShmPool {
                        wl_shm: wl_shm.clone(),
                        wl_shm_pool,
                        pool: Rc::new(RefCell::new(pool)),
                    },
                );
            }
            _ => unreachable!(),
        }
    }
}

impl wayland_server::Dispatch<wl_shm_pool::WlShmPool, EntityId> for State {
    fn request(
        state: &mut Self,
        _client: &wayland_server::Client,
        resource: &wl_shm_pool::WlShmPool,
        request: wl_shm_pool::Request,
        entity_id: &EntityId,
        _dhandle: &wayland_server::DisplayHandle,
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        match request {
            wl_shm_pool::Request::CreateBuffer {
                id,
                offset,
                width,
                height,
                stride,
                format,
            } => {
                let vm = state
                    .world
                    .borrow::<NonSendSync<ViewMut<ShmPool>>>()
                    .expect("borrow failed");

                let shm_pool = vm.get(*entity_id).expect("pool has no entity");

                if !matches!(
                    format.into_result(),
                    Ok(wl_shm::Format::Argb8888) | Ok(wl_shm::Format::Xrgb8888)
                ) {
                    resource.post_error(wl_shm::Error::InvalidFormat, "Invalid format.");
                    return;
                }

                if let Err(msg) = validate_buffer_parameters(offset, width, height, stride, 4) {
                    resource.post_error(wl_shm::Error::InvalidStride, msg);
                    return;
                }

                let buffer_size = stride * height;
                if (offset + buffer_size) as usize > shm_pool.pool.borrow().size {
                    resource
                        .post_error(wl_shm::Error::InvalidStride, "Size exceeds pool capacity.");
                    return;
                }

                let entity_id = state.world.add_entity(());
                let wl_buffer = data_init.init(id, entity_id);

                let buffer = Buffer::Shm {
                    wl_buffer,
                    offset: offset as u32,
                    pool: shm_pool.pool.clone(),
                    metadata: PlaneMetadata {
                        width: width as u32,
                        height: height as u32,
                        stride: stride as u32,
                        bpp: 4,
                    },
                };

                let mut vm = state
                    .world
                    .borrow::<NonSendSync<ViewMut<Buffer>>>()
                    .expect("borrow failed");
                vm.add_component_unchecked(entity_id, buffer);
            }
            wl_shm_pool::Request::Resize { size } => {
                let vm = state
                    .world
                    .borrow::<NonSendSync<ViewMut<ShmPool>>>()
                    .expect("borrow failed");

                let shm_pool = vm.get(*entity_id).expect("pool has no entity");

                let mut pool = shm_pool.pool.borrow_mut();
                if size <= pool.size as i32 {
                    resource.post_error(wl_shm::Error::InvalidStride, "Invalid size provided.");
                    return;
                }

                match pool.resize(size as usize) {
                    Ok(_) => (),
                    Err(err) => {
                        error!(?err, "failed to remap shm");
                        resource.post_error(wl_shm::Error::InvalidFd, "mmap operation failed.");
                    }
                }
            }
            wl_shm_pool::Request::Destroy => (),
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        _resource: &wl_shm_pool::WlShmPool,
        entity_id: &EntityId,
    ) {
        // Buffers continue to be valid after their backing pool is destroyed.
        state.world.delete_entity(*entity_id);
    }
}
