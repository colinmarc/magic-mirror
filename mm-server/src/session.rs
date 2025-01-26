// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{path::PathBuf, sync::Arc, time};

use anyhow::{anyhow, bail};
use crossbeam_channel as crossbeam;
use mm_protocol as protocol;
use pathsearch::find_executable_in_path;
use tracing::{debug_span, info};

use crate::{codec::probe_codec, vulkan::VkContext, waking_sender::WakingSender};

mod audio;
pub mod compositor;
pub mod control;
mod handle;
mod input;
mod reactor;
mod video;

use control::{AudioStreamParams, ControlMessage, DisplayParams, SessionEvent, VideoStreamParams};
pub use handle::SessionHandle;
pub use input::GamepadLayout;
use reactor::Reactor;
pub use reactor::EPOCH;

/// How long to wait for the compositor to accept a new attachment.
const ATTACH_TIMEOUT: time::Duration = time::Duration::from_secs(10);

pub struct Session {
    pub id: u64,
    pub display_params: DisplayParams,
    pub application_id: String,
    pub started: time::SystemTime,
    pub detached_since: Option<time::Instant>,
    pub permanent_gamepads: Vec<protocol::Gamepad>,
    pub defunct: bool,

    comp_thread_handle: std::thread::JoinHandle<anyhow::Result<()>>,
    control_sender: WakingSender<ControlMessage>,
    operator_attachment_id: Option<u64>,

    pub bug_report_dir: Option<PathBuf>,

    vk: Arc<VkContext>,
}

pub struct Attachment {
    pub session_id: u64,
    pub attachment_id: u64,
    pub events: crossbeam::Receiver<SessionEvent>,
    pub control: WakingSender<ControlMessage>,
}

impl Session {
    /// Launches a standalone compositor and the application process. Blocks
    /// until both have started up and connected over a unix socket.
    pub fn launch(
        vk: Arc<VkContext>,
        id: u64,
        application_id: &str,
        application_config: &super::config::AppConfig,
        display_params: DisplayParams,
        permanent_gamepads: Vec<protocol::Gamepad>,
        bug_report_dir: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        // Do an early check that the executable exists.
        let exe = application_config.command.first().unwrap();
        find_executable_in_path(exe).ok_or(anyhow!("command {:?} not in PATH", exe))?;

        // Launch the compositor, which in turn launches the app.
        let (ready_send, ready_recv) = oneshot::channel();
        let vk_clone = vk.clone();
        let app_name = application_id.to_owned();
        let app_cfg = application_config.clone();
        let gamepads = permanent_gamepads
            .iter()
            .map(|pad| (pad.id, GamepadLayout::GenericDualStick)) // TODO layout.
            .collect();

        let bug_report_dir_clone = bug_report_dir.clone();
        let comp_thread_handle = std::thread::spawn(move || {
            tracy_client::set_thread_name!("compositor");

            let span = debug_span!("session", session_id = id, app = app_name);
            let _guard = span.enter();

            Reactor::run(
                vk_clone,
                app_cfg,
                display_params,
                gamepads,
                bug_report_dir_clone,
                ready_send,
            )
        });

        info!(session_id = id, application = ?application_id, "launching session");

        // Wait until the compositor is ready.
        let control_sender = match ready_recv.recv() {
            Ok(s) => s,
            Err(_) => {
                return match comp_thread_handle.join() {
                    Ok(Ok(())) => Err(anyhow!("compositor thread exited unexpectedly")),
                    Ok(Err(e)) => Err(e),
                    Err(_) => Err(anyhow!("compositor thread panicked")),
                }
            }
        };

        Ok(Self {
            id,
            application_id: application_id.to_string(),
            display_params,
            permanent_gamepads,
            started: time::SystemTime::now(),
            defunct: false,
            detached_since: None,
            operator_attachment_id: None,
            comp_thread_handle,
            control_sender,
            bug_report_dir,
            vk,
        })
    }

    pub fn update_display_params(&mut self, display_params: DisplayParams) -> anyhow::Result<()> {
        if self.defunct {
            return Err(anyhow!("session defunct"));
        }

        match self
            .control_sender
            .send(ControlMessage::UpdateDisplayParams(display_params))
        {
            Ok(_) => {
                self.display_params = display_params;
                Ok(())
            }
            Err(crossbeam::SendError(_)) => {
                self.defunct = true;
                Err(anyhow!("compositor died"))
            }
        }
    }

    pub fn attach(
        &mut self,
        id: u64,
        operator: bool,
        video_params: VideoStreamParams,
        audio_params: AudioStreamParams,
    ) -> anyhow::Result<Attachment> {
        if self.defunct {
            return Err(anyhow!("session defunct"));
        } else if !operator {
            unimplemented!()
        } else if self.operator_attachment_id.is_some() {
            return Err(anyhow!("session already has an operator"));
        }

        info!(
            session_id = self.id,
            attachment_id = id,
            operator,
            "new attachment"
        );

        let (events_send, events_recv) = crossbeam_channel::unbounded();
        let (ready_send, ready_recv) = oneshot::channel();
        if self
            .control_sender
            .send(ControlMessage::Attach {
                id,
                sender: events_send,
                video_params,
                audio_params,
                ready: ready_send,
            })
            .is_err()
        {
            self.defunct = true;
            bail!("compositor died");
        }

        if ready_recv.recv_timeout(ATTACH_TIMEOUT).is_err() {
            let _ = self.control_sender.send(ControlMessage::Detach(id));
            bail!("attachment rejected");
        }

        self.operator_attachment_id = Some(id);
        self.detached_since = None;

        Ok(Attachment {
            session_id: self.id,
            attachment_id: id,
            events: events_recv,
            control: self.control_sender.clone(),
        })
    }

    pub fn detach(&mut self, attachment: Attachment) -> anyhow::Result<()> {
        if self.defunct {
            return Err(anyhow!("session defunct"));
        }

        self.operator_attachment_id = None;
        self.detached_since = Some(time::Instant::now());
        match self
            .control_sender
            .send(ControlMessage::Detach(attachment.attachment_id))
        {
            Ok(_) => Ok(()),
            Err(crossbeam::SendError(_)) => {
                self.defunct = true;
                Err(anyhow!("compositor died"))
            }
        }
    }

    pub fn stop(self) -> anyhow::Result<()> {
        if let Err(crossbeam::TrySendError::Full(_)) =
            self.control_sender.try_send(ControlMessage::Stop)
        {
            bail!("compositor channel full");
        }

        match self.comp_thread_handle.join() {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(v) => Err(anyhow!("compositor thread panicked: {:?}", v)),
        }
    }

    pub fn supports_stream(&self, params: VideoStreamParams) -> bool {
        if params.width != self.display_params.width || params.height != self.display_params.height
        {
            return false;
        }

        probe_codec(self.vk.clone(), params.codec)
    }
}
