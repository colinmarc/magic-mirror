// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use ash::vk;
use tracing::instrument;

use super::VkContext;

#[derive(Clone)]
pub struct VkTimelineSemaphore(Arc<Inner>);

struct Inner {
    vk: Arc<VkContext>,
    sema: vk::Semaphore,
}

#[derive(Clone)]
pub struct VkTimelinePoint(Arc<Inner>, u64);

impl From<VkTimelinePoint> for u64 {
    fn from(value: VkTimelinePoint) -> Self {
        value.1
    }
}

impl std::ops::Add<u64> for VkTimelinePoint {
    type Output = Self;

    fn add(self, rhs: u64) -> Self {
        Self(self.0, self.1 + rhs)
    }
}

impl std::ops::Add<u64> for &VkTimelinePoint {
    type Output = VkTimelinePoint;

    fn add(self, rhs: u64) -> Self::Output {
        VkTimelinePoint(self.0.clone(), self.1 + rhs)
    }
}

impl std::ops::AddAssign<u64> for VkTimelinePoint {
    fn add_assign(&mut self, rhs: u64) {
        self.1 += rhs
    }
}

impl VkTimelineSemaphore {
    pub fn new(vk: Arc<VkContext>, initial_value: u64) -> anyhow::Result<Self> {
        let sema = unsafe {
            vk.device.create_semaphore(
                &vk::SemaphoreCreateInfo::default().push_next(
                    &mut vk::SemaphoreTypeCreateInfo::default()
                        .semaphore_type(vk::SemaphoreType::TIMELINE)
                        .initial_value(initial_value),
                ),
                None,
            )?
        };

        Ok(Self(Arc::new(Inner { vk, sema })))
    }

    pub fn new_point(&self, value: u64) -> VkTimelinePoint {
        VkTimelinePoint(self.0.clone(), value)
    }

    pub fn as_semaphore(&self) -> vk::Semaphore {
        self.0.sema
    }
}

impl VkTimelinePoint {
    pub fn value(&self) -> u64 {
        self.1
    }

    pub fn timeline(&self) -> VkTimelineSemaphore {
        VkTimelineSemaphore(self.0.clone())
    }

    #[instrument(level = "trace", skip_all)]
    pub unsafe fn wait(&self) -> anyhow::Result<()> {
        let device = &self.0.vk.device;
        device.wait_semaphores(
            &vk::SemaphoreWaitInfo::default()
                .semaphores(&[self.0.sema])
                .values(&[self.1]),
            1_000_000_000, // 1 second
        )?;

        Ok(())
    }

    #[instrument(level = "trace", skip_all)]
    pub unsafe fn signal(&self) -> anyhow::Result<()> {
        let device = &self.0.vk.device;
        device.signal_semaphore(
            &vk::SemaphoreSignalInfo::default()
                .semaphore(self.0.sema)
                .value(self.1),
        )?;

        Ok(())
    }

    pub unsafe fn poll(&self) -> anyhow::Result<bool> {
        let device = &self.0.vk.device;
        let value = device.get_semaphore_counter_value(self.0.sema)?;
        Ok(value >= self.1)
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        unsafe {
            self.vk.device.device_wait_idle().unwrap();
            self.vk.device.destroy_semaphore(self.sema, None)
        };
    }
}
