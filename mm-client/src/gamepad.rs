use std::collections::HashMap;

use anyhow::anyhow;
use gilrs::{Event, EventType, Gamepad};

use mm_protocol::{
    self as protocol,
    gamepad_input::{Button, ButtonState},
    gamepad_motion::Axis,
};
use protocol::gamepad_available::GamepadLayout;
use tracing::{debug, error};

#[derive(Debug, Clone)]
pub enum GamepadEvent {
    Available(u64, GamepadLayout),
    Unavailable(u64),
    Input(u64, Button, ButtonState),
    Motion(u64, Axis, f64),
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
                    GamepadEvent::Input(self.id, button, ButtonState::Pressed).into(),
                )?;
            }

            *state = true;
            Ok(())
        };

        let set_released = |state: &mut bool, button| {
            if *state {
                proxy.send_event(
                    GamepadEvent::Input(self.id, button, ButtonState::Released).into(),
                )?;
            }

            *state = false;
            Ok(())
        };

        match axis {
            gilrs::Axis::DPadX if value == 0.0 => {
                set_released(&mut self.dpad.left, Button::DpadLeft)?;
                set_released(&mut self.dpad.right, Button::DpadRight)?;
            }
            gilrs::Axis::DPadX if value < 0.0 => {
                set_pressed(&mut self.dpad.left, Button::DpadLeft)?;
                set_released(&mut self.dpad.right, Button::DpadRight)?;
            }
            gilrs::Axis::DPadX if value > 0.0 => {
                set_released(&mut self.dpad.left, Button::DpadLeft)?;
                set_pressed(&mut self.dpad.right, Button::DpadRight)?;
            }
            gilrs::Axis::DPadY if value == 0.0 => {
                set_released(&mut self.dpad.up, Button::DpadUp)?;
                set_released(&mut self.dpad.down, Button::DpadDown)?;
            }
            gilrs::Axis::DPadY if value < 0.0 => {
                set_pressed(&mut self.dpad.up, Button::DpadUp)?;
                set_released(&mut self.dpad.down, Button::DpadDown)?;
            }
            gilrs::Axis::DPadY if value > 0.0 => {
                set_released(&mut self.dpad.up, Button::DpadUp)?;
                set_pressed(&mut self.dpad.down, Button::DpadDown)?;
            }
            _ => unreachable!(),
        };

        Ok(())
    }
}

pub fn spawn_gamepad_monitor<T>(proxy: winit::event_loop::EventLoopProxy<T>) -> anyhow::Result<()>
where
    T: From<GamepadEvent> + Send,
{
    let mut gilrs =
        gilrs::Gilrs::new().map_err(|e| anyhow!("failed to create gilrs context: {e:?}"))?;

    std::thread::spawn(move || {
        let mut remote_gamepads = HashMap::new();

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

            let _ = proxy.send_event(GamepadEvent::Available(protocol_id, layout).into());
        }

        loop {
            let Some(Event { id, event: ev, .. }) = gilrs.next_event_blocking(None) else {
                continue;
            };

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
                    .send_event(GamepadEvent::Available(protocol_id, layout(pad)).into())
                    .is_err()
                {
                    break;
                }
            }

            let pad = remote_gamepads.get_mut(&id).unwrap();
            if handle_gilrs_event(&proxy, pad, ev).is_err() {
                break;
            };
        }
    });

    Ok(())
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
        EventType::ButtonPressed(button, _) => input_event(pad.id, button, ButtonState::Pressed),
        EventType::ButtonReleased(button, _) => input_event(pad.id, button, ButtonState::Released),
        EventType::AxisChanged(axis, value, _) => {
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

            Some(GamepadEvent::Motion(pad.id, axis, value as _))
        }
        EventType::ButtonChanged(button, value, _) => {
            // Not sure why gilrs doesn't consider this an axis.
            match button {
                gilrs::Button::LeftTrigger2 => Some(GamepadEvent::Motion(
                    pad.id,
                    Axis::LeftTrigger,
                    value.max(0.0) as _,
                )),
                gilrs::Button::RightTrigger2 => Some(GamepadEvent::Motion(
                    pad.id,
                    Axis::LeftTrigger,
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
    state: ButtonState,
) -> Option<GamepadEvent> {
    gilrs_button_to_proto(button).map(|button| GamepadEvent::Input(protocol_id, button, state))
}

fn gamepad_id(uuid: [u8; 16]) -> u64 {
    // Truncating a UUID is squicky, but serves our purposes fine.
    let (_, last_64) = uuid.split_at(8);
    let last_64: [u8; 8] = last_64.try_into().unwrap();
    u64::from_ne_bytes(last_64)
}

fn layout(pad: Gamepad) -> GamepadLayout {
    match pad.vendor_id() {
        Some(0x54c) => GamepadLayout::ControllerLayoutSonyDualshock,
        _ => GamepadLayout::ControllerLayoutGenericDualStick,
    }
}

fn girls_axis_to_proto(axis: gilrs::Axis) -> Option<Axis> {
    let axis = match axis {
        gilrs::Axis::LeftStickX => Axis::LeftX,
        gilrs::Axis::LeftStickY => Axis::LeftY,
        gilrs::Axis::RightStickX => Axis::RightX,
        gilrs::Axis::RightStickY => Axis::RightY,
        gilrs::Axis::LeftZ => Axis::RightTrigger,
        gilrs::Axis::RightZ => Axis::RightTrigger,
        _ => return None,
    };

    Some(axis)
}

fn gilrs_button_to_proto(button: gilrs::Button) -> Option<Button> {
    let button = match button {
        gilrs::Button::South => Button::South,
        gilrs::Button::East => Button::East,
        gilrs::Button::North => Button::North,
        gilrs::Button::West => Button::West,
        gilrs::Button::C => Button::C,
        gilrs::Button::Z => Button::Z,
        gilrs::Button::LeftTrigger => Button::ShoulderLeft,
        gilrs::Button::LeftTrigger2 => Button::TriggerLeft,
        gilrs::Button::RightTrigger => Button::ShoulderRight,
        gilrs::Button::RightTrigger2 => Button::TriggerRight,
        gilrs::Button::Select => Button::Select,
        gilrs::Button::Start => Button::Start,
        gilrs::Button::Mode => Button::Logo,
        gilrs::Button::LeftThumb => Button::JoystickLeft,
        gilrs::Button::RightThumb => Button::JoystickRight,
        gilrs::Button::DPadUp => Button::DpadUp,
        gilrs::Button::DPadDown => Button::DpadDown,
        gilrs::Button::DPadLeft => Button::DpadLeft,
        gilrs::Button::DPadRight => Button::DpadRight,
        _ => return None,
    };

    Some(button)
}
