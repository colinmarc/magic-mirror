// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::ffi::{OsStr, OsString};

use anyhow::{anyhow, bail};
use pathsearch::find_executable_in_path;
use tracing::{info, trace};
use unshare::{Child, ExitStatus};

use crate::config::AppConfig;

pub fn spawn_child(
    app_config: AppConfig,
    xdg_runtime_dir: &OsStr,
    socket_name: &OsStr,
    x11_display: Option<&OsStr>,
    pipe: mio::unix::pipe::Sender,
) -> anyhow::Result<unshare::Child> {
    // This gets dropped when we return, closing the write side (in this process)
    let stdout = unshare::Stdio::dup_file(&pipe)?;
    let stderr = unshare::Stdio::dup_file(&pipe)?;

    let mut args = app_config.command.clone();
    let exe = args.remove(0);
    let exe_path =
        find_executable_in_path(&exe).ok_or(anyhow!("command {:?} not in PATH", &exe))?;

    let mut envs: Vec<(OsString, OsString)> = app_config.env.clone().into_iter().collect();
    envs.push(("WAYLAND_DISPLAY".into(), socket_name.into()));

    if let Some(x11_display) = x11_display {
        envs.push(("DISPLAY".into(), x11_display.to_owned()));
    }

    envs.push(("XDG_RUNTIME_DIR".into(), xdg_runtime_dir.into()));

    // Shadow pipewire.
    envs.push(("PIPEWIRE_REMOTE".into(), "(null)".into()));

    // Shadow dbus.
    // TODO: we can set up our own broker and provide desktop portal
    // functionality.
    // let dbus_socket = Path::join(Path::new(xdg_runtime_dir), "dbus");
    // envs.push(("DBUS_SESSION_BUS_ADDRESS".into(), dbus_socket.into()));

    tracing::debug!(
        exe = exe_path.to_string_lossy().to_string(),
        env = ?envs,
        "launching child process"
    );

    let mut command = unshare::Command::new(&exe_path);
    let command = command
        .args(&args)
        .envs(envs)
        .stdin(unshare::Stdio::null())
        .stdout(stdout)
        .stderr(stderr);

    let command = unsafe {
        command.pre_exec(|| {
            // Creates a new process group.
            rustix::process::setsid()?;
            Ok(())
        })
    };

    match command.spawn() {
        Ok(child) => {
            trace!(pid = child.id(), "child process started");
            Ok(child)
        }
        Err(e) => Err(anyhow!(
            "failed to spawn child process '{}': {:#}",
            exe_path.to_string_lossy(),
            e
        )),
    }
}

pub fn signal_child(pid: i32, sig: rustix::process::Signal) -> anyhow::Result<()> {
    // Signal the whole process group. We used setsid, so the group should be
    // the same as the child pid.
    let pid = rustix::process::Pid::from_raw(pid).unwrap();
    rustix::process::kill_process_group(pid, sig)?;

    Ok(())
}

pub fn wait_child(child: &mut Child) -> anyhow::Result<()> {
    let exit_status = child.wait()?;

    info!(
        exit_status = exit_status.code().unwrap_or_default(),
        "child process exited"
    );

    match exit_status {
        ExitStatus::Exited(c) if c != 0 => {
            bail!("child process exited with error code {}", c)
        }
        _ => Ok(()),
    }
}
