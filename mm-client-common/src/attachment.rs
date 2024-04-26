// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use std::sync::{Arc, Mutex};

use crossbeam_channel as crossbeam;
use futures::channel::oneshot;
use tracing::error;

use crate::{
    codec, conn, display_params, input,
    packet::{self, PacketRing},
    ClientError,
};
use mm_protocol as protocol;

pub use protocol::audio_channels::Channel as AudioChannel;

#[derive(Debug, Clone, uniffi::Record)]
pub struct AttachmentConfig {
    /// The width of the video stream.
    pub width: u32,
    /// The height of the video stream.
    pub height: u32,

    /// The codec to use for the video stream. Leaving it empty allows the
    /// server to decide.
    pub video_codec: Option<codec::VideoCodec>,

    /// The profile (bit depth and colorspace) to use for the video stream.
    /// Leaving it empty allows the server to decide.
    pub video_profile: Option<codec::VideoProfile>,

    /// The quality preset, from 1-10. A None or 0 indicates the server should
    /// decide.
    pub quality_preset: Option<u32>,

    /// The codec to use for the audio stream. Leaving it empty allows the
    /// server to decide.
    pub audio_codec: Option<codec::AudioCodec>,

    /// The sample rate to use for the audio stream. Leaving it empty allows the
    /// server to decide.
    pub sample_rate: Option<u32>,

    /// The channel layout to use for the audio stream. An empty vec indicates
    /// the server should decide.
    pub channels: Vec<AudioChannel>,

    /// An offset to apply to the stream_seq of incoming video packets. The
    /// offset is applied on the client side, and exists as a convenient way to
    /// way to ensure sequence numbers stay monotonic, even across individual
    /// attachment streams.
    pub video_stream_seq_offset: u64,

    /// An offset to apply to the stream_seq of incoming audio packets. The
    /// offset is applied on the client side, and exists as a convenient way to
    /// way to ensure sequence numbers stay monotonic, even across individual
    /// attachment streams.
    pub audio_stream_seq_offset: u64,
}

/// The settled video stream params, after the server has applied its defaults.
#[derive(Debug, Clone, uniffi::Record)]
pub struct VideoStreamParams {
    pub width: u32,
    pub height: u32,

    pub codec: codec::VideoCodec,
    pub profile: codec::VideoProfile,
}

/// The settled audio stream params, after the server has applied its defaults.
#[derive(Debug, Clone, uniffi::Record)]
pub struct AudioStreamParams {
    pub codec: codec::AudioCodec,
    pub sample_rate: u32,
    pub channels: Vec<AudioChannel>,
}

/// A handle for sending messages to the server over an attachment stream.
///
/// An attachment is ended once the corresponding AttachmentDelegate receives
/// the attachment_ended or parameters_changed (with reattach_required = true)
/// callbacks. Using it past that point will silently drop events.
#[derive(uniffi::Object)]
pub struct Attachment {
    sid: u64,
    client: Arc<Mutex<super::InnerClient>>,

    // We store a copy of these so that we can send messages on the attachment
    // stream without locking the client mutex.
    outgoing_tx: crossbeam::Sender<conn::OutgoingMessage>,
    conn_waker: Arc<mio::Waker>,
}

impl Attachment {
    pub(crate) fn new(
        sid: u64,
        client: Arc<Mutex<super::InnerClient>>,
        attached: protocol::Attached,
        delegate: Arc<dyn AttachmentDelegate>,
        video_stream_seq_offset: u64,
    ) -> Result<Self, ClientError> {
        let session_id = attached.session_id;
        let attachment_id = attached.attachment_id;
        let state = AttachmentState {
            session_id,
            attachment_id,

            delegate,
            attached_msg: attached,

            video_packet_ring: PacketRing::new(),
            video_stream_seq: 0,
            prev_video_stream_seq: 0,
            video_stream_seq_offset,

            audio_packet_ring: PacketRing::new(),
            audio_stream_seq: 0,
            prev_audio_stream_seq: 0,
            audio_stream_seq_offset: 0,

            notify_detached: None,
            reattach_required: false,
        };

        let client_clone = client.clone();
        let mut guard = client.lock().unwrap();

        let Some(super::ConnHandle {
            outgoing_tx, waker, ..
        }) = &guard.conn_handle
        else {
            return Err(ClientError::Defunct);
        };

        let outgoing_tx = outgoing_tx.clone();
        let conn_waker = waker.clone();

        // Track the attachment in the client, so that the reactor thread will
        // send us messages.
        guard.attachments.insert(sid, state);

        Ok(Self {
            sid,
            client: client_clone,
            outgoing_tx,
            conn_waker,
        })
    }
}

/// Used by client implementations to handle attachment events.
#[uniffi::export(with_foreign)]
pub trait AttachmentDelegate: Send + Sync + std::fmt::Debug {
    /// The video stream is starting or restarting.
    fn video_stream_start(&self, stream_seq: u64, params: VideoStreamParams);

    /// A video packet is available.
    fn video_packet(&self, packet: Arc<packet::Packet>);

    /// The audio stream is starting or restarting.
    fn audio_stream_start(&self, stream_seq: u64, params: AudioStreamParams);

    /// An audio packet is available.
    fn audio_packet(&self, packet: Arc<packet::Packet>);

    // The cursor was updated.
    fn update_cursor(
        &self,
        icon: input::CursorIcon,
        image: Vec<u8>,
        hotspot_x: u32,
        hotspot_y: u32,
    );

    // The pointer should be locked to the given location.
    fn lock_pointer(&self, x: f64, y: f64);

    /// The remote session display params were changed. This usually requires
    /// the client to reattach. If reattach_required is true, the attachment
    /// should be considered ended. [attachment_ended] will not be called.
    fn display_params_changed(
        &self,
        params: display_params::DisplayParams,
        reattach_required: bool,
    );

    /// An error was sent by the server. Usually, the attachment will be
    /// subsequently ended.
    fn error(&self, error_code: crate::ErrorCode, error_text: String);

    /// The attachment was ended by the server.
    fn attachment_ended(&self);
}

impl Attachment {
    fn send(&self, msg: impl Into<protocol::MessageType>, fin: bool) -> Result<(), ClientError> {
        if self
            .outgoing_tx
            .send(conn::OutgoingMessage {
                sid: self.sid,
                msg: msg.into(),
                fin,
            })
            .is_err()
        {
            return self.client.lock().unwrap().close();
        }

        let _ = self.conn_waker.wake();
        Ok(())
    }
}

#[uniffi::export]
impl Attachment {
    /// Sends keyboard input to the server.
    pub fn keyboard_input(
        &self,
        key: input::Key,
        state: input::KeyState,
        char: u32,
    ) -> Result<(), ClientError> {
        self.send(
            protocol::KeyboardInput {
                key: key.into(),
                state: state.into(),
                char,
            },
            false,
        )
    }

    /// Notifies the server that the pointer has left the video area. This
    /// should also be called if the pointer enters a letterbox.
    pub fn pointer_entered(&self) -> Result<(), ClientError> {
        self.send(protocol::PointerEntered {}, false)
    }

    /// Notifies the server that the pointer has entered the video area. This
    /// should not consider the letterbox.
    pub fn pointer_left(&self) -> Result<(), ClientError> {
        self.send(protocol::PointerLeft {}, false)
    }

    /// Sends pointer motion to the server.
    pub fn pointer_motion(&self, x: f64, y: f64) -> Result<(), ClientError> {
        self.send(protocol::PointerMotion { x, y }, false)
    }

    /// Sends pointer input to the server.
    pub fn pointer_input(
        &self,
        button: input::Button,
        state: input::ButtonState,
        x: f64,
        y: f64,
    ) -> Result<(), ClientError> {
        self.send(
            protocol::PointerInput {
                button: button.into(),
                state: state.into(),
                x,
                y,
            },
            false,
        )
    }

    /// Sends pointer scroll events to the server.
    pub fn pointer_scroll(
        &self,
        scroll_type: input::ScrollType,
        x: f64,
        y: f64,
    ) -> Result<(), ClientError> {
        self.send(
            protocol::PointerScroll {
                scroll_type: scroll_type.into(),
                x,
                y,
            },
            false,
        )
    }

    /// Ends the attachment.
    pub async fn detach(&self) -> Result<(), ClientError> {
        let (detached_tx, detached_rx) = oneshot::channel();

        {
            let mut guard = self.client.lock().unwrap();
            let Some(state) = guard.attachments.get_mut(&self.sid) else {
                return Err(ClientError::Detached);
            };

            state.notify_detached = Some(detached_tx);
        }

        self.send(protocol::Detach {}, true)?;
        Ok(detached_rx.await?)
    }
}

/// Internal state for an attachment.
pub(crate) struct AttachmentState {
    pub(crate) session_id: u64,
    pub(crate) attachment_id: u64,

    delegate: Arc<dyn AttachmentDelegate>,
    attached_msg: protocol::Attached,
    reattach_required: bool,

    video_packet_ring: PacketRing,
    video_stream_seq: u64,
    prev_video_stream_seq: u64,
    video_stream_seq_offset: u64,

    audio_packet_ring: PacketRing,
    audio_stream_seq: u64,
    prev_audio_stream_seq: u64,
    audio_stream_seq_offset: u64,

    // A future representing the end of the attachment. Some only if the
    // attachment was ended by the client.
    notify_detached: Option<oneshot::Sender<()>>,
}

impl AttachmentState {
    pub(crate) fn handle_message(&mut self, msg: protocol::MessageType) {
        match msg {
            protocol::MessageType::Attached(attached) => {
                error!(
                    "unexpected {} on already-attached stream",
                    protocol::MessageType::Attached(attached)
                );
            }
            protocol::MessageType::VideoChunk(chunk) => {
                // We always send packets for two streams - the current one and
                // (if there is one) the previous one.
                if chunk.stream_seq > self.video_stream_seq {
                    // A new stream started.
                    self.prev_video_stream_seq = self.video_stream_seq;
                    self.video_stream_seq = chunk.stream_seq;

                    let res = self.attached_msg.streaming_resolution.unwrap_or_default();

                    self.delegate.video_stream_start(
                        self.video_stream_seq + self.video_stream_seq_offset,
                        VideoStreamParams {
                            width: res.width,
                            height: res.height,
                            codec: self.attached_msg.video_codec(),
                            profile: self.attached_msg.video_profile(),
                        },
                    );

                    // Discard any older packets.
                    self.video_packet_ring
                        .discard(self.prev_video_stream_seq.saturating_sub(1));
                }

                if let Err(err) = self.video_packet_ring.recv_chunk(chunk) {
                    error!("error in packet ring: {:#}", err);
                }

                for mut packet in self
                    .video_packet_ring
                    .drain_completed(self.prev_video_stream_seq)
                {
                    packet.stream_seq += self.video_stream_seq_offset;
                    self.delegate.video_packet(Arc::new(packet));
                }

                if self.video_stream_seq != self.prev_video_stream_seq {
                    for mut packet in self
                        .video_packet_ring
                        .drain_completed(self.video_stream_seq)
                    {
                        packet.stream_seq += self.video_stream_seq_offset;
                        self.delegate.video_packet(Arc::new(packet));
                    }
                }
            }
            protocol::MessageType::AudioChunk(chunk) => {
                // We always send packets for two streams - the current one and
                // (if there is one) the previous one.
                if chunk.stream_seq > self.audio_stream_seq {
                    // A new stream started.
                    self.prev_audio_stream_seq = self.audio_stream_seq;
                    self.audio_stream_seq = chunk.stream_seq;

                    let channels = self
                        .attached_msg
                        .channels
                        .as_ref()
                        .map(|c| c.channels().collect())
                        .unwrap_or_default();

                    self.delegate.audio_stream_start(
                        self.audio_stream_seq + self.audio_stream_seq_offset,
                        AudioStreamParams {
                            codec: self.attached_msg.audio_codec(),
                            sample_rate: self.attached_msg.sample_rate_hz,
                            channels,
                        },
                    );

                    // Discard any older packets.
                    self.audio_packet_ring
                        .discard(self.prev_audio_stream_seq.saturating_sub(1));
                }

                if let Err(err) = self.audio_packet_ring.recv_chunk(chunk) {
                    error!("error in packet ring: {:#}", err);
                }

                for mut packet in self
                    .audio_packet_ring
                    .drain_completed(self.prev_audio_stream_seq)
                {
                    packet.stream_seq += self.audio_stream_seq_offset;
                    self.delegate.audio_packet(Arc::new(packet));
                }

                if self.audio_stream_seq != self.prev_audio_stream_seq {
                    for mut packet in self
                        .audio_packet_ring
                        .drain_completed(self.audio_stream_seq)
                    {
                        packet.stream_seq += self.audio_stream_seq_offset;
                        self.delegate.audio_packet(Arc::new(packet));
                    }
                }
            }
            protocol::MessageType::UpdateCursor(msg) => {
                self.delegate.update_cursor(
                    msg.icon(),
                    msg.image.to_vec(),
                    msg.hotspot_x,
                    msg.hotspot_y,
                );
            }
            protocol::MessageType::LockPointer(msg) => {
                self.delegate.lock_pointer(msg.x, msg.y);
            }
            protocol::MessageType::SessionParametersChanged(msg) => {
                let Some(params) = msg.display_params.and_then(|p| p.try_into().ok()) else {
                    error!(?msg, "invalid display params from server");
                    return;
                };

                self.delegate
                    .display_params_changed(params, msg.reattach_required);

                // Mute the attachment_ended callback once.
                self.reattach_required = msg.reattach_required;
            }
            protocol::MessageType::Error(error) => {
                self.delegate.error(error.err_code(), error.error_text);
            }
            v => error!("unexpected message on attachment stream: {}", v),
        }
    }

    pub(crate) fn handle_fin(mut self) {
        if let Some(tx) = self.notify_detached.take() {
            let _ = tx.send(());
        } else if self.reattach_required {
            self.reattach_required = false;
        } else {
            self.delegate.attachment_ended();
        }
    }
}
