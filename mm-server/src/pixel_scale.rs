// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use anyhow::anyhow;
use mm_protocol as protocol;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelScale(pub u32, pub u32);

impl PixelScale {
    pub fn is_fractional(&self) -> bool {
        (self.0 % self.1) != 0
    }
}

impl std::fmt::Display for PixelScale {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:.1}", self.0 as f64 / self.1 as f64)
    }
}

impl TryFrom<protocol::PixelScale> for PixelScale {
    type Error = anyhow::Error;

    fn try_from(scale: protocol::PixelScale) -> anyhow::Result<Self> {
        if scale.denominator == 0 && scale.numerator != 0 {
            Ok(Self(1, 1))
        } else if scale.denominator == 0 || scale.numerator == 0 {
            Err(anyhow!(
                "invalid pixel scale: {}/{}",
                scale.numerator,
                scale.denominator
            ))
        } else {
            Ok(Self(scale.numerator, scale.denominator))
        }
    }
}

impl From<PixelScale> for protocol::PixelScale {
    fn from(scale: PixelScale) -> Self {
        Self {
            numerator: scale.0,
            denominator: scale.1,
        }
    }
}

impl From<PixelScale> for smithay::output::Scale {
    fn from(scale: PixelScale) -> Self {
        if scale.is_fractional() {
            Self::Fractional(scale.0 as f64 / scale.1 as f64)
        } else {
            Self::Integer((scale.0 / scale.1) as i32)
        }
    }
}

impl Default for PixelScale {
    fn default() -> Self {
        Self(1, 1)
    }
}
