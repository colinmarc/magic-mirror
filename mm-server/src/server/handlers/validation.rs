// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use mm_protocol as protocol;
use tracing::debug;

use crate::{
    codec::{AudioCodec, VideoCodec},
    color::VideoProfile,
    compositor::{AudioStreamParams, DisplayParams, GamepadLayout, VideoStreamParams},
    pixel_scale::PixelScale,
    waking_sender::WakingSender,
};

pub enum ValidationError {
    Invalid(String),
    NotSupported(String),
}

pub type Result<T> = std::result::Result<T, ValidationError>;

pub fn validate_display_params(
    params: Option<protocol::VirtualDisplayParameters>,
) -> Result<DisplayParams> {
    if let Some(params) = params {
        let (width, height) = validate_resolution(params.resolution)?;
        let framerate = validate_framerate(params.framerate_hz)?;
        let ui_scale = validate_ui_scale(params.ui_scale)?;

        Ok(DisplayParams {
            width,
            height,
            framerate,
            ui_scale,
        })
    } else {
        Err(ValidationError::Invalid(
            "display parameters missing".into(),
        ))
    }
}

pub fn validate_attachment(
    params: protocol::Attach,
) -> Result<(VideoStreamParams, AudioStreamParams)> {
    let (width, height) = validate_resolution(params.streaming_resolution)?;
    let video_codec = validate_video_codec(params.video_codec)?;
    let preset = validate_preset(params.quality_preset)?;
    let video_profile = validate_profile(params.video_profile)?;

    let sample_rate = validate_sample_rate(params.sample_rate_hz)?;
    let channels = validate_channels(params.channels)?;
    let audio_codec = validate_audio_codec(params.audio_codec)?;

    Ok((
        VideoStreamParams {
            width,
            height,
            codec: video_codec,
            preset,
            profile: video_profile,
        },
        AudioStreamParams {
            sample_rate,
            channels,
            codec: audio_codec,
        },
    ))
}

pub fn validate_resolution(resolution: Option<protocol::Size>) -> Result<(u32, u32)> {
    match resolution {
        Some(ref size) => {
            let (width, height) = (size.width, size.height);
            if width != 0 && height != 0 && width % 2 == 0 && height % 2 == 0 {
                Ok((width, height))
            } else {
                debug!("rejecting invalid resolution: {}x{}", width, height);
                Err(ValidationError::Invalid(
                    "resolution must be non-zero and even".into(),
                ))
            }
        }
        None => Err(ValidationError::Invalid("resolution missing".into())),
    }
}

pub fn validate_ui_scale(ui_scale: Option<protocol::PixelScale>) -> Result<PixelScale> {
    match ui_scale {
        Some(scale) => match PixelScale::try_from(scale) {
            Ok(s) if !s.is_fractional() => Ok(s),
            Ok(_) => Err(ValidationError::NotSupported(
                "fractional UI scales are not supported".into(),
            )),
            Err(_) => Err(ValidationError::Invalid("invalid UI scale".into())),
        },
        None => Ok(PixelScale::ONE),
    }
}

fn validate_profile(profile: i32) -> Result<VideoProfile> {
    let p: protocol::VideoProfile = match profile.try_into() {
        Err(_) => return Err(ValidationError::Invalid("invalid video profile".into())),
        Ok(protocol::VideoProfile::Unknown) => return Ok(VideoProfile::Hd),
        Ok(p) => p,
    };

    match p.try_into() {
        Ok(p) => Ok(p),
        _ => Err(ValidationError::NotSupported(
            "unsupported video profile".into(),
        )),
    }
}

pub fn validate_video_codec(codec: i32) -> Result<VideoCodec> {
    let codec: protocol::VideoCodec = match codec.try_into() {
        Err(_) => return Err(ValidationError::Invalid("invalid video codec".into())),
        Ok(protocol::VideoCodec::Unknown) => return Ok(VideoCodec::H265),
        Ok(v) => v,
    };

    match codec.try_into() {
        Ok(c) => Ok(c),
        Err(_) => Err(ValidationError::Invalid("invalid video codec".into())),
    }
}

pub fn validate_preset(preset: u32) -> Result<u32> {
    match preset {
        0 => Ok(6), // Default to 6
        v if v <= 10 => Ok(v - 1),
        _ => Err(ValidationError::Invalid("invalid preset".into())),
    }
}
pub fn validate_framerate(framerate: u32) -> Result<u32> {
    match framerate {
        60 | 30 => Ok(framerate),
        _ => Err(ValidationError::NotSupported(
            "unsupported framerate".into(),
        )),
    }
}

pub fn validate_audio_codec(codec: i32) -> Result<AudioCodec> {
    let codec: protocol::AudioCodec = match codec.try_into() {
        Err(_) => return Err(ValidationError::Invalid("invalid audio codec".into())),
        Ok(protocol::AudioCodec::Unknown) => return Ok(AudioCodec::Opus),
        Ok(v) => v,
    };

    match codec.try_into() {
        Ok(c) => Ok(c),
        Err(_) => Err(ValidationError::Invalid("invalid audio codec".into())),
    }
}

pub fn validate_sample_rate(sample_rate: u32) -> Result<u32> {
    if sample_rate == 0 {
        Ok(48000)
    } else if !(16000..=48000).contains(&sample_rate) {
        Err(ValidationError::Invalid("invalid sample rate".into()))
    } else {
        Ok(sample_rate)
    }
}

pub fn validate_channels(channels: Option<protocol::AudioChannels>) -> Result<u32> {
    match channels {
        Some(map) => {
            let channels = map.channels.len() as u32;
            for ch in map.channels {
                if let Err(e) = protocol::audio_channels::Channel::try_from(ch) {
                    return Err(ValidationError::Invalid(format!("invalid channel: {}", e)));
                }
            }

            if channels == 2 {
                Ok(channels)
            } else {
                Err(ValidationError::NotSupported(
                    "unsupported number of channels".into(),
                ))
            }
        }
        None => Ok(2), // Default to stereo.
    }
}

pub fn validate_gamepad(gamepad: Option<protocol::Gamepad>) -> Result<(u64, GamepadLayout)> {
    let Some(gamepad) = gamepad else {
        return Err(ValidationError::Invalid("gamepad is required".into()));
    };

    let id = validate_gamepad_id(gamepad.id)?;
    let layout = validate_gamepad_layout(gamepad.layout)?;
    Ok((id, layout))
}

pub fn validate_gamepad_id(id: u64) -> Result<u64> {
    if id == 0 {
        Err(ValidationError::Invalid("id must be non-zero".into()))
    } else {
        Ok(id)
    }
}

pub fn validate_gamepad_layout(layout: i32) -> Result<GamepadLayout> {
    match layout.try_into() {
        Err(_) | Ok(protocol::gamepad::GamepadLayout::Unknown) => {
            Err(ValidationError::Invalid("invalid gamepad layout".into()))
        }
        Ok(_) => Ok(GamepadLayout::GenericDualStick), // TODO
    }
}

pub fn send_err(
    response: &WakingSender<protocol::MessageType>,
    code: protocol::error::ErrorCode,
    text: Option<String>,
) {
    if let Some(text) = text.as_ref() {
        debug!("client error: {:?}: {}", code, text);
    } else {
        debug!("client error: {:?}", code);
    }

    let err = protocol::Error {
        err_code: code.into(),
        error_text: text.unwrap_or_default(),
    };

    response.send(err.into()).ok();
}

pub fn send_validation_error(
    response: &WakingSender<protocol::MessageType>,
    err: ValidationError,
    is_attachment: bool,
) {
    match err {
        ValidationError::Invalid(text) => send_err(
            response,
            protocol::error::ErrorCode::ErrorProtocol,
            Some(text),
        ),
        ValidationError::NotSupported(text) if !is_attachment => send_err(
            response,
            protocol::error::ErrorCode::ErrorSessionParamsNotSupported,
            Some(text),
        ),
        ValidationError::NotSupported(text) => send_err(
            response,
            protocol::error::ErrorCode::ErrorAttachmentParamsNotSupported,
            Some(text),
        ),
    }
}
