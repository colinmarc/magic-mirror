// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use std::collections::{BTreeMap, VecDeque};

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
    fn fec_metadata(&self) -> Option<protocol::FecMetadata>;
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

    fn fec_metadata(&self) -> Option<mm_protocol::FecMetadata> {
        self.fec_metadata.clone()
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

    fn fec_metadata(&self) -> Option<mm_protocol::FecMetadata> {
        self.fec_metadata.clone()
    }
}

#[derive(Debug)]
enum FECDecoder {
    Plain(Vec<Option<bytes::Bytes>>),
    RaptorQ {
        dec: raptorq::Decoder,
        res: Option<bytes::Bytes>,
    },
}

#[derive(Debug)]
struct WipPacket {
    stream_seq: u64,
    seq: u64,
    pts: u64,
    frame_optional: bool,
    decoder: FECDecoder,
}

impl WipPacket {
    fn new(incoming: impl Chunk) -> Result<Self, PacketRingError> {
        let decoder = if let Some(md) = incoming.fec_metadata() {
            if md.fec_scheme() != protocol::fec_metadata::FecScheme::Raptorq {
                return Err(PacketRingError::UnsupportedFecScheme(md.fec_scheme));
            }

            let oti: &[u8] = &md.fec_oti;
            let Ok(config) = oti
                .try_into()
                .map(raptorq::ObjectTransmissionInformation::deserialize)
            else {
                return Err(PacketRingError::InvalidFecMetadata);
            };

            FECDecoder::RaptorQ {
                dec: raptorq::Decoder::new(config),
                res: None,
            }
        } else {
            FECDecoder::Plain(vec![None; incoming.num_chunks().max(1) as usize])
        };

        let mut this = Self {
            stream_seq: incoming.stream_seq(),
            seq: incoming.seq(),
            pts: incoming.pts(),
            frame_optional: incoming.frame_optional(),
            decoder,
        };

        this.insert(incoming)?;
        Ok(this)
    }

    fn insert(&mut self, incoming: impl Chunk) -> Result<(), PacketRingError> {
        match &mut self.decoder {
            FECDecoder::Plain(ref mut chunks) => {
                let chunk = incoming.chunk() as usize;
                let num_chunks = incoming.num_chunks() as usize;
                if num_chunks != chunks.len() || chunk >= num_chunks {
                    return Err(PacketRingError::InvalidChunk(chunk, num_chunks));
                } else if chunks[chunk].is_some() {
                    return Err(PacketRingError::DuplicateChunk(chunk));
                }

                chunks[chunk] = Some(incoming.data());
                Ok(())
            }
            FECDecoder::RaptorQ { dec, .. } => {
                let Some(md) = incoming.fec_metadata() else {
                    return Err(PacketRingError::InvalidFecMetadata);
                };

                let b: &[u8] = &md.fec_payload_id;
                let Ok(payload_id) = b.try_into().map(raptorq::PayloadId::deserialize) else {
                    return Err(PacketRingError::InvalidFecMetadata);
                };

                dec.add_new_packet(raptorq::EncodingPacket::new(
                    payload_id,
                    incoming.data().into(),
                ));
                Ok(())
            }
        }
    }

    fn is_complete(&mut self) -> bool {
        match &mut self.decoder {
            FECDecoder::Plain(chunks) => chunks.iter().all(|c| c.is_some()),
            FECDecoder::RaptorQ { dec, ref mut res } => {
                if res.is_some() {
                    true
                } else if let Some(data) = dec.get_result() {
                    *res = Some(bytes::Bytes::from(data));
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Reconstructs the completed frame. Panics if the packet is not yet
    /// recoverable.
    fn complete(self) -> Packet {
        let data = match self.decoder {
            FECDecoder::Plain(chunks) => {
                let chunks: Vec<_> = chunks
                    .into_iter()
                    .map(|c| c.expect("packet incomplete"))
                    .collect();

                chunks.into()
            }
            FECDecoder::RaptorQ { dec, res } => {
                let data = res.unwrap_or_else(|| {
                    bytes::Bytes::from(dec.get_result().expect("packet incomplete"))
                });

                [data].into()
            }
        };

        Packet {
            pts: self.pts,
            seq: self.seq,
            stream_seq: self.stream_seq,
            data,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, thiserror::Error)]
pub(crate) enum PacketRingError {
    #[error("invalid chunk {0} of {1}")]
    InvalidChunk(usize, usize),
    #[error("duplicate chunk {0}")]
    DuplicateChunk(usize),
    #[error("unsupported FEC scheme: {0}")]
    UnsupportedFecScheme(i32),
    #[error("invalid FEC metadatata")]
    InvalidFecMetadata,
}

#[derive(Default)]
pub(crate) struct PacketRing {
    // Oldest frames at the front, newest at the back.
    ring: VecDeque<WipPacket>,
    min_stream_seq: u64,
    min_seq: BTreeMap<u64, u64>, // Indexed by stream_seq.
    dropped: VecDeque<DroppedPacket>,
}

impl PacketRing {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn recv_chunk(&mut self, incoming: impl Chunk) -> Result<(), PacketRingError> {
        let stream_seq = incoming.stream_seq();
        let seq_floor = self.min_seq.get(&stream_seq).copied().unwrap_or_default();
        if incoming.stream_seq() < self.min_stream_seq || incoming.seq() < seq_floor {
            return Ok(());
        }

        match self
            .ring
            .iter_mut()
            .find(|wip| wip.stream_seq == incoming.stream_seq() && wip.seq == incoming.seq())
        {
            Some(wip) => wip.insert(incoming),
            None => {
                let wip = WipPacket::new(incoming)?;

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
                    let len = self.ring.len();
                    let front = self.ring.front_mut().unwrap();

                    if front.is_complete() || len <= RING_TARGET_SIZE {
                        break;
                    }

                    // If the oldest frame is incomplete, drop it to make room.
                    if !front.is_complete() {
                        let dropped = self.ring.pop_front().unwrap();

                        warn!(
                            seq = dropped.seq,
                            stream_seq = dropped.stream_seq,
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
        self.min_stream_seq = stream_seq + 1;
        self.ring.retain(|wip| wip.stream_seq > stream_seq);
        self.min_seq.retain(|x, _| *x > stream_seq);
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
            self.0.min_seq.insert(dropped.stream_seq, dropped.seq + 1);
            return Some(Err(dropped));
        }

        let ring = &mut self.0.ring;
        match ring
            .iter_mut()
            .enumerate()
            .find(|(_, wip)| wip.stream_seq == self.1)
        {
            Some((idx, ref mut v)) => {
                if v.is_complete() {
                    self.0.min_seq.insert(v.stream_seq, v.seq + 1);
                    Some(Ok(ring.remove(idx).unwrap().complete()))
                } else {
                    None
                }
            }
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
                fec_metadata: None,
            })
            .collect()
    }
}
