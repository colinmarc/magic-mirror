// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::sync::atomic::{AtomicU32, Ordering};

pub struct Serial(AtomicU32);

const START: u32 = 1000;

impl Serial {
    pub fn new() -> Self {
        Self(AtomicU32::new(START))
    }

    pub fn next(&self) -> u32 {
        // Wrap around, but skip zero.
        let _ = self
            .0
            .compare_exchange(0, START, Ordering::AcqRel, Ordering::SeqCst);

        self.0.fetch_add(1, Ordering::AcqRel)
    }
}
