// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

mod xwm;
use std::{
    io::{self, Read as _},
    os::fd::{AsFd, AsRawFd as _},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{anyhow, bail, Context as _};
use pathsearch::find_executable_in_path;
use tracing::{debug, trace};
pub use xwm::*;

use crate::{
    config::HomeIsolationMode,
    container::{Container, ContainerHandle},
    session::compositor::ClientState,
};

pub struct XWayland {
    pub display_socket: DisplaySocket,
    pub displayfd_recv: mio::unix::pipe::Receiver,
    pub child: ContainerHandle,

    extern_socket_dir: PathBuf,
    xwm_socket: Option<mio::net::UnixStream>,
}

// Where the socket gets mounted inside the container.
const CONONICAL_DISPLAY_PATH: &str = "/tmp/.X11-unix";

pub struct DisplaySocket(u32);

impl DisplaySocket {
    fn pick_unused() -> anyhow::Result<Self> {
        use rustix::net::*;

        // Because we're using a mount namespace, we don't need to worry about
        // system sockets in /tmp leaking into our container. However, because we
        // don't use a network namespace, it is possible for system abstract sockets
        // to be available. We can ensure that isn't the case by attempting to
        // bind the abstract socket.
        let mut display = 1;
        let sock = socket(AddressFamily::UNIX, SocketType::STREAM, None)?;

        loop {
            let dp = DisplaySocket(display);

            match rustix::net::bind(
                &sock,
                // By convention, the name is the same as the path.
                &SocketAddrUnix::new_abstract_name(dp.inner_path().as_os_str().as_encoded_bytes())?,
            ) {
                Ok(()) => return Ok(dp), // Discard the abstract socket.
                Err(e) if e.kind() == io::ErrorKind::AddrInUse => display += 1,
                Err(e) => return Err(e.into()),
            }
        }
    }

    pub fn display(&self) -> String {
        format!(":{}", self.0)
    }

    pub fn inner_path(&self) -> PathBuf {
        Path::new(CONONICAL_DISPLAY_PATH).join(format!("X{}", self.0))
    }
}

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

        let display_socket = DisplaySocket::pick_unused()?;

        // Put the socket in a folder, so we can bind-mount that to
        // /tmp/.X11-unix inside the (app) container.
        let extern_socket_path = xdg_runtime_dir
            .as_ref()
            .join(display_socket.inner_path().strip_prefix("/").unwrap());
        let extern_socket_dir = extern_socket_path.parent().unwrap();

        std::fs::create_dir_all(extern_socket_dir)?;
        let socket = mio::net::UnixListener::bind(&extern_socket_path)?;

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
        debug!(x11_socket = ?extern_socket_path, "spawned Xwayland instance");

        // Insert the client into the display handle. The order is important
        // here; XWayland never starts up at all unless it can roundtrip with
        // wayland.
        let _client = dh.insert_client(
            wayland_compositor.into(),
            Arc::new(ClientState { xwayland: true }),
        )?;

        Ok(Self {
            display_socket,
            displayfd_recv,
            child,

            extern_socket_dir: extern_socket_dir.to_owned(),
            xwm_socket: Some(xwm_compositor),
        })
    }

    pub fn poll_ready(&mut self) -> anyhow::Result<Option<mio::net::UnixStream>> {
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
        container.bind_mount(&self.extern_socket_dir, Path::new(CONONICAL_DISPLAY_PATH));
        container.set_env("DISPLAY", self.display_socket.display());
    }
}

fn unset_cloexec(socket_fd: impl AsFd) -> Result<(), rustix::io::Errno> {
    rustix::io::fcntl_setfd(socket_fd, rustix::io::FdFlags::empty())
}
