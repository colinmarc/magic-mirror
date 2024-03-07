// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use crate::waking_sender::WakingSender;

use super::{AttachedClients, AudioStreamParams, CompositorEvent};

mod pulse;
use anyhow::Context as _;
use bytes::BytesMut;
use crossbeam_channel as crossbeam;
use pulse::PulseServer;
use tracing::error;

struct EncodeFrame {
    buf: Vec<f32>,
    capture_ts: u64,
}

struct Encoder {
    thread_handle: Option<std::thread::JoinHandle<anyhow::Result<()>>>,
    close_tx: crossbeam::Sender<()>,
}

impl Drop for Encoder {
    fn drop(&mut self) {
        if let Some(handle) = self.thread_handle.take() {
            let _ = self.close_tx.send(());

            match handle.join() {
                Ok(Ok(())) => (),
                Ok(Err(e)) => error!("audio encoder thread died: {}", e),
                Err(_) => error!("audio encoder thread panicked"),
            }
        }
    }
}

pub struct EncodePipeline {
    server_thread_handle: Option<std::thread::JoinHandle<anyhow::Result<()>>>,
    server_close_tx: WakingSender<()>,

    attachments: AttachedClients,
    stream_seq: u64,

    encoder: Option<Encoder>,
    done_tx: crossbeam::Sender<EncodeFrame>,
    unencoded_rx: Arc<Mutex<crossbeam::Receiver<EncodeFrame>>>,
}

impl EncodePipeline {
    pub fn new(
        attachments: AttachedClients,
        xdg_runtime_dir: &Path,
    ) -> anyhow::Result<EncodePipeline> {
        // In this location, the server gets picked up without setting PULSE_SERVER
        // explicitly.
        std::fs::create_dir_all(Path::join(xdg_runtime_dir, "pulse"))?;
        let socket_name = Path::join(xdg_runtime_dir, "pulse/native");

        // The pulse server reads empty frames from the done channel, fills
        // them, and sends them back over the undecoded channel.
        let (unencoded_tx, unencoded_rx) = crossbeam::unbounded();
        let (done_tx, done_rx) = crossbeam::unbounded();

        let (mut server, close_tx) = PulseServer::new(&socket_name, unencoded_tx, done_rx)
            .context("creating PulseAudio server")?;

        let server_handle = std::thread::Builder::new()
            .name(format!("pulse server ({})", socket_name.to_string_lossy()))
            .spawn(move || server.run())?;

        Ok(Self {
            server_thread_handle: Some(server_handle),
            server_close_tx: close_tx,

            attachments,
            stream_seq: 0,

            encoder: None,
            done_tx,
            // We wrap the receiver in a mutex to ensure that only one encoder
            // is interacting with the pulse server at a time (and because it's
            // not Clone).
            unencoded_rx: Arc::new(Mutex::new(unencoded_rx)),
        })
    }

    pub fn stop_stream(&mut self) {
        self.encoder = None;
    }

    pub fn restart_stream(&mut self, params: AudioStreamParams) -> anyhow::Result<()> {
        // TODO: pass sample rate on input frames, do resampling on the pulse side.
        // For now we only support 48khz stereo anyway.
        assert_eq!(params.sample_rate, pulse::CAPTURE_SAMPLE_RATE);
        assert_eq!(params.channels, pulse::CAPTURE_CHANNEL_COUNT);

        assert!(self.encoder.is_none());
        let done_tx = self.done_tx.clone();
        let unencoded_rx = self.unencoded_rx.clone();

        let stream_seq = self.stream_seq;
        self.stream_seq += 1;

        let (close_tx, close_rx) = crossbeam::unbounded();

        let ch = match params.channels {
            1 => opus::Channels::Mono,
            2 => opus::Channels::Stereo,
            _ => panic!("unsupported number of channels: {}", params.channels),
        };

        let mut encoder = opus::Encoder::new(params.sample_rate, ch, opus::Application::LowDelay)
            .context("failed to create opus encoder")?;

        let attachments = self.attachments.clone();
        let thread_handle = std::thread::Builder::new()
            .name("audio encode".into())
            .spawn(move || {
                // Lock the receiver until the encoder thread exits.
                let unencoded_rx = unencoded_rx.lock().unwrap();

                let mut buf = BytesMut::new();
                let mut seq = 0;

                let mut in_flight = 3;
                for _ in 0..in_flight {
                    done_tx
                        .send(EncodeFrame {
                            buf: Vec::new(),
                            capture_ts: 0,
                        })
                        .unwrap();
                }

                let mut closing = false;
                while in_flight > 0 {
                    if let Ok(()) = close_rx.try_recv() {
                        closing = true;
                    }

                    let frame = match unencoded_rx.recv() {
                        Ok(frame) => frame,
                        Err(_) => return Ok(()), // Pulse server hung up.
                    };

                    buf.resize(frame.buf.len(), 0);

                    let len = encoder.encode_float(&frame.buf, &mut buf)?;
                    attachments.dispatch(CompositorEvent::AudioFrame {
                        stream_seq,
                        seq,
                        ts: frame.capture_ts,
                        frame: buf.split_to(len).freeze(),
                    });

                    seq += 1;

                    if !closing {
                        match done_tx.send(frame) {
                            Ok(()) => (),
                            Err(_) => return Ok(()), // Pulse server hung up.
                        }
                    } else {
                        in_flight -= 1;
                    }
                }

                Ok(())
            })?;

        self.encoder = Some(Encoder {
            thread_handle: Some(thread_handle),
            close_tx,
        });

        Ok(())
    }
}

impl Drop for EncodePipeline {
    fn drop(&mut self) {
        let _ = self.server_close_tx.send(());

        if let Some(handle) = self.server_thread_handle.take() {
            match handle.join() {
                Ok(Ok(())) => (),
                Ok(Err(e)) => error!("pulseaudio server error: {}", e),
                Err(_) => error!("pulseaudio server panicked"),
            }
        }
    }
}
