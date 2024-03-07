// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{process::Stdio, time};

use anyhow::Context;
use smithay::{
    reexports::{calloop, wayland_server},
    xwayland,
};
use tracing::trace;

use super::State;

/// Smithay's XWayland code is heavily integrated in calloop, so we need an
/// abstraction to integrate it into our mio-based event loop. This maintains
/// a separate event loop which runs in another thread.
pub(super) struct XWaylandLoop {
    pub(super) x11_display: u32,
    event_loop: calloop::EventLoop<'static, State>,
}

impl XWaylandLoop {
    pub fn new(dh: wayland_server::DisplayHandle) -> anyhow::Result<Self> {
        let event_loop: calloop::EventLoop<'_, State> =
            calloop::EventLoop::try_new().context("failed to create xwm event loop")?;

        let envs = std::iter::empty::<(String, String)>();
        let (xwayland, client) = smithay::xwayland::XWayland::spawn(
            &dh,
            None,
            envs,
            true,
            Stdio::null(),
            Stdio::null(),
            |_| {},
        )?;

        let x11_display = xwayland.display_number();

        let handle = event_loop.handle();
        event_loop
            .handle()
            .insert_source(xwayland, move |event, _, state| {
                trace!("X11 event: {:?}", event);

                if let xwayland::XWaylandEvent::Ready { x11_socket, .. } = event {
                    let xwm =
                        xwayland::xwm::X11Wm::start_wm(handle.clone(), x11_socket, client.clone())
                            .expect("failed to start xwm");

                    state.xwm = Some(xwm)
                }
            })
            .unwrap();

        Ok(Self {
            x11_display,
            event_loop,
        })
    }

    pub fn dispatch(&mut self, state: &mut State) -> anyhow::Result<()> {
        self.event_loop
            .dispatch(Some(time::Duration::from_millis(1)), state)
            .context("failed to dispatch X11 event loop")
    }
}
