// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use std::collections::VecDeque;

use mm_protocol as protocol;
use tracing::warn;

use super::{DroppedPacket, Packet};

const RING_TARGET_SIZE: usize = 5;

pub(crate) trait Chunk {
    fn seq(&self) -> u64;
    fn stream_seq(&self) -> u64;
    fn chunk(&self) -> u32;
    fn num_chunks(&self) -> u32;
    fn data(&self) -> bytes::Bytes;
    fn pts(&self) -> u64;
    fn frame_optional(&self) -> bool;
}

impl Chunk for protocol::VideoChunk {
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

    fn frame_optional(&self) -> bool {
        self.frame_optional
    }
}

impl Chunk for protocol::AudioChunk {
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

    fn frame_optional(&self) -> bool {
        false
    }
}

#[derive(Debug)]
struct WipPacket {
    stream_seq: u64,
    seq: u64,
    pts: u64,
    frame_optional: bool,
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
            stream_seq: self.stream_seq,
            data: data.into(),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, thiserror::Error)]
pub(crate) enum PacketRingError {
    #[error("invalid chunk {0} of {1}")]
    InvalidChunk(usize, usize),
    #[error("duplicate chunk {0}")]
    DuplicateChunk(usize),
}

#[derive(Default)]
pub(crate) struct PacketRing {
    // Oldest frames at the front, newest at the back.
    ring: VecDeque<WipPacket>,
    dropped: VecDeque<DroppedPacket>,
}

impl PacketRing {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn recv_chunk(&mut self, incoming: impl Chunk) -> Result<(), PacketRingError> {
        match self
            .ring
            .iter_mut()
            .find(|wip| wip.stream_seq == incoming.stream_seq() && wip.seq == incoming.seq())
        {
            Some(wip) => {
                let chunk = incoming.chunk() as usize;
                let num_chunks = incoming.num_chunks() as usize;
                if num_chunks != wip.chunks.len() || chunk >= num_chunks {
                    return Err(PacketRingError::InvalidChunk(chunk, num_chunks));
                } else if wip.chunks[chunk].is_some() {
                    return Err(PacketRingError::DuplicateChunk(chunk));
                }

                wip.chunks[chunk] = Some(incoming.data());
                Ok(())
            }
            None => {
                let mut wip = WipPacket {
                    stream_seq: incoming.stream_seq(),
                    seq: incoming.seq(),
                    pts: incoming.pts(),
                    frame_optional: incoming.frame_optional(),
                    chunks: vec![None; incoming.num_chunks().max(1) as usize],
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
                            frame_optional = dropped.frame_optional,
                            "dropped packet!",
                        );

                        self.dropped.push_back(DroppedPacket {
                            pts: dropped.pts,
                            seq: dropped.seq,
                            stream_seq: dropped.stream_seq,
                            optional: dropped.frame_optional,
                        })
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
    ///
    /// The iterator must be used to actually remove packets from the ring.
    /// Dropping the iterator early will not drop the remaining packets.
    pub(crate) fn drain_completed(&mut self, stream_seq: u64) -> DrainCompleted {
        DrainCompleted(self, stream_seq)
    }

    /// Removes all packets with the same stream_seq or lower.
    pub(crate) fn discard(&mut self, stream_seq: u64) {
        self.ring.retain(|wip| wip.stream_seq > stream_seq);
    }
}

pub(crate) struct DrainCompleted<'a>(&'a mut PacketRing, u64);

impl Iterator for DrainCompleted<'_> {
    type Item = Result<Packet, DroppedPacket>;

    fn next(&mut self) -> Option<Self::Item> {
        let dropped = self
            .0
            .dropped
            .iter()
            .position(|p| p.stream_seq == self.1)
            .and_then(|idx| self.0.dropped.remove(idx));
        if let Some(dropped) = dropped {
            return Some(Err(dropped));
        }

        let ring = &mut self.0.ring;
        match ring
            .iter()
            .enumerate()
            .find(|(_, wip)| wip.stream_seq == self.1)
        {
            Some((idx, v)) if v.is_complete() => Some(Ok(ring.remove(idx).unwrap().complete())),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ring() {
        let mut ring = PacketRing::default();

        let assert_frames = |ring: &mut PacketRing, s: &[u64]| {
            let completed = ring.drain_completed(0).collect::<Vec<_>>();
            assert_eq!(s.len(), completed.len());

            for (expected_seq, actual) in s.iter().zip(completed.into_iter()) {
                let actual = actual.expect("no dropped packet");
                assert_eq!(actual.seq, *expected_seq);
                assert_eq!(&actual.data(), &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
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

        // The ring should have dropped the partial frames and should indicate
        // that alongside the completed one.
        let completed = ring.drain_completed(0).collect::<Vec<_>>();
        assert_eq!(11, completed.len());
        assert_eq!(completed[0].as_ref().err().unwrap().seq, 0);
        assert_eq!(completed[1].as_ref().err().unwrap().seq, 1);
        assert_eq!(completed[2].as_ref().err().unwrap().seq, 2);
        assert_eq!(completed[3].as_ref().err().unwrap().seq, 3);
        assert_eq!(completed[4].as_ref().err().unwrap().seq, 4);
        assert_eq!(completed[5].as_ref().err().unwrap().seq, 5);
        assert_eq!(completed[6].as_ref().err().unwrap().seq, 6);
        assert_eq!(completed[7].as_ref().err().unwrap().seq, 7);
        assert_eq!(completed[8].as_ref().err().unwrap().seq, 8);
        assert_eq!(completed[9].as_ref().err().unwrap().seq, 9);
        assert_eq!(completed[10].as_ref().unwrap().seq, 10);

        let frame = completed.last().unwrap();
        assert_eq!(
            &frame.as_ref().unwrap().data(),
            &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]
        );
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
                frame_optional: false,
            })
            .collect()
    }
}
