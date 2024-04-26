// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use mm_protocol as protocol;

pub use protocol::gamepad_available::GamepadLayout;
pub use protocol::gamepad_input::{GamepadButton, GamepadButtonState};
pub use protocol::gamepad_motion::GamepadAxis;
pub use protocol::keyboard_input::{Key, KeyState};
pub use protocol::pointer_input::{Button, ButtonState};
pub use protocol::pointer_scroll::ScrollType;
pub use protocol::update_cursor::CursorIcon;
