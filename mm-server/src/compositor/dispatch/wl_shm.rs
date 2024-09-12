// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{
    os::fd::AsRawFd as _,
    sync::{Arc, RwLock},
};

use tracing::error;
use wayland_server::{
    protocol::{wl_shm, wl_shm_pool},
    Resource as _,
};

use crate::compositor::{
    buffers::{import_shm_buffer, validate_buffer_parameters, PlaneMetadata},
    shm::{Pool, ShmPool, ShmPoolKey},
    State,
};

impl wayland_server::GlobalDispatch<wl_shm::WlShm, ()> for State {
    fn bind(
        _state: &mut Self,
        _handle: &wayland_server::DisplayHandle,
        _client: &wayland_server::Client,
        resource: wayland_server::New<wl_shm::WlShm>,
        _global_data: &(),
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        let wl_shm = data_init.init(resource, ());
        wl_shm.format(wl_shm::Format::Xrgb8888);
        wl_shm.format(wl_shm::Format::Argb8888);
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

                state.shm_pools.insert_with_key(|k| {
                    let wl_shm_pool = data_init.init(id, k);
                    ShmPool {
                        _wl_shm: wl_shm.clone(),
                        _wl_shm_pool: wl_shm_pool,
                        // The pool shouldn't be unmapped until all buffers referencing it have been
                        // destroyed. We represent this constraint with an Arc.
                        pool: Arc::new(RwLock::new(pool)),
                    }
                });
            }
            _ => unreachable!(),
        }
    }
}

impl wayland_server::Dispatch<wl_shm_pool::WlShmPool, ShmPoolKey> for State {
    fn request(
        state: &mut Self,
        _client: &wayland_server::Client,
        resource: &wl_shm_pool::WlShmPool,
        request: wl_shm_pool::Request,
        data: &ShmPoolKey,
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
                let pool = state
                    .shm_pools
                    .get(*data)
                    .expect("shm_pool has no entry")
                    .pool
                    .clone();

                let format = match format.into_result() {
                    Ok(wl_shm::Format::Argb8888) => drm_fourcc::DrmFourcc::Argb8888,
                    Ok(wl_shm::Format::Xrgb8888) => drm_fourcc::DrmFourcc::Xrgb8888,
                    _ => {
                        resource.post_error(wl_shm::Error::InvalidFormat, "Invalid format.");
                        return;
                    }
                };

                if let Err(msg) = validate_buffer_parameters(offset, width, height, stride, 4) {
                    resource.post_error(wl_shm::Error::InvalidStride, msg);
                    return;
                }

                let buffer_size = stride * height;
                if (offset + buffer_size) as usize > pool.read().unwrap().size {
                    resource
                        .post_error(wl_shm::Error::InvalidStride, "Size exceeds pool capacity.");
                    return;
                }

                let format = PlaneMetadata {
                    format,
                    width: width as u32,
                    height: height as u32,
                    stride: stride as u32,
                    offset: offset as u32,
                };

                let res = state.buffers.try_insert_with_key(|k| {
                    let wl_buffer = data_init.init(id, k);
                    import_shm_buffer(state.vk.clone(), wl_buffer, pool, format)
                });

                if res.is_err() {
                    resource.post_error(wl_shm::Error::InvalidFd, "Import failed.");
                };
            }
            wl_shm_pool::Request::Resize { size } => {
                let shm_pool = state.shm_pools.get_mut(*data).expect("pool has no entry");
                let mut pool = shm_pool.pool.write().unwrap();

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
        data: &ShmPoolKey,
    ) {
        // Buffers continue to be valid after their backing pool is destroyed.
        state.shm_pools.remove(*data);
    }
}
