// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use std::io::Write;
use std::rc::Rc;
use std::sync::Arc;
use std::time;

use anyhow::anyhow;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use clap::Parser;
use ffmpeg_sys_next as ffmpeg_sys;
use mm_client::audio;
use mm_client::conn::ConnEvent;
use mm_client::video;
use mm_client::video::VideoStreamEvent;
use mm_protocol as protocol;
use protocol::MessageType;
use tracing::info;
use tracing::trace;
use tracing::{debug, error};
use tracing_subscriber::Layer;
use winit::event::ElementState;
use winit::event::KeyEvent;
use winit::event_loop::ControlFlow;
use winit::event_loop::EventLoopBuilder;
use winit::event_loop::EventLoopProxy;
use winit::keyboard::ModifiersState;
use winit::{
    event::{Event, WindowEvent},
    event_loop::EventLoop,
    window::WindowBuilder,
};

use mm_client::conn::*;
use mm_client::keys::winit_key_to_proto;
use mm_client::overlay::Overlay;
use mm_client::render::Renderer;
use mm_client::vulkan;

const INIT_TIMEOUT: time::Duration = time::Duration::from_secs(10);
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
    /// The name of the app, or the ID of an existing session.
    app: Option<String>,
    /// Print a list of matching sessions and exit.
    #[arg(short = 'L', long)]
    list: bool,
    /// End a session (which must be specified by ID) and exit.
    #[arg(short = 'K', long)]
    kill: bool,
    /// Always resume an existing session, and error if none match.
    #[arg(short, long)]
    resume: bool,
    /// Always launch a new session, even if one exists that matches.
    #[arg(short, long)]
    launch: bool,
    /// On exit, only detach, and do not end the session.
    #[arg(short, long)]
    detach: bool,
    /// The streaming resolution to use. If not specified, this will be tied to
    /// the client resolution, and automatically change when the client window
    /// resizes.
    #[arg(long, required = false, default_value = "auto")]
    resolution: Resolution,
    /// The UI scale to communicate to the server. If not specified, this will
    /// be determined from the client-side window scale factor.
    #[arg(long)]
    ui_scale: Option<f64>,
    /// Video codec to use.
    #[arg(long, default_value = "h265")]
    codec: Option<String>,
    /// Framerate to render at on the server side.
    #[arg(long, default_value = "30")]
    framerate: u32,
    /// Open in fullscreen mode.
    #[arg(long)]
    fullscreen: bool,
}

struct App {
    configured_resolution: Resolution,
    configured_codec: protocol::VideoCodec,
    configured_framerate: u32,

    window: Rc<winit::window::Window>,
    _proxy: EventLoopProxy<AppEvent>,

    exiting: bool,
    reattaching: bool,
    conn: BoundConn,
    session_id: u64,

    remote_display_params: protocol::VirtualDisplayParameters,
    attachment: Option<protocol::Attached>,

    attachment_sid: u64,
    last_keepalive: time::Instant,
    end_session_on_exit: bool,

    video_stream: video::VideoStream<AppEvent>,
    video_stream_seq: Option<u64>,
    audio_stream: audio::AudioStream,
    audio_stream_seq: Option<u64>,

    renderer: Renderer,
    window_width: u32,
    window_height: u32,
    window_ui_scale: f64,

    minimized: bool,
    next_frame: time::Instant,
    resize_cooldown: Option<time::Instant>,
    last_frame_received: time::Instant,

    cursor_modifiers: ModifiersState,
    cursor_x: f64,
    cursor_y: f64,
    cursor_entered: bool,

    overlay: Overlay,

    _vk: Arc<vulkan::VkContext>,
}

impl App {
    fn handle(&mut self, event: winit::event::Event<AppEvent>) -> anyhow::Result<bool> {
        if tracing::event_enabled!(tracing::Level::TRACE) {
            let event_debug = match event {
                Event::UserEvent(AppEvent::StreamMessage(_, ref msg)) => {
                    format!("StreamMessage({})", msg)
                }
                Event::UserEvent(AppEvent::Datagram(ref msg)) => format!("Datagram({})", msg),
                _ => format!("{:?}", event),
            };
            trace!(event = event_debug, "handling event");
        }

        self.renderer.handle_event(&event)?;

        match event {
            Event::UserEvent(AppEvent::ConnectionClosed) => {
                bail!("connection closed unexpectedly")
            }
            Event::UserEvent(AppEvent::StreamMessage(_, msg)) => match msg {
                protocol::MessageType::SessionEnded(_) => {
                    if !self.exiting {
                        info!("session ended by server");
                    }

                    return Ok(false);
                }
                protocol::MessageType::SessionParametersChanged(params) => {
                    if params.display_params.is_none()
                        || params.display_params.as_ref().unwrap().resolution.is_none()
                        || params.display_params.as_ref().unwrap().ui_scale.is_none()
                    {
                        bail!("session parameters changed without valid display params");
                    }

                    let new_params = params.display_params.unwrap();
                    self.remote_display_params = new_params;
                    if params.reattach_required {
                        // The server is about to close the stream. We'll
                        // reattach once that happens.
                        self.attachment = None;
                        self.reattaching = true;
                    }
                }
                protocol::MessageType::Attached(msg) => {
                    info!(msg.session_id, "successfully (re)attached to session");
                    self.reattaching = false;
                    self.attachment = Some(msg.clone());
                }
                protocol::MessageType::SessionUpdated(_) => {}
                protocol::MessageType::Error(e) => {
                    error!("server error: {:#}", server_error(e));
                }
                _ => bail!("unexpected message: {:?}", msg),
            },
            Event::UserEvent(AppEvent::StreamClosed(sid)) => {
                if sid == self.attachment_sid {
                    if self.exiting {
                        return Ok(false);
                    } else if self.reattaching {
                        // Open a new attachment stream.
                        debug!(
                            "reattaching to session with resolution: {:?}",
                            self.remote_display_params.resolution.clone()
                        );
                        self.attachment_sid = self.conn.send(
                            protocol::Attach {
                                attachment_type: protocol::AttachmentType::Operator.into(),
                                client_name: "mmclient".to_string(),
                                session_id: self.session_id,
                                streaming_resolution: self.remote_display_params.resolution.clone(),
                                video_codec: self.configured_codec.into(),
                                server_side_cursor_enabled: false,
                                ..Default::default()
                            },
                            None,
                            false,
                        )?;
                    } else {
                        bail!("server disconnected from session")
                    }
                }
            }
            Event::UserEvent(AppEvent::Datagram(msg)) => match msg {
                protocol::MessageType::VideoChunk(chunk) => {
                    self.last_frame_received = time::Instant::now();

                    // Detect stream restarts. In the case that we're
                    // reattaching, two unordered things have to happen: we have
                    // to get the new attachment ID, and we have to get a
                    // datagram with a new stream seq.
                    if let Some(attachment) = &self.attachment {
                        if chunk.attachment_id == attachment.attachment_id
                            && (self.video_stream_seq.is_none()
                                || chunk.stream_seq > self.video_stream_seq.unwrap())
                        {
                            let protocol::Size { width, height } =
                                attachment.streaming_resolution.clone().unwrap();
                            self.video_stream_seq = Some(chunk.stream_seq);
                            self.video_stream.reset(
                                chunk.stream_seq,
                                width,
                                height,
                                self.configured_codec,
                            )?;
                        }
                    }

                    self.video_stream.recv_chunk(chunk)?;
                    self.overlay.clear_message();
                }
                protocol::MessageType::AudioChunk(chunk) => {
                    // Detect stream restarts.
                    if let Some(attachment) = &self.attachment {
                        if chunk.attachment_id == attachment.attachment_id
                            && (self.audio_stream_seq.is_none()
                                || chunk.stream_seq > self.audio_stream_seq.unwrap())
                        {
                            self.audio_stream_seq = Some(chunk.stream_seq);
                            self.audio_stream.reset(
                                chunk.stream_seq,
                                attachment.sample_rate_hz,
                                attachment.channels.as_ref().unwrap().channels.len() as u32,
                            )?;
                        }
                    }

                    self.audio_stream.recv_chunk(chunk)?;
                }
                _ => bail!("unexpected datagram: {}", msg),
            },
            Event::UserEvent(AppEvent::VideoStreamReady(texture, params)) => {
                self.renderer.bind_video_texture(texture, params)?;
                self.overlay.clear_message();
            }
            Event::UserEvent(AppEvent::VideoFrameAvailable) => {
                if self.video_stream.flush_frames()? {
                    self.window.request_redraw();
                }
            }
            Event::NewEvents(_) => {
                if self.next_frame.elapsed() > time::Duration::ZERO {
                    self.window.request_redraw();
                }

                if self.last_keepalive.elapsed() > time::Duration::from_secs(1) {
                    self.conn
                        .send(protocol::KeepAlive {}, Some(self.attachment_sid), false)?;
                    self.last_keepalive = time::Instant::now();
                }

                let last_frame = self.last_frame_received.elapsed();
                if last_frame > time::Duration::from_secs(1) {
                    if last_frame > INIT_TIMEOUT {
                        // TODO: this fires when we've tabbed away.
                        bail!("timed out waiting for video frames");
                    } else {
                        self.overlay.set_message("waiting for server...");
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

                        let current_streaming_res = self
                            .attachment
                            .as_ref()
                            .and_then(|a| a.streaming_resolution.clone());
                        let remote_scale = self.remote_display_params.ui_scale.as_ref().unwrap();

                        let desired_ui_scale = determine_ui_scale(self.window.scale_factor());
                        let desired_streaming_res = Some(determine_resolution(
                            self.configured_resolution,
                            self.window_width,
                            self.window_height,
                        ));

                        // Update the session to match our desired resolution or
                        // scale. Note that this is skipped if there is no
                        // current attachment (and `current_streaming_res` is
                        // None).
                        if desired_streaming_res != current_streaming_res
                            || desired_ui_scale != *remote_scale
                        {
                            debug!(
                                "resizing session to {}x{}@{} (scale: {})",
                                desired_streaming_res.as_ref().unwrap().width,
                                desired_streaming_res.as_ref().unwrap().height,
                                self.configured_framerate,
                                desired_ui_scale.numerator as f64
                                    / desired_ui_scale.denominator as f64
                            );

                            // TODO: this is useful, but triggers when we get
                            // resized by the server, which is incorrect.
                            self.overlay.set_message("resizing...");

                            // This will trigger a new attachment at the new
                            // resolution once the server updates and notifies
                            // us.
                            self.conn.send(
                                protocol::UpdateSession {
                                    session_id: self.session_id,
                                    display_params: Some(protocol::VirtualDisplayParameters {
                                        resolution: desired_streaming_res,
                                        framerate_hz: self.configured_framerate,
                                        ui_scale: Some(desired_ui_scale),
                                    }),
                                },
                                None,
                                false,
                            )?;
                        }
                    }

                    self.resize_cooldown = None;
                }
            }
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::RedrawRequested => {
                    self.video_stream.flush_frames()?;

                    if let Some(pts) = self.video_stream.pts() {
                        self.audio_stream.sync(pts);
                    }

                    if !self.minimized && self.video_stream.is_ready() {
                        unsafe { self.renderer.render(|ui| self.overlay.build(ui))? };
                    }

                    self.next_frame = time::Instant::now() + MAX_FRAME_TIME;
                }
                WindowEvent::CloseRequested => self.detach()?,
                WindowEvent::Resized(size) => {
                    if size.width == 0 || size.height == 0 {
                        self.minimized = true;
                    } else {
                        debug!("resize event: {}x{}", size.width, size.height);
                        if size.width != self.window_width || size.height != self.window_height {
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
                    use protocol::keyboard_input::*;

                    if state == ElementState::Pressed
                        && logical_key == winit::keyboard::Key::Character("d".into())
                        && self.cursor_modifiers.control_key()
                    {
                        self.detach()?;
                    } else {
                        let char = match logical_key {
                            winit::keyboard::Key::Character(text) => {
                                text.chars().next().unwrap() as u32
                            }
                            _ => 0,
                        };

                        let state = match state {
                            _ if repeat => KeyState::Repeat,
                            ElementState::Pressed => KeyState::Pressed,
                            ElementState::Released => KeyState::Released,
                        };

                        let key = winit_key_to_proto(code);
                        if key == protocol::keyboard_input::Key::Unknown {
                            debug!("unknown key: {:?}", code);
                        } else {
                            let msg = protocol::KeyboardInput {
                                key: key.into(),
                                state: state.into(),
                                char,
                            };

                            self.conn.send(msg, Some(self.attachment_sid), false)?;
                        }
                    }
                }
                WindowEvent::CursorMoved { position, .. } => {
                    // Convert the position to a coordinate in the remote display.
                    let remote_size = self.remote_display_params.resolution.as_ref().unwrap();
                    self.cursor_x =
                        position.x * (remote_size.width as f64 / self.window_width as f64);
                    self.cursor_y =
                        position.y * (remote_size.height as f64 / self.window_height as f64);

                    let msg = protocol::PointerMotion {
                        x: self.cursor_x,
                        y: self.cursor_y,
                    };

                    self.conn.send(msg, Some(self.attachment_sid), false)?;
                }
                WindowEvent::CursorEntered { .. } => {
                    self.cursor_entered = true;
                    let msg = protocol::PointerEntered {};
                    self.conn.send(msg, Some(self.attachment_sid), false)?;
                }
                WindowEvent::CursorLeft { .. } => {
                    self.cursor_entered = false;
                    let msg = protocol::PointerLeft {};
                    self.conn.send(msg, Some(self.attachment_sid), false)?;
                }
                WindowEvent::MouseInput { state, button, .. } => {
                    use protocol::pointer_input::*;

                    if !self.cursor_entered {
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

                    let msg = protocol::PointerInput {
                        button: button.into(),
                        state: state.into(),
                        x: self.cursor_x,
                        y: self.cursor_y,
                    };

                    self.conn.send(msg, Some(self.attachment_sid), false)?;
                }
                _ => {
                    // debug!("window event: {:?}", event);
                }
            },
            _ => (),
        }

        Ok(true)
    }

    fn detach(&mut self) -> anyhow::Result<()> {
        self.exiting = true;

        if self.end_session_on_exit {
            match self.conn.send(
                protocol::EndSession {
                    session_id: self.session_id,
                },
                None,
                true,
            ) {
                Ok(_) => (),
                Err(e) => return Err(e).context("failed to end session"),
            }

            self.overlay.set_message("ending session...");
        } else {
            match self
                .conn
                .send(protocol::Detach {}, Some(self.attachment_sid), true)
            {
                Ok(_) => (),
                Err(e) => return Err(e).context("failed to detach cleanly"),
            }

            self.overlay.set_message("closing...");
        }

        Ok(())
    }
}

fn main() -> Result<()> {
    init_logging()?;

    let args = Cli::parse();
    let cmds: u8 = vec![args.list, args.kill, args.launch, args.resume]
        .into_iter()
        .map(|b| b as u8)
        .sum();
    if cmds > 1 {
        bail!("only one of --launch, --resume, --list, or --kill may be specified");
    } else if !args.list && args.app.is_none() {
        bail!("an app name or session ID must be specified");
    }

    let configured_codec = match args.codec.as_deref() {
        Some("h264") => protocol::VideoCodec::H264,
        Some("h265") | None => protocol::VideoCodec::H265,
        Some("av1") => protocol::VideoCodec::Av1,
        Some(v) => bail!("invalid codec: {:?}", v),
    };

    // TODO: anyhow errors are garbage for end-users.
    debug!("establishing connection to {:}", &args.host);
    let mut conn = Conn::new(&args.host).context("failed to establish connection")?;

    let sessions = match list_sessions(&mut conn) {
        Ok(sessions) => sessions,
        Err(e) => {
            conn.close()?;
            return Err(e);
        }
    };

    debug!("found {} running sessions", sessions.list.len());

    if args.list {
        return cmd_list(&args, sessions);
    } else if args.kill {
        match cmd_kill(&args, &mut conn, sessions) {
            Ok(_) => return Ok(()),
            Err(e) => {
                conn.close()?;
                return Err(e);
            }
        }
    }

    let event_loop: EventLoop<AppEvent> = EventLoopBuilder::with_user_event().build()?;

    let target = args.app.unwrap();
    let mut matched = find_sessions(sessions, &target);
    if !args.launch && matched.len() > 1 {
        bail!(
                "multiple sessions found matching {:?}, specify a session ID to attach or use --launch to create a new one.",
                target,
            );
    } else if args.resume && matched.is_empty() {
        bail!("no session found matching {:?}", target);
    }

    let window = if args.fullscreen {
        WindowBuilder::new()
            .with_fullscreen(Some(winit::window::Fullscreen::Borderless(None)))
            .build(&event_loop)?
    } else {
        WindowBuilder::new().build(&event_loop)?
    };

    let window = Rc::new(window);
    let window_size = window.inner_size();
    let window_ui_scale = window.scale_factor();

    let desired_params = protocol::VirtualDisplayParameters {
        resolution: Some(determine_resolution(
            args.resolution,
            window_size.width,
            window_size.height,
        )),
        framerate_hz: args.framerate,
        ui_scale: Some(determine_ui_scale(args.ui_scale.unwrap_or(window_ui_scale))),
    };

    let session_id = if args.launch || matched.is_empty() {
        info!("launching a new session for for app {:?}", target);

        let new_sess = match launch_session(&mut conn, &target, desired_params.clone()) {
            Ok(v) => v,
            Err(e) => {
                conn.close()?;
                return Err(e);
            }
        };

        new_sess.id
    } else {
        let session = matched.pop().unwrap();
        if session.display_params != Some(desired_params.clone()) {
            debug!("updating session params to {:?}", desired_params);

            match update_session(
                &mut conn,
                protocol::UpdateSession {
                    session_id: session.session_id,
                    display_params: Some(desired_params.clone()),
                },
            ) {
                Ok(_) => (),
                Err(e) => {
                    conn.close()?;
                    return Err(e);
                }
            };
        }

        session.session_id
    };

    // Refetch the session params.
    let session = list_sessions(&mut conn)?
        .list
        .into_iter()
        .find(|s| s.session_id == session_id)
        .ok_or(anyhow!("new session not found in session list"))?;

    let remote_display_params = session.display_params.unwrap();
    let streaming_resolution = remote_display_params.resolution.clone().unwrap();

    let vk = Arc::new(vulkan::VkContext::new(&window, cfg!(debug_assertions))?);
    let renderer = Renderer::new(vk.clone(), window.clone())?;

    debug!("attaching session {:?}", session.session_id);
    let attachment_sid = conn.send(
        protocol::Attach {
            attachment_type: protocol::AttachmentType::Operator.into(),
            client_name: "mmclient".to_string(),
            session_id: session.session_id,
            streaming_resolution: Some(streaming_resolution),
            video_codec: configured_codec.into(),
            server_side_cursor_enabled: false,
            ..Default::default()
        },
        None,
        false,
    )?;

    let proxy = event_loop.create_proxy();
    let conn = conn.bind_event_loop(proxy.clone());

    let audio_stream = audio::AudioStream::new()?;
    let video_stream = video::VideoStream::new(vk.clone(), proxy.clone());

    let now = time::Instant::now();

    let mut overlay = Overlay::new();
    overlay.set_message("connecting...");

    let mut app = Some(App {
        configured_codec,
        configured_framerate: args.framerate,
        configured_resolution: args.resolution,

        window,
        _proxy: proxy.clone(),

        exiting: false,
        reattaching: false,
        conn,
        session_id: session.session_id,

        attachment_sid,
        last_keepalive: now,
        end_session_on_exit: !args.detach,

        remote_display_params,
        attachment: None,

        video_stream,
        video_stream_seq: None,
        audio_stream,
        audio_stream_seq: None,

        renderer,
        window_width: window_size.width,
        window_height: window_size.height,
        window_ui_scale,

        minimized: false,
        next_frame: now + MAX_FRAME_TIME,
        resize_cooldown: None,
        last_frame_received: now,

        cursor_modifiers: ModifiersState::default(),
        cursor_x: 0.0,
        cursor_y: 0.0,
        cursor_entered: false,

        overlay,

        _vk: vk,
    });

    event_loop.run(move |event, el| {
        if app.is_some() {
            match app.as_mut().unwrap().handle(event) {
                Ok(true) => {
                    el.set_control_flow(ControlFlow::WaitUntil(app.as_ref().unwrap().next_frame));
                    return; // continue
                }
                Ok(false) => el.exit(),
                Err(e) => {
                    error!("{:#}", e);
                    el.exit()
                }
            }

            let app = app.take().unwrap(); // Drop everything.
            match app.conn.close() {
                Ok(_) => (),
                Err(e) => error!("failed to shutdown connection cleanly: {:#}", e),
            }
        }
    })?;

    Ok(())
}

pub enum AppEvent {
    StreamMessage(u64, MessageType),
    Datagram(MessageType),
    StreamClosed(u64),
    ConnectionClosed,
    VideoStreamReady(vulkan::VkImage, video::VideoStreamParams),
    VideoFrameAvailable,
}

impl From<ConnEvent> for AppEvent {
    fn from(event: ConnEvent) -> Self {
        use ConnEvent::*;

        match event {
            StreamMessage(sid, msg) => AppEvent::StreamMessage(sid, msg),
            Datagram(msg) => AppEvent::Datagram(msg),
            StreamClosed(sid) => AppEvent::StreamClosed(sid),
            ConnectionClosed => AppEvent::ConnectionClosed,
        }
    }
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

impl std::fmt::Debug for AppEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppEvent::StreamMessage(sid, msg) => write!(f, "StreamMessage({:?}, {:?})", sid, msg),
            AppEvent::Datagram(msg) => write!(f, "Datagram({:?})", msg),
            AppEvent::StreamClosed(sid) => write!(f, "StreamClosed({:?})", sid),
            AppEvent::ConnectionClosed => write!(f, "ConnectionClosed"),
            AppEvent::VideoStreamReady(_, params) => write!(f, "VideoStreamReady({:?})", params),
            AppEvent::VideoFrameAvailable => write!(f, "VideoFrameAvailable"),
        }
    }
}

fn init_logging() -> Result<()> {
    if cfg!(feature = "tracy") {
        use tracing_subscriber::layer::SubscriberExt;

        let filter = tracing_subscriber::EnvFilter::builder()
            .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
            .from_env()?
            .add_directive("mmclient=trace".parse()?)
            .add_directive("mm_client=trace".parse()?);

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
            .add_directive("mmclient=info".parse()?);
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }

    // Squash ffmpeg logs.
    unsafe {
        ffmpeg_sys::av_log_set_level(ffmpeg_sys::AV_LOG_QUIET);
        // TODO: the callback has to be variadic, which means using nightly rust.
        // ffmpeg_sys::av_log_set_callback(Some(ffmpeg_log_callback))
    }

    Ok(())
}

fn cmd_list(args: &Cli, sessions: protocol::SessionList) -> Result<()> {
    let sessions = if let Some(target) = args.app.as_ref() {
        find_sessions(sessions, target)
    } else {
        sessions.list
    };

    if sessions.is_empty() {
        println!("No (matching) sessions found.");
        return Ok(());
    }

    let now = time::SystemTime::now();
    let mut tw = tabwriter::TabWriter::new(std::io::stdout()).padding(4);
    writeln!(&mut tw, "Session ID\tApplication Name\tRuntime")?;
    writeln!(&mut tw, "----------\t----------------\t-------")?;

    for session in sessions {
        let session_start = session.session_start.and_then(|s| s.try_into().ok());
        let runtime = match session_start {
            Some(start) if start < now => {
                // Round to seconds.
                let secs = now.duration_since(start)?.as_secs();
                humantime::format_duration(time::Duration::from_secs(secs)).to_string()
            }
            _ => "".to_string(),
        };

        writeln!(
            &mut tw,
            "{}\t{}\t{}",
            session.session_id, session.application_name, runtime,
        )?;
    }

    tw.flush()?;
    Ok(())
}

fn cmd_kill(args: &Cli, conn: &mut Conn, sessions: protocol::SessionList) -> Result<()> {
    let target = args.app.as_ref().unwrap();
    let id = match target.parse::<u64>() {
        Ok(id) => id,
        Err(_) => {
            bail!("invalid session ID: {}", target);
        }
    };

    match sessions.list.into_iter().find(|s| s.session_id == id) {
        Some(s) => {
            debug!("killing session {:?}", s.session_id);
            end_session(conn, id)
        }
        None => Err(anyhow!("session not found: {}", id)),
    }
}

fn list_sessions(conn: &mut Conn) -> Result<protocol::SessionList> {
    match conn.blocking_request(protocol::ListSessions {}, INIT_TIMEOUT) {
        Ok(protocol::MessageType::SessionList(sessions)) => Ok(sessions),
        Ok(protocol::MessageType::Error(e)) => Err(server_error(e)),
        Ok(m) => Err(anyhow!("unexpected {} in response to ListSessions", m)),
        Err(e) => Err(e).context("failed to list sessions"),
    }
}

fn update_session(
    conn: &mut Conn,
    msg: protocol::UpdateSession,
) -> Result<protocol::SessionUpdated> {
    match conn.blocking_request(msg, INIT_TIMEOUT) {
        Ok(protocol::MessageType::SessionUpdated(session)) => Ok(session),
        Ok(protocol::MessageType::Error(e)) => Err(server_error(e)),
        Ok(m) => Err(anyhow!("unexpected {} in response to UpdateSession", m)),
        Err(e) => Err(e).context("failed to update session"),
    }
}

fn launch_session(
    conn: &mut Conn,
    app: &str,
    display_params: protocol::VirtualDisplayParameters,
) -> Result<protocol::SessionLaunched> {
    info!("launching session for app {:?}", app);

    match conn.blocking_request(
        protocol::LaunchSession {
            application_name: app.to_string(),
            server_side_cursor_enabled: false,
            display_params: Some(display_params),
        },
        INIT_TIMEOUT,
    ) {
        Ok(protocol::MessageType::SessionLaunched(session)) => Ok(session),
        Ok(protocol::MessageType::Error(e)) => Err(server_error(e)),
        Ok(m) => Err(anyhow!("unexpected {} in response to LaunchSession", m)),
        Err(e) => Err(e).context("failed to launch session"),
    }
}

fn end_session(conn: &mut Conn, id: u64) -> Result<()> {
    match conn.blocking_request(protocol::EndSession { session_id: id }, INIT_TIMEOUT) {
        Ok(protocol::MessageType::SessionEnded(_)) => Ok(()),
        Ok(protocol::MessageType::Error(e)) => Err(server_error(e)),
        Ok(m) => Err(anyhow!("unexpected {} in response to EndSession", m)),
        Err(e) => Err(e).context("failed to end session"),
    }
}

fn server_error(msg: protocol::Error) -> anyhow::Error {
    if msg.error_text.is_empty() {
        anyhow!("{}", msg.err_code().as_str_name())
    } else {
        anyhow!("{}: {}", msg.err_code().as_str_name(), msg.error_text)
    }
}

fn find_sessions(
    sessions: protocol::SessionList,
    app: &str,
) -> Vec<protocol::session_list::Session> {
    if let Ok(id) = app.parse::<u64>() {
        return match sessions.list.into_iter().find(|s| s.session_id == id) {
            Some(s) => vec![s],
            None => vec![],
        };
    }

    sessions
        .list
        .into_iter()
        .filter(|s| s.application_name == app)
        .collect()
}

fn determine_resolution(resolution: Resolution, width: u32, height: u32) -> protocol::Size {
    match resolution {
        Resolution::Auto => protocol::Size { width, height },
        Resolution::Height(h) => {
            let h = std::cmp::min(h, height);

            let w = (h * width / height).next_multiple_of(2);
            protocol::Size {
                width: w,
                height: h,
            }
        }
        Resolution::Custom(w, h) => protocol::Size {
            width: w,
            height: h,
        },
    }
}

fn determine_ui_scale(scale_factor: f64) -> protocol::PixelScale {
    match scale_factor {
        x if x < 1.0 => protocol::PixelScale {
            numerator: 1,
            denominator: 1,
        },
        _ => {
            // Multiplying by 6/6 captures most possible fractional scales.
            let numerator = (scale_factor * 6.0).round() as u32;
            let denominator = 6;
            if numerator % denominator == 0 {
                protocol::PixelScale {
                    numerator: numerator / denominator,
                    denominator: 1,
                }
            } else {
                protocol::PixelScale {
                    numerator,
                    denominator,
                }
            }
        }
    }
}
