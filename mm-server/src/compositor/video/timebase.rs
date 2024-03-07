// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::time;

#[cfg(feature = "ffmpeg_encode")]
use ffmpeg_next as ffmpeg;

#[derive(Copy, Clone)]
pub struct Timebase(u64, time::Instant);

impl Timebase {
    pub fn new(per_second: u64, epoch: time::Instant) -> Self {
        Self(per_second, epoch)
    }

    pub fn now(&self) -> u64 {
        let elapsed = self.1.elapsed();
        let elapsed_from_secs = elapsed.as_secs() * self.0;

        let step_nanos = 1_000_000_000 / self.0;
        let elapsed_from_nanos = elapsed.subsec_nanos() as u64 / step_nanos;

        elapsed_from_secs + elapsed_from_nanos
    }
}

#[cfg(feature = "ffmpeg_encode")]
impl From<Timebase> for ffmpeg::Rational {
    fn from(timebase: Timebase) -> ffmpeg::Rational {
        ffmpeg::Rational(1, timebase.0 as i32)
    }
}
