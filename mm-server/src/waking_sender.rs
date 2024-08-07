// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

pub struct WakingSender<T> {
    waker: Arc<mio::Waker>,
    sender: crossbeam_channel::Sender<T>,
}

impl<T> Clone for WakingSender<T> {
    fn clone(&self) -> Self {
        Self {
            waker: self.waker.clone(),
            sender: self.sender.clone(),
        }
    }
}

impl<T> WakingSender<T> {
    pub fn new(waker: Arc<mio::Waker>, sender: crossbeam_channel::Sender<T>) -> Self {
        assert!(
            !sender.is_full(),
            "WakingSender must be created with a non-zero capacity channel"
        );

        Self { waker, sender }
    }

    pub fn send(&self, msg: T) -> Result<(), crossbeam_channel::SendError<T>> {
        self.sender.send(msg)?;
        self.waker.wake().unwrap();
        Ok(())
    }

    pub fn try_send(&self, msg: T) -> Result<(), crossbeam_channel::TrySendError<T>> {
        self.sender.try_send(msg)?;
        self.waker.wake().unwrap();
        Ok(())
    }
}

pub struct WakingOneshot<T> {
    waker: Arc<mio::Waker>,
    sender: oneshot::Sender<T>,
}

impl<T> WakingOneshot<T> {
    pub fn new(waker: Arc<mio::Waker>, sender: oneshot::Sender<T>) -> Self {
        Self { waker, sender }
    }

    pub fn send(self, msg: T) -> Result<(), oneshot::SendError<T>> {
        self.sender.send(msg)?;
        self.waker.wake().unwrap();
        Ok(())
    }
}
