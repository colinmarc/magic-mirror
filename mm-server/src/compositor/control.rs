// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::ffi::OsString;

use crossbeam_channel::Sender;

use crate::{
    codec::{AudioCodec, VideoCodec},
    pixel_scale::PixelScale,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppLaunchConfig {
    pub exe_path: OsString,
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
    pub enable_xwayland: bool,
}

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
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct AudioStreamParams {
    pub sample_rate: u32,
    pub channels: u32,
    pub codec: AudioCodec,
}

#[derive(Debug, Clone)]
pub enum ControlMessage {
    Stop,
    Attach {
        id: u64,
        sender: Sender<CompositorEvent>,
        video_params: VideoStreamParams,
        audio_params: AudioStreamParams,
    },
    Detach(u64),
    UpdateDisplayParams(DisplayParams),
    KeyboardEvent {
        evdev_scancode: u32,
        state: smithay::backend::input::KeyState,
        char: Option<char>,
    },
    PointerEntered,
    PointerLeft,
    PointerMotion(f64, f64),
    PointerInput {
        x: f64,
        y: f64,
        button_code: u32,
        state: smithay::backend::input::ButtonState,
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
        hierarchical_layer: u32,
    },
    AudioFrame {
        stream_seq: u64,
        seq: u64,
        ts: u64,
        frame: bytes::Bytes,
    },
    Shutdown,
}
