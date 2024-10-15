// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

use crossbeam_channel as crossbeam;

use super::CompositorEvent;

#[derive(Debug, Clone)]
struct Inner {
    attachments: BTreeMap<u64, crossbeam::Sender<CompositorEvent>>,
}

#[derive(Debug, Clone)]
pub struct CompositorHandle(Arc<RwLock<Inner>>, Arc<mio::Waker>);

impl CompositorHandle {
    pub fn new(waker: Arc<mio::Waker>) -> Self {
        Self(
            Arc::new(RwLock::new(Inner {
                attachments: BTreeMap::new(),
            })),
            waker,
        )
    }

    pub fn insert_client(&self, id: u64, sender: crossbeam::Sender<CompositorEvent>) {
        self.0.write().unwrap().attachments.insert(id, sender);
    }

    pub fn remove_client(&self, id: u64) {
        self.0.write().unwrap().attachments.remove(&id);
    }

    pub fn remove_all(&self) {
        self.0.write().unwrap().attachments.clear();
    }

    pub fn dispatch(&self, event: CompositorEvent) {
        let attachments = &self.0.read().unwrap().attachments;
        for (_, sender) in attachments.iter() {
            sender.send(event.clone()).ok();
        }
    }

    pub fn wake(&self) -> std::io::Result<()> {
        self.1.wake()
    }

    pub fn kick_clients(&self) {
        let attachments = &mut self.0.write().unwrap().attachments;
        for (_, sender) in std::mem::take(attachments) {
            sender.send(CompositorEvent::Shutdown).ok();
        }
    }

    pub fn num_attachments(&self) -> usize {
        self.0.read().unwrap().attachments.len()
    }
}
