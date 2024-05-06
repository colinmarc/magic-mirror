// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use ash::vk;

// Bitrate is defined here in terms of 1080p, and scaled nonlinearly to the
// target resolution.
const BASELINE_AVG_BITRATE: f32 = 5_000_000.0;
const BASELINE_PEAK_BITRATE: f32 = 12_000_000.0;
const BASELINE_DIMS: f32 = 1920.0 * 1080.0;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum RateControlMode {
    ConstantQp(u32),
    Vbr(VbrSettings),
    Defaults,
}

impl RateControlMode {
    pub fn as_vk_flags(&self) -> vk::VideoEncodeRateControlModeFlagsKHR {
        match self {
            Self::ConstantQp(_) => vk::VideoEncodeRateControlModeFlagsKHR::DISABLED,
            Self::Vbr(_) => vk::VideoEncodeRateControlModeFlagsKHR::VBR,
            Self::Defaults => vk::VideoEncodeRateControlModeFlagsKHR::DEFAULT,
        }
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct VbrSettings {
    pub vbv_size_ms: u32,
    pub average_bitrate: u64,
    pub peak_bitrate: u64,
    pub max_qp: u32,
    pub min_qp: u32,
}

pub fn select_rc_mode(
    width: u32,
    height: u32,
    caps: &vk::VideoEncodeCapabilitiesKHR,
) -> RateControlMode {
    if caps
        .rate_control_modes
        .contains(vk::VideoEncodeRateControlModeFlagsKHR::VBR)
    {
        let scale = ((width * height) as f32 / BASELINE_DIMS).sqrt();
        RateControlMode::Vbr(VbrSettings {
            vbv_size_ms: 5000,
            average_bitrate: (BASELINE_AVG_BITRATE * scale).round() as u64,
            peak_bitrate: (BASELINE_PEAK_BITRATE * scale).round() as u64,
            min_qp: 25,
            max_qp: 35,
        })
    } else if caps
        .rate_control_modes
        .contains(vk::VideoEncodeRateControlModeFlagsKHR::DISABLED)
    {
        RateControlMode::ConstantQp(27)
    } else {
        RateControlMode::Defaults
    }
}
