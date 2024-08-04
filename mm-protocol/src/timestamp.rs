// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use std::time;

use crate::{ProtocolError, Timestamp};

impl TryFrom<Timestamp> for std::time::SystemTime {
    type Error = ProtocolError;

    fn try_from(value: Timestamp) -> Result<Self, Self::Error> {
        if value.seconds <= 0 || value.nanos < 0 {
            return Err(ProtocolError::InvalidMessage);
        }

        std::time::SystemTime::UNIX_EPOCH
            .checked_add(time::Duration::from_secs(value.seconds as u64))
            .and_then(|ts| ts.checked_add(time::Duration::from_nanos(value.nanos as u64)))
            .ok_or(ProtocolError::InvalidMessage)
    }
}

impl From<time::SystemTime> for Timestamp {
    fn from(value: time::SystemTime) -> Self {
        let d = value.duration_since(time::UNIX_EPOCH).unwrap();

        Self {
            seconds: d.as_secs() as i64,
            nanos: d.subsec_nanos() as i64,
        }
    }
}
