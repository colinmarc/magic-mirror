// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::time;

use simple_moving_average::SMA as _;
use tracing::info;

pub struct AttachmentStats {
    app_id: String,
    start: time::Instant,
    total_transfer: u64,

    sma: simple_moving_average::SingleSumSMA<f64, f64, 300>,

    last_log: time::Instant,
}

impl AttachmentStats {
    pub fn new(app_id: String) -> Self {
        let now = time::Instant::now();

        Self {
            app_id,
            start: now,
            total_transfer: 0,

            sma: simple_moving_average::SingleSumSMA::new(),

            last_log: now,
        }
    }

    pub fn record_frame(&mut self, _seq: u64, len: usize, duration: time::Duration) {
        self.total_transfer += len as u64;
        self.sma
            .add_sample((len as f64 * 8.0 / (1024.0 * 1024.0)) / duration.as_secs_f64());

        let avg = self.sma.get_average();

        if self.last_log.elapsed().as_secs() > 5 {
            self.last_log = time::Instant::now();

            let total_transfer_gb = self.total_transfer as f32 / (1024.0 * 1024.0 * 1024.0);
            info!(
                duration = ?self.start.elapsed(),
                current_bitrate_mbps = avg,
                total_transfer_gb,
                "{}", self.app_id
            );
        }

        #[cfg(feature = "tracy")]
        if _seq % 10 == 0 {
            tracy_client::plot!("video bitrate (KB/s)", avg);
        }
    }
}
