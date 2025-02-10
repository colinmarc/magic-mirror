// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use bytes::Bytes;
use either::Either;
use mm_protocol as protocol;
use tracing::{debug, error, instrument, trace_span};

use crate::{config, waking_sender::WakingSender};

/// A helper to write audio/video frames out as chunks to the client. Runs on
/// the encoder thread, not on the server thread.
pub struct StreamWriter {
    session_id: u64,
    attachment_id: u64,
    outgoing: WakingSender<Vec<u8>>,

    chunk_size: usize,
    max_dgram_len: usize,
    fec_ratios: Vec<f32>,

    audio_stream_seq: u64,
    audio_seq: u64,

    video_stream_seq: u64,
    video_seq: u64,
}

impl StreamWriter {
    pub fn new(
        session_id: u64,
        attachment_id: u64,
        config: &config::ServerConfig,
        outgoing: WakingSender<Vec<u8>>,
        max_dgram_len: usize,
    ) -> Self {
        // max_dgram_len is our overall MTU. The MM protocol header is 2-10 bytes,
        // and then we include seven varints (maximum 5 bytes each) and a bool of
        // metadata, plus an optional 12-ish bytes of FEC information. 64 bytes of
        // headroom should cover the worst case. However, a little extra will
        // increase the chance that the packet is coalesced into an existing QUIC
        // packet.
        let chunk_size = max_dgram_len - 128;

        Self {
            session_id,
            attachment_id,
            outgoing,

            chunk_size,
            max_dgram_len,
            fec_ratios: config.video_fec_ratios.clone(),

            // The first stream_seq is 1, but we increment immediately below.
            audio_stream_seq: 0,
            video_stream_seq: 0,
            audio_seq: 0,
            video_seq: 0,
        }
    }

    #[instrument(skip_all)]
    pub fn write_video_frame(
        &mut self,
        pts: u64,
        frame: Bytes,
        hierarchical_layer: u32,
        stream_restart: bool,
    ) -> (u64, u64) {
        if stream_restart {
            self.video_stream_seq += 1;
            self.video_seq = 0;

            debug!(
                stream_seq = self.video_stream_seq,
                "starting or restarting video stream"
            );
        }

        let seq = self.video_seq;
        let fec_ratio = self
            .fec_ratios
            .get(hierarchical_layer as usize)
            .copied()
            .unwrap_or_default();

        for chunk in iter_chunks(frame, self.chunk_size, fec_ratio) {
            let msg = protocol::VideoChunk {
                session_id: self.session_id,
                attachment_id: self.attachment_id,

                stream_seq: self.video_stream_seq,
                seq,
                data: chunk.data,
                chunk: chunk.index,
                num_chunks: chunk.num_chunks,
                hierarchical_layer,
                timestamp: pts,

                fec_metadata: chunk.fec_metadata,
            };

            let res: Result<_, protocol::ProtocolError> =
                trace_span!("encode_message").in_scope(|| {
                    let mut buf = vec![0; self.max_dgram_len];
                    let len = protocol::encode_message(&msg.into(), &mut buf)?;

                    buf.truncate(len);
                    Ok(buf)
                });

            match res {
                Ok(buf) => {
                    let _ = self.outgoing.send(buf);
                }
                Err(err) => {
                    error!(?err, "failed to encode video chunk");
                }
            };
        }

        self.video_seq += 1;
        (self.video_stream_seq, seq)
    }

    #[instrument(skip_all)]
    pub fn write_audio_frame(
        &mut self,
        pts: u64,
        frame: Bytes,
        stream_restart: bool,
    ) -> (u64, u64) {
        if stream_restart {
            self.audio_stream_seq += 1;
            self.audio_seq = 0;
            debug!(
                stream_seq = self.audio_stream_seq,
                "starting or restarting audio stream"
            );
        }

        let seq = self.audio_seq;
        for chunk in iter_chunks(frame, self.chunk_size, 0.0) {
            let msg = protocol::AudioChunk {
                session_id: self.session_id,
                attachment_id: self.attachment_id,

                stream_seq: self.audio_stream_seq,
                seq,
                data: chunk.data,
                chunk: chunk.index,
                num_chunks: chunk.num_chunks,
                timestamp: pts,

                fec_metadata: chunk.fec_metadata,
            };

            let res: Result<_, protocol::ProtocolError> =
                trace_span!("encode_message").in_scope(|| {
                    let mut buf = vec![0; self.max_dgram_len];
                    let len = protocol::encode_message(&msg.into(), &mut buf)?;

                    buf.truncate(len);
                    Ok(buf)
                });

            match res {
                Ok(buf) => {
                    let _ = self.outgoing.send(buf);
                }
                Err(err) => {
                    error!(?err, "failed to encode audio chunk");
                }
            };
        }

        self.audio_seq += 1;
        (self.audio_stream_seq, seq)
    }
}

pub struct Chunk {
    pub index: u32,
    pub num_chunks: u32,
    pub data: Bytes,
    pub fec_metadata: Option<protocol::FecMetadata>,
}

pub fn iter_chunks(
    mut buf: bytes::Bytes,
    mtu: usize,
    fec_ratio: f32,
) -> impl Iterator<Item = Chunk> {
    if fec_ratio > 0.0 {
        return Either::Left(iter_chunks_fec(buf, mtu, fec_ratio));
    }

    let num_chunks = buf.len().div_ceil(mtu) as u32;
    let mut next_chunk: u32 = 0;

    let span = trace_span!("iter_chunks");
    let _guard = span.enter();

    Either::Right(std::iter::from_fn(move || {
        if buf.is_empty() {
            return None;
        }

        let data = if buf.len() < mtu {
            buf.split_to(buf.len())
        } else {
            buf.split_to(mtu)
        };

        let chunk = next_chunk;
        next_chunk += 1;
        Some(Chunk {
            index: chunk,
            num_chunks,
            data,
            fec_metadata: None,
        })
    }))
}

#[instrument(skip_all)]
fn iter_chunks_fec(buf: Bytes, mtu: usize, ratio: f32) -> impl Iterator<Item = Chunk> {
    let encoder = raptorq::Encoder::with_defaults(&buf, mtu as u16);
    let oti = Bytes::copy_from_slice(&encoder.get_config().serialize());

    let base_chunks = buf.len().div_ceil(mtu) as u32;
    let repair_chunks = (base_chunks as f32 * ratio).ceil() as u32;
    let chunks = encoder.get_encoded_packets(repair_chunks);
    let num_chunks = chunks.len() as u32;

    chunks.into_iter().enumerate().map(move |(chunk, p)| Chunk {
        index: chunk as u32,
        num_chunks,
        data: Bytes::copy_from_slice(p.data()),
        fec_metadata: Some(mm_protocol::FecMetadata {
            fec_scheme: protocol::fec_metadata::FecScheme::Raptorq.into(),
            fec_payload_id: Bytes::copy_from_slice(&p.payload_id().serialize()),
            fec_oti: oti.clone(),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_iter_chunks() {
        let frame = bytes::Bytes::from(vec![9; 3536]);
        let mut chunks = iter_chunks(frame, 1200, 0.0);
        let chunk = chunks.next().unwrap();
        assert_eq!(chunk.index, 0);
        assert_eq!(chunk.num_chunks, 3);
        assert_eq!(chunk.data.len(), 1200);
        assert_eq!(chunk.fec_metadata, None);

        let chunk = chunks.next().unwrap();
        assert_eq!(chunk.index, 1);
        assert_eq!(chunk.num_chunks, 3);
        assert_eq!(chunk.data.len(), 1200);
        assert_eq!(chunk.fec_metadata, None);

        let chunk = chunks.next().unwrap();
        assert_eq!(chunk.index, 2);
        assert_eq!(chunk.num_chunks, 3);
        assert_eq!(chunk.data.len(), 1136);
        assert_eq!(chunk.fec_metadata, None);

        assert!(chunks.next().is_none());
    }

    #[test]
    fn test_iter_chunks_fec() {
        let frame = bytes::Bytes::from(vec![9; 3536]);
        let mut chunks = iter_chunks(frame, 1200, 0.15);
        let chunk = chunks.next().unwrap();
        assert_eq!(chunk.index, 0);
        assert_eq!(chunk.num_chunks, 4);
        assert_eq!(chunk.data.len(), 1200);
        assert_eq!(
            chunk.fec_metadata.as_ref().unwrap().fec_scheme(),
            protocol::fec_metadata::FecScheme::Raptorq
        );
        assert_eq!(chunk.fec_metadata.as_ref().unwrap().fec_oti.len(), 12);

        let chunk = chunks.next().unwrap();
        assert_eq!(chunk.index, 1);
        assert_eq!(chunk.num_chunks, 4);
        assert_eq!(chunk.data.len(), 1200);
        assert_eq!(
            chunk.fec_metadata.as_ref().unwrap().fec_scheme(),
            protocol::fec_metadata::FecScheme::Raptorq
        );
        assert_eq!(chunk.fec_metadata.as_ref().unwrap().fec_oti.len(), 12);

        let chunk = chunks.next().unwrap();
        assert_eq!(chunk.index, 2);
        assert_eq!(chunk.num_chunks, 4);
        assert_eq!(chunk.data.len(), 1200);
        assert_eq!(
            chunk.fec_metadata.as_ref().unwrap().fec_scheme(),
            protocol::fec_metadata::FecScheme::Raptorq
        );
        assert_eq!(chunk.fec_metadata.as_ref().unwrap().fec_oti.len(), 12);

        let chunk = chunks.next().unwrap();
        assert_eq!(chunk.index, 3);
        assert_eq!(chunk.num_chunks, 4);
        assert_eq!(chunk.data.len(), 1200);
        assert_eq!(
            chunk.fec_metadata.as_ref().unwrap().fec_scheme(),
            protocol::fec_metadata::FecScheme::Raptorq
        );
        assert_eq!(chunk.fec_metadata.as_ref().unwrap().fec_oti.len(), 12);

        assert!(chunks.next().is_none());
    }
}
