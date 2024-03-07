// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use crate::config::Config;
use hashbrown::HashMap; // For stable extract_if.
use std::sync::{Arc, Mutex};
use tracing::{error, info};

use crate::{session::Session, vulkan::VkContext};

pub type SharedState = Arc<Mutex<ServerState>>;

const DEFAULT_SESSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10 * 60);

pub struct ServerState {
    pub sessions: HashMap<u64, Session>,
    pub cfg: Config,
    pub vk: Arc<VkContext>,
}

impl ServerState {
    pub fn new(vk: Arc<VkContext>, cfg: Config) -> Self {
        Self {
            vk,
            cfg,
            sessions: HashMap::new(),
        }
    }

    /// Run periodic cleanup, e.g. ending defunct sessions.
    pub fn tick(&mut self) -> anyhow::Result<()> {
        self.sessions
            .extract_if(|_, s| {
                if s.defunct {
                    info!("cleaning up defunct session {}", s.id);
                    true
                } else if s
                    .detached_since
                    .map(|d| d.elapsed() > DEFAULT_SESSION_TIMEOUT)
                    .unwrap_or(false)
                {
                    info!("cleaning up idle session {}", s.id);
                    true
                } else {
                    false
                }
            })
            .for_each(|(_, s)| match s.stop() {
                Ok(()) => {}
                Err(e) => {
                    error!("session ended with error: {:#}", e);
                }
            });
        Ok(())
    }
}
