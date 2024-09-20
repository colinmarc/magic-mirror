// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use anyhow::Context as _;
use fuser as fuse;
use rand::distributions::{Alphanumeric, DistString as _};

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
    udevfs_mount: PathBuf,
    udevfs: fuser::BackgroundSession,

    state: Arc<Mutex<InputManagerState>>,
}

struct InputManagerState {}

/// A handle for a plugged gamepad.
pub struct DeviceHandle {}

impl InputDeviceManager {
    pub fn new(container: &mut Container) -> anyhow::Result<Self> {
        // let udevfs_mount = std::env::temp_dir().join(format!(
        //     "udevfs.{}",
        //     Alphanumeric.sample_string(&mut rand::thread_rng(), 16),
        // ));

        let udevfs_mount = container.extern_run_path().join(".udevfs");
        let state = Arc::new(Mutex::new(InputManagerState {}));

        std::fs::create_dir_all(&udevfs_mount)?;

        let udevfs = fuse::spawn_mount2(
            UdevFs::new(state.clone()),
            &udevfs_mount,
            &[fuse::MountOption::FSName("udevfs".to_string())],
        )
        .context("mounting udevfs")?;

        let internal_udevfs_path = container.intern_run_path().join(".udevfs");

        container.internal_bind_mount(
            internal_udevfs_path.join("sys/devices/virtual/input"),
            "/sys/devices/virtual/input",
        );

        container.internal_bind_mount(
            internal_udevfs_path.join("sys/class/input"),
            "/sys/class/input",
        );

        Ok(Self {
            udevfs_mount,
            udevfs,
            state,
        })
    }

    pub fn plug_gamepad(&mut self, id: u64, layout: GamepadLayout) -> anyhow::Result<DeviceHandle> {
        todo!()
    }
}

// impl Drop for InputDeviceManager {
//     fn drop(&mut self) {
//         let _ = std::fs::remove_dir_all(&self.udevfs_mount);
//     }
// }

impl DeviceHandle {
    pub fn unplug(self) {
        todo!()
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

    use super::InputDeviceManager;

    fn run_in_container<T>(cmd: impl AsRef<[T]>) -> anyhow::Result<String>
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

        let input_manager = InputDeviceManager::new(&mut container)?;

        let mut child = container.spawn()?;
        child.wait()?;

        let mut buf = String::new();
        File::from(pipe_rx).read_to_string(&mut buf)?;

        Ok(buf)
    }

    #[test_log::test]
    fn list_devices() -> anyhow::Result<()> {
        let output = run_in_container([
            "udevadm",
            "trigger",
            "--dry-run",
            "--verbose",
            "--subsystem-match",
            "input",
        ])?;

        pretty_assertions::assert_eq!(output, "foo bar baz");
        Ok(())
    }
}
