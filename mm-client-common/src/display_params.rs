// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use mm_protocol as protocol;

use crate::{pixel_scale::PixelScale, validation::*};

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DisplayParams {
    pub width: u32,
    pub height: u32,
    pub framerate: u32,
    pub ui_scale: PixelScale,
}

impl TryFrom<protocol::VirtualDisplayParameters> for DisplayParams {
    type Error = ValidationError;

    fn try_from(msg: protocol::VirtualDisplayParameters) -> Result<Self, Self::Error> {
        let res = required_field!(msg.resolution)?;

        Ok(DisplayParams {
            width: res.width,
            height: res.height,
            framerate: msg.framerate_hz,
            ui_scale: required_field!(msg.ui_scale)?.try_into()?,
        })
    }
}

impl From<DisplayParams> for protocol::VirtualDisplayParameters {
    fn from(value: DisplayParams) -> Self {
        protocol::VirtualDisplayParameters {
            resolution: Some(protocol::Size {
                width: value.width,
                height: value.height,
            }),
            framerate_hz: value.framerate,
            ui_scale: Some(value.ui_scale.into()),
        }
    }
}
