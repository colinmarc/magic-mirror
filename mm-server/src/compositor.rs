// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

mod audio;
mod control;
mod handlers;
mod video;
mod window;
mod xserver;

use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    io::{BufRead, BufReader},
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time,
};

use anyhow::{anyhow, bail, Context, Result};
use crossbeam_channel as crossbeam;
use lazy_static::*;
use pathsearch::find_executable_in_path;
use smithay::{
    reexports::wayland_server::{self, Resource},
    wayland::xwayland_shell,
    xwayland,
};
use tracing::{debug, info, trace, trace_span, warn};

use crate::{
    config::AppConfig, pixel_scale::PixelScale, vulkan::VkContext, waking_sender::WakingSender,
};
pub use control::*;
use window::Window;
use xserver::XWaylandLoop;

lazy_static! {
    static ref EPOCH: std::time::Instant = std::time::Instant::now();
}

#[derive(Debug, Clone)]
pub struct AttachedClients(Arc<RwLock<HashMap<u64, crossbeam::Sender<CompositorEvent>>>>);

impl AttachedClients {
    pub fn new() -> Self {
        Self(Arc::new(RwLock::new(HashMap::new())))
    }

    pub fn insert_client(&self, id: u64, sender: crossbeam::Sender<CompositorEvent>) {
        self.0.write().unwrap().insert(id, sender);
    }

    pub fn remove_client(&self, id: u64) {
        self.0.write().unwrap().remove(&id);
    }

    pub fn remove_all(&self) {
        self.0.write().unwrap().clear();
    }

    pub fn dispatch(&self, event: CompositorEvent) {
        let attachments = self.0.read().unwrap();
        for (_, sender) in attachments.iter() {
            sender.send(event.clone()).ok();
        }
    }

    pub fn shutdown(&self) {
        let mut attachments = self.0.write().unwrap();
        for (_, sender) in attachments.drain() {
            sender.send(CompositorEvent::Shutdown).ok();
        }
    }

    fn is_empty(&self) -> bool {
        self.0.read().unwrap().is_empty()
    }
}

const ACCEPT: mio::Token = mio::Token(0);
const DISPLAY: mio::Token = mio::Token(1);
const TIMER: mio::Token = mio::Token(2);
const WAKER: mio::Token = mio::Token(3);
const CHILD: mio::Token = mio::Token(4);

const ACCEPT_TIMEOUT: time::Duration = time::Duration::from_secs(10);

const SHUTDOWN_TIMEOUT: time::Duration = time::Duration::from_secs(30);

pub struct Compositor {
    xdg_runtime_dir: mktemp::Temp,
    socket_name: OsString,
    listening_socket: wayland_server::ListeningSocket,
    xwayland: Option<XWaylandLoop>,
    poll: mio::Poll,
    state: State,
    control_messages_recv: crossbeam::Receiver<ControlMessage>,
    control_messages_send: WakingSender<ControlMessage>,
    attachments: AttachedClients,

    bug_report_dir: Option<PathBuf>,

    /// A timer for waiting for the app to shut down.
    shutting_down: Option<time::Instant>,

    // At the bottom for drop order.
    display: wayland_server::Display<State>,
    num_clients: usize,
}

pub struct State {
    app_config: AppConfig,
    display_params: DisplayParams,
    new_display_params: Option<DisplayParams>,
    ui_scale: PixelScale, // Can differ from display_params if we force 1x scale.
    output: smithay::output::Output,
    window_stack: Vec<Window>,

    compositor_state: smithay::wayland::compositor::CompositorState,
    dmabuf_state: smithay::wayland::dmabuf::DmabufState,
    _dmabuf_global: smithay::wayland::dmabuf::DmabufGlobal,
    // output_manager_state: smithay::wayland::output::OutputManagerState,
    xdg_shell_state: smithay::wayland::shell::xdg::XdgShellState,
    shm_state: smithay::wayland::shm::ShmState,
    seat_state: smithay::input::SeatState<Self>,
    _text_input_state: smithay::wayland::text_input::TextInputManagerState,
    seat: smithay::input::Seat<Self>,
    keyboard_handle: smithay::input::keyboard::KeyboardHandle<Self>,
    pointer_handle: smithay::input::pointer::PointerHandle<Self>,

    xwayland_shell_state: xwayland_shell::XWaylandShellState,
    xwm: Option<xwayland::xwm::X11Wm>,
    // Windows that will map once a surface is attached.
    xwindows_pending_map_on_surface: Vec<xwayland::X11Surface>,

    video_pipeline: video::EncodePipeline,
    audio_pipeline: audio::EncodePipeline,
    _vk: Arc<VkContext>, // At the end for drop order.
}

#[derive(Debug, Default)]
pub struct ClientState {
    pub compositor_state: smithay::wayland::compositor::CompositorClientState,
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
    pub fn new(
        vk: Arc<VkContext>,
        app_config: AppConfig,
        display_params: DisplayParams,
        bug_report_dir: Option<PathBuf>,
    ) -> Result<Self> {
        let poll = mio::Poll::new()?;
        let display = wayland_server::Display::new().context("failed to create display")?;
        let dh = display.handle();

        let compositor_state = smithay::wayland::compositor::CompositorState::new::<State>(&dh);

        // let output_manager_state =
        //     smithay::wayland::output::OutputManagerState::new_with_xdg_output::<WaylandState>(&dh);
        let xdg_shell_state = smithay::wayland::shell::xdg::XdgShellState::new::<State>(&dh);
        let shm_state = smithay::wayland::shm::ShmState::new::<State>(&dh, vec![]);

        let mut seat_state = smithay::input::SeatState::new();
        let text_input_state: smithay::wayland::text_input::TextInputManagerState =
            smithay::wayland::text_input::TextInputManagerState::new::<State>(&dh);

        let mut seat = seat_state.new_wl_seat(&dh, "virtual");
        let keyboard_handle = seat.add_keyboard(Default::default(), 200, 200).unwrap();
        keyboard_handle.change_repeat_info(0, 0);
        let pointer_handle = seat.add_pointer();

        let mut dmabuf_state = smithay::wayland::dmabuf::DmabufState::new();
        let default_feedback = video::dmabuf_feedback(vk.clone())?;
        let dmabuf_global =
            dmabuf_state.create_global_with_default_feedback::<State>(&dh, &default_feedback);

        let xwayland_shell_state =
            smithay::wayland::xwayland_shell::XWaylandShellState::new::<State>(&dh);

        // Check force_1x_scale. The way this configuration value works is that
        // by setting the scale to 1.0, logical resolution in wayland will
        // always be the same as physical resolution. This should prevent any
        // server-side upscaling.
        //
        // Note that we still store the client's requested scale as the official
        // scale properties; this is treated as an implementation detail in the
        // compositor.
        let ui_scale = if app_config.force_1x_scale {
            PixelScale::ONE
        } else {
            display_params.ui_scale
        };

        let output =
            create_headless_output(&dh, display_params.width, display_params.height, ui_scale);

        // This is used for the child app to find and connect to our
        // wayland/pipewire/etc sockets.
        let xdg_runtime_dir = mktemp::Temp::new_dir().context("failed to create temp dir")?;

        let attachments = AttachedClients::new();
        let video_pipeline =
            video::EncodePipeline::new(vk.clone(), attachments.clone(), display_params)?;

        let audio_pipeline = audio::EncodePipeline::new(attachments.clone(), &xdg_runtime_dir)?;

        let state = State {
            app_config,
            display_params,
            new_display_params: None,
            ui_scale,
            output,
            compositor_state,
            dmabuf_state,
            _dmabuf_global: dmabuf_global,
            // output_manager_state,
            xdg_shell_state,
            shm_state,
            seat_state,
            _text_input_state: text_input_state,
            seat,
            keyboard_handle,
            pointer_handle,
            xwayland_shell_state,
            xwm: None,
            xwindows_pending_map_on_surface: Vec::new(),
            window_stack: Vec::new(),
            video_pipeline,
            audio_pipeline,
            _vk: vk,
        };

        // Bind the wayland socket.
        let socket_name = gen_socket_name();
        let listening_socket = wayland_server::ListeningSocket::bind_absolute(Path::join(
            &xdg_runtime_dir,
            &socket_name,
        ))?;

        // Bind the xwayland sockets.
        let xwayland = if state.app_config.xwayland {
            Some(XWaylandLoop::new(dh.clone())?)
        } else {
            None
        };

        let waker = Arc::new(mio::Waker::new(poll.registry(), WAKER)?);
        let (send, recv) = crossbeam::unbounded();
        let send = WakingSender::new(waker, send);

        Ok(Self {
            display,
            num_clients: 0,
            xdg_runtime_dir,
            socket_name,
            listening_socket,
            xwayland,
            poll,
            state,
            control_messages_recv: recv,
            control_messages_send: send,
            attachments,
            shutting_down: None,
            bug_report_dir,
        })
    }

    pub fn run(&mut self, ready: oneshot::Sender<WakingSender<ControlMessage>>) -> Result<()> {
        let mut events = mio::Events::with_capacity(64);

        let display_fd = self.display.backend().poll_fd().as_raw_fd();
        self.poll.registry().register(
            &mut mio::unix::SourceFd(&display_fd),
            DISPLAY,
            mio::Interest::READABLE,
        )?;

        let listener_fd = self.listening_socket.as_raw_fd();
        self.poll.registry().register(
            &mut mio::unix::SourceFd(&listener_fd),
            ACCEPT,
            mio::Interest::READABLE,
        )?;

        let mut timer = mio_timerfd::TimerFd::new(mio_timerfd::ClockId::Monotonic)?;
        timer.set_timeout_interval(&time::Duration::from_nanos(
            1_000_000_000 / self.state.display_params.framerate as u64,
        ))?;

        self.poll
            .registry()
            .register(&mut timer, TIMER, mio::Interest::READABLE)?;

        // Spawn the client with a pipe as stdout/stderr.
        let (pipe_send, mut pipe_recv) = mio::unix::pipe::new()?;
        let mut child = spawn_client(
            self.state.app_config.clone(),
            self.xdg_runtime_dir.as_os_str(),
            &self.socket_name,
            self.xwayland.as_ref().map(|xw| xw.x11_display),
            pipe_send,
        )
        .context("failed to start application process")?;

        self.poll
            .registry()
            .register(&mut pipe_recv, CHILD, mio::Interest::READABLE)?;
        let mut child_output = BufReader::new(&mut pipe_recv);

        let mut child_debug_log = if let Some(bug_report_dir) = self.bug_report_dir.as_ref() {
            let exe_name = Path::new(&self.state.app_config.command[0])
                .file_name()
                .unwrap();
            let path =
                bug_report_dir.join(format!("{}-{}.log", exe_name.to_string_lossy(), child.id()));

            Some(std::fs::File::create(path).context("failed to create child logfile")?)
        } else {
            None
        };

        let mut ready = Some(ready);
        let start = time::Instant::now();

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

                            self.num_clients += 1;
                            debug!("{} clients connected", self.num_clients);

                            // Notify the parent thread that we're ready.
                            if let Some(chan) = ready.take() {
                                debug!(
                                    width = self.state.display_params.width,
                                    height = self.state.display_params.height,
                                    framerate = self.state.display_params.framerate,
                                    scale = %self.state.ui_scale,
                                    "compositor ready"
                                );
                                chan.send(self.control_messages_send.clone())?;
                            }
                        }
                    }
                    CHILD if event.is_read_closed() => {
                        let exit_status = child.wait()?;
                        info!(
                            exit_status = exit_status.code().unwrap_or_default(),
                            "child process exited"
                        );

                        match exit_status {
                            unshare::ExitStatus::Exited(c) if c != 0 => {
                                bail!("child process exited with error code {}", c)
                            }
                            _ => return Ok(()),
                        }
                    }
                    CHILD if event.is_readable() => {
                        let mut buf = String::new();
                        loop {
                            buf.clear();
                            match child_output.read_line(&mut buf) {
                                Ok(_) => {
                                    if let Some(child_debug_log) = &mut child_debug_log {
                                        std::io::Write::write_all(child_debug_log, buf.as_bytes())?;
                                    }

                                    let buf = buf.trim();
                                    if !buf.is_empty() {
                                        trace!(target: "mmserver::compositor::child", "{}", buf);
                                    }
                                }
                                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                    break;
                                }
                                Err(e) => {
                                    debug!("child error: {:?}", e);
                                    break;
                                }
                            }
                        }
                    }
                    DISPLAY => {
                        if let Some(xwayland) = &mut self.xwayland {
                            xwayland.dispatch(&mut self.state)?;
                        }

                        self.display.dispatch_clients(&mut self.state)?;
                    }
                    TIMER => {
                        timer.read()?;

                        // TODO: this shouldn't be necessary.
                        self.display.dispatch_clients(&mut self.state)?;

                        if let Some(xwayland) = &mut self.xwayland {
                            xwayland.dispatch(&mut self.state)?;

                            // Bookkeeping for X11 windows.
                            self.state.map_delayed_xwindows()?;
                        }

                        // TODO: first frame?
                        if !self.state.window_stack.is_empty() {
                            if let Some(chan) = ready.take() {
                                debug!("compositor ready");
                                chan.send(self.control_messages_send.clone())?;
                            }
                        }

                        // Check that we haven't timed out waiting for the client.
                        if ready.is_some() && start.elapsed() > ACCEPT_TIMEOUT {
                            bail!("timed out waiting for client");
                        }

                        // Check if we need to hard kill the client.
                        if let Some(shutting_down) = self.shutting_down {
                            if shutting_down.elapsed() > SHUTDOWN_TIMEOUT {
                                warn!("graceful shutdown failed, killing client");

                                signal_child(child.id() as i32, nix::sys::signal::SIGKILL)?;
                                return Ok(());
                            }
                        }

                        // Check if we need to rebuild the video capture pipeline.
                        if let Some(new_params) = self.state.new_display_params.take() {
                            self.update_display_params(new_params)?;
                            timer.set_timeout_interval(&time::Duration::from_nanos(
                                1_000_000_000 / self.state.display_params.framerate as u64,
                            ))?;
                        }

                        // If no one is watching, don't render.
                        if !self.attachments.is_empty() {
                            self.render().context("render failed")?;
                        }
                    }
                    WAKER => loop {
                        match self.control_messages_recv.try_recv() {
                            Ok(ControlMessage::Stop) => {
                                self.attachments.shutdown();
                                self.state.video_pipeline.stop_stream();
                                self.state.audio_pipeline.stop_stream();

                                trace!("shutting down");
                                self.shutting_down = Some(time::Instant::now());

                                // Give the app a chance to clean up.
                                signal_child(child.id() as i32, nix::sys::signal::SIGTERM)?;
                            }
                            Ok(ControlMessage::Attach {
                                id,
                                sender,
                                video_params,
                                audio_params,
                            }) => {
                                self.attachments.insert_client(id, sender);
                                self.state.audio_pipeline.restart_stream(audio_params)?;
                                self.state.video_pipeline.restart_stream(video_params);
                            }
                            Ok(ControlMessage::Detach(id)) => {
                                self.attachments.remove_client(id);
                                if self.attachments.is_empty() {
                                    self.state.audio_pipeline.stop_stream();
                                    self.state.video_pipeline.stop_stream();
                                }
                            }
                            Ok(ControlMessage::UpdateDisplayParams(params)) => {
                                // Updates once per render.
                                self.state.new_display_params = Some(params);
                            }
                            Ok(ControlMessage::KeyboardEvent {
                                evdev_scancode,
                                state,
                                char,
                            }) => {
                                let text_input =
                                    smithay::wayland::text_input::TextInputSeat::text_input(
                                        &self.state.seat,
                                    );

                                let mut text_sent = false;
                                match char {
                                    Some(ch)
                                        if state == smithay::backend::input::KeyState::Pressed =>
                                    {
                                        text_input.with_focused_text_input(|ti, _surf| {
                                            text_sent = true;
                                            ti.commit_string(Some(ch.into()));
                                        });

                                        if text_sent {
                                            text_input.done(false);
                                        }
                                    }
                                    _ => (),
                                }

                                if !text_sent {
                                    let handle = self.state.keyboard_handle.clone();
                                    handle.input(
                                        &mut self.state,
                                        evdev_scancode,
                                        state,
                                        smithay::utils::SERIAL_COUNTER.next_serial(),
                                        EPOCH.elapsed().as_millis() as u32,
                                        |_, _, _| {
                                            smithay::input::keyboard::FilterResult::<State>::Forward
                                        },
                                    );
                                }
                            }
                            Ok(ControlMessage::PointerEntered) => {
                                // Nothing to do.
                            }
                            Ok(ControlMessage::PointerLeft) => {
                                let handle = self.state.pointer_handle.clone();
                                handle.motion(
                                    &mut self.state,
                                    None,
                                    &smithay::input::pointer::MotionEvent {
                                        location: (-1.0, -1.0).into(),
                                        serial: smithay::utils::SERIAL_COUNTER.next_serial(),
                                        time: EPOCH.elapsed().as_millis() as u32,
                                    },
                                );
                                handle.frame(&mut self.state);
                            }
                            Ok(ControlMessage::PointerMotion(x, y)) => {
                                let location: smithay::utils::Point<f64, smithay::utils::Physical> =
                                    (x, y).into();
                                let scale: smithay::output::Scale = self.state.ui_scale.into();

                                let handle = self.state.pointer_handle.clone();
                                let (focus, location) = if let Some(window) =
                                    self.state.window_at(location)
                                {
                                    let window_origin = match window.popup_bounds() {
                                        Some(bounds) => bounds.loc,
                                        None => (0, 0).into(),
                                    };

                                    // Smithay wants to do the math to calculate
                                    // relative coords for us, but that's
                                    // different for X surfaces (with a scale of
                                    // one) and normal surfaces.
                                    match window.ty {
                                        window::SurfaceType::X11Window(_)
                                        | window::SurfaceType::X11Popup(..) => (
                                            Some((
                                                window,
                                                window_origin.to_logical(window::TODO_X11_SCALE),
                                            )),
                                            location.to_logical(window::TODO_X11_SCALE as f64),
                                        ),
                                        _ => (
                                            Some((
                                                window,
                                                window_origin
                                                    .to_f64()
                                                    .to_logical(scale.fractional_scale())
                                                    .to_i32_round(),
                                            )),
                                            location.to_logical(scale.fractional_scale()),
                                        ),
                                    }
                                } else {
                                    (None, location.to_logical(scale.fractional_scale()))
                                };

                                handle.motion(
                                    &mut self.state,
                                    focus,
                                    &smithay::input::pointer::MotionEvent {
                                        location,
                                        serial: smithay::utils::SERIAL_COUNTER.next_serial(),
                                        time: EPOCH.elapsed().as_millis() as u32,
                                    },
                                );
                                handle.frame(&mut self.state);
                            }
                            Ok(ControlMessage::PointerInput {
                                button_code, state, ..
                            }) => {
                                let handle = self.state.pointer_handle.clone();
                                handle.button(
                                    &mut self.state,
                                    &smithay::input::pointer::ButtonEvent {
                                        button: button_code,
                                        state,
                                        serial: smithay::utils::SERIAL_COUNTER.next_serial(),
                                        time: EPOCH.elapsed().as_millis() as u32,
                                    },
                                );
                                handle.frame(&mut self.state);
                            }
                            Err(crossbeam::TryRecvError::Empty) => break,
                            Err(crossbeam::TryRecvError::Disconnected) => {
                                panic!("control channel disconnected")
                            }
                        }
                    },
                    _ => unreachable!(),
                }
            }

            // Flush keyboard events, etc.
            self.display.flush_clients()?;
        }
    }

    fn render(&mut self) -> Result<()> {
        let _tracy_frame = tracy_client::non_continuous_frame!("composite");
        if self.state.window_stack.is_empty() {
            return Ok(());
        }

        let now = EPOCH.elapsed().as_millis() as u32;
        unsafe { self.state.video_pipeline.begin()? };

        for window in self.state.window_stack.iter_mut().filter(|w| w.visible) {
            let mut surfaces = Vec::new();

            smithay::wayland::compositor::with_surface_tree_downward(
                &window.surface,
                (),
                |_, _, &()| smithay::wayland::compositor::TraversalAction::DoChildren(()),
                |surf, _states, &()| {
                    // TODO: don't render subsurfaces that aren't committed.
                    surfaces.push(surf.clone());
                },
                |_, _, &()| true,
            );

            // TODO: calculate the rectangle based on buffer size
            let dest = match window.popup_bounds() {
                Some(bounds) => bounds,
                None => smithay::utils::Rectangle::from_loc_and_size(
                    (0, 0),
                    (
                        self.state.display_params.width as i32,
                        self.state.display_params.height as i32,
                    ),
                ),
            };

            for surf in surfaces.into_iter() {
                unsafe { self.state.video_pipeline.composite_surface(&surf, dest)? };

                // Send frame callbacks.
                window.send_frame_callbacks(now);
            }
        }

        unsafe { self.state.video_pipeline.end_and_submit()? };
        Ok(())
    }

    fn update_display_params(&mut self, params: DisplayParams) -> anyhow::Result<()> {
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

            let mode = smithay::output::Mode {
                size: (params.width as i32, params.height as i32).into(),
                refresh: (params.framerate * 1000) as i32,
            };

            let output_scale = new_ui_scale.into();
            self.state
                .output
                .change_current_state(Some(mode), None, Some(output_scale), None);
            self.state.output.set_preferred(mode);

            for window in self.state.window_stack.iter_mut() {
                if scale_changed && window.surface.version() >= 6 {
                    window
                        .surface
                        .preferred_buffer_scale(output_scale.integer_scale());
                }

                if window.visible {
                    window.configure_activated(params.width, params.height, new_ui_scale)?;
                }
            }

            self.attachments
                .dispatch(CompositorEvent::DisplayParamsChanged {
                    params,
                    reattach: size_changed || framerate_changed,
                });
            self.state.display_params = params;
            self.state.ui_scale = new_ui_scale;
        } else {
            // Simulate a param change if we are forcing 1x scale.
            if params.ui_scale != old.ui_scale {
                self.attachments
                    .dispatch(CompositorEvent::DisplayParamsChanged {
                        params,
                        reattach: false,
                    });
            }

            for toplevel in self.state.xdg_shell_state.toplevel_surfaces().iter() {
                self.state.output.enter(toplevel.wl_surface());
            }
        }

        if size_changed || framerate_changed {
            self.state.video_pipeline.resize(params);

            // TODO: if we support multiple attachments, or attachments that
            // differ in resolution from the render res, we need to check for
            // that here. For now, it's safe to just kill the attachment streams.
            self.attachments.remove_all();
            self.state.audio_pipeline.stop_stream();
            self.state.video_pipeline.stop_stream();
        }

        Ok(())
    }
}

fn create_headless_output(
    dh: &wayland_server::DisplayHandle,
    width: u32,
    height: u32,
    ui_scale: PixelScale,
) -> smithay::output::Output {
    let mode = smithay::output::Mode {
        size: (width as i32, height as i32).into(),
        refresh: 60_000,
    };

    // TODO: the name should be the operator attached!
    let output = smithay::output::Output::new(
        "magic-mirror".to_string(),
        smithay::output::PhysicalProperties {
            size: (width as i32, height as i32).into(),
            subpixel: smithay::output::Subpixel::Unknown,
            make: "Magic".into(),
            model: "Mirror".into(),
        },
    );

    output.create_global::<State>(dh);
    output.change_current_state(Some(mode), None, Some(ui_scale.into()), Some((0, 0).into()));
    output.set_preferred(mode);

    output
}

fn spawn_client(
    app_config: AppConfig,
    xdg_runtime_dir: &OsStr,
    socket_name: &OsStr,
    x11_display: Option<u32>,
    pipe: mio::unix::pipe::Sender,
) -> anyhow::Result<unshare::Child> {
    // This gets dropped when we return, closing the write side (in this process)
    let stdout = unshare::Stdio::dup_file(&pipe)?;
    let stderr = unshare::Stdio::dup_file(&pipe)?;

    let mut args = app_config.command.clone();
    let exe = args.remove(0);
    let exe_path =
        find_executable_in_path(&exe).ok_or(anyhow!("command {:?} not in PATH", &exe))?;

    let mut envs: Vec<(OsString, OsString)> = app_config.env.clone().into_iter().collect();
    envs.push(("WAYLAND_DISPLAY".into(), socket_name.into()));

    if let Some(x11_display) = x11_display {
        envs.push(("DISPLAY".into(), format!(":{}", x11_display).into()));
    }

    envs.push(("XDG_RUNTIME_DIR".into(), xdg_runtime_dir.into()));

    // Shadow pipewire.
    envs.push(("PIPEWIRE_REMOTE".into(), "(null)".into()));

    // Shadow dbus.
    // TODO: we can set up our own broker and provide desktop portal
    // functionality.
    // let dbus_socket = Path::join(Path::new(xdg_runtime_dir), "dbus");
    // envs.push(("DBUS_SESSION_BUS_ADDRESS".into(), dbus_socket.into()));

    debug!(
        exe = exe_path.to_string_lossy().to_string(),
        env = ?envs,
        "launching child process"
    );

    let mut command = unshare::Command::new(&exe_path);
    let command = command
        .args(&args)
        .envs(envs)
        .stdin(unshare::Stdio::null())
        .stdout(stdout)
        .stderr(stderr);

    let command = unsafe {
        command.pre_exec(|| {
            // Creates a new process group.
            nix::unistd::setsid()?;
            Ok(())
        })
    };

    match command.spawn() {
        Ok(child) => {
            trace!(pid = child.id(), "child process started");
            Ok(child)
        }
        Err(e) => Err(anyhow!(
            "failed to spawn child process '{}': {:#}",
            exe_path.to_string_lossy(),
            e
        )),
    }
}

fn signal_child(pid: i32, sig: nix::sys::signal::Signal) -> anyhow::Result<()> {
    // Signal the whole process group. We used setsid, so the group should be
    // the same as the child pid.
    let pid = nix::unistd::Pid::from_raw(pid);
    nix::sys::signal::killpg(pid, Some(sig))?;

    Ok(())
}

fn gen_socket_name() -> OsString {
    use rand::Rng;
    let id: u64 = rand::thread_rng().gen();
    format!("magic-mirror-{}", id).into()
}
