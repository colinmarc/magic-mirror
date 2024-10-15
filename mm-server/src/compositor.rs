// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

mod audio;
mod buffers;
mod child;
mod control;
mod dispatch;
mod handle;
mod input;
mod oneshot_render;
mod output;
mod protocols;
mod sealed;
mod seat;
mod serial;
mod shm;
mod stack;
mod surface;
mod video;
mod xwayland;

use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io::{BufRead, BufReader},
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    sync::Arc,
    time,
};

use anyhow::{bail, Context as _};
use child::*;
pub use control::*;
use crossbeam_channel as crossbeam;
pub use handle::*;
use hashbrown::HashMap;
pub use input::GamepadLayout;
use lazy_static::lazy_static;
use protocols::*;
pub use seat::{ButtonState, KeyState};
use slotmap::SlotMap;
use tracing::{debug, trace, trace_span};
use wayland_protocols::{
    wp::{
        linux_dmabuf::zv1::server::zwp_linux_dmabuf_v1,
        pointer_constraints::zv1::server::zwp_pointer_constraints_v1,
        presentation_time::server::{wp_presentation, wp_presentation_feedback},
        relative_pointer::zv1::server::zwp_relative_pointer_manager_v1,
        text_input::zv3::server::zwp_text_input_manager_v3,
    },
    xdg::shell::server::xdg_wm_base,
    xwayland::shell::v1::server::xwayland_shell_v1,
};
use wayland_server::{
    protocol::{self, wl_output, wl_shm},
    Resource as _,
};

use crate::{
    config::AppConfig,
    pixel_scale::PixelScale,
    vulkan::{VkContext, VkTimelinePoint},
    waking_sender::WakingSender,
};

lazy_static! {
    static ref EPOCH: std::time::Instant = std::time::Instant::now();
}

const READY_TIMEOUT: std::time::Duration = time::Duration::from_secs(10);

const DISPLAY: mio::Token = mio::Token(0);
const ACCEPT: mio::Token = mio::Token(1);
const CHILD: mio::Token = mio::Token(2);
const WAKER: mio::Token = mio::Token(3);
const TIMER: mio::Token = mio::Token(4);

const XDISPLAY: mio::Token = mio::Token(10);
const XWAYLAND: mio::Token = mio::Token(11);
const XWAYLAND_READY: mio::Token = mio::Token(12);

pub struct Compositor {
    poll: mio::Poll,
    waker: Arc<mio::Waker>,

    state: State,

    listening_socket: wayland_server::ListeningSocket,
    display: wayland_server::Display<State>,

    child: ChildHandle,
    child_debug_log: Option<File>,

    xwayland: Option<xwayland::XWayland>,
    xwayland_debug_log: Option<File>,

    ready_once: Option<oneshot::Sender<WakingSender<ControlMessage>>>,
    timer: mio_timerfd::TimerFd,
    sleeping: bool,
}

pub struct State {
    serial: serial::Serial,

    surfaces: SlotMap<surface::SurfaceKey, surface::Surface>,
    buffers: SlotMap<buffers::BufferKey, buffers::Buffer>,
    shm_pools: SlotMap<shm::ShmPoolKey, shm::ShmPool>,
    cached_dmabuf_feedback: buffers::CachedDmabufFeedback,
    pending_presentation_feedback: Vec<(
        wp_presentation_feedback::WpPresentationFeedback,
        VkTimelinePoint,
    )>,

    surface_stack: Vec<surface::SurfaceKey>,
    active_surface: Option<surface::SurfaceKey>,

    output_proxies: Vec<wl_output::WlOutput>,

    default_seat: seat::Seat,
    input_manager: input::InputDeviceManager,
    gamepads: HashMap<u64, input::GamepadHandle>,

    app_config: AppConfig,
    display_params: DisplayParams,
    new_display_params: Option<DisplayParams>,
    ui_scale: PixelScale, // Can differ from display_params if we force 1x scale.

    handle: CompositorHandle,
    pending_attachments: Vec<ControlMessage>,

    video_pipeline: Option<video::EncodePipeline>,
    new_video_stream_params: Option<VideoStreamParams>,
    video_stream_seq: u64,

    audio_pipeline: audio::EncodePipeline,

    xwm: Option<xwayland::Xwm>,
    xwayland_surface_lookup: HashMap<u64, surface::SurfaceKey>,

    // At the bottom for drop order.
    vk: Arc<VkContext>,
}

impl State {
    fn is_active(&self) -> bool {
        self.handle.num_attachments() > 0 || !self.pending_attachments.is_empty()
    }

    fn effective_display_params(&self) -> DisplayParams {
        let mut params = self.display_params;
        params.ui_scale = self.ui_scale;
        params
    }
}

#[derive(Debug, Default)]
pub struct ClientState {
    xwayland: bool,
}

impl wayland_server::backend::ClientData for ClientState {
    fn initialized(&self, _client_id: wayland_server::backend::ClientId) {}
    fn disconnected(
        &self,
        _client_id: wayland_server::backend::ClientId,
        _reason: wayland_server::backend::DisconnectReason,
    ) {
    }
}

impl Compositor {
    pub fn run(
        vk: Arc<VkContext>,
        app_config: AppConfig,
        display_params: DisplayParams,
        permanent_gamepads: Vec<(u64, GamepadLayout)>,
        bug_report_dir: Option<PathBuf>,
        ready_send: oneshot::Sender<WakingSender<ControlMessage>>,
    ) -> anyhow::Result<()> {
        let mut display = wayland_server::Display::new().context("failed to create display")?;

        let ui_scale = if app_config.force_1x_scale {
            PixelScale::ONE
        } else {
            display_params.ui_scale
        };

        trace!(
            %ui_scale,
            width = display_params.width,
            height = display_params.height,
            "configuring virtual display"
        );

        // Create wayland globals.
        let dh = display.handle();
        create_global::<protocol::wl_compositor::WlCompositor>(&dh, 6);
        create_global::<protocol::wl_output::WlOutput>(&dh, 4);
        create_global::<xdg_wm_base::XdgWmBase>(&dh, 6);

        create_global::<protocol::wl_seat::WlSeat>(&dh, 9);
        create_global::<zwp_pointer_constraints_v1::ZwpPointerConstraintsV1>(&dh, 1);
        create_global::<zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1>(&dh, 1);
        create_global::<zwp_text_input_manager_v3::ZwpTextInputManagerV3>(&dh, 1);

        create_global::<wl_shm::WlShm>(&dh, 1);
        create_global::<zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1>(&dh, 5);
        create_global::<wp_presentation::WpPresentation>(&dh, 1);

        create_global::<xwayland_shell_v1::XwaylandShellV1>(&dh, 1);
        create_global::<wl_drm::WlDrm>(&dh, 2);

        let mut container = Container::new(app_config.clone()).context("initializing container")?;

        let poll = mio::Poll::new()?;
        let waker = Arc::new(mio::Waker::new(poll.registry(), WAKER)?);
        let handle = CompositorHandle::new(waker.clone());

        let audio_pipeline =
            audio::EncodePipeline::new(handle.clone(), container.extern_run_path())?;

        let cached_dmabuf_feedback = buffers::CachedDmabufFeedback::new(vk.clone())?;

        // Set up input emulation (this is just for gamepads).
        let mut input_manager = input::InputDeviceManager::new(&mut container)?;
        let mut gamepads = HashMap::new();

        for (pad_id, layout) in permanent_gamepads {
            let dev = input_manager.plug_gamepad(pad_id, layout, true)?;
            gamepads.insert(pad_id, dev);
        }

        let state = State {
            serial: serial::Serial::new(),

            surfaces: SlotMap::default(),
            buffers: SlotMap::default(),
            shm_pools: SlotMap::default(),
            cached_dmabuf_feedback,
            pending_presentation_feedback: Vec::new(),

            surface_stack: Vec::new(),
            active_surface: None,

            output_proxies: Vec::new(),

            default_seat: seat::Seat::default(),
            input_manager,
            gamepads,

            app_config: app_config.clone(),
            display_params,
            ui_scale,
            new_display_params: None,
            handle,
            pending_attachments: Vec::new(),

            video_pipeline: None,
            new_video_stream_params: None,
            video_stream_seq: 1,

            audio_pipeline,

            xwm: None,
            xwayland_surface_lookup: HashMap::default(),

            vk,
        };

        let display_fd = display.backend().poll_fd().as_raw_fd();
        poll.registry().register(
            &mut mio::unix::SourceFd(&display_fd),
            DISPLAY,
            mio::Interest::READABLE,
        )?;

        // Bind the listening socket.
        let socket_name = gen_socket_name();
        let socket_path = container.extern_run_path().join(&socket_name);
        let listening_socket = wayland_server::ListeningSocket::bind_absolute(socket_path.clone())?;
        trace!(?socket_path, "bound wayland socket");

        let listener_fd = listening_socket.as_raw_fd();
        poll.registry().register(
            &mut mio::unix::SourceFd(&listener_fd),
            ACCEPT,
            mio::Interest::READABLE,
        )?;

        // Spawn Xwayland, if we're using it.
        let (xwayland, xwayland_debug_log) = if app_config.xwayland {
            let xwayland_debug_log = if let Some(bug_report_dir) = bug_report_dir.as_ref() {
                let path = bug_report_dir.join("xwayland.log");
                Some(std::fs::File::create(path).context("failed to create xwayland logfile")?)
            } else {
                None
            };

            let mut xwayland = xwayland::XWayland::spawn(
                &mut display.handle(),
                container.extern_run_path().as_os_str(),
            )
            .context("spawning Xwayland")?;

            // Xwayland writes to this pipe when it's ready.
            poll.registry().register(
                &mut xwayland.displayfd_recv,
                XWAYLAND_READY,
                mio::Interest::READABLE,
            )?;

            // Stderr/stdout of the xwayland process.
            poll.registry().register(
                xwayland.output.get_mut(),
                XWAYLAND,
                mio::Interest::READABLE,
            )?;

            (Some(xwayland), xwayland_debug_log)
        } else {
            (None, None)
        };

        // Spawn the client with a pipe as stdout/stderr.
        let (pipe_send, mut pipe_recv) = mio::unix::pipe::new()?;
        container.set_stdout(&pipe_send)?;
        container.set_stderr(&pipe_send)?;
        drop(pipe_send);

        // Set the wayland socket and X11 sockets. The wayland socket is a
        // relative path inside XDG_RUNTIME_DIR. The X11 socket is special
        // and has to be in /tmp (even if we set DISPLAY to an absolute path,
        // that path has to be rooted in /tmp).
        container.set_env("WAYLAND_DISPLAY", &socket_name);
        if let Some(xwayland) = &xwayland {
            container.bind_mount(&xwayland.display_socket, xwayland::SOCKET_PATH);
            container.set_env("DISPLAY", xwayland::SOCKET_PATH);
        }

        // Shadow pipewire, just in case.
        container.set_env("PIPEWIRE_REMOTE", "(null)");

        let child = container
            .spawn()
            .context("starting application container")?;

        poll.registry().register(
            &mut mio::unix::SourceFd(&child.pidfd().as_raw_fd()),
            CHILD,
            mio::Interest::READABLE,
        )?;

        poll.registry()
            .register(&mut pipe_recv, CHILD, mio::Interest::READABLE)?;

        // Use `glxinfo` and `eglinfo` to generate more debugging help.
        if let Some(bug_report_dir) = bug_report_dir.as_ref() {
            let p = bug_report_dir.to_owned();
            let wayland_socket = socket_name.clone();
            let x11_socket = xwayland.as_ref().map(|x| x.display_socket.clone());
            std::thread::spawn(move || {
                save_glxinfo_eglinfo(
                    &p,
                    &wayland_socket,
                    x11_socket.as_ref().map(|p| p.as_os_str()),
                );
            });
        }

        // If bug report mode is enabled, save the stdout/stderr of the child to
        // a logfile.
        let child_debug_log = if let Some(bug_report_dir) = bug_report_dir.as_ref() {
            let path = bug_report_dir.join(format!("child-{}.log", child.pid().as_raw_nonzero()));
            Some(std::fs::File::create(path).context("failed to create child logfile")?)
        } else {
            None
        };

        // Framerate timer (simulates vblank).
        let mut timer = mio_timerfd::TimerFd::new(mio_timerfd::ClockId::Monotonic)?;

        poll.registry()
            .register(&mut timer, TIMER, mio::Interest::READABLE)?;

        let mut compositor = Self {
            poll,
            waker,

            display,
            state,
            listening_socket,

            child,
            child_debug_log,

            xwayland,
            xwayland_debug_log,

            ready_once: Some(ready_send),
            timer,
            sleeping: false,
        };

        compositor.main_loop(pipe_recv)
    }

    fn main_loop(
        &mut self,
        mut child_pipe: mio::unix::pipe::Receiver,
    ) -> Result<(), anyhow::Error> {
        let mut events = mio::Events::with_capacity(64);

        let (control_send, control_recv) = crossbeam::unbounded();
        let control_send = WakingSender::new(self.waker.clone(), control_send);

        let start = time::Instant::now();
        let mut child_output = BufReader::new(&mut child_pipe);

        loop {
            trace_span!("poll").in_scope(|| self.poll.poll(&mut events, None))?;

            for event in events.iter() {
                match event.token() {
                    ACCEPT => {
                        if let Some(client_stream) = self.listening_socket.accept()? {
                            let _client = self
                                .display
                                .handle()
                                .insert_client(client_stream, Arc::new(ClientState::default()))?;

                            debug!("client app connected");
                        }
                    }
                    CHILD if event.is_read_closed() => {
                        self.child.wait()?;
                        self.state.handle.kick_clients();

                        if self.ready_once.is_some() {
                            // The client exited immediately, which is an error.
                            bail!("client exited without doing anything");
                        } else {
                            return Ok(());
                        }
                    }
                    CHILD if event.is_readable() => {
                        dump_child_output(&mut child_output, &mut self.child_debug_log)
                    }
                    WAKER => loop {
                        match control_recv.try_recv() {
                            Ok(ControlMessage::Stop) => {
                                self.state.handle.kick_clients();
                                trace!("shutting down");

                                // Usually, TERM doesn't work, because the
                                // process is PID 1 in the container.
                                self.child.signal(rustix::process::Signal::Kill)?;
                            }
                            Ok(msg) => self.handle_control_message(msg)?,
                            Err(crossbeam::TryRecvError::Empty) => break,
                            Err(crossbeam::TryRecvError::Disconnected) => {
                                panic!("control channel disconnected")
                            }
                        }
                    },
                    DISPLAY => {
                        trace!("dispatching display");
                        self.display
                            .dispatch_clients(&mut self.state)
                            .context("failed to dispatch the wayland display")?;
                    }
                    XDISPLAY => {
                        trace!("dispatching xwm");
                        self.state
                            .dispatch_xwm()
                            .context("failed to dispatch the xwm")?;
                    }
                    XWAYLAND_READY => {
                        let xwayland = self.xwayland.as_mut().unwrap();
                        if let Some(socket) = xwayland.is_ready()? {
                            self.poll
                                .registry()
                                .deregister(&mut xwayland.displayfd_recv)?;
                            debug!("starting xwm");

                            // The XWM connection to the Xwayland server.
                            let xwm = xwayland::Xwm::new(socket)?;
                            self.poll.registry().register(
                                &mut mio::unix::SourceFd(&xwm.display_fd().as_raw_fd()),
                                XDISPLAY,
                                mio::Interest::READABLE,
                            )?;

                            self.state.xwm = Some(xwm);
                        }
                    }
                    XWAYLAND if event.is_read_closed() => {
                        let exit = self.xwayland.as_mut().unwrap().child.wait()?;
                        bail!(
                            "Xwayland exited unexpectedly with status {}",
                            exit.code().unwrap_or_default()
                        );
                    }
                    XWAYLAND if event.is_readable() => {
                        dump_child_output(
                            &mut self.xwayland.as_mut().unwrap().output,
                            &mut self.xwayland_debug_log,
                        );
                    }
                    TIMER => {
                        self.timer.read()?;

                        // Check if we need to resize the virtual display.
                        if let Some(new_params) = self.state.new_display_params.take() {
                            self.update_display_params(new_params)?;

                            // Update the render timer to match the new framerate.
                            self.timer
                                .set_timeout_interval(&time::Duration::from_secs_f64(
                                    1.0 / self.state.display_params.framerate as f64,
                                ))?;
                        }

                        self.frame()?;
                    }
                    _ => unreachable!(),
                }
            }

            self.idle()?;

            // Check that we haven't timed out waiting for the client to start up.
            if self.ready_once.is_some() && self.state.surfaces_ready() {
                self.ready_once.take().unwrap().send(control_send.clone())?;
            } else if self.ready_once.is_some() && start.elapsed() > READY_TIMEOUT {
                self.child.signal(rustix::process::Signal::Kill)?;
                bail!("timed out waiting for client");
            }

            // Sleep if we're not active.
            if !self.sleeping && !self.state.is_active() {
                self.sleeping = true;
                self.timer
                    .set_timeout_interval(&time::Duration::from_secs(1))?;
            } else if self.sleeping && self.state.is_active() {
                self.sleeping = false;
                self.timer
                    .set_timeout_interval(&time::Duration::from_secs_f64(
                        1.0 / self.state.display_params.framerate as f64,
                    ))?;
            }
        }
    }

    fn idle(&mut self) -> anyhow::Result<()> {
        // Accept any waiting clients, but only if we're not mid-resize.
        if !self.state.pending_attachments.is_empty()
            && self.state.new_display_params.is_none()
            && self.state.surfaces_ready()
        {
            let pending_attachments = self.state.pending_attachments.drain(..).collect::<Vec<_>>();
            for attach_msg in pending_attachments {
                if let ControlMessage::Attach {
                    id,
                    sender,
                    video_params,
                    audio_params,
                    ready,
                } = attach_msg
                {
                    // Check if the caller is still waiting.
                    if ready.send(()).is_ok() {
                        self.attach(id, sender, video_params, audio_params)?;
                    }
                } else {
                    unreachable!()
                }
            }
        }

        // Update the window stack, if it changed.
        self.state
            .update_focus_and_visibility(self.state.is_active())?;

        // Send any pending surface configures.
        self.state.configure_surfaces()?;

        // Check if the pointer is locked.
        self.state.update_pointer_lock();

        // Send pending pointer frames.
        self.state.default_seat.pointer_frame();

        // Send pending controller SYN_REPORT events.
        for (_, dev) in self.state.gamepads.iter_mut() {
            dev.frame()
        }

        // Release any unused buffers.
        self.state.release_buffers()?;

        // Send presentation feedback.
        self.state.send_presentation_feedback()?;

        // Flush events to the app.
        self.display.flush_clients()?;

        Ok(())
    }

    fn update_display_params(&mut self, params: DisplayParams) -> anyhow::Result<()> {
        let now = EPOCH.elapsed().as_millis() as u32;
        let old = self.state.display_params;

        let old_ui_scale = self.state.ui_scale;
        let new_ui_scale = if self.state.app_config.force_1x_scale {
            PixelScale::ONE
        } else {
            params.ui_scale
        };

        let size_changed = old.width != params.width || old.height != params.height;
        let scale_changed = old_ui_scale != new_ui_scale;
        let framerate_changed = old.framerate != params.framerate;

        if size_changed || scale_changed || framerate_changed {
            debug!(
                old_width = old.width,
                new_width = params.width,
                old_height = old.height,
                new_height = params.height,
                old_framerate = old.framerate,
                new_framerate = params.framerate,
                old_ui_scale = %old_ui_scale,
                new_ui_scale = %new_ui_scale,
                "resizing output",
            );

            // If the size or framerate is different, force the client to reattach.
            // TODO: if we support multiple attachments, or attachments that
            // differ in resolution from the render res, we need to check for
            // that here. For now, it's safe to just kill the attachment streams.
            let force_reattach = size_changed || framerate_changed;

            // Reconfigure all surfaces to be the right size.
            for surface in &self.state.surface_stack {
                let surf = &mut self.state.surfaces[*surface];

                let xwin = surf.role.current.as_ref().and_then(|role| {
                    if let surface::SurfaceRole::XWayland { serial } = role {
                        self.state.xwm.as_ref().unwrap().xwindow_for_serial(*serial)
                    } else {
                        None
                    }
                });

                surf.reconfigure(
                    DisplayParams {
                        ui_scale: new_ui_scale,
                        ..params
                    },
                    xwin,
                );

                if force_reattach {
                    // Try to trick the surface into thinking it's moving to a
                    // different monitor. This helps some games adjust to mode
                    // changes.
                    for wl_output in &self.state.output_proxies {
                        if wl_output.client() == surf.wl_surface.client() {
                            surf.wl_surface.leave(wl_output);
                            surf.wl_surface.enter(wl_output);
                        }
                    }

                    // Discharge any pending frame callbacks, since we won't
                    // render the current content, and some clients get stuck
                    // otherwise.
                    surf.content = None;
                    if let Some(cb) = surf.frame_callback.current.take() {
                        cb.done(now);
                    }
                }
            }

            self.state.display_params = params;
            self.state.ui_scale = new_ui_scale;
            self.state.emit_output_params();

            self.state
                .handle
                .dispatch(CompositorEvent::DisplayParamsChanged {
                    params,
                    reattach: force_reattach,
                });

            if force_reattach {
                // Clear any pending attachments which don't match the new output.
                self.state.pending_attachments.retain(|pending| {
                    let ControlMessage::Attach {
                        video_params: VideoStreamParams { width, height, .. },
                        ..
                    } = pending
                    else {
                        unreachable!()
                    };

                    *width == params.width && *height == params.height
                });

                // Clear any current attachments.
                self.state.handle.remove_all();
                self.state.audio_pipeline.stop_stream();

                self.state.video_pipeline = None;
                self.state.new_video_stream_params = None;

                // Configure all windows to be suspended.
                self.state.update_focus_and_visibility(false)?;
            } else {
                // Reconfigure for the new scale.
                self.state.update_focus_and_visibility(true)?;
            }
        } else if params.ui_scale != old.ui_scale {
            // Synthesize a param change if we are forcing 1x scale.
            self.state
                .handle
                .dispatch(CompositorEvent::DisplayParamsChanged {
                    params,
                    reattach: false,
                });
        }

        Ok(())
    }

    fn frame(&mut self) -> anyhow::Result<()> {
        let _tracy_frame = tracy_client::non_continuous_frame!("composite");

        if self.state.handle.num_attachments() == 0 {
            return Ok(());
        }

        if self.state.surface_stack.is_empty() {
            return Ok(());
        }

        if let Some(params) = self.state.new_video_stream_params.take() {
            self.state.video_pipeline = Some(video::EncodePipeline::new(
                self.state.vk.clone(),
                self.state.video_stream_seq,
                self.state.handle.clone(),
                self.state.display_params,
                params,
            )?);

            self.state.video_stream_seq += 1;
        } else if self.state.video_pipeline.is_none() {
            return Ok(());
        }

        let now = EPOCH.elapsed().as_millis() as u32;

        let video_pipeline = self.state.video_pipeline.as_mut().unwrap();
        unsafe { video_pipeline.begin()? };

        // Iterate backwards to find the first fullscreen window.
        let first_visible_idx = self
            .state
            .surface_stack
            .iter()
            .rposition(|id| {
                self.state.surfaces[*id]
                    .configuration
                    .map_or(true, |conf| conf.fullscreen)
            })
            .unwrap_or_default();

        let mut presentation_feedback =
            Vec::with_capacity(self.state.surface_stack.len() - first_visible_idx);
        for id in self.state.surface_stack[first_visible_idx..].iter() {
            let surface = &mut self.state.surfaces[*id];

            let conf = surface
                .configuration
                .expect("mapped surface has no configuration");

            let content = surface
                .content
                .as_mut()
                .expect("mapped surface has no content");

            let buffer = &mut self.state.buffers[content.buffer];

            let sync = match &mut buffer.backing {
                buffers::BufferBacking::Dmabuf {
                    fd,
                    interop_sema,
                    interop_sema_tripped,
                    ..
                } if !*interop_sema_tripped => {
                    // Grab a semaphore for explicit sync interop.
                    buffers::import_dmabuf_fence_as_semaphore(
                        self.state.vk.clone(),
                        *interop_sema,
                        fd,
                    )?;

                    // Make sure we only wait for the semaphore once per commit.
                    *interop_sema_tripped = true;
                    Some(video::TextureSync::BinaryAcquire(*interop_sema))
                }
                _ => None,
            };

            unsafe { buffer.release_wait = video_pipeline.composite_surface(buffer, sync, conf)? };
            if let Some(callback) = surface.frame_callback.current.take().as_mut() {
                callback.done(now);
            }

            if let Some(fb) = content.wp_presentation_feedback.take() {
                presentation_feedback.push(fb);
            }

            trace!(?surface, ?conf, "compositing surface");
        }

        let tp_render = unsafe { video_pipeline.end_and_submit()? };
        for fb in presentation_feedback.drain(..) {
            self.state
                .pending_presentation_feedback
                .push((fb, tp_render.clone()));
        }

        // Render the cursor, if needed.
        self.state.render_cursor()?;

        Ok(())
    }

    fn attach(
        &mut self,
        id: u64,
        sender: crossbeam::Sender<CompositorEvent>,
        video_params: VideoStreamParams,
        audio_params: AudioStreamParams,
    ) -> anyhow::Result<()> {
        if self.state.handle.num_attachments() > 0 {
            unimplemented!();
        }

        self.state.handle.insert_client(id, sender);
        self.state.new_video_stream_params = Some(video_params);
        self.state.audio_pipeline.restart_stream(audio_params)?;
        self.state.update_focus_and_visibility(true)?;

        self.state.dispatch_cursor();
        if let Some(coords) = self.state.default_seat.pointer_locked() {
            let (x, y) = coords.into();
            self.state
                .handle
                .dispatch(CompositorEvent::PointerLocked(x, y));
        }

        Ok(())
    }

    fn handle_control_message(&mut self, msg: ControlMessage) -> anyhow::Result<()> {
        trace!(?msg, "control message");

        // Attachments get handled asynchronously.
        if matches!(msg, ControlMessage::Attach { .. }) {
            self.state.pending_attachments.push(msg);
            return Ok(());
        }

        match msg {
            ControlMessage::Detach(id) => {
                self.state.handle.remove_client(id);
                self.state.pending_attachments.retain(|msg| {
                    let ControlMessage::Attach { id: pending_id, .. } = msg else {
                        unreachable!();
                    };

                    *pending_id != id
                });

                if !self.state.is_active() {
                    self.state.audio_pipeline.stop_stream();
                    self.state.video_pipeline = None;
                    self.state.update_focus_and_visibility(false)?;
                }
            }
            ControlMessage::UpdateDisplayParams(params) => {
                // Updates once per render.
                self.state.new_display_params = Some(params);
            }
            ControlMessage::KeyboardInput {
                key_code,
                char,
                state,
            } => {
                trace!(key_code, ?char, ?state, "keyboard input");

                // Attempt to send the char via text-input, then fall back to
                // sending the keypress.
                match char {
                    Some(c) if self.state.default_seat.has_text_input() => {
                        if matches!(state, KeyState::Pressed | KeyState::Repeat) {
                            self.state
                                .default_seat
                                .text_input_char(&self.state.serial, c);
                        }
                    }
                    _ => {
                        let mut state = state;

                        // Simulate a press and release on repeat.
                        if state == KeyState::Repeat {
                            self.state.default_seat.keyboard_input(
                                &self.state.serial,
                                key_code,
                                KeyState::Released,
                            );

                            state = KeyState::Pressed
                        }

                        self.state
                            .default_seat
                            .keyboard_input(&self.state.serial, key_code, state);
                    }
                }
            }
            ControlMessage::PointerInput {
                x,
                y,
                button_code,
                state,
            } => {
                if let Some((id, surface_coords)) = self.state.surface_under((x, y)) {
                    let wl_surface = self.state.surfaces[id].wl_surface.clone();

                    self.state.default_seat.pointer_input(
                        &self.state.serial,
                        wl_surface,
                        surface_coords,
                        (x, y),
                        button_code,
                        state,
                    );
                } else {
                    self.state.default_seat.lift_pointer(&self.state.serial);
                }
            }

            ControlMessage::PointerMotion(x, y) => {
                if let Some((id, surface_coords)) = self.state.surface_under((x, y)) {
                    let wl_surface = self.state.surfaces[id].wl_surface.clone();

                    self.state.default_seat.update_pointer(
                        &self.state.serial,
                        wl_surface,
                        surface_coords,
                        (x, y),
                    );
                } else {
                    self.state.default_seat.lift_pointer(&self.state.serial);
                }
            }
            ControlMessage::RelativePointerMotion(x, y) => {
                let scale = self
                    .state
                    .default_seat
                    .pointer_focus()
                    .and_then(|wl_surface| wl_surface.data().copied())
                    .and_then(|id| self.state.surfaces.get(id))
                    .map(|surf| surf.effective_scale())
                    .unwrap_or_default();

                let vector = surface::buffer_vector_to_surface((x, y), scale);
                self.state.default_seat.relative_pointer_motion(vector);
            }
            ControlMessage::PointerAxis(x, y) => {
                let scale = self
                    .state
                    .default_seat
                    .pointer_focus()
                    .and_then(|wl_surface| wl_surface.data().copied())
                    .and_then(|id| self.state.surfaces.get(id))
                    .map(|surf| surf.effective_scale())
                    .unwrap_or_default();

                // Note that the protocol and wayland use inverted vectors.
                let vector = surface::buffer_vector_to_surface((-x, -y), scale);
                self.state.default_seat.pointer_axis(vector);
            }
            ControlMessage::PointerAxisDiscrete(x, y) => {
                self.state.default_seat.pointer_axis_discrete((-x, -y));
            }
            ControlMessage::PointerEntered => {
                // Nothing to do - we update focus when the pointer moves.
            }
            ControlMessage::PointerLeft => {
                self.state.default_seat.lift_pointer(&self.state.serial);
            }
            ControlMessage::GamepadAvailable(id) => {
                if !self.state.gamepads.contains_key(&id) {
                    self.state.gamepads.insert(
                        id,
                        self.state.input_manager.plug_gamepad(
                            id,
                            input::GamepadLayout::GenericDualStick,
                            false,
                        )?,
                    );
                }
            }
            ControlMessage::GamepadUnavailable(id) => {
                use hashbrown::hash_map::Entry;
                match self.state.gamepads.entry(id) {
                    Entry::Occupied(v) if !v.get().permanent => {
                        v.remove();
                    }
                    _ => (),
                }
            }
            ControlMessage::GamepadAxis {
                id,
                axis_code,
                value,
            } => {
                if let Some(gamepad) = self.state.gamepads.get_mut(&id) {
                    gamepad.axis(axis_code, value);
                }
            }
            ControlMessage::GamepadTrigger {
                id,
                trigger_code,
                value,
            } => {
                if let Some(gamepad) = self.state.gamepads.get_mut(&id) {
                    gamepad.trigger(trigger_code, value);
                }
            }
            ControlMessage::GamepadInput {
                id,
                button_code,
                state,
            } => {
                if let Some(gamepad) = self.state.gamepads.get_mut(&id) {
                    gamepad.input(button_code, state);
                }
            }
            // Handled above.
            ControlMessage::Stop | ControlMessage::Attach { .. } => unreachable!(),
        }

        Ok(())
    }
}

fn create_global<G: wayland_server::Resource + 'static>(
    dh: &wayland_server::DisplayHandle,
    version: u32,
) where
    State: wayland_server::GlobalDispatch<G, ()>,
{
    let _ = dh.create_global::<State, G, ()>(version, ());
}

fn gen_socket_name() -> OsString {
    use rand::Rng;
    let id: u64 = rand::thread_rng().gen();
    format!("magic-mirror-{}", id).into()
}

fn dump_child_output(pipe: &mut impl BufRead, debug_log: &mut Option<std::fs::File>) {
    let mut buf = String::new();

    loop {
        buf.clear();
        match pipe.read_line(&mut buf) {
            Ok(_) => {
                if let Some(debug_log) = debug_log {
                    let _ = std::io::Write::write_all(debug_log, buf.as_bytes());
                }

                let buf = buf.trim();
                if !buf.is_empty() {
                    trace!(target: "mmserver::compositor::child", "{}", buf);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(e) => {
                debug!("child error: {:?}", e);
                break;
            }
        }
    }
}

fn save_glxinfo_eglinfo(
    bug_report_dir: impl AsRef<Path>,
    socket_name: &OsStr,
    x11_display: Option<&OsStr>,
) {
    use std::process::Command;

    if let Some(x11_display) = x11_display {
        match Command::new("glxinfo")
            .env_clear()
            .env("DISPLAY", x11_display)
            .output()
        {
            Ok(output) => {
                let _ = std::fs::write(bug_report_dir.as_ref().join("glxinfo.log"), output.stdout);
            }
            Err(e) => debug!("failed to run glxinfo: {:#}", e),
        }
    }

    match Command::new("eglinfo")
        .env_clear()
        .env("WAYLAND_DISPLAY", socket_name)
        .output()
    {
        Ok(output) => {
            let _ = std::fs::write(bug_report_dir.as_ref().join("eglinfo.log"), output.stdout);
        }
        Err(e) => debug!("failed to run eglinfo: {:#}", e),
    }
}
