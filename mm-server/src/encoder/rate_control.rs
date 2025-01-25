// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use ash::vk;
use tracing::warn;

use crate::session::control::VideoStreamParams;

// Bitrate is defined here in terms of 1080p, and scaled nonlinearly to the
// target resolution. Values are indexed by quality preset. Values 7/8/9 are
// only used if CRF is unsupported by the driver.
const BASELINE_AVG_BITRATE_MBPS: [f32; 10] = [2.5, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0, 12.0, 25.0, 50.0];
const BASELINE_PEAK_BITRATE_MBPS: [f32; 10] =
    [5.0, 8.0, 10.0, 15.0, 20.0, 30.0, 40.0, 60.0, 80.0, 100.0];
const BASELINE_DIMS: f32 = 1920.0 * 1080.0;
const VBV_SIZE: u32 = 2500;

#[derive(Debug, Clone)]
pub enum RateControlMode {
    ConstantQp(CascadingQp),
    Vbr(LayeredVbr),
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

#[derive(Debug, Clone, Copy)]
pub struct CascadingQp {
    target: u32,
    max: u32,
}

impl CascadingQp {
    pub fn layer(&self, layer: u32) -> u32 {
        layer_qp(self.target, layer).min(self.max)
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct VbrSettings {
    pub average_bitrate: u64,
    pub peak_bitrate: u64,
    pub max_qp: u32,
    pub min_qp: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct LayeredVbr {
    pub vbv_size_ms: u32,

    base: VbrSettings,
    num_layers: u32,
}

impl LayeredVbr {
    pub fn layer(&self, layer: u32) -> VbrSettings {
        if self.num_layers <= 1 {
            return self.base;
        }

        let bitrate_denominator = 2_u64.pow(layer + 1);
        let max_qp = layer_qp(self.base.max_qp, layer).clamp(self.base.min_qp, self.base.max_qp);

        VbrSettings {
            average_bitrate: self.base.average_bitrate / bitrate_denominator,
            peak_bitrate: self.base.peak_bitrate / bitrate_denominator,
            max_qp,
            min_qp: self.base.min_qp,
        }
    }
}

pub fn select_rc_mode(
    params: VideoStreamParams,
    caps: &vk::VideoEncodeCapabilitiesKHR,
    min_qp: u32,
    max_qp: u32,
    structure: &super::gop_structure::HierarchicalP,
) -> RateControlMode {
    assert!(params.preset <= 9);

    let min_qp = 17.max(min_qp);
    let target_qp = 40 - (2 * params.preset); // 22 - 40;

    let supports_crf = caps
        .rate_control_modes
        .contains(vk::VideoEncodeRateControlModeFlagsKHR::DISABLED);
    let supports_vbr = caps
        .rate_control_modes
        .contains(vk::VideoEncodeRateControlModeFlagsKHR::VBR);

    if params.preset >= 7 && supports_crf {
        // Presets 7/8/9 use a very low constant QP.
        RateControlMode::ConstantQp(CascadingQp {
            target: target_qp.clamp(min_qp, max_qp),
            max: max_qp,
        })
    } else if supports_vbr {
        // 6 and lower use VBR, starting with a high peak and reducing as the
        // presets get lower.
        let scale = ((params.width * params.height) as f32 / BASELINE_DIMS).sqrt();

        const MBPS: f32 = 1_000_000.0;
        let average_bitrate =
            (BASELINE_AVG_BITRATE_MBPS[params.preset as usize] * MBPS * scale).round() as u64;
        let peak_bitrate =
            (BASELINE_PEAK_BITRATE_MBPS[params.preset as usize] * MBPS * scale).round() as u64;

        RateControlMode::Vbr(LayeredVbr {
            vbv_size_ms: VBV_SIZE,
            base: VbrSettings {
                average_bitrate,
                peak_bitrate,
                min_qp,
                max_qp: target_qp.clamp(min_qp, max_qp),
            },
            num_layers: structure.layers,
        })
    } else if supports_crf {
        // Fall back to CRF with a high QP.
        RateControlMode::ConstantQp(CascadingQp {
            target: target_qp.clamp(min_qp, max_qp),
            max: max_qp,
        })
    } else {
        warn!("no rate control modes available, using driver defaults!");
        RateControlMode::Defaults
    }
}

/// Determines the constant QP for a layer given the target QP.
fn layer_qp(target_qp: u32, layer: u32) -> u32 {
    // Example: for a target QP of 22, the QP for each layer is:
    //   22, 27, 29, 31...
    target_qp + (3 * layer.min(1)) + (layer * 2)
}
