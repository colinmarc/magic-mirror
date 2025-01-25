use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs::File,
    io::{BufRead, BufReader},
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    sync::Arc,
    time,
};

use anyhow::{bail, Context as _};
use crossbeam_channel as crossbeam;
use lazy_static::lazy_static;
use tracing::{debug, trace, trace_span};

use super::{
    audio,
    compositor::{self, xwayland, Compositor},
    control::{AudioStreamParams, ControlMessage, DisplayParams, SessionEvent, VideoStreamParams},
    input, video, GamepadLayout, SessionHandle,
};
use crate::{
    config::AppConfig,
    container::{Container, ContainerHandle},
    pixel_scale::PixelScale,
    vulkan::VkContext,
    waking_sender::WakingSender,
};

lazy_static! {
    pub static ref EPOCH: std::time::Instant = std::time::Instant::now();
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

pub struct Reactor {
    poll: mio::Poll,
    waker: Arc<mio::Waker>,

    compositor: Compositor,
    session_handle: SessionHandle,

    listening_socket: wayland_server::ListeningSocket,
    wayland_display: wayland_server::Display<Compositor>,

    app_config: AppConfig,
    child: ContainerHandle,
    child_debug_log: Option<File>,

    display_params: DisplayParams,
    new_display_params: Option<DisplayParams>,

    audio_pipeline: audio::EncodePipeline,
    video_pipeline: Option<video::EncodePipeline>,
    new_video_stream_params: Option<VideoStreamParams>,
    video_stream_seq: u64,

    input_manager: input::InputDeviceManager,
    gamepads: BTreeMap<u64, input::GamepadHandle>,

    xwayland: Option<xwayland::XWayland>,
    xwayland_debug_log: Option<File>,

    pending_attachments: Vec<ControlMessage>,

    ready_once: Option<oneshot::Sender<WakingSender<ControlMessage>>>,
    timer: mio_timerfd::TimerFd,
    sleeping: bool,
    shutting_down: bool,

    vk: Arc<VkContext>,
}

impl Reactor {
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
        compositor::create_globals(&dh);

        let mut container = Container::new(
            app_config.command.clone(),
            app_config.home_isolation_mode.clone(),
        )
        .context("initializing container")?;

        for (k, v) in &app_config.env {
            container.set_env(k, v);
        }

        let poll = mio::Poll::new()?;
        let waker = Arc::new(mio::Waker::new(poll.registry(), WAKER)?);
        let handle = SessionHandle::new(waker.clone());

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

        // Set up the pulse audio server.
        let audio_pipeline =
            audio::EncodePipeline::new(handle.clone(), container.extern_run_path())?;

        // Set up compositor state.
        let compositor = compositor::Compositor::new(
            vk.clone(),
            handle.clone(),
            DisplayParams {
                ui_scale, // Overridden by force_1x_scale.
                ..display_params
            },
        )?;

        // Set up input emulation (this is just for gamepads).
        let mut input_manager = input::InputDeviceManager::new(&mut container)?;
        let mut gamepads = BTreeMap::new();

        for (pad_id, layout) in permanent_gamepads {
            let dev = input_manager.plug_gamepad(pad_id, layout, true)?;
            gamepads.insert(pad_id, dev);
        }

        // Spawn Xwayland, if we're using it.
        let (xwayland, xwayland_recv, xwayland_debug_log) = if app_config.xwayland {
            let mut xwayland_debug_log = if let Some(bug_report_dir) = bug_report_dir.as_ref() {
                let path = bug_report_dir.join("xwayland.log");
                Some(std::fs::File::create(path).context("failed to create xwayland logfile")?)
            } else {
                None
            };

            let (output_send, mut output_recv) = mio::unix::pipe::new()?;
            let mut xwayland = match xwayland::XWayland::spawn(
                &mut display.handle(),
                container.extern_run_path(),
                output_send,
            ) {
                Ok(xw) => xw,
                Err(e) => {
                    // Make sure we save any errors.
                    dump_child_output(
                        &mut BufReader::new(&mut output_recv),
                        &mut xwayland_debug_log,
                    );

                    return Err(e).context("spawning Xwayland");
                }
            };

            // Xwayland writes to this pipe when it's ready.
            poll.registry().register(
                &mut xwayland.displayfd_recv,
                XWAYLAND_READY,
                mio::Interest::READABLE,
            )?;

            // Stderr/stdout of the xwayland process.
            poll.registry()
                .register(&mut output_recv, XWAYLAND, mio::Interest::READABLE)?;

            (Some(xwayland), Some(output_recv), xwayland_debug_log)
        } else {
            (None, None, None)
        };

        // Spawn the client with a pipe as stdout/stderr.
        let (pipe_send, mut pipe_recv) = mio::unix::pipe::new()?;
        container.set_stdout(&pipe_send)?;
        container.set_stderr(&pipe_send)?;
        drop(pipe_send);

        // Set the wayland socket and X11 sockets. The wayland socket is a
        // relative path inside XDG_RUNTIME_DIR. The X11 socket is special
        // and has to be in a specific location for XCB to work on all systems.
        container.set_env("WAYLAND_DISPLAY", &socket_name);
        if let Some(xwayland) = &xwayland {
            xwayland.prepare_socket(&mut container);
        }

        // Shadow pipewire, just in case.
        container.set_env("PIPEWIRE_REMOTE", "(null)");

        let child = match container.spawn() {
            Ok(ch) => ch,
            Err(e) => {
                // Make sure we pump the child stdio and catch any container-related
                // error.
                let mut debug_log = bug_report_dir
                    .as_ref()
                    .and_then(|dir| std::fs::File::create(dir.join("child.log")).ok());
                let mut child_output = BufReader::new(&mut pipe_recv);
                dump_child_output(&mut child_output, &mut debug_log);
                return Err(e).context("starting application container");
            }
        };

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

        let mut reactor = Self {
            poll,
            waker,

            wayland_display: display,
            compositor,

            session_handle: handle,
            listening_socket,

            app_config,
            child,
            child_debug_log,

            display_params,
            new_display_params: None,

            audio_pipeline,
            video_pipeline: None,
            new_video_stream_params: None,
            video_stream_seq: 0,

            input_manager,
            gamepads,

            pending_attachments: Vec::new(),

            xwayland,
            xwayland_debug_log,

            ready_once: Some(ready_send),
            timer,
            sleeping: false,
            shutting_down: false,

            vk,
        };

        reactor.main_loop(pipe_recv, xwayland_recv)
    }

    fn main_loop(
        &mut self,
        mut child_pipe: mio::unix::pipe::Receiver,
        mut xwayland_pipe: Option<mio::unix::pipe::Receiver>,
    ) -> Result<(), anyhow::Error> {
        let mut events = mio::Events::with_capacity(64);

        let (control_send, control_recv) = crossbeam::unbounded();
        let control_send = WakingSender::new(self.waker.clone(), control_send);

        let start = time::Instant::now();
        let mut child_output = BufReader::new(&mut child_pipe);
        let mut xwayland_output = xwayland_pipe.as_mut().map(BufReader::new);

        loop {
            trace_span!("poll").in_scope(|| self.poll.poll(&mut events, None))?;

            for event in events.iter() {
                match event.token() {
                    ACCEPT => {
                        if let Some(client_stream) = self.listening_socket.accept()? {
                            let _client = self.wayland_display.handle().insert_client(
                                client_stream,
                                Arc::new(compositor::ClientState::default()),
                            )?;

                            debug!("client app connected");
                        }
                    }
                    CHILD if event.is_read_closed() => {
                        self.child.wait()?;
                        self.session_handle.kick_clients();

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
                                self.session_handle.kick_clients();
                                self.shutting_down = true;
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
                        self.wayland_display
                            .dispatch_clients(&mut self.compositor)
                            .context("failed to dispatch the wayland display")?;
                    }
                    XDISPLAY => {
                        trace!("dispatching xwm");
                        self.compositor
                            .dispatch_xwm()
                            .context("failed to dispatch the xwm")?;
                    }
                    XWAYLAND_READY => {
                        let xwayland = self.xwayland.as_mut().unwrap();
                        if let Some(socket) = xwayland.is_ready()? {
                            self.poll
                                .registry()
                                .deregister(&mut xwayland.displayfd_recv)?;

                            // Setup the XWM connection to the Xwayland server.
                            let fd = self.compositor.insert_xwayland(socket)?;
                            self.poll.registry().register(
                                &mut mio::unix::SourceFd(&fd.as_raw_fd()),
                                XDISPLAY,
                                mio::Interest::READABLE,
                            )?;
                        }
                    }
                    XWAYLAND if event.is_read_closed() => {
                        self.xwayland.as_mut().unwrap().child.wait()?;
                    }
                    XWAYLAND if event.is_readable() => {
                        dump_child_output(
                            xwayland_output.as_mut().unwrap(),
                            &mut self.xwayland_debug_log,
                        );
                    }
                    TIMER => {
                        self.timer.read()?;

                        // Check if we need to resize the virtual display.
                        if let Some(new_params) = self.new_display_params.take() {
                            self.update_display_params(new_params)?;

                            // Update the render timer to match the new framerate.
                            self.timer
                                .set_timeout_interval(&time::Duration::from_secs_f64(
                                    1.0 / self.display_params.framerate as f64,
                                ))?;
                        }

                        self.frame()?;
                    }
                    _ => unreachable!(),
                }
            }

            if !self.shutting_down {
                self.idle()?;
            }

            // Check that we haven't timed out waiting for the client to start up.
            if self.ready_once.is_some() && self.compositor.surfaces_ready() {
                self.ready_once.take().unwrap().send(control_send.clone())?;
            } else if self.ready_once.is_some() && start.elapsed() > READY_TIMEOUT {
                self.child.signal(rustix::process::Signal::Kill)?;
                bail!("timed out waiting for client");
            }

            // Sleep if we're not active.
            if !self.sleeping && !self.active() {
                self.sleeping = true;
                self.timer
                    .set_timeout_interval(&time::Duration::from_secs(1))?;
            } else if self.sleeping && self.active() {
                self.sleeping = false;
                self.timer
                    .set_timeout_interval(&time::Duration::from_secs_f64(
                        1.0 / self.display_params.framerate as f64,
                    ))?;
            }
        }
    }

    fn idle(&mut self) -> anyhow::Result<()> {
        // Accept any waiting clients, but only if we're not mid-resize.
        if !self.pending_attachments.is_empty()
            && self.new_display_params.is_none()
            && self.compositor.surfaces_ready()
        {
            let pending_attachments = self.pending_attachments.drain(..).collect::<Vec<_>>();
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

        // Perform compositor upkeep.
        self.compositor.idle(self.active())?;

        // Send pending controller SYN_REPORT events.
        for (_, dev) in self.gamepads.iter_mut() {
            dev.frame()
        }

        // Flush events to the app.
        self.wayland_display.flush_clients()?;

        Ok(())
    }

    fn active(&self) -> bool {
        self.session_handle.num_attachments() > 0 || !self.pending_attachments.is_empty()
    }

    fn update_display_params(&mut self, params: DisplayParams) -> anyhow::Result<()> {
        let old = self.display_params;

        let old_ui_scale = self.display_params.ui_scale;
        let new_ui_scale = if self.app_config.force_1x_scale {
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

            self.compositor.update_display_params(
                DisplayParams {
                    ui_scale: new_ui_scale,
                    ..params
                },
                // If we're forcing clients to reattach, the attachment is about
                // end, so configure the surfaces as inactive.
                !force_reattach,
            )?;

            self.session_handle
                .dispatch(SessionEvent::DisplayParamsChanged {
                    params,
                    reattach: force_reattach,
                });

            if force_reattach {
                // Clear any pending attachments which don't match the new output.
                self.pending_attachments.retain(|pending| {
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
                self.session_handle.remove_all();
                self.audio_pipeline.stop_stream();

                self.video_pipeline = None;
                self.new_video_stream_params = None;
            }
        } else if params.ui_scale != old.ui_scale {
            // Synthesize a param change if we are forcing 1x scale.
            self.session_handle
                .dispatch(SessionEvent::DisplayParamsChanged {
                    params,
                    reattach: false,
                });
        }

        self.display_params = DisplayParams {
            ui_scale: new_ui_scale,
            ..params
        };

        Ok(())
    }

    fn frame(&mut self) -> anyhow::Result<()> {
        let _tracy_frame = tracy_client::non_continuous_frame!("composite");

        if self.session_handle.num_attachments() == 0 {
            return Ok(());
        }

        if !self.compositor.surfaces_ready() {
            return Ok(());
        }

        if let Some(params) = self.new_video_stream_params.take() {
            self.video_stream_seq += 1;
            self.video_pipeline = Some(video::EncodePipeline::new(
                self.vk.clone(),
                self.video_stream_seq,
                self.session_handle.clone(),
                self.display_params,
                params,
            )?);
        }

        let Some(video_pipeline) = &mut self.video_pipeline else {
            return Ok(());
        };

        // Composite visible surfaces.
        self.compositor.composite_frame(video_pipeline)?;

        // Render the cursor, if needed.
        self.compositor.render_cursor()?;

        Ok(())
    }

    fn attach(
        &mut self,
        id: u64,
        sender: crossbeam::Sender<SessionEvent>,
        video_params: VideoStreamParams,
        audio_params: AudioStreamParams,
    ) -> anyhow::Result<()> {
        if self.session_handle.num_attachments() > 0 {
            unimplemented!();
        }

        self.session_handle.insert_client(id, sender);
        self.new_video_stream_params = Some(video_params);
        self.audio_pipeline.restart_stream(audio_params)?;
        self.compositor.update_focus_and_visibility(true)?;

        self.compositor.dispatch_cursor();
        if let Some(coords) = self.compositor.default_seat.pointer_locked() {
            let (x, y) = coords.into();
            self.session_handle
                .dispatch(SessionEvent::PointerLocked(x, y));
        }

        Ok(())
    }

    fn handle_control_message(&mut self, msg: ControlMessage) -> anyhow::Result<()> {
        trace!(?msg, "control message");

        if self.shutting_down {
            // We're about to shut down, so ignore all messages.
            return Ok(());
        }

        // Attachments get handled asynchronously.
        if matches!(msg, ControlMessage::Attach { .. }) {
            self.pending_attachments.push(msg);
            return Ok(());
        }

        match msg {
            ControlMessage::Detach(id) => {
                self.session_handle.remove_client(id);
                self.pending_attachments.retain(|msg| {
                    let ControlMessage::Attach { id: pending_id, .. } = msg else {
                        unreachable!();
                    };

                    *pending_id != id
                });

                if !self.active() {
                    self.audio_pipeline.stop_stream();
                    self.video_pipeline = None;
                    self.compositor.update_focus_and_visibility(false)?;
                }
            }
            ControlMessage::RequestVideoRefresh(stream_seq) => {
                if let Some(video) = &mut self.video_pipeline {
                    if self.video_stream_seq == stream_seq {
                        video.request_refresh();
                    } else {
                        debug!(
                            requested_stream_seq = stream_seq,
                            current_stream_seq = self.video_stream_seq,
                            "ignoring refresh request"
                        );
                    }
                }
            }
            ControlMessage::UpdateDisplayParams(params) => {
                // Updates once per render.
                self.new_display_params = Some(params);
            }
            ControlMessage::KeyboardInput { .. }
            | ControlMessage::PointerInput { .. }
            | ControlMessage::PointerMotion { .. }
            | ControlMessage::RelativePointerMotion { .. }
            | ControlMessage::PointerAxis(_, _)
            | ControlMessage::PointerAxisDiscrete(_, _)
            | ControlMessage::PointerEntered
            | ControlMessage::PointerLeft => self.compositor.handle_input_event(msg),
            ControlMessage::GamepadAvailable(id) => {
                use std::collections::btree_map::Entry;
                if let Entry::Vacant(e) = self.gamepads.entry(id) {
                    e.insert(self.input_manager.plug_gamepad(
                        id,
                        input::GamepadLayout::GenericDualStick,
                        false,
                    )?);
                }
            }
            ControlMessage::GamepadUnavailable(id) => {
                use std::collections::btree_map::Entry;
                match self.gamepads.entry(id) {
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
                if let Some(gamepad) = self.gamepads.get_mut(&id) {
                    gamepad.axis(axis_code, value);
                }
            }
            ControlMessage::GamepadTrigger {
                id,
                trigger_code,
                value,
            } => {
                if let Some(gamepad) = self.gamepads.get_mut(&id) {
                    gamepad.trigger(trigger_code, value);
                }
            }
            ControlMessage::GamepadInput {
                id,
                button_code,
                state,
            } => {
                if let Some(gamepad) = self.gamepads.get_mut(&id) {
                    gamepad.input(button_code, state);
                }
            }
            // Handled above.
            ControlMessage::Stop | ControlMessage::Attach { .. } => unreachable!(),
        }

        Ok(())
    }
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
            Ok(1..) => {
                if let Some(debug_log) = debug_log {
                    let _ = std::io::Write::write_all(debug_log, buf.as_bytes());
                }

                let buf = buf.trim();
                if !buf.is_empty() {
                    trace!(target: "mmserver::compositor::child", "{}", buf);
                }
            }
            Ok(0) => break,
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
