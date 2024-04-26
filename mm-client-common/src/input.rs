// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use mm_protocol as protocol;

pub use protocol::gamepad::GamepadLayout;
pub use protocol::gamepad_input::{GamepadButton, GamepadButtonState};
pub use protocol::gamepad_motion::GamepadAxis;
pub use protocol::keyboard_input::{Key, KeyState};
pub use protocol::pointer_input::{Button, ButtonState};
pub use protocol::pointer_scroll::ScrollType;
pub use protocol::update_cursor::CursorIcon;

use crate::validation::ValidationError;

#[derive(Debug, Clone, Copy, uniffi::Record)]
pub struct Gamepad {
    pub id: u64,
    pub layout: GamepadLayout,
}

impl From<Gamepad> for protocol::Gamepad {
    fn from(value: Gamepad) -> Self {
        Self {
            id: value.id,
            layout: value.layout.into(),
        }
    }
}

impl TryFrom<protocol::Gamepad> for Gamepad {
    type Error = ValidationError;

    fn try_from(value: protocol::Gamepad) -> Result<Self, Self::Error> {
        let layout = value
            .layout
            .try_into()
            .map_err(|_| ValidationError::InvalidEnum("layout".to_string()))?;

        if value.id == 0 {
            return Err(ValidationError::Required("id".to_string()));
        }

        Ok(Self {
            id: value.id,
            layout,
        })
    }
}
