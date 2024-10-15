// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{
    os::fd::{AsFd, BorrowedFd, OwnedFd},
    path::{Path, PathBuf},
};

use anyhow::{bail, Context as _};
use rustix::process::{Pid, Signal, WaitId, WaitidOptions};
use tracing::{debug, info};

mod container;
pub use container::{Container, HomeIsolationMode};

/// A handle to a running container.
pub struct ChildHandle {
    pid: Pid,
    pidfd: OwnedFd,

    run_path: PathBuf,
}

impl AsFd for ChildHandle {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.pidfd()
    }
}

impl ChildHandle {
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
            rustix::process::waitid(WaitId::PidFd(self.as_fd()), WaitidOptions::EXITED)
                .context("waitid")?
                .and_then(|x| x.exit_status())
                .unwrap_or_default();

        info!(exit_status, "child process exited");
        if exit_status != 0 {
            bail!("child process exited with status: {exit_status}");
        }

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
        let fd = container::fuse_mount(&self.pidfd, &dst, fsname.as_ref().to_owned(), st_mode)?;

        Ok(fd)
    }
}

impl Drop for ChildHandle {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.run_path);
    }
}
