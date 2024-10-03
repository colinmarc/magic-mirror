// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use crossbeam_channel::Sender;

use crate::{
    codec::{AudioCodec, VideoCodec},
    color::VideoProfile,
    compositor::ButtonState,
    pixel_scale::PixelScale,
};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct DisplayParams {
    pub width: u32,
    pub height: u32,
    pub framerate: u32,
    pub ui_scale: PixelScale,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct VideoStreamParams {
    pub width: u32,
    pub height: u32,
    pub codec: VideoCodec,
    pub preset: u32,
    pub profile: VideoProfile,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct AudioStreamParams {
    pub sample_rate: u32,
    pub channels: u32,
    pub codec: AudioCodec,
}

#[derive(Debug)]
pub enum ControlMessage {
    Stop,
    Attach {
        id: u64,
        sender: Sender<CompositorEvent>,
        video_params: VideoStreamParams,
        audio_params: AudioStreamParams,
        ready: oneshot::Sender<()>,
    },
    Detach(u64),
    UpdateDisplayParams(DisplayParams),
    KeyboardInput {
        key_code: u32,
        state: super::KeyState,
        char: Option<char>,
    },
    PointerEntered,
    PointerLeft,
    PointerMotion(f64, f64),
    RelativePointerMotion(f64, f64),
    PointerInput {
        x: f64,
        y: f64,
        button_code: u32,
        state: ButtonState,
    },
    PointerAxis(f64, f64),
    PointerAxisDiscrete(f64, f64),
    // GamepadAvailable(u64),
    // GamepadUnavailable(u64),
    GamepadAxis {
        _id: u64,
        axis_code: u32,
        value: f64,
    },
    GamepadTrigger {
        _id: u64,
        trigger_code: u32,
        value: f64,
    },
    GamepadInput {
        _id: u64,
        button_code: u32,
        state: ButtonState,
    },
}

#[derive(Debug, Clone)]
pub enum CompositorEvent {
    DisplayParamsChanged {
        params: DisplayParams,
        reattach: bool,
    },
    VideoFrame {
        stream_seq: u64,
        seq: u64,
        ts: u64,
        frame: bytes::Bytes,
        /// A lower value means a higher priority.
        _hierarchical_layer: u32,
    },
    AudioFrame {
        stream_seq: u64,
        seq: u64,
        ts: u64,
        frame: bytes::Bytes,
    },
    CursorUpdate {
        image: Option<bytes::Bytes>,
        icon: Option<cursor_icon::CursorIcon>,
        hotspot_x: u32,
        hotspot_y: u32,
    },
    PointerLocked(f64, f64),
    PointerReleased,
    Shutdown,
}
