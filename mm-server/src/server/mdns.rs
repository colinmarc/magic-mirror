// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::net::SocketAddr;

use anyhow::bail;
use tracing::debug;

pub struct MdnsService {
    daemon: mdns_sd::ServiceDaemon,
    service_name: String,
}

impl MdnsService {
    pub fn new(
        addr: SocketAddr,
        hostname: Option<&str>,
        instance_name: Option<&str>,
    ) -> anyhow::Result<Self> {
        let daemon = mdns_sd::ServiceDaemon::new()?;

        let txt = [(
            "mmp",
            std::str::from_utf8(mm_protocol::ALPN_PROTOCOL_VERSION).unwrap(),
        )];

        let hostname = match hostname {
            Some(h) => h.to_owned(),
            None => mdns_hostname()?,
        };

        let instance_name = match instance_name {
            Some(s) => s.to_owned(),
            None => mdns_instance_name(&hostname)?,
        };

        let ip = addr.ip();
        let (ip, ip_auto) = if ip.is_unspecified() {
            (vec![], true)
        } else {
            (vec![ip], false)
        };

        let mut service_info = mdns_sd::ServiceInfo::new(
            "_magic-mirror._udp.local.",
            &instance_name,
            &hostname,
            &ip[..],
            addr.port(),
            &txt[..],
        )?;

        if ip_auto {
            service_info = service_info.enable_addr_auto();
        }

        let service_name = service_info.get_fullname().to_owned();
        daemon.register(service_info)?;

        debug!(hostname, instance_name, ip = ?ip.first(), ip_auto, "advertizing service");

        Ok(Self {
            daemon,
            service_name,
        })
    }
}

impl Drop for MdnsService {
    fn drop(&mut self) {
        loop {
            match self.daemon.unregister(&self.service_name) {
                Ok(_) => break,
                Err(mdns_sd::Error::Again) => continue,
                Err(err) => {
                    debug!(?err, "error shutting down mdns daemon");
                    return;
                }
            }
        }

        loop {
            match self.daemon.shutdown() {
                Ok(_) => return,
                Err(mdns_sd::Error::Again) => continue,
                Err(err) => {
                    debug!(?err, "error shutting down mdns daemon");
                    return;
                }
            }
        }
    }
}

fn mdns_hostname() -> anyhow::Result<String> {
    let uname = rustix::system::uname();

    let hostname = uname.nodename().to_str()?;
    if hostname.is_empty() {
        bail!("empty hostname");
    }

    if hostname.ends_with(".local") {
        return Ok(format!("{hostname}."));
    } else if hostname.contains('.') {
        bail!("hostname appears to be a qualified domain");
    }

    Ok(format!("{hostname}.local."))
}

fn mdns_instance_name(hostname: &str) -> anyhow::Result<String> {
    if hostname.is_empty() {
        bail!("empty hostname");
    }

    let hostname = match hostname.split_once('.') {
        Some((host, _)) => host,
        None => hostname,
    };

    Ok(hostname.to_uppercase())
}
