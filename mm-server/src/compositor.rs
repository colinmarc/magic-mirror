// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

mod audio;
mod buffers;
mod child;
mod control;
mod protocols;
mod sealed;
mod seat;
mod serial;
mod shm;
mod stack;
mod surface;
mod video;

use anyhow::{bail, Context as _};
use slotmap::{SecondaryMap, SlotMap};
use tracing::{debug, trace, trace_span};
use wayland_protocols::xdg::shell::server::xdg_wm_base;
use wayland_server::protocol::{self, wl_shm};

use std::{
    ffi::{OsStr, OsString},
    io::{BufRead, BufReader},
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time,
};

use crossbeam_channel as crossbeam;
use hashbrown::HashMap;
use lazy_static::lazy_static;

use crate::{
    config::AppConfig, pixel_scale::PixelScale, vulkan::VkContext, waking_sender::WakingSender,
};

use child::*;
pub use control::*;
pub use seat::{ButtonState, KeyState};

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

pub struct State {
    serial: serial::Serial,

    surfaces: SlotMap<surface::SurfaceKey, surface::Surface>,
    shm_pools: SlotMap<shm::ShmPoolKey, shm::ShmPool>,
    buffers: SlotMap<buffers::BufferKey, buffers::Buffer>,
    textures: SecondaryMap<buffers::BufferKey, buffers::Texture>,

    surface_stack: Vec<surface::SurfaceKey>,
    surface_positioning: SecondaryMap<surface::SurfaceKey, stack::Position>,
    focused_surface: Option<surface::SurfaceKey>,

    default_seat: seat::Seat,
    pointer_coords: Option<glam::DVec2>, // Global space.

    app_config: AppConfig,
    display_params: DisplayParams,
    ui_scale: PixelScale, // Can differ from display_params if we force 1x scale.

    attachments: AttachedClients,
    pending_attachments: Vec<ControlMessage>,

    video_pipeline: Option<video::EncodePipeline>,
    new_video_stream_params: Option<VideoStreamParams>,
    video_stream_seq: u64,

    audio_pipeline: audio::EncodePipeline,

    // At the bottom for drop order.
    vk: Arc<VkContext>,
}

#[derive(Debug, Default)]
pub struct ClientState {}

impl wayland_server::backend::ClientData for ClientState {
    fn initialized(&self, _client_id: wayland_server::backend::ClientId) {}
    fn disconnected(
        &self,
        _client_id: wayland_server::backend::ClientId,
        _reason: wayland_server::backend::DisconnectReason,
    ) {
    }
}

pub struct Compositor {
    state: State,
    display: wayland_server::Display<State>,
    xdg_runtime_dir: mktemp::Temp,
    bug_report_dir: Option<PathBuf>,

    shutting_down: Option<time::Instant>,
}

impl Compositor {
    pub fn new(
        vk: Arc<VkContext>,
        app_config: AppConfig,
        display_params: DisplayParams,
        bug_report_dir: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let display = wayland_server::Display::new().context("failed to create display")?;

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
        create_global::<protocol::wl_seat::WlSeat>(&dh, 9);
        create_global::<wl_shm::WlShm>(&dh, 1);
        create_global::<xdg_wm_base::XdgWmBase>(&dh, 6);
        // dh.create_global::<State, wl_drm::WlDrm, ()>(2, ());

        // Used for the wayland and xwayland sockets, among other things.
        let xdg_runtime_dir = mktemp::Temp::new_dir().context("failed to create temp dir")?;

        let attachments = AttachedClients::new();
        let audio_pipeline = audio::EncodePipeline::new(attachments.clone(), &xdg_runtime_dir)?;

        let state = State {
            serial: serial::Serial::new(),

            surfaces: SlotMap::default(),
            shm_pools: SlotMap::default(),
            buffers: SlotMap::default(),
            textures: SecondaryMap::default(),

            surface_stack: Vec::new(),
            surface_positioning: SecondaryMap::default(),
            focused_surface: None,

            default_seat: seat::Seat::default(),
            pointer_coords: None,

            app_config,
            display_params,
            attachments,
            pending_attachments: Vec::new(),

            video_pipeline: None,
            new_video_stream_params: None,
            video_stream_seq: 0,

            audio_pipeline,

            ui_scale,
            vk,
        };

        Ok(Self {
            display,
            state,
            xdg_runtime_dir,
            bug_report_dir,

            shutting_down: None,
        })
    }

    pub fn run(
        &mut self,
        ready_send: oneshot::Sender<WakingSender<ControlMessage>>,
    ) -> Result<(), anyhow::Error> {
        let mut poll = mio::Poll::new()?;
        let mut events = mio::Events::with_capacity(64);

        const DISPLAY: mio::Token = mio::Token(0);
        const ACCEPT: mio::Token = mio::Token(1);
        const CHILD: mio::Token = mio::Token(2);
        const WAKER: mio::Token = mio::Token(3);
        const TIMER: mio::Token = mio::Token(4);

        let display_fd = self.display.backend().poll_fd().as_raw_fd();
        poll.registry().register(
            &mut mio::unix::SourceFd(&display_fd),
            DISPLAY,
            mio::Interest::READABLE,
        )?;

        // Bind the listening socket.
        let socket_name = gen_socket_name();
        let socket_path = Path::join(&self.xdg_runtime_dir, &socket_name);
        let listening_socket = wayland_server::ListeningSocket::bind_absolute(socket_path.clone())?;
        trace!(?socket_path, "bound wayland socket");

        let listener_fd = listening_socket.as_raw_fd();
        poll.registry().register(
            &mut mio::unix::SourceFd(&listener_fd),
            ACCEPT,
            mio::Interest::READABLE,
        )?;

        // Spawn the client with a pipe as stdout/stderr.
        let (pipe_send, mut pipe_recv) = mio::unix::pipe::new()?;
        let mut child = spawn_child(
            self.state.app_config.clone(),
            self.xdg_runtime_dir.as_os_str(),
            &socket_name,
            None,
            pipe_send,
        )
        .context("failed to start application process")?;

        poll.registry()
            .register(&mut pipe_recv, CHILD, mio::Interest::READABLE)?;
        let mut child_output = BufReader::new(&mut pipe_recv);

        // Framerate timer (simulates vblank).
        let mut timer = mio_timerfd::TimerFd::new(mio_timerfd::ClockId::Monotonic)?;
        timer.set_timeout_interval(&time::Duration::from_nanos(
            1_000_000_000 / self.state.display_params.framerate as u64,
        ))?;

        poll.registry()
            .register(&mut timer, TIMER, mio::Interest::READABLE)?;

        // If bug report mode is enabled, save the stdout/stderr of the child to
        // a logfile.
        let mut child_debug_log = if let Some(bug_report_dir) = self.bug_report_dir.as_ref() {
            let path = bug_report_dir.join(format!("child-{}.log", child.id()));
            Some(std::fs::File::create(path).context("failed to create child logfile")?)
        } else {
            None
        };

        // Use `glxinfo` and `eglinfo` to generate more debugging help.
        if let Some(bug_report_dir) = self.bug_report_dir.as_ref() {
            let p = bug_report_dir.to_owned();
            let wayland_socket = socket_name.clone();
            // let x11_socket = self.xwayland.as_ref().map(|xwm| xwm.x11_display);
            let x11_socket = None;
            std::thread::spawn(move || {
                save_glxinfo_eglinfo(&p, &wayland_socket, x11_socket);
            });
        }

        let (control_send, control_recv) = crossbeam::unbounded();
        let waker = Arc::new(mio::Waker::new(poll.registry(), WAKER)?);
        let control_send = WakingSender::new(waker, control_send);

        let mut ready_once = Some(ready_send);

        loop {
            trace_span!("poll").in_scope(|| poll.poll(&mut events, None))?;

            for event in events.iter() {
                match event.token() {
                    ACCEPT => {
                        if let Some(client_stream) = listening_socket.accept()? {
                            let _client = self
                                .display
                                .handle()
                                .insert_client(client_stream, Arc::new(ClientState::default()))?;

                            debug!("client app connected");
                        }
                    }
                    CHILD if event.is_read_closed() => {
                        wait_child(&mut child)?;
                        self.state.attachments.shutdown();

                        if ready_once.is_some() {
                            // The client exited immediately, which is an error.
                            bail!("client exited without doing anything");
                        }
                    }
                    CHILD if event.is_readable() => {
                        dump_child_output(&mut child_output, &mut child_debug_log)
                    }
                    WAKER => loop {
                        match control_recv.try_recv() {
                            Ok(ControlMessage::Stop) => {
                                self.state.attachments.shutdown();
                                self.state.audio_pipeline.stop_stream();

                                trace!("shutting down");
                                self.shutting_down = Some(time::Instant::now());

                                // Give the app a chance to clean up.
                                signal_child(child.id() as i32, nix::sys::signal::SIGTERM)?;
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
                        self.display.dispatch_clients(&mut self.state)?;
                    }
                    TIMER => {
                        timer.read()?;
                        self.frame()?;
                    }
                    _ => unreachable!(),
                }
            }

            self.idle()?;

            if ready_once.is_some() && self.state.surfaces_ready() {
                ready_once.take().unwrap().send(control_send.clone())?;
            }
        }
    }

    fn idle(&mut self) -> anyhow::Result<()> {
        // Accept any waiting clients.
        if !self.state.pending_attachments.is_empty() && self.state.surfaces_ready() {
            let pending_attachments = self.state.pending_attachments.drain(..).collect::<Vec<_>>();
            for attach_msg in pending_attachments {
                if let ControlMessage::Attach {
                    id,
                    sender,
                    video_params,
                    audio_params,
                } = attach_msg
                {
                    self.attach(id, sender, video_params, audio_params)?;
                }
            }
        }

        // Update the window stack, if it changed.
        let suspended =
            self.state.attachments.is_empty() && self.state.pending_attachments.is_empty();
        self.state.update_focus_and_visibility(!suspended);

        // Send any pending surface configures.
        self.state.configure_surfaces();

        // Send pending pointer frames.
        self.state.default_seat.pointer_frame();

        // Flush events to the app.
        self.display.flush_clients()?;

        Ok(())
    }

    fn frame(&mut self) -> anyhow::Result<()> {
        let _tracy_frame = tracy_client::non_continuous_frame!("composite");

        if self.state.attachments.is_empty() {
            return Ok(());
        }

        if self.state.surface_stack.is_empty() {
            return Ok(());
        }

        let now = EPOCH.elapsed().as_millis() as u32;

        if let Some(params) = self.state.new_video_stream_params.take() {
            self.state.video_pipeline = Some(video::EncodePipeline::new(
                self.state.vk.clone(),
                self.state.video_stream_seq,
                self.state.attachments.clone(),
                self.state.display_params,
                params,
            )?);

            self.state.video_stream_seq += 1;
        }

        let video_pipeline = self.state.video_pipeline.as_mut().unwrap();
        unsafe { video_pipeline.begin()? };

        // Iterate backwards to find the first fullscreen window.
        let first_visible_idx = self.state.surface_stack.iter().rposition(|id| {
            self.state
                .surface_positioning
                .get(*id)
                .expect("surface in stack has no position")
                .is_fullscreen(&self.state.display_params)
        });

        for id in self.state.surface_stack[first_visible_idx.unwrap_or_default()..].iter() {
            let content = self
                .state
                .surfaces
                .get_mut(*id)
                .expect("surface has no entry")
                .content
                .as_mut()
                .expect("mapped surface has no content");

            let position = self
                .state
                .surface_positioning
                .get(*id)
                .expect("mapped surface has no bbox");

            let texture = self
                .state
                .textures
                .get(content.buffer)
                .expect("texture not imported");

            unsafe {
                if let Some(release_point) = video_pipeline.composite_surface(texture, *position)? {
                    // add release point to buffer
                    todo!()
                }
            };

            if let Some(callback) = content.frame_callback.take().as_mut() {
                callback.done(now);
            }
        }

        unsafe { video_pipeline.end_and_submit()? };

        // for mut window in windows.into_iter() {
        //     // Send frame callbacks.
        //     window.send_frame_callbacks(now);
        // }

        Ok(())
    }

    fn attach(
        &mut self,
        id: u64,
        sender: crossbeam::Sender<CompositorEvent>,
        video_params: VideoStreamParams,
        audio_params: AudioStreamParams,
    ) -> anyhow::Result<()> {
        if !self.state.attachments.is_empty() {
            unimplemented!();
        }

        self.state.attachments.insert_client(id, sender);
        self.state.new_video_stream_params = Some(video_params);
        self.state.audio_pipeline.restart_stream(audio_params)?;

        // TODO: does it work to spawn clients suspended?
        self.state.update_focus_and_visibility(true);

        // // Send the current cursor state.
        // match self.state.cursor_status {
        //     smithay::input::pointer::CursorImageStatus::Hidden => {
        //         self.attachments.dispatch(CompositorEvent::CursorUpdate {
        //             image: None,
        //             icon: None,
        //             hotspot_x: 0,
        //             hotspot_y: 0,
        //         });
        //     }
        //     smithay::input::pointer::CursorImageStatus::Named(icon) => {
        //         if icon != cursor_icon::CursorIcon::Default {
        //             self.attachments.dispatch(
        //                 CompositorEvent::CursorUpdate {
        //                     image: None,
        //                     icon: Some(icon),
        //                     hotspot_x: 0,
        //                     hotspot_y: 0,
        //                 },
        //             );
        //         }
        //     }
        //     smithay::input::pointer::CursorImageStatus::Surface(_) => {
        //         self.render_cursor()?;
        //     }
        // }

        // if self.state.cursor_locked {
        //     let cursor_loc = self.state.pointer_handle.current_location();
        //     let scale: smithay::output::Scale = self.state.ui_scale.into();
        //     let (x, y) =
        //         cursor_loc.to_physical(scale.fractional_scale()).into();
        //
        //     self.attachments
        //         .dispatch(CompositorEvent::PointerLocked(x, y));
        // }

        Ok(())
    }

    fn handle_control_message(&mut self, msg: ControlMessage) -> anyhow::Result<()> {
        // Attachments get handled asynchronously.
        if matches!(msg, ControlMessage::Attach { .. }) {
            self.state.pending_attachments.push(msg);
            return Ok(());
        }

        match msg {
            ControlMessage::Detach(id) => {
                self.state.attachments.remove_client(id);
                if self.state.attachments.is_empty() {
                    self.state.audio_pipeline.stop_stream();
                    self.state.video_pipeline = None;
                    // self.state.cursor_locked = false;
                    self.state.update_focus_and_visibility(false);
                }
            }
            ControlMessage::UpdateDisplayParams(params) => {
                // Updates once per render.
                // self.state.new_display_params = Some(params);
            }
            ControlMessage::KeyboardInput {
                evdev_scancode,
                char,
                state,
            } => {
                trace!(evdev_scancode, ?char, ?state, "keyboard input");
                // TODO text input
                self.state
                    .default_seat
                    .keyboard_input(&self.state.serial, evdev_scancode, state);
            }
            ControlMessage::PointerInput {
                x,
                y,
                button_code,
                state,
            } => {
                if let Some((id, surface_coords)) = self.state.surface_under((x, y)) {
                    let wl_surface = self
                        .state
                        .surfaces
                        .get(id)
                        .expect("surface has no entry")
                        .wl_surface
                        .clone();

                    self.state.default_seat.pointer_input(
                        &self.state.serial,
                        wl_surface,
                        surface_coords,
                        button_code,
                        state,
                    );
                }
            }

            ControlMessage::PointerEntered => {
                // Nothing to do - we update focus when the pointer moves.
            }
            ControlMessage::PointerLeft => {
                self.state.pointer_coords = None;
                self.state.default_seat.lift_pointer(&self.state.serial);
            }
            ControlMessage::PointerMotion(x, y) => {
                // if self.state.cursor_locked {
                //     return Ok(());
                // }

                self.state.pointer_coords = Some((x, y).into());

                if let Some((id, surface_coords)) = self.state.surface_under((x, y)) {
                    let wl_surface = self
                        .state
                        .surfaces
                        .get(id)
                        .expect("surface has no entry")
                        .wl_surface
                        .clone();

                    self.state.default_seat.set_pointer(
                        &self.state.serial,
                        wl_surface,
                        surface_coords,
                    );
                } else {
                    self.state.default_seat.lift_pointer(&self.state.serial);
                }
            }
            ControlMessage::RelativePointerMotion(x, y) => {
                // todo
            }
            ControlMessage::PointerAxis(x, y) => todo!(),
            ControlMessage::PointerAxisDiscrete(x, y) => todo!(),
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
    x11_display: Option<u32>,
) {
    use std::process::Command;

    if let Some(x11_display) = x11_display {
        match Command::new("glxinfo")
            .env_clear()
            .env("DISPLAY", format!(":{}", x11_display))
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
