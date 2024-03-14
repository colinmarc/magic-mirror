// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{
    sync::{Arc, Mutex},
    time,
};

use anyhow::anyhow;
use anyhow::Result;
use crossbeam_channel as crossbeam;
use lazy_static::lazy_static;
use pathsearch::find_executable_in_path;
use tracing::{debug_span, info};

use crate::vulkan::VkContext;
use crate::waking_sender::WakingSender;
use crate::{
    codec::probe_codec,
    compositor::{
        AppLaunchConfig, AudioStreamParams, Compositor, CompositorEvent, ControlMessage,
        DisplayParams, VideoStreamParams,
    },
};

pub struct Session {
    pub id: u64,
    pub display_params: DisplayParams,
    pub application_name: String,
    pub started: time::SystemTime,
    pub started_instant: time::Instant,
    pub detached_since: Option<time::Instant>,
    pub defunct: bool,

    comp_thread_handle: std::thread::JoinHandle<Result<()>>,
    control_sender: WakingSender<ControlMessage>,
    operator_attachment_id: Option<u64>,
    vk: Arc<VkContext>,
}

pub struct Attachment {
    pub attachment_id: u64,
    pub events: crossbeam::Receiver<CompositorEvent>,
    pub control: WakingSender<ControlMessage>,
}

impl Session {
    /// Launches a standalone compositor and the application process. Blocks
    /// until both have started up and connected over a unix socket.
    pub fn launch(
        vk: Arc<VkContext>,
        application_name: &str,
        application_config: &super::config::AppConfig,
        display_params: DisplayParams,
    ) -> Result<Self> {
        let id = generate_id();

        let mut args = application_config.command.clone();
        let exe = args.remove(0);
        let exe_path = find_executable_in_path(&exe)
            .ok_or(anyhow!("command {:?} not in PATH", &exe))?
            .into();

        let env = application_config.env.clone().into_iter().collect();

        let launch_config = AppLaunchConfig {
            exe_path,
            args,
            env,
            enable_xwayland: application_config.xwayland,
        };

        // Launch the compositor, which in turn launches the app.
        let (ready_send, ready_recv) = oneshot::channel();
        let vk_clone = vk.clone();
        let comp_thread_handle = std::thread::spawn(move || {
            tracy_client::set_thread_name!("Compositor");

            let span = debug_span!("session", session_id = id);
            let _guard = span.enter();

            let mut compositor = Compositor::new(vk_clone, launch_config, display_params)?;
            compositor.run(ready_send)
        });

        info!(session_id = id, application = ?application_name, "launching session");

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
            application_name: application_name.to_string(),
            display_params,
            started: time::SystemTime::now(),
            started_instant: time::Instant::now(),
            defunct: false,
            detached_since: None,
            operator_attachment_id: None,
            comp_thread_handle,
            control_sender,
            vk,
        })
    }

    pub fn update_display_params(&mut self, display_params: DisplayParams) -> Result<()> {
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
        operator: bool,
        video_params: VideoStreamParams,
        audio_params: AudioStreamParams,
    ) -> Result<Attachment> {
        if self.defunct {
            return Err(anyhow!("session defunct"));
        } else if !operator {
            unimplemented!()
        } else if self.operator_attachment_id.is_some() {
            return Err(anyhow!("session already has an operator"));
        }

        let id = generate_id();

        info!(
            session_id = self.id,
            attachment_id = id,
            operator,
            "new attachment"
        );

        let (events_send, events_recv) = crossbeam_channel::unbounded();
        match self.control_sender.send(ControlMessage::Attach {
            id,
            sender: events_send,
            video_params,
            audio_params,
        }) {
            Ok(_) => {}
            Err(crossbeam::SendError(_)) => {
                self.defunct = true;
                return Err(anyhow!("compositor died"));
            }
        }

        self.operator_attachment_id = Some(id);
        self.detached_since = None;
        Ok(Attachment {
            attachment_id: id,
            events: events_recv,
            control: self.control_sender.clone(),
        })
    }

    pub fn detach(&mut self, attachment: Attachment) -> Result<()> {
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

    pub fn stop(self) -> Result<()> {
        if let Err(crossbeam::TrySendError::Full(_)) =
            self.control_sender.try_send(ControlMessage::Stop)
        {
            return Err(anyhow!("compositor channel full"));
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

lazy_static! {
    static ref ID_GENERATOR: Mutex<tiny_id::ShortCodeGenerator<char>> =
        Mutex::new(tiny_id::ShortCodeGenerator::new_numeric(6));
}

fn generate_id() -> u64 {
    ID_GENERATOR.lock().unwrap().next_int()
}
