// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

#[derive(Debug, Clone, thiserror::Error)]
pub enum ValidationError {
    #[error("{0} must not be null")]
    Required(String),
    #[error("{0}: invalid enum value")]
    InvalidEnum(String),
    #[error("{0}: invalid timestamp")]
    InvalidTimestamp(String),
}

macro_rules! required_field {
    ($msg:ident.$field:ident) => {
        $msg.$field
            .ok_or(crate::validation::ValidationError::Required(
                stringify!($ident).to_string(),
            ))
    };
}

pub(crate) use required_field;
