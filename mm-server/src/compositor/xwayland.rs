// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

mod xwm;
use std::{
    io::Read as _,
    os::fd::{AsFd, AsRawFd as _},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{anyhow, bail, Context as _};
use pathsearch::find_executable_in_path;
use tracing::{debug, trace};
pub use xwm::*;

use crate::{
    compositor::ClientState,
    config::HomeIsolationMode,
    container::{ChildHandle, Container},
};

pub struct XWayland {
    pub display_socket: PathBuf,
    pub displayfd_recv: mio::unix::pipe::Receiver,
    pub child: ChildHandle,

    xwm_socket: Option<mio::net::UnixStream>,
}

// Where the socket gets mounted inside the container.
const SOCKET_PATH: &str = "/tmp/.X11-unix/X1";
const DISPLAY: &str = ":1";

impl XWayland {
    pub fn spawn(
        dh: &mut wayland_server::DisplayHandle,
        xdg_runtime_dir: impl AsRef<Path>,
        stdio: impl AsFd,
    ) -> anyhow::Result<Self> {
        let (xwm_xwayland, xwm_compositor) = mio::net::UnixStream::pair()?;
        let (wayland_xwayland, wayland_compositor) = mio::net::UnixStream::pair()?;

        // XWayland writes the the display number and a newline to this pipe when
        // it's ready.
        let (displayfd_send, displayfd_recv) = mio::unix::pipe::new()?;

        // Put the socket in a folder, so we can bind-mount that to
        // /tmp/.X11-unix inside the container.
        let socket_path = xdg_runtime_dir
            .as_ref()
            .join(Path::new(SOCKET_PATH).strip_prefix("/tmp").unwrap());
        std::fs::create_dir_all(socket_path.parent().unwrap())?;

        let socket = mio::net::UnixListener::bind(&socket_path)?;

        let exe = find_executable_in_path("Xwayland")
            .ok_or(anyhow!("Xwayland not in PATH"))?
            .as_os_str()
            .to_owned();
        let args = vec![
            exe,
            "-verbose".into(),
            "-rootless".into(),
            "-terminate".into(),
            "-force-xrandr-emulation".into(),
            "-wm".into(),
            xwm_xwayland.as_raw_fd().to_string().into(),
            "-displayfd".into(),
            displayfd_send.as_raw_fd().to_string().into(),
            "-listenfd".into(),
            socket.as_raw_fd().to_string().into(),
        ];

        let mut container = Container::new(args, HomeIsolationMode::Tmpfs)?;

        container.set_env(
            "WAYLAND_SOCKET",
            format!("{}", wayland_xwayland.as_raw_fd()),
        );

        container.set_stdout(stdio.as_fd())?;
        container.set_stderr(stdio.as_fd())?;

        unsafe {
            container.pre_exec(move || {
                // unset the CLOEXEC flag from the sockets we need to pass
                // to xwayland.
                unset_cloexec(&wayland_xwayland)?;
                unset_cloexec(&xwm_xwayland)?;
                unset_cloexec(&displayfd_send)?;
                unset_cloexec(&socket)?;

                Ok(())
            });
        }

        let child = container.spawn().context("failed to spawn XWayland")?;
        debug!(x11_socket = ?socket_path, "spawned Xwayland instance");

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

    pub fn prepare_socket(&self, container: &mut Container) {
        container.bind_mount(
            self.display_socket.parent().unwrap(),
            Path::new(SOCKET_PATH).parent().unwrap(),
        );
        container.set_env("DISPLAY", DISPLAY);
    }
}

fn unset_cloexec(socket_fd: impl AsFd) -> Result<(), rustix::io::Errno> {
    rustix::fs::fcntl_setfd(socket_fd, rustix::fs::FdFlags::empty())?;

    Ok(())
}
