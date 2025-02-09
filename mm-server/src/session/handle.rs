// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{collections::BTreeMap, sync::Arc};

use crossbeam_channel as crossbeam;
use parking_lot::Mutex;

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
pub struct SessionHandle(Arc<Mutex<Inner>>, Arc<mio::Waker>);

impl SessionHandle {
    pub fn new(waker: Arc<mio::Waker>) -> Self {
        Self(
            Arc::new(Mutex::new(Inner {
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
            .lock()
            .attachments
            .insert(id, Client { events, writer });
    }

    pub fn remove_client(&self, id: u64) {
        self.0.lock().attachments.remove(&id);
    }

    pub fn remove_all(&self) {
        self.0.lock().attachments.clear();
    }

    pub fn dispatch(&self, event: SessionEvent) {
        let attachments = &self.0.lock().attachments;
        for (_, client) in attachments.iter() {
            let _ = client.events.send(event.clone());
        }
    }

    pub fn dispatch_audio_frame(&self, pts: u64, frame: bytes::Bytes, stream_restart: bool) {
        let attachments = &mut self.0.lock().attachments;
        for (_, client) in attachments.iter_mut() {
            let (stream_seq, seq) =
                client
                    .writer
                    .write_audio_frame(pts, frame.clone(), stream_restart);
            let _ = client.events.send(SessionEvent::AudioFrame {
                _stream_seq: stream_seq,
                seq,
                frame: frame.clone(),
            });
        }
    }

    pub fn dispatch_video_frame(
        &self,
        pts: u64,
        frame: bytes::Bytes,
        hierarchical_layer: u32,
        stream_restart: bool,
    ) {
        let attachments = &mut self.0.lock().attachments;
        for (_, client) in attachments.iter_mut() {
            let (stream_seq, seq) = client.writer.write_video_frame(
                pts,
                frame.clone(),
                hierarchical_layer,
                stream_restart,
            );

            let _ = client.events.send(SessionEvent::VideoFrame {
                stream_seq,
                seq,
                frame: frame.clone(),
            });
        }
    }

    pub fn wake(&self) -> std::io::Result<()> {
        self.1.wake()
    }

    pub fn kick_clients(&self) {
        let attachments = &mut self.0.lock().attachments;
        for (_, client) in std::mem::take(attachments) {
            let _ = client.events.send(SessionEvent::Shutdown);
        }
    }

    pub fn num_attachments(&self) -> usize {
        self.0.lock().attachments.len()
    }
}
