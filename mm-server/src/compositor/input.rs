// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{
    io::Cursor,
    sync::{Arc, Mutex},
    time,
};

use fuser as fuse;
use murmur3::murmur3_32 as murmur3;
use southpaw::{
    sys::{EV_ABS, EV_KEY},
    AbsAxis, AbsInfo, InputEvent, KeyCode, Scancode,
};
use tracing::{debug, error};

use crate::compositor::Container;

mod udevfs;
use udevfs::*;

/// A simulated gamepad layout.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum GamepadLayout {
    #[default]
    GenericDualStick,
}

/// Manages input devices (mostly gamepads) n a container using a variety of
/// well-intentioned but horrible hacks.
pub struct InputDeviceManager {
    southpaw: southpaw::DeviceTree,
    state: Arc<Mutex<InputManagerState>>,
}

struct DeviceState {
    id: u64,
    short_id: u32, // Used by udevfs.
    counter: u16,
    plugged: time::SystemTime,
    devname: String,
}

#[derive(Default)]
struct InputManagerState {
    counter: u16,
    devices: Vec<DeviceState>,
}

impl InputManagerState {
    fn find_device(&self, short_id: u32) -> Option<&DeviceState> {
        self.devices.iter().find(|dev| dev.short_id == short_id)
    }
}

/// A handle for a plugged gamepad.
pub struct GamepadHandle {
    device: southpaw::Device,
    ev_buffer: Vec<southpaw::InputEvent>,
    pub permanent: bool,
}
impl GamepadHandle {
    pub(crate) fn axis(&mut self, axis_code: u32, value: f64) {
        let value = value.clamp(-1.0, 1.0) * 128.0 + 128.0;
        self.ev_buffer.push(InputEvent::new(
            EV_ABS,
            axis_code as u16,
            value.floor() as i32,
        ));
    }

    pub(crate) fn trigger(&mut self, trigger_code: u32, value: f64) {
        // todo!()
    }

    pub(crate) fn input(&mut self, button_code: u32, state: super::ButtonState) {
        // TODO: the DualSense sends D-pad buttons as ABS_HAT0{X,Y}.

        let value = match state {
            super::ButtonState::Pressed => 1,
            super::ButtonState::Released => 0,
        };

        self.ev_buffer
            .push(InputEvent::new(EV_KEY, button_code as u16, value));
    }

    pub(crate) fn frame(&mut self) {
        if let Err(err) = self.device.publish_packet(&self.ev_buffer) {
            error!(?err, "failed to publish event packet to device");
        }

        self.ev_buffer.clear();
    }
}

impl InputDeviceManager {
    pub fn new(container: &mut Container) -> anyhow::Result<Self> {
        let state = Arc::new(Mutex::new(InputManagerState::default()));

        let udevfs_path = container.intern_run_path().join(".udevfs");
        let southpaw_path = container.intern_run_path().join(".southpaw");

        let udevfs = UdevFs::new(state.clone());
        let udevfs_path_clone = udevfs_path.clone();

        let southpaw = southpaw::DeviceTree::new();
        let southpaw_clone = southpaw.clone();
        let southpaw_path_clone = southpaw_path.clone();

        container.setup_hook(move |c| {
            let mode = 0o755 | rustix::fs::FileType::Directory.as_raw_mode();

            let device_fd = c.fuse_mount(udevfs_path_clone, "udevfs", mode)?;
            let mut session = fuse::Session::from_fd(device_fd, udevfs, fuse::SessionACL::Owner);
            std::thread::spawn(move || session.run());

            let device_fd = c.fuse_mount(southpaw_path_clone, "southpaw", mode)?;
            southpaw_clone.wrap_fd(device_fd);

            Ok(())
        });

        container.internal_bind_mount(
            udevfs_path.join("sys/devices/virtual/input"),
            "/sys/devices/virtual/input",
        );
        container.internal_bind_mount(udevfs_path.join("sys/class/input"), "/sys/class/input");
        container.internal_bind_mount(udevfs_path.join("run/udev"), "/run/udev");
        container.internal_bind_mount(southpaw_path, "/dev/input");

        // Without this, udev refuses to accept our FUSE filesystem.
        container.set_env("SYSTEMD_DEVICE_VERIFY_SYSFS", "false");

        Ok(Self { state, southpaw })
    }

    pub fn plug_gamepad(
        &mut self,
        id: u64,
        _layout: GamepadLayout,
        permanent: bool,
    ) -> anyhow::Result<GamepadHandle> {
        debug!(id, ?_layout, "gamepad plugged");

        let mut guard = self.state.lock().unwrap();

        guard.counter += 1;
        let counter = guard.counter;
        let devname = format!("event{counter}");

        let xy_absinfo = AbsInfo {
            value: 128,
            minimum: 0,
            maximum: 255,
            ..Default::default()
        };

        let trigger_absinfo = AbsInfo {
            value: 0,
            minimum: 0,
            maximum: 255,
            ..Default::default()
        };

        let dpad_absinfo = AbsInfo {
            value: 0,
            minimum: -1,
            maximum: 1,
            ..Default::default()
        };

        let device = southpaw::Device::builder()
            .name("Magic Mirror Emulated DualSense Controller")
            .id(southpaw::BusType::Usb, 0x54c, 0xce5, 0x8111)
            .supported_key_codes([
                KeyCode::BtnSouth,
                KeyCode::BtnNorth,
                KeyCode::BtnEast,
                KeyCode::BtnWest,
                KeyCode::BtnTl,
                KeyCode::BtnTr,
                KeyCode::BtnTl2,
                KeyCode::BtnTr2,
                KeyCode::BtnSelect,
                KeyCode::BtnStart,
                KeyCode::BtnMode,
                KeyCode::BtnThumbl,
                KeyCode::BtnThumbr,
                // Scancode::AbsoluteAxis(AbsAxis::X),
                // Scancode::AbsoluteAxis(AbsAxis::Y),
                // Scancode::AbsoluteAxis(AbsAxis::RX),
                // Scancode::AbsoluteAxis(AbsAxis::RY),
                // Scancode::AbsoluteAxis(AbsAxis::Z),
                // Scancode::AbsoluteAxis(AbsAxis::RZ),
                // Scancode::AbsoluteAxis(AbsAxis::HAT0X),
                // Scancode::AbsoluteAxis(AbsAxis::HAT0Y),
            ])
            .supported_absolute_axis(AbsAxis::X, xy_absinfo)
            .supported_absolute_axis(AbsAxis::Y, xy_absinfo)
            .supported_absolute_axis(AbsAxis::RX, xy_absinfo)
            .supported_absolute_axis(AbsAxis::RY, xy_absinfo)
            .add_to_tree(&mut self.southpaw, &devname)?;

        let short_id = murmur3(&mut Cursor::new(id.to_ne_bytes()), 0).unwrap();
        guard.devices.push(DeviceState {
            id,
            short_id,
            counter,
            devname,
            plugged: time::SystemTime::now(),
        });

        Ok(GamepadHandle {
            device,
            ev_buffer: Vec::new(),
            permanent,
        })
    }
}

#[cfg(test)]
mod test {
    use std::{collections::HashMap, fs::File, io::Read as _};

    use rustix::pipe::{pipe_with, PipeFlags};

    use crate::{
        compositor::Container,
        config::{AppConfig, HomeIsolationMode},
    };

    use super::{GamepadLayout, InputDeviceManager};

    fn run_in_container_with_gamepads<T>(cmd: impl AsRef<[T]>) -> anyhow::Result<String>
    where
        T: AsRef<str>,
    {
        let command = cmd
            .as_ref()
            .iter()
            .map(|s| s.as_ref().to_owned().into())
            .collect();

        let app_config = AppConfig {
            description: None,
            command,
            env: HashMap::new(),
            xwayland: false,
            force_1x_scale: false,
            home_isolation_mode: HomeIsolationMode::Unisolated,
        };

        let mut container = Container::new(app_config)?;
        let (pipe_rx, pipe_tx) = pipe_with(PipeFlags::CLOEXEC)?;
        container.set_stdout(pipe_tx)?;

        container.set_env("SYSTEMD_LOG_LEVEL", "debug");
        let mut input_manager = InputDeviceManager::new(&mut container)?;

        let mut child = container.spawn()?;

        let _ = input_manager.plug_gamepad(1234, GamepadLayout::GenericDualStick, false)?;
        let _ = input_manager.plug_gamepad(5678, GamepadLayout::GenericDualStick, false)?;
        let _ = child.wait();

        let mut buf = String::new();
        File::from(pipe_rx).read_to_string(&mut buf)?;

        Ok(buf)
    }

    #[test_log::test]
    fn list_devices_subsystem() -> anyhow::Result<()> {
        let output = run_in_container_with_gamepads([
            "udevadm",
            "--debug",
            "trigger",
            "--dry-run",
            "--verbose",
            "--subsystem-match",
            "input",
        ])?;

        pretty_assertions::assert_eq!(
            output,
            "/sys/devices/virtual/input/event1\n/sys/devices/virtual/input/event2\n"
        );
        Ok(())
    }

    #[test_log::test]
    fn gilrs_gamepad_info() -> anyhow::Result<()> {
        let output = run_in_container_with_gamepads([
            "/home/colinmarc/src/gilrs/target/debug/examples/gamepad_info",
        ])?;

        pretty_assertions::assert_eq!(
            output,
            "/sys/devices/virtual/input/event1\n/sys/devices/virtual/input/event2\n"
        );

        Ok(())
    }
}
