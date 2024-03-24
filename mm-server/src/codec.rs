// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use anyhow::anyhow;
use mm_protocol as protocol;

use crate::vulkan::VkContext;

#[cfg(feature = "ffmpeg_encode")]
use ffmpeg_next as ffmpeg;

/// A codec used for an attachment video stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    H265,
    Av1,
}

/// A codec used for an attachment audio stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodec {
    Opus,
}

impl TryFrom<protocol::VideoCodec> for VideoCodec {
    type Error = anyhow::Error;

    fn try_from(codec: protocol::VideoCodec) -> anyhow::Result<Self> {
        match codec {
            protocol::VideoCodec::Unknown => Err(anyhow!("codec unset")),
            protocol::VideoCodec::H264 => Ok(Self::H264),
            protocol::VideoCodec::H265 => Ok(Self::H265),
            protocol::VideoCodec::Av1 => Ok(Self::Av1),
        }
    }
}

impl From<VideoCodec> for protocol::VideoCodec {
    fn from(codec: VideoCodec) -> Self {
        match codec {
            VideoCodec::H264 => protocol::VideoCodec::H264,
            VideoCodec::H265 => protocol::VideoCodec::H265,
            VideoCodec::Av1 => protocol::VideoCodec::Av1,
        }
    }
}

impl TryFrom<protocol::AudioCodec> for AudioCodec {
    type Error = anyhow::Error;

    fn try_from(codec: protocol::AudioCodec) -> anyhow::Result<Self> {
        match codec {
            protocol::AudioCodec::Unknown => Err(anyhow!("codec unset")),
            protocol::AudioCodec::Opus => Ok(Self::Opus),
        }
    }
}

impl From<AudioCodec> for protocol::AudioCodec {
    fn from(codec: AudioCodec) -> Self {
        match codec {
            AudioCodec::Opus => protocol::AudioCodec::Opus,
        }
    }
}

#[cfg(feature = "ffmpeg_encode")]
impl From<VideoCodec> for ffmpeg::codec::Id {
    fn from(codec: VideoCodec) -> Self {
        match codec {
            VideoCodec::H264 => ffmpeg::codec::Id::H264,
            VideoCodec::H265 => ffmpeg::codec::Id::H265,
            VideoCodec::Av1 => ffmpeg::codec::Id::AV1,
        }
    }
}

pub fn probe_codec(_vk: Arc<VkContext>, codec: VideoCodec) -> bool {
    #[cfg(feature = "vulkan_encode")]
    match codec {
        VideoCodec::H264 if _vk.device_info.supports_h264 => return true,
        VideoCodec::H265 if _vk.device_info.supports_h265 => return true,
        _ => (),
    }

    #[cfg(feature = "ffmpeg_encode")]
    if ffmpeg::encoder::find(codec.into()).is_some() {
        return true;
    }

    if cfg!(feature = "svt_encode") && matches!(codec, VideoCodec::H265 | VideoCodec::Av1) {
        return true;
    }

    false
}
