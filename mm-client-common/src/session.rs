// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use std::time;

use mm_protocol as protocol;

use crate::display_params;
use crate::validation::*;

/// A launchable application on the server.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Application {
    pub id: String,
    pub description: String,
    pub folder: Vec<String>,
}

impl TryFrom<protocol::application_list::Application> for Application {
    type Error = ValidationError;

    fn try_from(value: protocol::application_list::Application) -> Result<Self, Self::Error> {
        Ok(Application {
            id: value.id,
            description: value.description,
            folder: value.folder,
        })
    }
}

/// A running session on the server.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Session {
    pub id: u64,
    pub application_id: String,
    pub start: time::SystemTime,
    pub display_params: display_params::DisplayParams,
}

impl TryFrom<protocol::session_list::Session> for Session {
    type Error = ValidationError;

    fn try_from(msg: protocol::session_list::Session) -> Result<Self, Self::Error> {
        let start = match required_field!(msg.session_start)?.try_into() {
            Ok(ts) => Ok(ts),
            Err(_) => Err(ValidationError::InvalidTimestamp(
                "session_start".to_string(),
            )),
        }?;

        Ok(Session {
            id: msg.session_id,
            application_id: msg.application_id,
            start,
            display_params: required_field!(msg.display_params)?.try_into()?,
        })
    }
}
