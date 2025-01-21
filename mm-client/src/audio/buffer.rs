// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use std::collections::VecDeque;

pub struct PlaybackBuffer<F>
where
    F: dasp::Frame,
{
    /// A queue of audio frames.
    samples: VecDeque<F>,
    /// The PTS and packet length (in frames) for each packet. Kept in sync with
    /// `samples`.
    pts: VecDeque<(u64, usize)>,
}

impl<F> PlaybackBuffer<F>
where
    F: dasp::Frame,
{
    pub fn new() -> Self {
        PlaybackBuffer {
            samples: VecDeque::new(),
            pts: VecDeque::new(),
        }
    }

    /// Returns the number of frames in the buffer.
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// Adds frames to the back of the buffer.
    pub fn buffer(&mut self, pts: u64, frames: &[F]) {
        self.pts.push_back((pts, frames.len()));
        self.samples.extend(frames.iter());
    }

    /// Returns the PTS of the head packet in the audio buffer.
    pub fn current_pts(&self) -> u64 {
        self.pts
            .front()
            .expect("current_pts called before buffer")
            .0
    }

    /// Returns an iterator that pops frames from the front of the buffer.
    pub fn drain(&mut self) -> Draining<F> {
        Draining { buffer: self }
    }

    /// Discards the first N frames from the buffer.
    pub fn skip(&mut self, frames: usize) {
        self.samples.drain(..frames);

        let mut remaining = frames;
        loop {
            let (_, len) = self.pts.front_mut().expect("skip called before buffer");
            if *len <= remaining {
                remaining -= *len;
                self.pts.pop_front();
            } else {
                *len -= remaining;
                break;
            }
        }
    }
}

pub struct Draining<'a, F>
where
    F: dasp::Frame,
{
    buffer: &'a mut PlaybackBuffer<F>,
}

impl<F> Iterator for Draining<'_, F>
where
    F: dasp::Frame,
{
    type Item = F;

    fn next(&mut self) -> Option<Self::Item> {
        let frame = self.buffer.samples.pop_front()?;

        if let Some((_, remaining)) = self.buffer.pts.front_mut() {
            *remaining -= 1;
            if *remaining == 0 {
                self.buffer.pts.pop_front();
            }
        }

        Some(frame)
    }
}
