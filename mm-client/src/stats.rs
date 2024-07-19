// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time,
};

use lazy_static::lazy_static;
use simple_moving_average::{SingleSumSMA, SMA as _};

lazy_static! {
    pub static ref STATS: Arc<Stats> = Arc::new(Stats::default());
}

#[derive(Default)]
pub struct Stats {
    inner: RwLock<Inner>,
}

struct Frame {
    first_chunk_recvd: time::Instant,
    last_chunk_recvd: time::Instant,
    len: usize,
}

struct Inner {
    in_flight_frames: HashMap<(u64, u64), Frame>,

    video_bitrate: SingleSumSMA<f32, f32, 60>,
    video_bytes: u64,
    last_frame: time::Instant,

    connection_rtt: time::Duration,
    video_latency: SingleSumSMA<u64, u64, 60>,
}

impl Stats {
    /// Tracks the connection roundtrip time.
    pub fn set_rtt(&self, rtt: time::Duration) {
        self.inner.write().unwrap().connection_rtt = rtt;
    }

    /// Starts tracking a frame, and tracks total video frame bytes transferred.
    /// Should be called whenever a chunk arnives.
    pub fn frame_chunk_received(&self, stream_seq: u64, seq: u64, len: usize) {
        let now = time::Instant::now();
        let mut inner = self.inner.write().unwrap();

        inner
            .in_flight_frames
            .entry((stream_seq, seq))
            .or_insert(Frame {
                first_chunk_recvd: now,
                last_chunk_recvd: now,
                len: 0,
            });

        inner.video_bytes += len as u64;
    }

    /// Tracks the size of the frame and the time it took to receive all its chunks. Should be called whenever a frame is complete.
    pub fn full_frame_received(&self, stream_seq: u64, seq: u64, len: usize) {
        let now = time::Instant::now();
        let mut inner = self.inner.write().unwrap();

        if let Some(frame) = inner.in_flight_frames.get_mut(&(stream_seq, seq)) {
            frame.len = len;
            frame.last_chunk_recvd = now;
        }
    }

    /// Tracks the total frame time. Should be called right before the frame is rendered.
    pub fn frame_rendered(&self, stream_seq: u64, seq: u64) {
        let now = time::Instant::now();
        let mut inner = self.inner.write().unwrap();

        // Add a bitrate measurement.
        let duration = (now - inner.last_frame).as_secs_f32();
        inner.last_frame = now;

        let sample = inner.video_bytes as f32 * 8.0 / duration;
        inner.video_bitrate.add_sample(sample);
        inner.video_bytes = 0;

        // Finish tracking the frame, and measure latency.
        if let Some(frame) = inner.in_flight_frames.remove(&(stream_seq, seq)) {
            inner
                .video_latency
                .add_sample((now - frame.first_chunk_recvd).as_nanos() as u64)
        }
    }

    pub fn frame_discarded(&self, stream_seq: u64, seq: u64) {
        self.inner
            .write()
            .unwrap()
            .in_flight_frames
            .remove(&(stream_seq, seq));
    }

    /// Returns the average video bitrate in bits per second.
    pub fn video_bitrate(&self) -> f32 {
        self.inner.read().unwrap().video_bitrate.get_average()
    }

    /// Returns the average total video latency in milliseconds.
    pub fn video_latency(&self) -> f32 {
        let inner = self.inner.read().unwrap();

        let avg = inner.video_latency.get_average() + inner.connection_rtt.as_nanos() as u64;
        avg as f32 / 1_000_000.0
    }
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            in_flight_frames: HashMap::new(),

            video_bitrate: SingleSumSMA::new(),
            video_bytes: 0,
            last_frame: time::Instant::now(),

            connection_rtt: time::Duration::ZERO,
            video_latency: SingleSumSMA::new(),
        }
    }
}
