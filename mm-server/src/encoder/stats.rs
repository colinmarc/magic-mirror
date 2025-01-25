// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{sync::Arc, time};

use parking_lot::Mutex;

#[derive(Default, Clone)]
pub struct EncodeStats {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    start: time::Instant,
    stream_stats: LayerStats,
    keyframe_stats: LayerStats,
    layer_stats: Vec<LayerStats>,
}

impl Default for Inner {
    fn default() -> Self {
        let start = time::Instant::now();

        Self {
            start,
            stream_stats: LayerStats::new(start),
            keyframe_stats: LayerStats::new(start),
            layer_stats: Vec::new(),
        }
    }
}

struct LayerStats {
    start: time::Instant,
    min: usize,
    max: usize,
    total: u64,
}

impl LayerStats {
    fn new(start: time::Instant) -> Self {
        Self {
            start,
            min: 0,
            max: 0,
            total: 0,
        }
    }

    fn record_frame_size(&mut self, len: usize) {
        self.total += len as u64;

        if self.min == 0 || len < self.min {
            self.min = len;
        }

        if len > self.max {
            self.max = len;
        }
    }
}

impl std::fmt::Debug for LayerStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let period = self.start.elapsed();

        let mut f = f.debug_struct("EncodeStats");

        f.field("frame_min", &self.min);
        f.field("frame_max", &self.max);
        f.field("rate", &calculate_rate(period, self.total));

        f.finish()
    }
}

impl EncodeStats {
    pub fn record_frame_size(&self, is_keyframe: bool, layer: u32, len: usize) {
        let mut inner = self.inner.lock();

        inner.stream_stats.record_frame_size(len);
        if is_keyframe {
            inner.keyframe_stats.record_frame_size(len);
        } else {
            let layer = layer as usize;
            let layers = (layer + 1).max(inner.layer_stats.len());

            let start = inner.start;
            inner
                .layer_stats
                .resize_with(layers, || LayerStats::new(start));

            inner.layer_stats[layer].record_frame_size(len);
        }
    }
}

impl std::fmt::Debug for EncodeStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.inner.lock();

        let mut f = f.debug_struct("EncodeStats");

        f.field("duration", &inner.start.elapsed());
        f.field("totals", &inner.stream_stats);
        f.field("IDR", &inner.keyframe_stats);
        for (layer, stats) in inner.layer_stats.iter().enumerate() {
            f.field(&format!("P{layer}"), &stats);
        }

        f.finish()
    }
}

fn calculate_rate(dur: time::Duration, total: u64) -> f32 {
    // Total is in bytes, we want mbit/s.
    let total_mbits = total as f32 / (1024.0 * 1024.0) * 8.0;
    total_mbits / dur.as_secs_f32()
}
