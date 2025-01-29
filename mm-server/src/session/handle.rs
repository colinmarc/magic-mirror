// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

use crossbeam_channel as crossbeam;

use super::control::SessionEvent;
use crate::server::stream::StreamWriter;

struct Client {
    events: crossbeam::Sender<SessionEvent>,
    writer: StreamWriter,
}

struct Inner {
    attachments: BTreeMap<u64, Client>,
}

#[derive(Clone)]
pub struct SessionHandle(Arc<RwLock<Inner>>, Arc<mio::Waker>);

impl SessionHandle {
    pub fn new(waker: Arc<mio::Waker>) -> Self {
        Self(
            Arc::new(RwLock::new(Inner {
                attachments: BTreeMap::new(),
            })),
            waker,
        )
    }

    pub fn insert_client(
        &self,
        id: u64,
        events: crossbeam::Sender<SessionEvent>,
        writer: StreamWriter,
    ) {
        self.0
            .write()
            .unwrap()
            .attachments
            .insert(id, Client { events, writer });
    }

    pub fn remove_client(&self, id: u64) {
        self.0.write().unwrap().attachments.remove(&id);
    }

    pub fn remove_all(&self) {
        self.0.write().unwrap().attachments.clear();
    }

    pub fn dispatch(&self, event: SessionEvent) {
        let attachments = &self.0.read().unwrap().attachments;
        for (_, client) in attachments.iter() {
            let _ = client.events.send(event.clone());
        }
    }

    pub fn dispatch_audio_frame(&self, stream_seq: u64, seq: u64, pts: u64, frame: bytes::Bytes) {
        let attachments = &self.0.read().unwrap().attachments;
        for (_, client) in attachments.iter() {
            let _ = client.events.send(SessionEvent::AudioFrame {
                _stream_seq: stream_seq,
                seq,
                frame: frame.clone(),
            });

            client
                .writer
                .write_audio_frame(stream_seq, seq, pts, frame.clone());
        }
    }

    pub fn dispatch_video_frame(
        &self,
        stream_seq: u64,
        seq: u64,
        pts: u64,
        frame: bytes::Bytes,
        hierarchical_layer: u32,
    ) {
        let attachments = &self.0.read().unwrap().attachments;
        for (_, client) in attachments.iter() {
            let _ = client.events.send(SessionEvent::VideoFrame {
                stream_seq,
                seq,
                frame: frame.clone(),
            });

            client.writer.write_video_frame(
                stream_seq,
                seq,
                pts,
                frame.clone(),
                hierarchical_layer,
            );
        }
    }

    pub fn wake(&self) -> std::io::Result<()> {
        self.1.wake()
    }

    pub fn kick_clients(&self) {
        let attachments = &mut self.0.write().unwrap().attachments;
        for (_, client) in std::mem::take(attachments) {
            let _ = client.events.send(SessionEvent::Shutdown);
        }
    }

    pub fn num_attachments(&self) -> usize {
        self.0.read().unwrap().attachments.len()
    }
}
