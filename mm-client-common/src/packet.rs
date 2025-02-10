// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

mod ring;
use std::collections::VecDeque;

pub(crate) use ring::*;

#[derive(Debug, Clone, uniffi::Object)]
pub struct Packet {
    pub(crate) pts: u64,
    pub(crate) seq: u64,
    pub(crate) stream_seq: u64,
    data: VecDeque<bytes::Bytes>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct DroppedPacket {
    pub pts: u64,
    pub seq: u64,
    pub stream_seq: u64,
    pub hierarchical_layer: u32,
}

#[uniffi::export]
impl Packet {
    pub fn pts(&self) -> u64 {
        self.pts
    }

    pub fn stream_seq(&self) -> u64 {
        self.stream_seq
    }

    pub fn seq(&self) -> u64 {
        self.seq
    }

    pub fn data(&self) -> Vec<u8> {
        if self.data.len() == 1 {
            self.data[0].to_vec()
        } else {
            use bytes::buf::BufMut;

            let mut buf = Vec::with_capacity(self.len());
            for chunk in self.data.iter() {
                buf.put(chunk.clone());
            }

            buf
        }
    }
}

impl Packet {
    pub fn len(&self) -> usize {
        self.data.iter().map(|c| c.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    // Copies the packet data into dst. The length of dst must match the
    pub fn copy_to_slice(&self, mut dst: &mut [u8]) {
        use bytes::buf::BufMut;

        for chunk in self.data.iter() {
            dst.put(chunk.clone());
        }
    }
}
