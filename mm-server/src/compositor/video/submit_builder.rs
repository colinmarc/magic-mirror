// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

pub struct TimelinePoint(vk::Semaphore, u64);

impl From<(vk::Semaphore, u64)> for TimelinePoint {
    fn from((sema, tp): (vk::Semaphore, u64)) -> Self {
        Self(sema, tp)
    }
}

struct SingleSubmit {
    cb: vk::CommandBuffer,
    waits: Vec<TimelinePoint>,
    signals: Vec<TimelinePoint>,
    wait_dst_stage_mask: vk::PipelineStageFlags,
}

struct SubmitBuilder {
    submits: Vec<SingleSubmit>,
}

impl SubmitBuilder {
    fn new() -> Self {
        Self {
            submits: Vec::new(),
        }
    }

    fn add<TP>(
        &mut self,
        cb: vk::CommandBuffer,
        waits: impl IntoIterator<Item = TP>,
        submits: impl IntoIterator<Item = TP>,
        wait_dst_stage_mask: vk::PipelineStageFlags,
    ) -> &mut Self
    where
        TP: Into<TimelinePoint>,
    {
        self.submits.push(SingleSubmit {
            cb,
            waits: waits.into_iter().map(|tp| tp.into()).collect(),
            signals: submits.into_iter().map(|tp| tp.into()).collect(),
            wait_dst_stage_mask,
        });

        self
    }

    fn submit(self, device: &ash::Device, queue: vk::Queue, fence: Option<vk::Fence>) {
        let mut submits = Vec::new();

        let wait_semas: Vec<Vec<vk::Semaphore>> = self
            .submits
            .iter()
            .map(|submit| submit.waits.iter().map(|tp| tp.0).collect())
            .collect();

        let wait_vals: Vec<Vec<u64>> = self
            .submits
            .iter()
            .map(|submit| submit.waits.iter().map(|tp| tp.1).collect())
            .collect();

        let signal_semas = self
            .submits
            .iter()
            .map(|submit| submit.signals.iter().map(|tp| tp.0).collect())
            .collect();

        let signal_vals = self
            .submits
            .iter()
            .map(|submit| submit.signals.iter().map(|tp| tp.1).collect())
            .collect();

        let mut timeline_infos = self
            .submits
            .iter()
            .enumerate()
            .map(|(idx, _)| {
                vk::TimelineSemaphoreSubmitInfo::default()
                    .wait_semaphore_values(&wait_vals[idx])
                    .signal_semaphore_values(&signal_vals[idx])
            })
            .collect();

        for (idx, submit) in self.submits.iter().enumerate() {
            let mut timeline_info = vk::TimelineSemaphoreSubmitInfo::default()
                .wait_semaphore_values(&wait_vals[idx])
                .signal_semaphore_values(&signal_vals[idx]);

            let submit_info = vk::SubmitInfo::default()
                .command_buffers(&[submit.cb])
                .wait_semaphores(&wait_semas[idx])
                .signal_semaphores(&signal_semas[idx])
                .wait_dst_stage_mask(&[submit.wait_dst_stage_mask])
                .push_next(&mut timeline_infos[idx]);

            submits.push(submit_info);
        }

        unsafe { device.queue_submit(queue, &submits, vk::Fence::null()) };
    }
}
