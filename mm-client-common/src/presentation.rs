// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use std::sync::Arc;

use crate::{display_params::DisplayParams, input, Attachment, AttachmentConfig, Client, Session};

#[derive(Debug, Clone, thiserror::Error, uniffi::Error)]
pub enum PresentationError {
    #[error("session does not exist")]
    SessionDoesNotExist,
    #[error("client error")]
    ClientError(#[from] super::ClientError),
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, uniffi::Enum)]
/// A running operation.
pub enum PresentationOperation {
    UpdatingDisplayParams { live: bool },
}

#[derive(Debug, Clone, uniffi::Enum)]
/// Describes what to put on the screen.
pub enum PresentationStatus {
    /// The presentation is idle.
    None,
    /// The presentation is connected to a session.
    Connected,
    /// The presentation was attached, but the attachment ended cleanly.
    Ended,
    /// The presentation is performing an operation, like resizing the
    /// underlying session.
    Operation(PresentationOperation),
    /// The presentation hit an error and must be reset.
    Errored(PresentationError),
}

/// A multiplexed attachment presentation, forming the core of any client.
/// Connects to one session from one server at a time, but may connect to
/// multiple over its lifetime.
#[derive(uniffi::Object)]
pub struct Presentation {
    delegate: Arc<dyn PresentationDelegate>,
}

#[uniffi::export]
impl Presentation {
    #[uniffi::constructor]
    pub fn new(delegate: Arc<dyn PresentationDelegate>) -> Self {
        Self { delegate }
    }

    /// Resizes an existing session (if the parameters don't match) and then
    /// attaches to it.
    pub fn resize_and_attach(
        &self,
        client: &Client,
        session: Session,
        updated_display_params: DisplayParams,
        config: AttachmentConfig,
    ) {
        todo!()
    }

    /// Launches a new session and then attaches to it.
    pub fn launch_and_attach(
        &self,
        client: &Client,
        application_id: String,
        display_params: DisplayParams,
        permanent_gamepads: Vec<input::Gamepad>,
        config: AttachmentConfig,
    ) {
        todo!()
    }

    /// Attaches or reattaches using the last used configuration.
    pub fn reattach(&self) {
        todo!()
    }

    /// Updates the display parameters of the current session and then
    /// transparently reconnects.
    pub fn update_display_parameters_live(&self) {
        todo!()
    }

    /// Closes the current attachment, if any, and resets the presentation.
    pub fn reset(&self) {}

    /// Returns the current attachment handle, if there is one.
    pub fn attachment(&self) -> Option<Arc<Attachment>> {
        todo!()
    }
}

/// Used by client implementations to observe state changes on the presentation.
#[uniffi::export(with_foreign)]
pub trait PresentationDelegate: Send + Sync + std::fmt::Debug {
    // The current status changed.
    fn status_changed(&self, status: PresentationStatus);

    // The cursor was updated.
    fn update_cursor(
        &self,
        icon: input::CursorIcon,
        image: Option<Vec<u8>>,
        hotspot_x: u32,
        hotspot_y: u32,
    );

    /// The pointer should be locked to the given location.
    fn lock_pointer(&self, x: f64, y: f64);

    /// The pointer should be released.
    fn release_pointer(&self);
}
