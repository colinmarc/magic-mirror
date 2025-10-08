// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use std::{collections::HashMap, time};

use anyhow::{anyhow, bail};
use gilrs::{Event, EventType};
use mm_client_common::input::{
    Gamepad, GamepadAxis, GamepadButton, GamepadButtonState, GamepadLayout,
};
use tracing::{debug, error, trace};

#[derive(Debug, Clone)]
pub enum GamepadEvent {
    Available(Gamepad),
    Unavailable(u64),
    Input(u64, GamepadButton, GamepadButtonState),
    Motion(u64, GamepadAxis, f64),
}

#[derive(Debug, Default, Clone, Copy)]
struct RemoteGamepad {
    id: u64,
    dpad: DpadState,
}

// Some gamepads treat the dpad as an axis, but we treat it as a
// bunch of buttons. Therefore, it requires a bit of special handling.
#[derive(Debug, Default, Clone, Copy)]
struct DpadState {
    up: bool,
    down: bool,
    left: bool,
    right: bool,
}

impl RemoteGamepad {
    fn update_dpad<T>(
        &mut self,
        axis: gilrs::Axis,
        value: f32,
        proxy: &winit::event_loop::EventLoopProxy<T>,
    ) -> Result<(), winit::event_loop::EventLoopClosed<T>>
    where
        T: From<GamepadEvent> + Send,
    {
        let set_pressed = |state: &mut bool, button| {
            if !*state {
                proxy.send_event(
                    GamepadEvent::Input(self.id, button, GamepadButtonState::Pressed).into(),
                )?;
            }

            *state = true;
            Ok(())
        };

        let set_released = |state: &mut bool, button| {
            if *state {
                proxy.send_event(
                    GamepadEvent::Input(self.id, button, GamepadButtonState::Released).into(),
                )?;
            }

            *state = false;
            Ok(())
        };

        match axis {
            gilrs::Axis::DPadX if value == 0.0 => {
                set_released(&mut self.dpad.left, GamepadButton::DpadLeft)?;
                set_released(&mut self.dpad.right, GamepadButton::DpadRight)?;
            }
            gilrs::Axis::DPadX if value < 0.0 => {
                set_pressed(&mut self.dpad.left, GamepadButton::DpadLeft)?;
                set_released(&mut self.dpad.right, GamepadButton::DpadRight)?;
            }
            gilrs::Axis::DPadX if value > 0.0 => {
                set_released(&mut self.dpad.left, GamepadButton::DpadLeft)?;
                set_pressed(&mut self.dpad.right, GamepadButton::DpadRight)?;
            }
            gilrs::Axis::DPadY if value == 0.0 => {
                set_released(&mut self.dpad.up, GamepadButton::DpadUp)?;
                set_released(&mut self.dpad.down, GamepadButton::DpadDown)?;
            }
            gilrs::Axis::DPadY if value < 0.0 => {
                set_pressed(&mut self.dpad.up, GamepadButton::DpadUp)?;
                set_released(&mut self.dpad.down, GamepadButton::DpadDown)?;
            }
            gilrs::Axis::DPadY if value > 0.0 => {
                set_released(&mut self.dpad.up, GamepadButton::DpadUp)?;
                set_pressed(&mut self.dpad.down, GamepadButton::DpadDown)?;
            }
            _ => unreachable!(),
        };

        Ok(())
    }
}

/// Spawns a thread to watch for gamepad events. Returns the initial list of
/// available gamepads.
pub fn spawn_gamepad_monitor<T>(
    proxy: winit::event_loop::EventLoopProxy<T>,
) -> anyhow::Result<Vec<Gamepad>>
where
    T: From<GamepadEvent> + Send,
{
    let mut gilrs =
        gilrs::Gilrs::new().map_err(|e| anyhow!("failed to create gilrs context: {e:?}"))?;

    let (initial_tx, initial_rx) = oneshot::channel();

    std::thread::spawn(move || {
        let mut remote_gamepads = HashMap::new();
        let mut initial = Vec::new();

        for (id, pad) in gilrs.gamepads() {
            let protocol_id = gamepad_id(pad.uuid());
            let layout = layout(pad);

            remote_gamepads.insert(
                id,
                RemoteGamepad {
                    id: protocol_id,
                    ..Default::default()
                },
            );

            initial.push(Gamepad {
                id: protocol_id,
                layout,
            });
        }

        if initial_tx.send(initial).is_err() {
            return;
        }

        loop {
            let Some(Event { id, event: ev, .. }) = gilrs.next_event_blocking(None) else {
                continue;
            };

            trace!(?id, ?ev, "gamepad event");

            if let EventType::Disconnected = ev {
                if let Some(pad) = remote_gamepads.remove(&id) {
                    if proxy
                        .send_event(GamepadEvent::Unavailable(pad.id).into())
                        .is_err()
                    {
                        break;
                    }
                }

                continue;
            };

            if let EventType::Connected = ev {
                let Some(pad) = gilrs.connected_gamepad(id) else {
                    error!(?ev, "no gamepad matching event");
                    continue;
                };

                let protocol_id = gamepad_id(pad.uuid());
                remote_gamepads.insert(
                    id,
                    RemoteGamepad {
                        id: protocol_id,
                        ..Default::default()
                    },
                );

                if proxy
                    .send_event(
                        GamepadEvent::Available(Gamepad {
                            id: protocol_id,
                            layout: layout(pad),
                        })
                        .into(),
                    )
                    .is_err()
                {
                    break;
                }

                continue;
            }

            let pad = remote_gamepads.get_mut(&id).unwrap();
            if handle_gilrs_event(&proxy, pad, ev).is_err() {
                break;
            };
        }
    });

    match initial_rx.recv_timeout(time::Duration::from_secs(1)) {
        Ok(initial) => Ok(initial),
        Err(_) => bail!("gamepad monitor thread panicked"),
    }
}

fn handle_gilrs_event<T>(
    proxy: &winit::event_loop::EventLoopProxy<T>,
    pad: &mut RemoteGamepad,
    ev: gilrs::EventType,
) -> Result<(), winit::event_loop::EventLoopClosed<T>>
where
    T: From<GamepadEvent> + Send,
{
    let gev = match ev {
        EventType::ButtonPressed(button, _) => {
            input_event(pad.id, button, GamepadButtonState::Pressed)
        }
        EventType::ButtonReleased(button, _) => {
            input_event(pad.id, button, GamepadButtonState::Released)
        }
        EventType::AxisChanged(axis, mut value, _) => {
            // Some gamepads treat the dpad as an axis. The protocol
            // treats it as a bunch of buttons.
            if matches!(axis, gilrs::Axis::DPadX | gilrs::Axis::DPadY) {
                pad.update_dpad(axis, value, proxy)?;
                return Ok(());
            }

            let Some(axis) = girls_axis_to_proto(axis) else {
                debug!(?ev, "skipping unknown axis event");
                return Ok(());
            };

            // Gilrs treats 1.0 as up.
            if matches!(axis, GamepadAxis::LeftY | GamepadAxis::RightY) {
                value *= -1.0;
            }

            Some(GamepadEvent::Motion(pad.id, axis, value as _))
        }
        EventType::ButtonChanged(button, value, _) => {
            // Not sure why gilrs doesn't consider this an axis.
            match button {
                gilrs::Button::LeftTrigger2 => Some(GamepadEvent::Motion(
                    pad.id,
                    GamepadAxis::LeftTrigger,
                    value.max(0.0) as _,
                )),
                gilrs::Button::RightTrigger2 => Some(GamepadEvent::Motion(
                    pad.id,
                    GamepadAxis::RightTrigger,
                    value.max(0.0) as _,
                )),
                _ => None,
            }
        }
        EventType::Dropped => None,
        // TODO: do we need these?
        EventType::ButtonRepeated(_, _) => None,
        // Handled above.
        EventType::Connected | EventType::Disconnected => unreachable!(),
    };

    if let Some(ev) = gev {
        proxy.send_event(ev.into())?;
    } else {
        debug!(?ev, "ignoring gamepad event")
    }

    Ok(())
}

fn input_event(
    protocol_id: u64,
    button: gilrs::Button,
    state: GamepadButtonState,
) -> Option<GamepadEvent> {
    gilrs_button_to_proto(button).map(|button| GamepadEvent::Input(protocol_id, button, state))
}

fn gamepad_id(uuid: [u8; 16]) -> u64 {
    // Truncating a UUID is squicky, but serves our purposes fine.
    let (_, last_64) = uuid.split_at(8);
    let last_64: [u8; 8] = last_64.try_into().unwrap();
    u64::from_ne_bytes(last_64)
}

fn layout(pad: gilrs::Gamepad) -> GamepadLayout {
    match pad.vendor_id() {
        Some(0x54c) => GamepadLayout::SonyDualshock,
        _ => GamepadLayout::GenericDualStick,
    }
}

fn girls_axis_to_proto(axis: gilrs::Axis) -> Option<GamepadAxis> {
    let axis = match axis {
        gilrs::Axis::LeftStickX => GamepadAxis::LeftX,
        gilrs::Axis::LeftStickY => GamepadAxis::LeftY,
        gilrs::Axis::RightStickX => GamepadAxis::RightX,
        gilrs::Axis::RightStickY => GamepadAxis::RightY,
        gilrs::Axis::LeftZ => GamepadAxis::LeftTrigger,
        gilrs::Axis::RightZ => GamepadAxis::RightTrigger,
        _ => return None,
    };

    Some(axis)
}

fn gilrs_button_to_proto(button: gilrs::Button) -> Option<GamepadButton> {
    let button = match button {
        gilrs::Button::South => GamepadButton::South,
        gilrs::Button::East => GamepadButton::East,
        gilrs::Button::North => GamepadButton::North,
        gilrs::Button::West => GamepadButton::West,
        gilrs::Button::C => GamepadButton::C,
        gilrs::Button::Z => GamepadButton::Z,
        gilrs::Button::LeftTrigger => GamepadButton::ShoulderLeft,
        gilrs::Button::LeftTrigger2 => GamepadButton::TriggerLeft,
        gilrs::Button::RightTrigger => GamepadButton::ShoulderRight,
        gilrs::Button::RightTrigger2 => GamepadButton::TriggerRight,
        gilrs::Button::Select => GamepadButton::Select,
        gilrs::Button::Start => GamepadButton::Start,
        gilrs::Button::Mode => GamepadButton::Logo,
        gilrs::Button::LeftThumb => GamepadButton::JoystickLeft,
        gilrs::Button::RightThumb => GamepadButton::JoystickRight,
        gilrs::Button::DPadUp => GamepadButton::DpadUp,
        gilrs::Button::DPadDown => GamepadButton::DpadDown,
        gilrs::Button::DPadLeft => GamepadButton::DpadLeft,
        gilrs::Button::DPadRight => GamepadButton::DpadRight,
        _ => return None,
    };

    Some(button)
}
