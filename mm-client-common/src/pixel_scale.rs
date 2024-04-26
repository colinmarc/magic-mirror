// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use mm_protocol as protocol;

use crate::validation::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct PixelScale {
    numerator: u32,
    denominator: u32,
}

impl PixelScale {
    pub const ONE: Self = Self {
        numerator: 1,
        denominator: 1,
    };

    pub fn new(numerator: u32, denominator: u32) -> Self {
        Self {
            numerator,
            denominator,
        }
    }

    pub fn is_fractional(&self) -> bool {
        (self.numerator % self.denominator) != 0
    }
}

impl std::fmt::Display for PixelScale {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:.1}", self.numerator as f64 / self.denominator as f64)
    }
}

impl TryFrom<protocol::PixelScale> for PixelScale {
    type Error = ValidationError;

    fn try_from(scale: protocol::PixelScale) -> Result<Self, Self::Error> {
        if scale.denominator == 0 && scale.numerator != 0 {
            Ok(Self::ONE)
        } else if scale.denominator == 0 || scale.numerator == 0 {
            Err(ValidationError::Required("denominator".to_string()))
        } else {
            Ok(Self {
                numerator: scale.numerator,
                denominator: scale.denominator,
            })
        }
    }
}

impl From<PixelScale> for protocol::PixelScale {
    fn from(scale: PixelScale) -> Self {
        Self {
            numerator: scale.numerator,
            denominator: scale.denominator,
        }
    }
}
