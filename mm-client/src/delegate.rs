// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use std::sync::Arc;

use mm_client_common as client;
use tracing::error;

// An implementation of client-common's AttachmentDelegate that converts
// callbacks into winit events.
#[derive(Debug)]
pub struct AttachmentProxy<T: From<AttachmentEvent> + std::fmt::Debug + Send + 'static>(
    winit::event_loop::EventLoopProxy<T>,
);

impl<T: From<AttachmentEvent> + std::fmt::Debug + Send + 'static> AttachmentProxy<T> {
    pub fn new(proxy: winit::event_loop::EventLoopProxy<T>) -> Self {
        Self(proxy)
    }

    fn proxy(&self, ev: AttachmentEvent) {
        let _ = self.0.send_event(ev.into());
    }
}

pub enum AttachmentEvent {
    VideoStreamStart(u64, client::VideoStreamParams),
    VideoPacket(Arc<client::Packet>),
    DroppedVideoPacket(client::DroppedPacket),
    AudioStreamStart(u64, client::AudioStreamParams),
    AudioPacket(Arc<client::Packet>),
    UpdateCursor {
        icon: client::input::CursorIcon,
        image: Option<Vec<u8>>,
        hotspot_x: u32,
        hotspot_y: u32,
    },
    LockPointer(f64, f64),
    ReleasePointer,
    DisplayParamsChanged {
        params: client::display_params::DisplayParams,
        reattach_required: bool,
    },
    AttachmentEnded,
}

impl std::fmt::Debug for AttachmentEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AttachmentEvent::VideoStreamStart(stream_seq, _) => {
                write!(f, "VideoStreamStart({})", stream_seq)
            }
            AttachmentEvent::VideoPacket(packet) => {
                write!(f, "VideoPacket({}, {})", packet.stream_seq(), packet.seq())
            }
            AttachmentEvent::DroppedVideoPacket(dropped) => {
                write!(
                    f,
                    "DroppedVideoPacket({}, {}, optional={})",
                    dropped.stream_seq, dropped.seq, dropped.optional
                )
            }
            AttachmentEvent::AudioStreamStart(stream_seq, _) => {
                write!(f, "AudioStreamStart({})", stream_seq)
            }
            AttachmentEvent::AudioPacket(packet) => {
                write!(f, "AudioPacket({}, {})", packet.stream_seq(), packet.seq())
            }
            AttachmentEvent::UpdateCursor { icon, image, .. } => {
                let len = image.as_ref().map(|img| img.len()).unwrap_or_default();
                write!(f, "UpdateCursor({icon:?} image_len={len})",)
            }
            AttachmentEvent::LockPointer(x, y) => {
                write!(f, "LockPointer({}, {})", x, y)
            }
            AttachmentEvent::ReleasePointer => {
                write!(f, "ReleasePointer()")
            }
            AttachmentEvent::DisplayParamsChanged {
                reattach_required, ..
            } => {
                write!(f, "DisplayParamsChanged(reattach={})", reattach_required)
            }
            AttachmentEvent::AttachmentEnded => {
                write!(f, "AttachmentEnded")
            }
        }
    }
}

impl<T: From<AttachmentEvent> + std::fmt::Debug + Send + 'static> client::AttachmentDelegate
    for AttachmentProxy<T>
{
    fn video_stream_start(&self, stream_seq: u64, params: client::VideoStreamParams) {
        self.proxy(AttachmentEvent::VideoStreamStart(stream_seq, params))
    }

    fn video_packet(&self, packet: Arc<client::Packet>) {
        self.proxy(AttachmentEvent::VideoPacket(packet))
    }

    fn dropped_video_packet(&self, dropped: client::DroppedPacket) {
        self.proxy(AttachmentEvent::DroppedVideoPacket(dropped))
    }

    fn audio_stream_start(&self, stream_seq: u64, params: client::AudioStreamParams) {
        self.proxy(AttachmentEvent::AudioStreamStart(stream_seq, params))
    }

    fn audio_packet(&self, packet: Arc<client::Packet>) {
        self.proxy(AttachmentEvent::AudioPacket(packet))
    }

    fn update_cursor(
        &self,
        icon: client::input::CursorIcon,
        image: Option<Vec<u8>>,
        hotspot_x: u32,
        hotspot_y: u32,
    ) {
        self.proxy(AttachmentEvent::UpdateCursor {
            icon,
            image,
            hotspot_x,
            hotspot_y,
        })
    }

    fn lock_pointer(&self, x: f64, y: f64) {
        self.proxy(AttachmentEvent::LockPointer(x, y))
    }

    fn release_pointer(&self) {
        self.proxy(AttachmentEvent::ReleasePointer)
    }

    fn display_params_changed(
        &self,
        params: client::display_params::DisplayParams,
        reattach_required: bool,
    ) {
        self.proxy(AttachmentEvent::DisplayParamsChanged {
            params,
            reattach_required,
        })
    }

    fn error(&self, err: client::ClientError) {
        error!("error: {err:?}");
        self.proxy(AttachmentEvent::AttachmentEnded)
    }

    fn attachment_ended(&self) {
        self.proxy(AttachmentEvent::AttachmentEnded)
    }
}
