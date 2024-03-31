// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

#![allow(dead_code)]

use mm_protocol as protocol;

/// A combination of color primaries, white point, and transfer function. We
/// generally ignore white point, since we deal only with colorspaces using the
/// D65 white point.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ColorSpace {
    /// Uses BT.709 primaries and the sRGB transfer function.
    Srgb,
    /// Uses BT.709 primaries and a linear transfer function. Usually encoded as
    /// a float with negative values and values above 1.0 used to represent the
    /// extended space.
    LinearExtSrgb,
    /// Uses BT.2020 primaries and the ST2084 (PQ) transfer function.
    Hdr10,
}

impl ColorSpace {
    pub fn from_primaries_and_tf(
        primaries: Primaries,
        transfer_function: TransferFunction,
    ) -> Option<Self> {
        match (primaries, transfer_function) {
            (Primaries::Srgb, TransferFunction::Srgb) => Some(ColorSpace::Srgb),
            (Primaries::Srgb, TransferFunction::Linear) => Some(ColorSpace::LinearExtSrgb),
            (Primaries::Bt2020, TransferFunction::Pq) => Some(ColorSpace::Hdr10),
            _ => None,
        }
    }
}

// A configuration for a compressed video bitstream.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum VideoProfile {
    // Uses a bit depth of 8, BT.709 primaries and transfer function.
    Hd,
    // Uses a bit depth of 10, BT.2020 primaries and the ST2084 (PQ) transfer function.
    Hdr10,
}

impl TryFrom<protocol::VideoProfile> for VideoProfile {
    type Error = String;

    fn try_from(profile: protocol::VideoProfile) -> Result<Self, Self::Error> {
        match profile {
            protocol::VideoProfile::Hd => Ok(VideoProfile::Hd),
            protocol::VideoProfile::Hdr10 => Ok(VideoProfile::Hdr10),
            _ => Err("invalid video profile".into()),
        }
    }
}

impl From<VideoProfile> for protocol::VideoProfile {
    fn from(profile: VideoProfile) -> Self {
        match profile {
            VideoProfile::Hd => protocol::VideoProfile::Hd,
            VideoProfile::Hdr10 => protocol::VideoProfile::Hdr10,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum TransferFunction {
    Linear,
    Srgb,
    Pq,
}

#[derive(Debug, Clone, Copy)]
pub enum Primaries {
    Srgb,
    Bt2020,
}
