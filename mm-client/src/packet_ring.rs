// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use bytes::Buf;
use mm_protocol as protocol;
use std::collections::VecDeque;

use tracing::warn;

const RING_TARGET_SIZE: usize = 5;

pub trait Chunk {
    fn session_id(&self) -> u64;
    fn attachment_id(&self) -> u64;
    fn seq(&self) -> u64;
    fn stream_seq(&self) -> u64;
    fn chunk(&self) -> u32;
    fn num_chunks(&self) -> u32;
    fn data(&self) -> bytes::Bytes;
    fn pts(&self) -> u64;
}

impl Chunk for protocol::VideoChunk {
    fn session_id(&self) -> u64 {
        self.session_id
    }

    fn attachment_id(&self) -> u64 {
        self.attachment_id
    }

    fn seq(&self) -> u64 {
        self.seq
    }

    fn stream_seq(&self) -> u64 {
        self.stream_seq
    }

    fn chunk(&self) -> u32 {
        self.chunk
    }

    fn num_chunks(&self) -> u32 {
        self.num_chunks
    }

    fn data(&self) -> bytes::Bytes {
        self.data.clone()
    }

    fn pts(&self) -> u64 {
        self.timestamp
    }
}

impl Chunk for protocol::AudioChunk {
    fn session_id(&self) -> u64 {
        self.session_id
    }

    fn attachment_id(&self) -> u64 {
        self.attachment_id
    }

    fn seq(&self) -> u64 {
        self.seq
    }

    fn stream_seq(&self) -> u64 {
        self.stream_seq
    }

    fn chunk(&self) -> u32 {
        self.chunk
    }

    fn num_chunks(&self) -> u32 {
        self.num_chunks
    }

    fn data(&self) -> bytes::Bytes {
        self.data.clone()
    }

    fn pts(&self) -> u64 {
        self.timestamp
    }
}

#[derive(Debug)]
pub struct Packet {
    pub pts: u64,
    pub seq: u64,
    data: VecDeque<bytes::Bytes>,
    chunk_offset: usize,
}

impl Buf for Packet {
    fn remaining(&self) -> usize {
        self.data.iter().map(|c| c.len()).sum::<usize>() - self.chunk_offset
    }

    fn chunk(&self) -> &[u8] {
        match self.data.front() {
            Some(chunk) => &chunk[self.chunk_offset..],
            None => &[],
        }
    }

    fn advance(&mut self, mut count: usize) {
        while count > 0 {
            let current = self.data.front().unwrap();
            let current_remaining = current.len() - self.chunk_offset;
            if count >= current_remaining {
                self.chunk_offset = 0;
                self.data.pop_front();
                count -= current_remaining;
            } else {
                self.chunk_offset += count;
                return;
            }
        }
    }

    fn copy_to_bytes(&mut self, len: usize) -> bytes::Bytes {
        if len == self.remaining() && self.data.len() == 1 {
            self.data.pop_front().unwrap()
        } else {
            Buf::copy_to_bytes(self, len)
        }
    }
}

#[derive(Debug)]
struct WipPacket {
    stream_seq: u64,
    seq: u64,
    pts: u64,
    chunks: Vec<Option<bytes::Bytes>>,
}

impl WipPacket {
    fn is_complete(&self) -> bool {
        self.chunks.iter().all(|c| c.is_some())
    }

    /// Reconstructs the completed frame. Panics if all chunks are not yet
    /// available.
    fn complete(self) -> Packet {
        let data: Vec<_> = self.chunks.into_iter().map(|c| c.unwrap()).collect();
        Packet {
            pts: self.pts,
            seq: self.seq,
            data: data.into(),
            chunk_offset: 0,
        }
    }
}

#[derive(Default)]
pub struct PacketRing {
    // Oldest frames at the front, newest at the back.
    ring: VecDeque<WipPacket>,
}

impl PacketRing {
    pub fn new() -> Self {
        Self {
            ring: VecDeque::new(),
        }
    }

    pub fn recv_chunk(&mut self, incoming: impl Chunk) -> anyhow::Result<()> {
        match self
            .ring
            .iter_mut()
            .find(|wip| wip.stream_seq == incoming.stream_seq() && wip.seq == incoming.seq())
        {
            Some(wip) => {
                let chunk = incoming.chunk() as usize;
                let num_chunks = incoming.num_chunks() as usize;
                if num_chunks != wip.chunks.len() || chunk >= num_chunks {
                    return Err(anyhow::anyhow!(
                        "frame chunk data invalid: chunk {} of {} (expecting {})",
                        chunk,
                        num_chunks,
                        wip.chunks.len(),
                    ));
                } else if wip.chunks[chunk].is_some() {
                    return Err(anyhow::anyhow!(
                        "duplicate frame chunk: {}:{}",
                        incoming.seq(),
                        chunk
                    ));
                }

                wip.chunks[chunk] = Some(incoming.data());
                Ok(())
            }
            None => {
                let mut wip = WipPacket {
                    stream_seq: incoming.stream_seq(),
                    seq: incoming.seq(),
                    pts: incoming.pts(),
                    chunks: vec![None; incoming.num_chunks() as usize],
                };

                wip.chunks[incoming.chunk() as usize] = Some(incoming.data());

                // Insert into the ring in order with respect to packets with
                // the same stream_seq.
                if let Some(idx) = self
                    .ring
                    .iter()
                    .position(|p| p.stream_seq == wip.stream_seq && p.seq > wip.seq)
                {
                    self.ring.insert(idx, wip);
                } else {
                    self.ring.push_back(wip);
                }

                loop {
                    let front = self.ring.front().unwrap();

                    if front.is_complete() || self.ring.len() <= RING_TARGET_SIZE {
                        break;
                    }

                    // If the oldest frame is incomplete, drop it to make room.
                    if !front.is_complete() {
                        let dropped = self.ring.pop_front().unwrap();
                        warn!(
                            seq = dropped.seq,
                            stream_seq = dropped.stream_seq,
                            num_chunks = dropped.chunks.len(),
                            missing_chunks = dropped.chunks.iter().filter(|c| c.is_none()).count(),
                            "dropped packet!",
                        );
                    } else {
                        break;
                    }
                }

                Ok(())
            }
        }
    }

    /// Removes packets matching the stream_seq for which all chunks are
    /// accounted for, and returns them as an iterator. Stops before the first
    /// incomplete packet that matches.
    pub fn drain_completed(&mut self, stream_seq: u64) -> DrainCompleted {
        DrainCompleted(self, stream_seq)
    }

    /// Removes all packets with the same stream_seq or lower.
    pub fn discard(&mut self, stream_seq: u64) {
        self.ring.retain(|wip| wip.stream_seq > stream_seq);
    }
}

pub struct DrainCompleted<'a>(&'a mut PacketRing, u64);

impl<'a> Iterator for DrainCompleted<'a> {
    type Item = Packet;

    fn next(&mut self) -> Option<Self::Item> {
        let ring = &mut self.0.ring;

        match ring
            .iter()
            .enumerate()
            .find(|(_, wip)| wip.stream_seq == self.1)
        {
            Some((idx, v)) if v.is_complete() => Some(ring.remove(idx).unwrap().complete()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use bytes::Buf;
    use std::io::Read;

    use super::*;

    #[test]
    fn test_ring() {
        let mut ring = PacketRing::default();

        let assert_frames = |ring: &mut PacketRing, s: &[u64]| {
            let completed = ring.drain_completed(0).collect::<Vec<_>>();
            assert_eq!(s.len(), completed.len());

            for (expected_seq, actual) in s.iter().zip(completed.into_iter()) {
                assert_eq!(actual.seq, *expected_seq);

                let mut buf = Vec::new();
                assert_eq!(actual.reader().read_to_end(&mut buf).unwrap(), 10);
                assert_eq!(&buf, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
            }
        };

        let frame_one = make_chunks(0, &[&[0, 1, 2], &[3, 4, 5, 6], &[7, 8], &[9]]); // 4 chunks
        let frame_two = make_chunks(1, &[&[0, 1, 2, 3, 4], &[5, 6], &[7, 8, 9]]); // 3 chunks
        let frame_three = make_chunks(2, &[&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]]); // 1 chunk

        ring.recv_chunk(frame_three[0].clone()).unwrap(); // Frame three complete.
        ring.recv_chunk(frame_two[1].clone()).unwrap();
        ring.recv_chunk(frame_one[0].clone()).unwrap();

        assert_eq!(ring.drain_completed(0).collect::<Vec<_>>().len(), 0);

        ring.recv_chunk(frame_one[1].clone()).unwrap();
        ring.recv_chunk(frame_one[2].clone()).unwrap();
        ring.recv_chunk(frame_two[0].clone()).unwrap();

        assert_eq!(ring.drain_completed(0).collect::<Vec<_>>().len(), 0);

        ring.recv_chunk(frame_one[3].clone()).unwrap(); // Frame one complete.
        assert_frames(&mut ring, &[0]);

        ring.recv_chunk(frame_two[2].clone()).unwrap(); // Frame two complete, frame three was already complete.
        assert_frames(&mut ring, &[1, 2]);

        assert_eq!(ring.drain_completed(0).collect::<Vec<_>>().len(), 0);
    }

    #[test]
    fn test_ring_drop() {
        let mut ring = PacketRing::default();
        for i in 0..10 {
            // Send ten partial frames (each missing one chunk.)
            let chunks = make_chunks(i, &[&[0, 1], &[2, 3]]);
            ring.recv_chunk(chunks[0].clone()).unwrap();
        }

        // Then send a complete frame.
        let chunks = make_chunks(10, &[&[0, 1], &[2, 3], &[4, 5], &[6, 7], &[8, 9]]);
        for chunk in chunks {
            ring.recv_chunk(chunk).unwrap();
        }

        for i in 11..20 {
            // Send more partial frames.
            let chunks = make_chunks(i, &[&[0, 1], &[2, 3]]);
            ring.recv_chunk(chunks[0].clone()).unwrap();
        }

        // The ring should have dropped the partial frames and should now return
        // the complete one.
        let mut completed = ring.drain_completed(0).collect::<Vec<_>>();
        assert_eq!(1, completed.len());
        assert_eq!(completed[0].seq, 10);

        let mut buf = Vec::new();
        let frame = completed.pop().unwrap();
        assert_eq!(frame.reader().read_to_end(&mut buf).unwrap(), 10);
        assert_eq!(&buf, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
    }

    fn make_chunks(seq: u64, chunks: &[&[u8]]) -> Vec<protocol::VideoChunk> {
        chunks
            .iter()
            .enumerate()
            .map(|(i, chunk)| protocol::VideoChunk {
                attachment_id: 0,
                session_id: 0,
                stream_seq: 0,
                seq,
                chunk: i as u32,
                num_chunks: chunks.len() as u32,
                data: bytes::Bytes::copy_from_slice(chunk),
                timestamp: 0,
            })
            .collect()
    }
}
