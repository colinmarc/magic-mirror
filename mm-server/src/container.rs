// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{
    ffi::CStr,
    os::fd::{AsFd, BorrowedFd, OwnedFd},
    path::{Path, PathBuf},
};

use anyhow::{bail, Context as _};
use rustix::{
    mount::MountAttrFlags,
    process::{Pid, Signal, WaitId, WaitIdOptions},
};
use tracing::{debug, info};

mod ipc;
mod runtime;
pub use runtime::Container;

/// A handle to a running container.
pub struct ContainerHandle {
    pid: Pid,
    pidfd: OwnedFd,

    run_path: PathBuf,
}

impl AsFd for ContainerHandle {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.pidfd()
    }
}

impl ContainerHandle {
    pub fn pid(&self) -> Pid {
        self.pid
    }

    pub(crate) fn pidfd(&self) -> BorrowedFd<'_> {
        self.pidfd.as_fd()
    }

    pub fn signal(&mut self, signal: Signal) -> anyhow::Result<()> {
        debug!(?signal, pid = self.pid.as_raw_nonzero(), "signaling child");

        rustix::process::pidfd_send_signal(self, signal).context("pidfd_send_signal")?;
        Ok(())
    }

    pub fn wait(&mut self) -> anyhow::Result<()> {
        let exit_status =
            rustix::process::waitid(WaitId::PidFd(self.as_fd()), WaitIdOptions::EXITED)
                .context("waitid")?
                .and_then(|x| x.exit_status())
                .unwrap_or_default();

        info!(exit_status, "child process exited");
        if exit_status != 0 {
            bail!("child process exited with status: {exit_status}");
        }

        Ok(())
    }

    /// Mounts a named filesystem inside the container at the given path.
    pub fn fs_mount<S>(
        &self,
        dst: impl AsRef<Path>,
        fstype: impl AsRef<str>,
        attr: MountAttrFlags,
        options: impl AsRef<[(S, S)]>,
    ) -> anyhow::Result<()>
    where
        S: AsRef<CStr>,
    {
        let options = options
            .as_ref()
            .iter()
            .map(|(k, v)| (k.as_ref(), v.as_ref()))
            .collect::<Vec<_>>();

        runtime::fs_mount_into(&self.pidfd, dst, fstype.as_ref().to_owned(), attr, &options)?;
        Ok(())
    }

    /// Opens /dev/fuse inside the container, mounts it to the given path,
    /// and returns the FD for use in a FUSE daemon.
    pub fn fuse_mount(
        &self,
        dst: impl AsRef<Path>,
        fsname: impl AsRef<str>,
        st_mode: u32,
    ) -> anyhow::Result<OwnedFd> {
        let fd = runtime::fuse_mount_into(&self.pidfd, &dst, fsname.as_ref().to_owned(), st_mode)?;

        Ok(fd)
    }
}

impl Drop for ContainerHandle {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.run_path);
    }
}
