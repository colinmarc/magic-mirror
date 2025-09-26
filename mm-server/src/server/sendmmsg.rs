// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::os::fd::{AsFd, AsRawFd};
use std::time;
use std::{io::IoSlice, net::SocketAddr};

use nix::sys::socket::{
    cmsg_space, setsockopt, sockopt::TxTime, ControlMessage, MsgFlags, MultiHeaders,
    SockaddrStorage,
};
use tracing::instrument;

use crate::server::LocalSocketInfo;

#[derive(Default)]
pub struct SendMmsg<'a> {
    iovs: Vec<[IoSlice<'a>; 1]>,
    addrs: Vec<Option<SockaddrStorage>>,
    txtimes: Vec<u64>,
}

impl<'a> SendMmsg<'a> {
    #[instrument(skip_all)]
    pub fn sendmsg(mut self, buf: &'a [u8], addr: SocketAddr, txtime: time::Instant) -> Self {
        self.iovs.push([IoSlice::new(buf)]);
        self.addrs.push(Some(addr.into()));

        let txtime = std_time_to_u64(&txtime);
        self.txtimes.push(txtime);

        self
    }

    #[instrument(skip_all)]
    pub fn finish(&mut self, fd: &impl AsRawFd, local_addr: LocalSocketInfo) -> Result<(), nix::Error> {
        let mut data: MultiHeaders<SockaddrStorage> = MultiHeaders::preallocate(
            self.iovs.len(),
            Some(Vec::with_capacity(cmsg_space::<u64>() * self.iovs.len() + cmsg_space::<libc::in6_pktinfo>())),
        );
        let mut cmsgs = self
            .txtimes
            .iter()
            .map(ControlMessage::TxTime)
            .collect::<Vec<_>>();

        cmsgs.push(match &local_addr {
            LocalSocketInfo::V4(info, _) => ControlMessage::Ipv4PacketInfo(info),
            LocalSocketInfo::V6(info, _) => ControlMessage::Ipv6PacketInfo(info),
        });

        loop {
            match nix::sys::socket::sendmmsg(
                fd.as_raw_fd(),
                &mut data,
                &self.iovs,
                &self.addrs,
                &cmsgs,
                MsgFlags::empty(),
            ) {
                Ok(_) => break,
                Err(nix::errno::Errno::EAGAIN) => continue,
                Err(e) => return Err(e),
            };
        }

        Ok(())
    }
}

pub fn new<'a>() -> SendMmsg<'a> {
    SendMmsg::default()
}

#[cfg(target_os = "linux")]
pub fn set_so_txtime(sock: &impl AsFd) -> anyhow::Result<()> {
    let config = nix::libc::sock_txtime {
        clockid: nix::libc::CLOCK_MONOTONIC,
        flags: 0,
    };

    setsockopt(&sock, TxTime, &config)?;

    Ok(())
}

#[cfg(target_os = "linux")]
fn std_time_to_u64(time: &std::time::Instant) -> u64 {
    const NANOS_PER_SEC: u64 = 1_000_000_000;

    const INSTANT_ZERO: std::time::Instant = unsafe { std::mem::transmute(std::time::UNIX_EPOCH) };

    let raw_time = time.duration_since(INSTANT_ZERO);

    let sec = raw_time.as_secs();
    let nsec = raw_time.subsec_nanos();

    sec * NANOS_PER_SEC + nsec as u64
}
