// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use hashbrown::HashMap;
use parking_lot::Mutex;
use tracing::{error, info};

use crate::config::Config;
use crate::{session::Session, vulkan::VkContext};

pub type SharedState = Arc<Mutex<ServerState>>;

pub struct ServerState {
    // TODO: we'd rather use a BTreeMap, but we want
    // hash_brown::HashMap::extract_if.
    pub sessions: HashMap<u64, Session>,
    pub session_seq: usize,
    pub id_generator: tiny_id::ShortCodeGenerator<char>,
    pub cfg: Config,
    pub vk: Arc<VkContext>,
}

impl ServerState {
    pub fn new(vk: Arc<VkContext>, cfg: Config) -> Self {
        Self {
            vk,
            cfg,
            sessions: HashMap::new(),
            session_seq: 0,
            id_generator: tiny_id::ShortCodeGenerator::new_numeric(6),
        }
    }

    pub fn generate_session_id(&mut self) -> (usize, u64) {
        let seq = self.session_seq;
        self.session_seq += 1;

        (seq, self.id_generator.next_int())
    }

    /// Run periodic cleanup, e.g. ending defunct sessions.
    pub fn tick(&mut self) -> anyhow::Result<()> {
        self.sessions
            .extract_if(|_, s| {
                if s.defunct {
                    info!("cleaning up defunct session {}", s.id);
                    return true;
                }

                let session_timeout = self.cfg.apps[&s.application_id].session_timeout;
                if s.detached_since
                    .zip(session_timeout)
                    .is_some_and(|(t, timeout)| t.elapsed() > timeout)
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
