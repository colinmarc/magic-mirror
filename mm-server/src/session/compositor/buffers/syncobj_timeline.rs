// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{
    io,
    os::fd::{AsFd as _, OwnedFd},
    sync::Arc,
};

use ash::vk;
use drm::control::{syncobj, Device as _};
use tracing::{instrument, trace};
use wayland_protocols::wp::linux_drm_syncobj::v1::server::wp_linux_drm_syncobj_timeline_v1;

use crate::vulkan::VkContext;

slotmap::new_key_type! { pub struct SyncobjTimelineKey; }

pub struct SyncobjTimeline(Arc<TimelineHandle>);

struct TimelineHandle {
    pub _wp_syncobj_timeline: wp_linux_drm_syncobj_timeline_v1::WpLinuxDrmSyncobjTimelineV1,
    handle: syncobj::Handle,
    vk: Arc<VkContext>,
}

impl Drop for TimelineHandle {
    fn drop(&mut self) {
        let _ = self.vk.drm_device.destroy_syncobj(self.handle);
    }
}

#[derive(Clone)]
pub struct SyncobjTimelinePoint {
    pub value: u64,
    handle: Arc<TimelineHandle>,
}

impl SyncobjTimelinePoint {
    pub fn signal(&self) -> io::Result<()> {
        trace!(handle = ?self.handle.handle, value = self.value, "signaling timeline point");

        self.handle
            .vk
            .drm_device
            .syncobj_timeline_signal(&[self.handle.handle], &[self.value])
    }

    #[instrument(skip_all)]
    pub fn import_as_semaphore(&self, semaphore: vk::Semaphore) -> anyhow::Result<()> {
        trace!(
            value = self.value,
            ?semaphore,
            "importing timeline point as semaphore"
        );

        let device = &self.handle.vk.drm_device;

        // First, we export a sync file by creating a new syncobj and copying
        // the timeline point to 0 on the new syncobj.
        let syncobj = device.create_syncobj(false)?;
        scopeguard::defer! {
            self.handle.vk
                .drm_device
                .destroy_syncobj(syncobj)
                .expect("failed to destroy syncobj")
        };

        device.syncobj_timeline_transfer(self.handle.handle, syncobj, self.value, 0)?;
        let sync_fd = device.syncobj_to_fd(syncobj, true)?;

        // Then we can import it into a vulkan semaphore.
        unsafe { super::import_sync_file_as_semaphore(self.handle.vk.clone(), sync_fd, semaphore) }
    }
}

impl SyncobjTimeline {
    pub fn import(
        vk: Arc<VkContext>,
        wp_syncobj_timeline: wp_linux_drm_syncobj_timeline_v1::WpLinuxDrmSyncobjTimelineV1,
        fd: OwnedFd,
    ) -> io::Result<Self> {
        let handle = vk.drm_device.fd_to_syncobj(fd.as_fd(), false)?;

        Ok(Self(Arc::new(TimelineHandle {
            _wp_syncobj_timeline: wp_syncobj_timeline,
            handle,
            vk,
        })))
    }

    pub fn new_timeline_point(&self, value: u64) -> SyncobjTimelinePoint {
        SyncobjTimelinePoint {
            value,
            handle: self.0.clone(),
        }
    }
}
