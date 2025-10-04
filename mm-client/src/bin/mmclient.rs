// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use std::{sync::Arc, time};

use anyhow::{anyhow, bail};
use clap::Parser;
use ffmpeg_sys_next as ffmpeg_sys;
use mm_client::{
    audio,
    cursor::{cursor_icon_from_proto, load_cursor_image},
    delegate::{AttachmentEvent, AttachmentProxy},
    flash::Flash,
    gamepad::{spawn_gamepad_monitor, GamepadEvent},
    keys::winit_key_to_proto,
    overlay::Overlay,
    render::Renderer,
    stats::STATS,
    video::{self, VideoStreamEvent},
    vulkan,
};
use mm_client_common as client;
use mm_protocol as protocol;
use pollster::FutureExt as _;
use tracing::{debug, error, info, trace, warn};
use tracing_subscriber::Layer as _;
use winit::{event_loop::ControlFlow, window};

const DEFAULT_CONNECT_TIMEOUT: time::Duration = time::Duration::from_secs(1);
const DEFAULT_REQUEST_TIMEOUT: time::Duration = time::Duration::from_secs(30);

const MAX_FRAME_TIME: time::Duration = time::Duration::from_nanos(1_000_000_000 / 24);
const RESIZE_COOLDOWN: time::Duration = time::Duration::from_millis(500);

#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
enum Resolution {
    #[default]
    Auto,
    Height(u32),
    Custom(u32, u32),
}

impl From<&str> for Resolution {
    fn from(s: &str) -> Self {
        if s == "auto" {
            Resolution::Auto
        } else if let Some((w, h)) = s.split_once('x') {
            Resolution::Custom(
                w.parse().expect("invalid resolution width"),
                h.parse().expect("invalid resolution height"),
            )
        } else {
            Resolution::Height(s.parse().expect("invalid resolution height"))
        }
    }
}

#[derive(Debug, Parser)]
#[command(name = "mmclient")]
#[command(about = "The Magic Mirror reference client", long_about = None)]
struct Cli {
    /// The server to connect to.
    #[arg(value_name = "HOST[:PORT]")]
    host: String,
    /// The id of the app, or the ID of an existing session.
    app: Option<String>,
    /// Print a list of launchable applications and exit.
    #[arg(long)]
    list_apps: bool,
    /// Print a list of matching sessions and exit.
    #[arg(short = 'L', long)]
    list: bool,
    /// End a session (which may be specified by name or ID) and exit.
    #[arg(short = 'K', long)]
    kill: bool,
    /// Always resume an existing session, and error if none match.
    #[arg(short, long)]
    resume: bool,
    /// Always launch a new session, even if one exists that matches.
    #[arg(short, long)]
    launch: bool,
    /// On exit, automatically kill the session.
    #[arg(short = 'x', long)]
    kill_on_exit: bool,
    /// The streaming resolution to use. If not specified, this will be tied to
    /// the client resolution, and automatically change when the client window
    /// resizes.
    #[arg(long, required = false, default_value = "auto")]
    resolution: Resolution,
    /// Request 10-bit video output from the server. This will only work if
    /// both your display and the application in question support rendering
    /// HDR color.
    #[arg(long, required = false)]
    hdr: bool,
    /// The UI scale to communicate to the server. If not specified, this will
    /// be determined from the client-side window scale factor.
    #[arg(long, required = false)]
    ui_scale: Option<f64>,
    /// Video codec to use.
    #[arg(long, default_value = "h265")]
    codec: Option<String>,
    /// Framerate to render at on the server side.
    #[arg(long, default_value = "30")]
    framerate: u32,
    /// The quality preset to use, from 0-9.
    #[arg(short, long, default_value = "6")]
    preset: u32,
    /// Open in fullscreen mode.
    #[arg(long)]
    fullscreen: bool,
    /// Enable the overlay, which shows various stats.
    #[arg(long)]
    overlay: bool,
}

struct AttachmentWindow {
    configured_resolution: Resolution,
    configured_ui_scale: Option<f64>,
    configured_framerate: u32,

    window: Arc<winit::window::Window>,
    attachment: client::Attachment,
    attachment_config: client::AttachmentConfig,
    delegate: Arc<AttachmentProxy<AppEvent>>,

    session: client::Session,

    video_stream: video::VideoStream<AppEvent>,
    audio_stream: audio::AudioStream,

    renderer: Renderer,
    window_width: u32,
    window_height: u32,
    window_ui_scale: f64,

    minimized: bool,
    next_frame: time::Instant,
    last_frame_received: time::Instant,
    resize_cooldown: Option<time::Instant>,

    needs_refresh: Option<u64>,
    refresh_cooldown: Option<time::Instant>,

    cursor_modifiers: winit::keyboard::ModifiersState,
    cursor_pos: Option<(f64, f64)>,

    flash: Flash,
    overlay: Option<Overlay>,

    stats_timer: time::Instant,

    _vk: Arc<vulkan::VkContext>,
}

struct App {
    client: client::Client,
    args: Cli,
    attachment_window: Option<AttachmentWindow>,
    proxy: winit::event_loop::EventLoopProxy<AppEvent>,

    end_session_on_exit: bool,
}

pub enum AppEvent {
    VideoStreamReady(Arc<vulkan::VkImage>, video::VideoStreamParams),
    VideoFrameAvailable,
    AttachmentEvent(AttachmentEvent),
    GamepadEvent(GamepadEvent),
}

impl From<VideoStreamEvent> for AppEvent {
    fn from(event: VideoStreamEvent) -> Self {
        use VideoStreamEvent::*;

        match event {
            VideoStreamReady(tex, params) => AppEvent::VideoStreamReady(tex, params),
            VideoFrameAvailable => AppEvent::VideoFrameAvailable,
        }
    }
}

impl From<AttachmentEvent> for AppEvent {
    fn from(value: AttachmentEvent) -> Self {
        Self::AttachmentEvent(value)
    }
}

impl From<GamepadEvent> for AppEvent {
    fn from(event: GamepadEvent) -> Self {
        Self::GamepadEvent(event)
    }
}

impl std::fmt::Debug for AppEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppEvent::VideoStreamReady(_, params) => write!(f, "VideoStreamReady({params:?})"),
            AppEvent::VideoFrameAvailable => write!(f, "VideoFrameAvailable"),
            AppEvent::AttachmentEvent(ev) => std::fmt::Debug::fmt(ev, f),
            AppEvent::GamepadEvent(ev) => std::fmt::Debug::fmt(ev, f),
        }
    }
}

impl winit::application::ApplicationHandler<AppEvent> for App {
    fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        if self.attachment_window.is_none() {
            let window = match init_window(&self.args, &self.client, event_loop, &self.proxy) {
                Ok(w) => w,
                Err(e) => {
                    error!("failed to attach to session: {:#}", e);
                    event_loop.exit();
                    return;
                }
            };

            self.attachment_window = Some(window);
        }
    }

    fn window_event(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        window_id: winit::window::WindowId,
        event: winit::event::WindowEvent,
    ) {
        let Some(win) = &mut self.attachment_window else {
            return;
        };

        if win.window.id() != window_id {
            return;
        }

        if let Err(e) = win.renderer.handle_event(&event) {
            error!("renderer error: {:#}", e);
            event_loop.exit();
            return;
        }

        let res = win.handle_window_event(event);
        win.schedule_next_frame(event_loop, res);
    }

    fn device_event(
        &mut self,
        _event_loop: &winit::event_loop::ActiveEventLoop,
        _device_id: winit::event::DeviceId,
        event: winit::event::DeviceEvent,
    ) {
        let Some(win) = &mut self.attachment_window else {
            return;
        };

        let winit::event::DeviceEvent::MouseMotion { delta: (x, y) } = event else {
            return;
        };

        if let Some((x, y)) = win.motion_vector_to_attachment_space(x, y) {
            win.attachment.relative_pointer_motion(x, y)
        }
    }

    fn user_event(&mut self, event_loop: &winit::event_loop::ActiveEventLoop, event: AppEvent) {
        let Some(win) = &mut self.attachment_window else {
            return;
        };

        let res = win.handle_app_event(event_loop, &self.client, event);
        win.schedule_next_frame(event_loop, res);
    }

    fn about_to_wait(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        let Some(win) = &mut self.attachment_window else {
            return;
        };

        let res = win.idle(&self.client);
        win.schedule_next_frame(event_loop, res);
    }

    fn exiting(&mut self, _event_loop: &winit::event_loop::ActiveEventLoop) {
        if let Some(AttachmentWindow {
            attachment,
            session,
            ..
        }) = self.attachment_window.take()
        {
            debug!("detaching from session");
            match attachment.detach().block_on() {
                Ok(()) | Err(client::ClientError::Detached) => (),
                Err(err) => error!(?err, "failed to detach cleanly"),
            }

            if self.end_session_on_exit {
                debug!("ending session");

                match self
                    .client
                    .end_session(session.id, DEFAULT_REQUEST_TIMEOUT)
                    .block_on()
                {
                    Ok(()) => (),
                    Err(client::ClientError::ServerError(err))
                        if err.err_code() == protocol::error::ErrorCode::ErrorSessionNotFound => {}
                    Err(err) => error!(?err, "failed to end session"),
                }
            }
        }
    }
}

impl AttachmentWindow {
    fn handle_window_event(&mut self, event: winit::event::WindowEvent) -> anyhow::Result<bool> {
        trace!(?event, "handling window event");

        use winit::event::*;
        match event {
            WindowEvent::RedrawRequested => {
                self.video_stream.prepare_frame()?;
                self.video_stream.mark_frame_rendered();

                if !self.minimized && self.video_stream.is_ready() {
                    unsafe {
                        self.renderer.render(|ui| {
                            self.flash.build(ui)?;
                            if let Some(ref mut overlay) = self.overlay {
                                overlay.build(ui)?;
                            }

                            Ok(())
                        })?;
                    };
                }

                self.next_frame = time::Instant::now() + MAX_FRAME_TIME;
            }
            WindowEvent::CloseRequested => return Ok(false),
            WindowEvent::Resized(size) => {
                if size.width == 0 || size.height == 0 {
                    self.minimized = true;
                } else {
                    debug!("resize event: {}x{}", size.width, size.height);
                    if size.width != self.window_width || size.height != self.window_height {
                        if let Some(ref mut overlay) = self.overlay {
                            overlay.reposition();
                        }

                        // Trigger a stream resize, but debounce first.
                        self.resize_cooldown = Some(time::Instant::now() + RESIZE_COOLDOWN);
                    }

                    self.minimized = false;
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                debug!("window scale factor changed to {}", scale_factor);

                // Winit sends us a Resized event, immediately after this
                // one, with the new physical resolution.
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.cursor_modifiers = modifiers.state();
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: winit::keyboard::PhysicalKey::Code(code),
                        logical_key,
                        state,
                        repeat,
                        ..
                    },
                ..
            } => {
                if state == ElementState::Pressed
                    && logical_key == winit::keyboard::Key::Character("d".into())
                    && self.cursor_modifiers.control_key()
                {
                    return Ok(false);
                } else {
                    let char = match logical_key {
                        winit::keyboard::Key::Character(text) => text.chars().next(),
                        _ => None,
                    };

                    let state = match state {
                        _ if repeat => client::input::KeyState::Repeat,
                        ElementState::Pressed => client::input::KeyState::Pressed,
                        ElementState::Released => client::input::KeyState::Released,
                    };

                    let key = winit_key_to_proto(code);
                    if key == protocol::keyboard_input::Key::Unknown {
                        debug!("unknown key: {:?}", code);
                    } else {
                        self.attachment
                            .keyboard_input(key, state, char.map_or(0, Into::into));
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let new_position = self.renderer.get_texture_aspect().and_then(|aspect| {
                    // Calculate coordinates in [-1.0, 1.0];
                    let (clip_x, clip_y) = (
                        (position.x / self.window_width as f64) * 2.0 - 1.0,
                        (position.y / self.window_height as f64) * 2.0 - 1.0,
                    );

                    // Stretch the space to account for letterboxing.
                    let clip_x = clip_x * aspect.0;
                    let clip_y = clip_y * aspect.1;

                    // In the letterbox.
                    if clip_x.abs() > 1.0 || clip_y.abs() > 1.0 {
                        return None;
                    }

                    // Convert to texture coordinates.
                    let x = (clip_x + 1.0) / 2.0;
                    let y = (clip_y + 1.0) / 2.0;

                    // Convert the position to physical coordinates in the remote display.
                    let cursor_x = x * self.attachment_config.width as f64;
                    let cursor_y = y * self.attachment_config.height as f64;

                    Some((cursor_x, cursor_y))
                });

                if let Some((cursor_x, cursor_y)) = new_position {
                    self.attachment.pointer_motion(cursor_x, cursor_y);

                    if new_position.is_some() && self.cursor_pos.is_none() {
                        self.attachment.pointer_entered();
                    } else if new_position.is_none() && self.cursor_pos.is_some() {
                        self.attachment.pointer_left();
                    }

                    self.cursor_pos = new_position;
                }
            }
            WindowEvent::CursorEntered { .. } => {
                // Handled on the CursorMoved event.
            }
            WindowEvent::CursorLeft { .. } => {
                if self.cursor_pos.take().is_some() {
                    self.attachment.pointer_left()
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                use protocol::pointer_input::*;

                if self.cursor_pos.is_none() {
                    return Ok(true);
                }

                let button = match button {
                    winit::event::MouseButton::Left => Button::Left,
                    winit::event::MouseButton::Right => Button::Right,
                    winit::event::MouseButton::Middle => Button::Middle,
                    winit::event::MouseButton::Back => Button::Back,
                    winit::event::MouseButton::Forward => Button::Forward,
                    winit::event::MouseButton::Other(id) => {
                        debug!("skipping unknown mouse button: {}", id);
                        return Ok(true);
                    }
                };

                let state = match state {
                    ElementState::Pressed => ButtonState::Pressed,
                    ElementState::Released => ButtonState::Released,
                };

                let (cursor_x, cursor_y) = self.cursor_pos.unwrap();
                self.attachment
                    .pointer_input(button, state, cursor_x, cursor_y);
            }
            WindowEvent::MouseWheel {
                delta: MouseScrollDelta::LineDelta(x, y),
                phase: TouchPhase::Moved,
                ..
            } => self.attachment.pointer_scroll(
                client::input::ScrollType::Discrete,
                x as f64,
                y as f64,
            ),
            WindowEvent::MouseWheel {
                delta: MouseScrollDelta::PixelDelta(vector),
                phase: TouchPhase::Moved,
                ..
            } => {
                if let Some((x, y)) = self.motion_vector_to_attachment_space(vector.x, vector.y) {
                    self.attachment
                        .pointer_scroll(client::input::ScrollType::Continuous, x, y);
                }
            }
            _ => (),
        }

        Ok(true)
    }

    fn handle_app_event(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        client: &client::Client,
        event: AppEvent,
    ) -> anyhow::Result<bool> {
        trace!(?event, "handling event");

        use AttachmentEvent::*;
        match event {
            AppEvent::AttachmentEvent(ev) => match ev {
                VideoStreamStart(stream_seq, params) => {
                    self.attachment_config.video_stream_seq_offset =
                        stream_seq.max(self.attachment_config.video_stream_seq_offset);
                    self.video_stream.reset(
                        stream_seq,
                        params.width,
                        params.height,
                        params.codec,
                    )?;
                    self.needs_refresh = None;
                }
                VideoPacket(packet) => {
                    self.last_frame_received = time::Instant::now();
                    self.video_stream.recv_packet(packet)?;
                }
                DroppedVideoPacket(dropped) => {
                    // Only request a keyframe once every ten seconds.
                    if dropped.hierarchical_layer == 0 {
                        self.needs_refresh = Some(dropped.stream_seq);
                    }
                }
                AudioStreamStart(stream_seq, params) => {
                    self.attachment_config.audio_stream_seq_offset =
                        stream_seq.max(self.attachment_config.audio_stream_seq_offset);
                    self.audio_stream.reset(
                        stream_seq,
                        params.sample_rate,
                        params.channels.len() as u32,
                    )?;
                }
                AudioPacket(packet) => {
                    self.audio_stream.recv_packet(packet)?;
                }
                UpdateCursor {
                    icon,
                    image,
                    hotspot_x,
                    hotspot_y,
                } => {
                    if let Some(image) = image {
                        if let Ok(cursor) = load_cursor_image(&image, hotspot_x, hotspot_y)
                            .map(|src| event_loop.create_custom_cursor(src))
                        {
                            self.window.set_cursor(cursor);
                            self.window.set_cursor_visible(true);
                        } else {
                            error!(image_len = image.len(), "custom cursor image update failed");
                        }
                    } else if icon == protocol::update_cursor::CursorIcon::None {
                        self.window.set_cursor_visible(false);
                    } else {
                        self.window.set_cursor(cursor_icon_from_proto(icon));
                        self.window.set_cursor_visible(true);
                    }
                }
                LockPointer(x, y) => {
                    debug!(x, y, "Locking cursor.");

                    // On most platforms, we have to lock the cursor before we
                    // warp it. On mac, it's the other way around.
                    #[cfg(not(target_vendor = "apple"))]
                    self.window
                        .set_cursor_grab(winit::window::CursorGrabMode::Locked)
                        .or_else(|_| { 
                            debug!("Could not lock cursor. Falling back to confining cursor.");
                            self.window.set_cursor_grab(winit::window::CursorGrabMode::Confined)
                        })?;

                    if let Some(aspect) = self.renderer.get_texture_aspect() {
                        let width = self.attachment_config.width;
                        let height = self.attachment_config.height;

                        // Map vector to [-0.5, 0.5].
                        let x = (x / width as f64) - 0.5;
                        let y = (y / height as f64) - 0.5;

                        // Squish the space to account for letterboxing.
                        let x = x / aspect.0;
                        let y = y / aspect.1;

                        // Map to the screen size.
                        let x = (x + 0.5) * self.window_width as f64;
                        let y = (y + 0.5) * self.window_height as f64;

                        let pos: winit::dpi::PhysicalPosition<f64> = (x, y).into();
                        self.window.set_cursor_position(pos)?;
                    }
                    
                    #[cfg(target_vendor = "apple")]
                    self.window
                        .set_cursor_grab(winit::window::CursorGrabMode::Locked)?;
                }
                ReleasePointer => {
                    self.window
                        .set_cursor_grab(winit::window::CursorGrabMode::None)?;
                }
                DisplayParamsChanged {
                    params,
                    reattach_required,
                } => {
                    if reattach_required {
                        self.attachment_config.width = params.width;
                        self.attachment_config.height = params.height;

                        // TODO: this blocks the app, which is not ideal.
                        // We could spawn a thread for this, or reuse one.
                        debug!("reattaching to session after resize");
                        self.attachment = client
                            .attach_session(
                                self.session.id,
                                self.attachment_config.clone(),
                                self.delegate.clone(),
                                DEFAULT_REQUEST_TIMEOUT,
                            )
                            .block_on()?;
                    }

                    self.session.display_params = params;
                }
                AttachmentEnded => {
                    info!("attachment ended by server");

                    return Ok(false);
                }
            },
            AppEvent::VideoStreamReady(texture, params) => {
                self.renderer.bind_video_texture(texture, params)?;
            }
            AppEvent::VideoFrameAvailable => {
                if self.video_stream.prepare_frame()?.is_some() {
                    self.window.request_redraw();
                }
            }
            AppEvent::GamepadEvent(gev) => match gev {
                GamepadEvent::Available(pad) => self.attachment.gamepad_available(pad),
                GamepadEvent::Unavailable(id) => self.attachment.gamepad_unavailable(id),
                GamepadEvent::Input(id, button, state) => {
                    self.attachment.gamepad_input(id, button, state)
                }
                GamepadEvent::Motion(id, axis, value) => {
                    self.attachment.gamepad_motion(id, axis, value)
                }
            },
        }

        Ok(true)
    }

    fn idle(&mut self, client: &client::Client) -> anyhow::Result<bool> {
        if self.next_frame.elapsed() > time::Duration::ZERO {
            self.window.request_redraw();
        }

        if self.stats_timer.elapsed() > time::Duration::from_millis(100) {
            STATS.set_connection_rtt(client.stats().rtt)
        }

        let last_frame = self.last_frame_received.elapsed();
        if last_frame > time::Duration::from_secs(1) {
            if last_frame > DEFAULT_REQUEST_TIMEOUT {
                // TODO: this fires when we've tabbed away.
                bail!("timed out waiting for video frames");
            } else {
                self.flash.set_message("waiting for server...");
            }
        }

        // Debounced processing of the resize event.
        if self.resize_cooldown.is_some()
            && self.resize_cooldown.unwrap().elapsed() > time::Duration::ZERO
        {
            let size = self.window.inner_size();
            let scale_factor = self.window.scale_factor();

            if size.width != self.window_width
                || size.height != self.window_height
                || scale_factor != self.window_ui_scale
            {
                debug!(
                    width = size.width,
                    height = size.height,
                    scale_factor,
                    "window resized"
                );

                self.window_width = size.width;
                self.window_height = size.height;
                self.window_ui_scale = scale_factor;

                let desired_ui_scale = determine_ui_scale(
                    self.configured_ui_scale
                        .unwrap_or(self.window.scale_factor()),
                );

                let (desired_width, desired_height) = determine_resolution(
                    self.configured_resolution,
                    self.window_width,
                    self.window_height,
                );

                let desired_params = client::display_params::DisplayParams {
                    width: desired_width,
                    height: desired_height,
                    ui_scale: desired_ui_scale,
                    framerate: self.configured_framerate,
                };

                // Update the session to match our desired resolution or
                // scale. Note that this is skipped if there is no
                // current attachment (and `current_streaming_res` is
                // None).
                if desired_params != self.session.display_params {
                    debug!(
                        "resizing session to {}x{}@{} (scale: {})",
                        desired_width, desired_height, self.configured_framerate, desired_ui_scale,
                    );

                    self.flash.set_message("resizing...");

                    // TODO: this blocks the app.
                    client
                        .update_session_display_params(
                            self.session.id,
                            desired_params,
                            DEFAULT_REQUEST_TIMEOUT,
                        )
                        .block_on()?;
                }
            }

            self.resize_cooldown = None;
        }

        // Request a video refresh if we need one, but only every ten seconds.
        if self.needs_refresh.is_some()
            && self
                .refresh_cooldown
                .is_none_or(|t| t.elapsed() > time::Duration::from_secs(10))
        {
            let stream_seq = self.needs_refresh.unwrap();

            debug!(stream_seq, "requesting video refresh");
            self.attachment.request_video_refresh(stream_seq);
            self.refresh_cooldown = Some(time::Instant::now());
            self.needs_refresh = None;
        }

        Ok(true)
    }

    fn schedule_next_frame(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        res: anyhow::Result<bool>,
    ) {
        match res {
            Ok(true) => {
                event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
            }
            Ok(false) => event_loop.exit(),
            Err(e) => {
                error!("{:#}", e);
                event_loop.exit()
            }
        }
    }

    fn motion_vector_to_attachment_space(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        let (aspect_x, aspect_y) = self.renderer.get_texture_aspect()?;

        // Map vector to [0, 1]. (It can also be negative.)
        let (x, y) = (
            (x / self.window_width as f64),
            (y / self.window_height as f64),
        );

        // Stretch the space to account for letterboxing. For
        // example, if the video texture only takes up one third
        // of the screen vertically, and we scroll up one third
        // of the window height, the resulting vector should be [0,
        // -1.0].
        let x = x * aspect_x;
        let y = y * aspect_y;

        Some((
            x * self.attachment_config.width as f64,
            y * self.attachment_config.height as f64,
        ))
    }
}

pub fn main() -> anyhow::Result<()> {
    init_logging()?;

    let args = Cli::parse();
    let cmds: u8 = vec![
        args.list_apps,
        args.list,
        args.kill,
        args.launch,
        args.resume,
    ]
    .into_iter()
    .map(|b| b as u8)
    .sum();
    if cmds > 1 {
        bail!("only one of --launch, --resume, --list, or --kill may be specified");
    } else if !(args.list || args.list_apps) && args.app.is_none() {
        bail!("an app name or session ID must be specified");
    } else if args.list_apps && args.app.is_some() {
        bail!("an app name or session ID may not be specified alongside --list-apps")
    }

    debug!("establishing connection to {:}", &args.host);
    let client = client::Client::new(&args.host, "mmclient", DEFAULT_CONNECT_TIMEOUT).block_on()?;

    if args.list_apps {
        return cmd_list_apps(&client);
    } else if args.list {
        return cmd_list_sessions(&args, &client);
    } else if args.kill {
        return cmd_kill(&args, &client);
    }

    let event_loop = winit::event_loop::EventLoop::with_user_event().build()?;
    let proxy = event_loop.create_proxy();

    let end_session_on_exit = args.kill_on_exit;
    let mut app = App {
        client,
        args,
        attachment_window: None,
        proxy,

        end_session_on_exit,
    };

    event_loop.run_app(&mut app)?;

    Ok(())
}

fn init_window(
    args: &Cli,
    client: &client::Client,
    event_loop: &winit::event_loop::ActiveEventLoop,
    proxy: &winit::event_loop::EventLoopProxy<AppEvent>,
) -> anyhow::Result<AttachmentWindow> {
    let sessions = client.list_sessions(DEFAULT_REQUEST_TIMEOUT).block_on()?;
    let target = args.app.clone().unwrap();
    let matched = filter_sessions(sessions, args.app.as_ref().unwrap());

    if !args.launch && matched.len() > 1 {
        bail!(
            "multiple sessions found matching {:?}, specify a session ID to attach or use \
             --launch to create a new one.",
            target,
        );
    } else if args.resume && matched.is_empty() {
        bail!("no session found matching {:?}", target);
    }

    let configured_codec = match args.codec.as_deref() {
        Some("h264") => client::codec::VideoCodec::H264,
        Some("h265") | None => client::codec::VideoCodec::H265,
        Some("av1") => client::codec::VideoCodec::Av1,
        Some(v) => bail!("invalid codec: {:?}", v),
    };

    let configured_profile = if args.hdr {
        protocol::VideoProfile::Hdr10
    } else {
        protocol::VideoProfile::Hd
    };

    let session = if args.launch || matched.is_empty() {
        None
    } else {
        Some(matched[0].clone())
    };

    let window_attr = if args.fullscreen {
        window::Window::default_attributes()
            .with_fullscreen(Some(window::Fullscreen::Borderless(None)))
    } else {
        window::Window::default_attributes()
    };

    let window = Arc::new(event_loop.create_window(window_attr)?);
    let vk = unsafe {
        Arc::new(vulkan::VkContext::new(
            window.clone(),
            cfg!(debug_assertions),
        )?)
    };

    let renderer = Renderer::new(vk.clone(), window.clone(), args.hdr)?;

    let window_size = window.inner_size();
    let window_ui_scale = window.scale_factor();

    let (width, height) =
        determine_resolution(args.resolution, window_size.width, window_size.height);

    let desired_params = client::display_params::DisplayParams {
        width,
        height,
        framerate: args.framerate,
        ui_scale: determine_ui_scale(args.ui_scale.unwrap_or(window_ui_scale)),
    };

    let initial_gamepads = spawn_gamepad_monitor(proxy.clone())?;

    let session_id = if let Some(session) = session {
        if session.display_params != desired_params {
            debug!("updating session params to {:?}", desired_params);
            client
                .update_session_display_params(session.id, desired_params, DEFAULT_REQUEST_TIMEOUT)
                .block_on()?;
        }

        session.id
    } else {
        let target = args.app.as_ref().unwrap();
        let target = target.rsplit("/").next().unwrap();

        info!("launching a new session for for app {:?}", target);

        client
            .launch_session(
                target.into(),
                desired_params.clone(),
                initial_gamepads.clone(),
                DEFAULT_REQUEST_TIMEOUT,
            )
            .block_on()?
            .id
    };

    // Refetch the session params.
    let session = client
        .list_sessions(DEFAULT_REQUEST_TIMEOUT)
        .block_on()?
        .into_iter()
        .find(|s| s.id == session_id)
        .ok_or(anyhow!("new session not found in session list"))?;

    let now = time::Instant::now();

    let mut flash = Flash::new();
    flash.set_message("connecting...");

    let overlay = if args.overlay {
        Some(Overlay::new(args.framerate))
    } else {
        None
    };

    let delegate = Arc::new(AttachmentProxy::new(proxy.clone()));

    let audio_stream = audio::AudioStream::new()?;
    let video_stream = video::VideoStream::new(vk.clone(), proxy.clone());
    spawn_gamepad_monitor(proxy.clone())?;

    let attachment_config = client::AttachmentConfig {
        width: session.display_params.width,
        height: session.display_params.height,
        video_codec: Some(configured_codec),
        video_profile: Some(configured_profile),
        quality_preset: Some(args.preset + 1),
        audio_codec: None,
        sample_rate: None,
        channels: Vec::new(),
        video_stream_seq_offset: 0,
        audio_stream_seq_offset: 0,
    };

    debug!(session_id = session.id, "attaching to session");
    let attachment = client
        .attach_session(
            session.id,
            attachment_config.clone(),
            delegate.clone(),
            DEFAULT_REQUEST_TIMEOUT,
        )
        .block_on()?;

    Ok(AttachmentWindow {
        configured_resolution: args.resolution,
        configured_framerate: args.framerate,
        configured_ui_scale: args.ui_scale,

        window,
        attachment,
        attachment_config,
        delegate,

        session,

        video_stream,
        audio_stream,

        renderer,
        window_width: window_size.width,
        window_height: window_size.height,
        window_ui_scale,

        minimized: false,
        next_frame: now + MAX_FRAME_TIME,
        last_frame_received: now,
        resize_cooldown: None,

        needs_refresh: None,
        refresh_cooldown: None,

        cursor_modifiers: winit::keyboard::ModifiersState::default(),
        cursor_pos: None,

        flash,
        overlay,

        stats_timer: now,

        _vk: vk,
    })
}

fn init_logging() -> anyhow::Result<()> {
    if cfg!(feature = "tracy") {
        use tracing_subscriber::layer::SubscriberExt;

        let filter = tracing_subscriber::EnvFilter::builder()
            .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
            .from_env()?
            .add_directive("mmclient=trace".parse()?)
            .add_directive("mm_client=trace".parse()?)
            .add_directive("mm_client_common=trace".parse()?);

        tracing::subscriber::set_global_default(
            tracing_subscriber::registry()
                .with(tracing_tracy::TracyLayer::default().with_filter(filter)),
        )
        .expect("setup tracy layer");
    } else if let Ok(env_filter) = tracing_subscriber::EnvFilter::try_from_default_env() {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    } else {
        let filter = tracing_subscriber::EnvFilter::builder()
            .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
            .from_env()?
            .add_directive("mmclient=info".parse()?)
            .add_directive("mm_client=info".parse()?)
            .add_directive("mm_client_common=info".parse()?);
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }

    // Squash ffmpeg logs.
    unsafe {
        ffmpeg_sys::av_log_set_level(ffmpeg_sys::AV_LOG_QUIET);
        // TODO: the callback has to be variadic, which means using nightly
        // rust.
        // ffmpeg_sys::av_log_set_callback(Some(ffmpeg_log_callback))
    }

    Ok(())
}

fn determine_ui_scale(scale_factor: f64) -> client::pixel_scale::PixelScale {
    let scale = match scale_factor {
        x if x < 1.0 => client::pixel_scale::PixelScale::ONE,
        _ => {
            // Multiplying by 6/6 captures most possible fractional scales.
            let numerator = (scale_factor * 6.0).round() as u32;
            let denominator = 6;
            if numerator % denominator == 0 {
                client::pixel_scale::PixelScale::new(numerator / denominator, 1)
            } else {
                client::pixel_scale::PixelScale::new(numerator, denominator)
            }
        }
    };

    if scale.is_fractional() {
        let rounded = scale.round_up();
        warn!(
            requested = %scale,
            using = %rounded,
            "fractional scale not supported, rounding up"
        );

        return rounded;
    }

    scale
}

fn determine_resolution(resolution: Resolution, width: u32, height: u32) -> (u32, u32) {
    match resolution {
        Resolution::Auto => (width.next_multiple_of(2), height.next_multiple_of(2)),
        Resolution::Height(h) => {
            let h = std::cmp::min(h, height).next_multiple_of(2);
            let w = (h * width / height).next_multiple_of(2);
            (w, h)
        }
        Resolution::Custom(w, h) => (w, h),
    }
}

fn filter_sessions(sessions: Vec<client::Session>, app: &str) -> Vec<client::Session> {
    if let Ok(id) = app.parse::<u64>() {
        return match sessions.into_iter().find(|s| s.id == id) {
            Some(s) => vec![s],
            None => vec![],
        };
    }

    sessions
        .into_iter()
        .filter(|s| s.application_id == app)
        .collect()
}

fn cmd_list_apps(client: &client::Client) -> anyhow::Result<()> {
    let apps = client
        .list_applications(DEFAULT_REQUEST_TIMEOUT)
        .block_on()?;
    if apps.is_empty() {
        println!("No launchable applications found.");
        return Ok(());
    }

    let mut apps = apps
        .into_iter()
        .map(|app| {
            let mut name = String::new();
            for dir in &app.folder {
                name.push_str(dir);
                name.push('/');
            }
            name.push_str(&app.id);

            (name, app.description)
        })
        .collect::<Vec<_>>();
    apps.sort();

    let mut tw = tabwriter::TabWriter::new(std::io::stdout()).padding(4);

    use std::io::Write as _;
    writeln!(&mut tw, "Name\tDescription")?;
    writeln!(&mut tw, "----\t-----------")?;

    for (name, desc) in apps {
        if desc.len() <= 80 {
            writeln!(&mut tw, "{}\t{}", name, desc)?;
        } else {
            writeln!(&mut tw, "{}\t{}...", name, &desc[..77])?;
        }
    }

    tw.flush()?;
    Ok(())
}

fn cmd_list_sessions(args: &Cli, client: &client::Client) -> anyhow::Result<()> {
    let sessions = client.list_sessions(DEFAULT_REQUEST_TIMEOUT).block_on()?;
    let sessions = if let Some(target) = args.app.as_ref() {
        filter_sessions(sessions, target)
    } else {
        sessions
    };

    if sessions.is_empty() {
        println!("No (matching) sessions found.");
        return Ok(());
    }

    let now = time::SystemTime::now();
    let mut tw = tabwriter::TabWriter::new(std::io::stdout()).padding(4);

    use std::io::Write as _;
    writeln!(&mut tw, "Session ID\tApplication Name\tRuntime")?;
    writeln!(&mut tw, "----------\t----------------\t-------")?;

    for session in sessions {
        let runtime = {
            // Round to seconds.
            let secs = now.duration_since(session.start)?.as_secs();
            humantime::format_duration(time::Duration::from_secs(secs)).to_string()
        };

        writeln!(
            &mut tw,
            "{}\t{}\t{}",
            session.id, session.application_id, runtime,
        )?;
    }

    tw.flush()?;
    Ok(())
}

fn cmd_kill(args: &Cli, client: &client::Client) -> anyhow::Result<()> {
    let target = args.app.as_ref().unwrap();
    let sessions = filter_sessions(
        client.list_sessions(DEFAULT_REQUEST_TIMEOUT).block_on()?,
        target,
    );

    if sessions.is_empty() {
        println!("No (matching) sessions found.");
        return Ok(());
    } else if sessions.len() > 1 {
        bail!("Multiple sessions matched!");
    }

    client
        .end_session(sessions[0].id, DEFAULT_REQUEST_TIMEOUT)
        .block_on()?;
    Ok(())
}
