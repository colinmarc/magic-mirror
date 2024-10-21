// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use std::{sync::atomic::AtomicU64, time};

#[derive(Default)]
pub(crate) struct StatsCollector {
    pub(crate) bytes_tx: AtomicU64,
    pub(crate) bytes_rx: AtomicU64,
    pub(crate) rtt_us: AtomicU64,
}

impl StatsCollector {
    pub(crate) fn snapshot(&self) -> ClientStats {
        let rtt_us = self.rtt_us.load(std::sync::atomic::Ordering::SeqCst);
        ClientStats {
            bytes_tx: self.bytes_tx.load(std::sync::atomic::Ordering::SeqCst),
            bytes_rx: self.bytes_rx.load(std::sync::atomic::Ordering::SeqCst),
            rtt: time::Duration::from_micros(rtt_us),
        }
    }
}

/// A snapshot of the client's connection statistics.
#[derive(uniffi::Record, Clone, Copy)]
pub struct ClientStats {
    pub bytes_tx: u64,
    pub bytes_rx: u64,
    pub rtt: time::Duration,
}
