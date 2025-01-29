// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{
    fs::{File, OpenOptions},
    os::fd::{AsFd, BorrowedFd},
};

use anyhow::anyhow;
use libc::dev_t;

pub struct DrmDevice(File);

impl AsFd for DrmDevice {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl drm::Device for DrmDevice {}
impl drm::control::Device for DrmDevice {}

impl DrmDevice {
    pub fn new(dev: dev_t) -> anyhow::Result<Self> {
        let path = drm::node::DrmNode::from_dev_id(dev)?
            .dev_path()
            .ok_or(anyhow!("no device file found"))?;

        let mut options = OpenOptions::new();
        options.read(true);
        options.write(true);

        Ok(Self(options.open(path)?))
    }
}
