// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::{io, time};

use rustix::event::{eventfd, poll, EventfdFlags, PollFd, PollFlags};
use rustix::io::{read, write, Errno};

/// An IPC barrier using eventfd(2).
pub struct EventfdBarrier {
    a: OwnedFd,
    b: OwnedFd,
    other: bool,
}

impl EventfdBarrier {
    pub fn new() -> io::Result<(Self, Self)> {
        let a = eventfd(0, EventfdFlags::NONBLOCK)?;
        let b = eventfd(0, EventfdFlags::NONBLOCK)?;

        let a2 = a.try_clone()?;
        let b2 = b.try_clone()?;

        Ok((
            Self { a, b, other: false },
            Self {
                a: a2,
                b: b2,
                other: true,
            },
        ))
    }

    // Waits at the barrier, timing out after the given duration.
    pub fn sync(&self, timeout: time::Duration) -> rustix::io::Result<()> {
        if self.other {
            wait_eventfd(&self.a, timeout)?;
            signal_eventfd(&self.b)?;
        } else {
            signal_eventfd(&self.a)?;
            wait_eventfd(&self.b, timeout)?;
        }

        Ok(())
    }
}

/// Creates an IPC channel for sending a file descriptor.
pub fn fd_oneshot() -> io::Result<(FdSender, FdReceiver)> {
    let (sender, receiver) = uds::UnixSeqpacketConn::pair()?;
    Ok((FdSender(sender), FdReceiver(receiver)))
}

pub struct FdSender(uds::UnixSeqpacketConn);

impl FdSender {
    pub fn send_timeout(self, fd: OwnedFd, timeout: time::Duration) -> io::Result<()> {
        self.0.set_write_timeout(Some(timeout))?;

        let raw_fd = fd.as_raw_fd();
        self.0.send_fds(&[], &[raw_fd])?;

        // The FD gets dropped here, along with our end of the connection.
        Ok(())
    }
}

pub struct FdReceiver(uds::UnixSeqpacketConn);

impl FdReceiver {
    pub fn recv_timeout(self, timeout: time::Duration) -> io::Result<OwnedFd> {
        self.0.set_read_timeout(Some(timeout))?;

        let mut fds = [-1];

        self.0.recv_fds(&mut [], &mut fds)?;
        if fds[0] <= 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected message received",
            ));
        }

        let fd = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        Ok(fd)
    }
}

fn signal_eventfd(fd: impl AsFd) -> rustix::io::Result<()> {
    loop {
        match write(&fd, &1_u64.to_ne_bytes()).map(|_| ()) {
            Err(Errno::INTR) => continue,
            v => return v,
        }
    }
}

fn wait_eventfd(fd: impl AsFd, timeout: time::Duration) -> rustix::io::Result<()> {
    let mut pollfd = [PollFd::new(&fd, PollFlags::IN)];
    let mut buf = [0; 8];
    loop {
        match poll(&mut pollfd, timeout.as_millis() as _) {
            Ok(0) => return Err(Errno::TIMEDOUT),
            Ok(_) => return read(fd, &mut buf).map(|_| ()),
            Err(Errno::INTR) => continue,
            Err(e) => return Err(e),
        }
    }
}
