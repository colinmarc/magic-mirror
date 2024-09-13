// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

mod xwm;
use anyhow::{anyhow, bail, Context as _};
use pathsearch::find_executable_in_path;
pub use xwm::*;

use std::{
    ffi::OsStr,
    io::Read as _,
    os::fd::{AsFd, AsRawFd as _},
    path::{Path, PathBuf},
    sync::Arc,
};

use tracing::{debug, trace};

use crate::compositor::ClientState;

pub struct XWayland {
    pub display_socket: PathBuf,
    pub displayfd_recv: mio::unix::pipe::Receiver,
    pub child: unshare::Child,
    pub output: std::io::BufReader<mio::unix::pipe::Receiver>,

    xwm_socket: Option<mio::net::UnixStream>,
}

impl XWayland {
    pub fn spawn(
        dh: &mut wayland_server::DisplayHandle,
        xdg_runtime_dir: &OsStr,
    ) -> anyhow::Result<Self> {
        // Used to capture stdout and stderr.
        let (output_send, output_recv) = mio::unix::pipe::new()?;

        // These get dropped when we return, closing the write side (in this process)
        let stdout = unshare::Stdio::dup_file(&output_send)?;
        let stderr = unshare::Stdio::dup_file(&output_send)?;

        let (xwm_xwayland, xwm_compositor) = mio::net::UnixStream::pair()?;
        let (wayland_xwayland, wayland_compositor) = mio::net::UnixStream::pair()?;

        // XWayland writes the the display number and a newline to this pipe when
        // it's ready.
        let (displayfd_send, displayfd_recv) = mio::unix::pipe::new()?;

        // Rather than messing with global /tmp, we create a scoped X11 socket. At
        // some point in the past, X11 didn't support this, but clients should
        // support arbitrary paths by now.
        let socket_path = Path::new(xdg_runtime_dir).join(".X11-unix").join("XMM");
        std::fs::create_dir_all(socket_path.parent().unwrap())?;

        let socket = mio::net::UnixListener::bind(&socket_path)?;

        let exe = find_executable_in_path("Xwayland").ok_or(anyhow!("Xwayland not in PATH"))?;
        let mut command = unshare::Command::new(exe);
        command
            .stdout(stdout)
            .stderr(stderr)
            .arg("-verbose")
            .arg("-rootless")
            .arg("-terminate")
            .arg("-force-xrandr-emulation")
            .arg("-wm")
            .arg(xwm_xwayland.as_raw_fd().to_string())
            .arg("-displayfd")
            .arg(displayfd_send.as_raw_fd().to_string())
            .arg("-listenfd")
            .arg(socket.as_raw_fd().to_string());

        // Setup the environment; clear everything except PATH and XDG_RUNTIME_DIR.
        command.env_clear();
        for (key, value) in std::env::vars_os() {
            if key.to_str() == Some("PATH") {
                command.env(key, value);
                continue;
            }
        }

        command.env("XDG_RUNTIME_DIR", xdg_runtime_dir);
        command.env(
            "WAYLAND_SOCKET",
            format!("{}", wayland_xwayland.as_raw_fd()),
        );

        unsafe {
            command.pre_exec(move || {
                // Creates a new process group. This prevents SIGINT sent to
                // mmserver from reaching Xwayland.
                rustix::process::setsid()?;

                // unset the CLOEXEC flag from the sockets we need to pass
                // to xwayland.
                unset_cloexec(&wayland_xwayland)?;
                unset_cloexec(&xwm_xwayland)?;
                unset_cloexec(&displayfd_send)?;
                unset_cloexec(&socket)?;

                Ok(())
            });
        }

        let child = match command.spawn() {
            Ok(child) => child,
            Err(e) => bail!("failed to spawn Xwayland: {:#}", e),
        };

        debug!(x11_socket = ?socket_path, pid = child.id(), "spawned Xwayland instance");

        // Insert the client into the display handle. The order is important
        // here; XWayland never starts up at all unless it can roundtrip with
        // wayland.
        let _client = dh.insert_client(
            wayland_compositor.into(),
            Arc::new(ClientState { xwayland: true }),
        )?;

        Ok(Self {
            display_socket: socket_path,
            displayfd_recv,
            child,
            output: std::io::BufReader::new(output_recv),

            xwm_socket: Some(xwm_compositor),
        })
    }

    pub fn is_ready(&mut self) -> anyhow::Result<Option<mio::net::UnixStream>> {
        if self.xwm_socket.is_none() {
            bail!("XWayland already marked as ready")
        }

        let mut buf = [0; 64];

        match self.displayfd_recv.read(&mut buf) {
            Ok(len) => {
                if (buf[..len]).contains(&b'\n') {
                    trace!("Xwayland ready");
                    return Ok(self.xwm_socket.take());
                } else {
                    // Not ready yet.
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => (),
            Err(err) => return Err(err).context("reading from xwayland pipe failed"),
        }

        Ok(None)
    }
}

fn unset_cloexec(socket_fd: impl AsFd) -> Result<(), rustix::io::Errno> {
    rustix::fs::fcntl_setfd(socket_fd, rustix::fs::FdFlags::empty())?;

    Ok(())
}
