// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use ash::vk;
use tracing::warn;

use crate::compositor::VideoStreamParams;

// Bitrate is defined here in terms of 1080p, and scaled nonlinearly to the
// target resolution. Values are indexed by quality preset. Values 7/8/9 are
// only used if CRF is unsupported by the driver.
const BASELINE_AVG_BITRATE_MBPS: [f32; 10] = [3.0, 3.0, 3.0, 4.0, 6.0, 8.0, 10.0, 12.0, 25.0, 50.0];
const BASELINE_PEAK_BITRATE_MBPS: [f32; 10] =
    [3.0, 4.0, 6.0, 8.0, 12.0, 16.0, 20.0, 24.0, 50.0, 100.0];
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
    params: VideoStreamParams,
    caps: &vk::VideoEncodeCapabilitiesKHR,
) -> RateControlMode {
    assert!(params.preset <= 9);

    let mut min_qp = 17;
    let mut target_qp = 40 - (2 * params.preset); // 22 - 40;

    let supports_crf = caps
        .rate_control_modes
        .contains(vk::VideoEncodeRateControlModeFlagsKHR::DISABLED);
    let supports_vbr = caps
        .rate_control_modes
        .contains(vk::VideoEncodeRateControlModeFlagsKHR::VBR);

    if params.preset >= 7 && supports_crf {
        // Presets 7/8/9 use a very low constant QP.
        RateControlMode::ConstantQp(target_qp)
    } else if supports_vbr {
        // 6 and lower use VBR, starting with a high peak and reducing as the
        // presets get lower.
        let scale = ((params.width * params.height) as f32 / BASELINE_DIMS).sqrt();

        const MBPS: f32 = 1_000_000.0;
        let average_bitrate =
            (BASELINE_AVG_BITRATE_MBPS[params.preset as usize] * MBPS * scale).round() as u64;
        let peak_bitrate =
            (BASELINE_PEAK_BITRATE_MBPS[params.preset as usize] * MBPS * scale).round() as u64;

        RateControlMode::Vbr(VbrSettings {
            vbv_size_ms: 2500,
            average_bitrate,
            peak_bitrate,
            min_qp,
            max_qp: target_qp,
        })
    } else if supports_crf {
        // Fall back to CRF with a high QP.
        RateControlMode::ConstantQp(target_qp)
    } else {
        warn!("no rate control modes available, using driver defaults!");
        RateControlMode::Defaults
    }
}
