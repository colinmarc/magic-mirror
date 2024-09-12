// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{
    os::fd::{AsFd, OwnedFd},
    sync::{Arc, RwLock},
};

use anyhow::bail;
use nix::sys::mman::{mmap, munmap, MapFlags, ProtFlags};
use wayland_server::protocol::{wl_shm, wl_shm_pool};

// TODO: malicious or broken clients can cause us to crash with SIGBUS. We
// should handle that with a exception handler.

slotmap::new_key_type! { pub struct ShmPoolKey; }

pub struct ShmPool {
    pub _wl_shm: wl_shm::WlShm,
    pub _wl_shm_pool: wl_shm_pool::WlShmPool,
    pub pool: Arc<RwLock<Pool>>,
}

#[derive(Debug)]
pub struct Pool {
    fd: OwnedFd,
    ptr: *mut u8,
    pub size: usize,
}

impl Pool {
    pub fn new(fd: OwnedFd, size: usize) -> anyhow::Result<Self> {
        let ptr = unsafe { map(&fd, size)? };

        Ok(Pool { fd, size, ptr })
    }

    pub fn data(&self, offset: usize, len: usize) -> &[u8] {
        assert!(offset + len <= self.size);
        unsafe { std::slice::from_raw_parts(self.ptr.add(offset), len) }
    }

    pub fn resize(&mut self, new_size: usize) -> anyhow::Result<()> {
        if self.ptr.is_null() {
            bail!("mmap defunct");
        }

        self.unmap();

        let ptr = unsafe { map(&self.fd, new_size)? };
        self.ptr = ptr;
        self.size = new_size;

        Ok(())
    }

    fn unmap(&mut self) {
        assert!(!self.ptr.is_null());

        unsafe { munmap(self.ptr as *mut _, self.size).expect("munmap failed") }
        self.ptr = std::ptr::null_mut();
        self.size = 0;
    }
}

unsafe impl Send for Pool {}

unsafe impl Sync for Pool {}

unsafe fn map(fd: impl AsFd, size: usize) -> anyhow::Result<*mut u8> {
    if size == 0 {
        bail!("zero-sized mmap");
    }

    let ptr = mmap(
        None,
        size.try_into()?,
        ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
        MapFlags::MAP_SHARED,
        Some(fd),
        0,
    )?;

    Ok(ptr as *mut u8)
}

impl Drop for Pool {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            self.unmap();
        }
    }
}
