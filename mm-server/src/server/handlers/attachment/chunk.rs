// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use bytes::Bytes;
use either::Either;
use mm_protocol as protocol;

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
