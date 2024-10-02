// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{
    os::fd::{AsFd, BorrowedFd, OwnedFd},
    path::PathBuf,
};

use anyhow::{bail, Context as _};
use rustix::process::{Pid, Signal, WaitId, WaitidOptions};
use tracing::{debug, info};

mod container;
pub use container::*;

/// A handle to a running container.
pub struct ChildHandle {
    pid: Pid,
    pidfd: OwnedFd,

    run_path: PathBuf,
}

impl AsFd for ChildHandle {
    fn as_fd(&self) -> std::os::unix::prelude::BorrowedFd<'_> {
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
}

impl Drop for ChildHandle {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.run_path);
    }
}
