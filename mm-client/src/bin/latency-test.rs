// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use std::{sync::Arc, time};

use anyhow::bail;
use ash::vk;
use clap::Parser;
use mm_client::{conn::*, video::*, vulkan::*};
use mm_protocol as protocol;
use tracing::{debug, error, warn};
use winit::{
    event::Event,
    event_loop::{ControlFlow, EventLoop, EventLoopBuilder},
};

const APP_DIMENSION: u32 = 256;

#[derive(Debug, Parser)]
#[command(name = "mmclient")]
#[command(about = "The Magic Mirror reference client", long_about = None)]
struct Cli {
    /// The server to connect to.
    #[arg(value_name = "HOST[:PORT]")]
    host: String,
    /// The codec to use. Defaults to h265.
    #[arg(long)]
    codec: Option<String>,
    /// The framerate to use. Defaults to 60.
    #[arg(long)]
    framerate: Option<u32>,
    /// The number of tests to run. Defaults to 256.
    #[arg(short('n'), long)]
    samples: Option<usize>,
}

pub enum AppEvent {
    StreamMessage(u64, protocol::MessageType),
    Datagram(protocol::MessageType),
    StreamClosed(u64),
    ConnectionClosed,
    VideoStreamReady(VkImage, VideoStreamParams),
    VideoFrameAvailable,
}

impl std::fmt::Debug for AppEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use AppEvent::*;

        match self {
            StreamMessage(sid, msg) => write!(f, "StreamMessage({sid}, {msg:?})"),
            Datagram(msg) => write!(f, "Datagram({msg:?})"),
            StreamClosed(sid) => write!(f, "StreamClosed({sid})"),
            ConnectionClosed => write!(f, "ConnectionClosed"),
            VideoStreamReady(_, params) => write!(f, "VideoStreamReady({params:?})"),
            VideoFrameAvailable => write!(f, "VideoFrameAvailable"),
        }
    }
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

struct LatencyTest {
    stream: VideoStream<AppEvent>,
    attachment_id: Option<u64>,
    stream_seq: Option<u64>,
    video_texture: Option<VkImage>,

    codec: protocol::VideoCodec,

    conn: BoundConn,
    attachment_sid: u64,
    last_keepalive: time::Instant,
    frames_recvd: usize,

    copy_cb: vk::CommandBuffer,
    copy_fence: vk::Fence,
    copy_buffer: VkHostBuffer,

    next_block: usize,
    block_started: time::Instant,
    num_tests: usize,
    histogram: histo::Histogram,

    first_frame_recvd: Option<time::Instant>,
    total_video_bytes: usize,

    vk: Arc<VkContext>,
}

fn main() -> anyhow::Result<()> {
    let args = Cli::parse();

    if let Ok(env_filter) = tracing_subscriber::EnvFilter::try_from_default_env() {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    } else {
        tracing_subscriber::fmt().init();
    }

    // Invisible window.
    let event_loop: EventLoop<AppEvent> = EventLoopBuilder::with_user_event().build()?;
    let window = winit::window::WindowBuilder::new()
        .with_visible(false)
        .build(&event_loop)?;
    let vk = Arc::new(VkContext::new(&window, cfg!(debug_assertions))?);

    let codec = match args.codec.as_deref() {
        Some("h264") => protocol::VideoCodec::H264,
        Some("h265") | None => protocol::VideoCodec::H265,
        Some("av1") => protocol::VideoCodec::Av1,
        Some(v) => bail!("invalid codec: {:?}", v),
    };

    let resolution = protocol::Size {
        width: APP_DIMENSION,
        height: APP_DIMENSION,
    };

    let framerate = args.framerate.unwrap_or(60);

    // Create session, attach
    let mut conn = Conn::new(&args.host)?;
    let sess = match conn.blocking_request(
        protocol::LaunchSession {
            application_name: "latency-test".to_string(),
            display_params: Some(protocol::VirtualDisplayParameters {
                resolution: Some(resolution),
                framerate_hz: framerate,
                ..Default::default()
            }),
            server_side_cursor_enabled: false,
        },
        time::Duration::from_secs(1),
    )? {
        protocol::MessageType::SessionLaunched(protocol::SessionLaunched { id, .. }) => id,
        msg => bail!("unexpected response: {}", msg),
    };

    let attachment_sid = conn.send(
        protocol::Attach {
            session_id: sess,
            attachment_type: protocol::AttachmentType::Operator.into(),
            client_name: "latency-test".to_string(),
            video_codec: codec.into(),
            server_side_cursor_enabled: false,
            ..Default::default()
        },
        None,
        false,
    )?;

    let proxy = event_loop.create_proxy();
    let conn = conn.bind_event_loop(proxy.clone());

    // Just big enough for the Y plane.
    let copy_buffer = create_host_buffer(
        &vk.device,
        vk.device_info.host_visible_mem_type_index,
        vk::BufferUsageFlags::TRANSFER_DST,
        (APP_DIMENSION * APP_DIMENSION) as usize,
    )?;

    let copy_cb = create_command_buffer(&vk.device, vk.present_queue.command_pool)?;
    let copy_fence = create_fence(&vk.device, false)?;

    let mut app = LatencyTest {
        conn,
        attachment_id: None,
        attachment_sid,

        codec,

        stream: VideoStream::new(vk.clone(), proxy.clone()),
        stream_seq: None,
        video_texture: None,
        frames_recvd: 0,
        last_keepalive: time::Instant::now(),

        copy_cb,
        copy_fence,
        copy_buffer,

        next_block: 0,
        block_started: time::Instant::now(),
        num_tests: args.samples.unwrap_or(256),
        histogram: histo::Histogram::with_buckets(10),

        first_frame_recvd: None,
        total_video_bytes: 0,

        vk: vk.clone(),
    };

    event_loop.run(|ev, el| {
        el.set_control_flow(ControlFlow::Poll);

        match app.handle(ev) {
            Ok(true) => (),
            Ok(false) => {
                debug!("done!");
                el.exit();
            }
            Err(e) => {
                error!("error: {}", e);
                el.exit();
            }
        }
    })?;

    drop(app.stream);
    unsafe {
        vk.device
            .free_command_buffers(vk.present_queue.command_pool, &[app.copy_cb]);
        vk.device.destroy_fence(app.copy_fence, None);
        destroy_host_buffer(&vk.device, &app.copy_buffer);
    }

    debug!("killing session...");
    app.conn
        .send(protocol::Detach {}, Some(attachment_sid), true)?;

    app.conn
        .send(protocol::EndSession { session_id: sess }, None, true)?;
    std::thread::sleep(time::Duration::from_millis(1000));

    debug!("disconnecting...");
    app.conn.close()?;

    println!("{}", app.histogram);

    if let Some(first_frame_recvd) = app.first_frame_recvd {
        println!(
            "transfer rate: {:.2} mpbs ({:.2}kb per frame)",
            app.total_video_bytes as f64 * 8.0
                / 1_000_000.0
                / first_frame_recvd.elapsed().as_secs_f64(),
            app.total_video_bytes as f64 / 1_000.0 / app.frames_recvd as f64
        );
    }

    Ok(())
}

impl LatencyTest {
    fn handle(&mut self, ev: Event<AppEvent>) -> anyhow::Result<bool> {
        match ev {
            Event::UserEvent(app_event) => match app_event {
                AppEvent::StreamMessage(sid, msg) if sid == self.attachment_sid => match msg {
                    protocol::MessageType::Attached(protocol::Attached {
                        session_id,
                        attachment_id,
                        ..
                    }) => {
                        if self.attachment_id.is_some() {
                            bail!("already attached");
                        } else {
                            self.attachment_id = Some(sid);
                            debug!(attachment_id, session_id, "attached to session");
                        }
                    }
                    protocol::MessageType::Error(protocol::Error {
                        err_code,
                        error_text,
                        ..
                    }) => {
                        bail!("server error: {}: {}", err_code, error_text);
                    }
                    msg => debug!("unexpected message: {}", msg),
                },
                AppEvent::Datagram(protocol::MessageType::VideoChunk(chunk)) => {
                    if self.first_frame_recvd.is_none() {
                        self.first_frame_recvd = Some(time::Instant::now());
                    }

                    if self.stream_seq.is_none() && self.attachment_id.is_some() {
                        self.stream_seq = Some(chunk.stream_seq);
                        self.stream.reset(
                            self.attachment_id.unwrap(),
                            chunk.stream_seq,
                            APP_DIMENSION,
                            APP_DIMENSION,
                            self.codec,
                        )?;
                    }

                    self.total_video_bytes += chunk.data.len();
                    self.stream.recv_chunk(chunk)?;
                }
                AppEvent::Datagram(protocol::MessageType::AudioChunk(_)) => (),
                AppEvent::VideoStreamReady(tex, params) => {
                    assert_eq!(params.width, 256);
                    assert_eq!(params.height, 256);

                    self.video_texture = Some(tex);
                }
                AppEvent::VideoFrameAvailable => {
                    if self.stream.prepare_frame()? {
                        self.frames_recvd += 1;

                        match self.frames_recvd.cmp(&100) {
                            std::cmp::Ordering::Less => (),
                            std::cmp::Ordering::Equal => {
                                debug!("starting test...");
                                self.send_space()?;
                                self.block_started = time::Instant::now();
                                self.next_block = 0;
                            }
                            std::cmp::Ordering::Greater => {
                                self.check_frame()?;
                                if self.next_block >= self.num_tests {
                                    return Ok(false);
                                }
                            }
                        }
                    }
                }
                ev => debug!("unxpected event: {:?}", ev),
            },
            Event::NewEvents(_) => {
                if self.last_keepalive.elapsed() > time::Duration::from_secs(1) {
                    self.conn
                        .send(protocol::KeepAlive {}, Some(self.attachment_sid), false)?;
                    self.last_keepalive = time::Instant::now();
                }
            }
            _ => (),
        }

        Ok(true)
    }

    fn send_space(&mut self) -> anyhow::Result<()> {
        debug!("sending space");
        self.conn.send(
            protocol::KeyboardInput {
                key: protocol::keyboard_input::Key::Space.into(),
                state: protocol::keyboard_input::KeyState::Pressed.into(),
                ..Default::default()
            },
            Some(self.attachment_sid),
            false,
        )?;

        self.conn.send(
            protocol::KeyboardInput {
                key: protocol::keyboard_input::Key::Space.into(),
                state: protocol::keyboard_input::KeyState::Released.into(),
                char: ' '.into(),
            },
            Some(self.attachment_sid),
            false,
        )?;

        Ok(())
    }

    fn check_frame(&mut self) -> anyhow::Result<()> {
        unsafe {
            self.submit_copy()?;
        }

        // Check the current block.
        if self.check_block(self.next_block.wrapping_sub(1)) {
            // Waiting...
        } else if self.check_block(self.next_block) {
            // Success!
            let elapsed = self.block_started.elapsed();
            debug!("block {} took {}ms", self.next_block, elapsed.as_millis());
            self.histogram.add(elapsed.as_millis() as u64);

            // Start the next one.
            // Sleep 10-100ms.
            use rand::Rng;
            let ms = (rand::thread_rng().gen::<u64>() % 90) + 10;
            std::thread::sleep(time::Duration::from_millis(ms));

            self.next_block += 1;
            self.block_started = time::Instant::now();
            self.send_space()?;
        } else if self.next_block > 0 {
            warn!("neither current or next block are highlighted");
        }

        if self.block_started.elapsed() > time::Duration::from_secs(3) {
            bail!("timed out waiting for block {}", self.next_block);
        }

        Ok(())
    }

    fn check_block(&mut self, idx: usize) -> bool {
        let data =
            unsafe { std::slice::from_raw_parts(self.copy_buffer.access as *mut u8, 256 * 256) };

        // Blocks are arranged in an 8x8 grid, and are 32x32 pixels.
        let idx = idx % 64;
        let y = (idx / 8) * 32 + 16;
        let x = (idx % 8) * 32 + 16;

        data[y * 256 + x] > 20
    }

    unsafe fn submit_copy(&mut self) -> anyhow::Result<()> {
        let device = &self.vk.device;
        let texture = self.video_texture.unwrap();

        // Reset the command buffer.
        device.reset_command_buffer(self.copy_cb, vk::CommandBufferResetFlags::empty())?;

        // Begin the command buffer.
        {
            let begin_info = vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::SIMULTANEOUS_USE);

            device.begin_command_buffer(self.copy_cb, &begin_info)?;
        }

        // Transfer the image to be readable.
        cmd_image_barrier(
            device,
            self.copy_cb,
            texture.image,
            vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::AccessFlags::empty(),
            vk::PipelineStageFlags::TRANSFER,
            vk::AccessFlags::TRANSFER_READ,
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
        );

        // Copy the texture to the staging buffer.
        {
            let region = vk::BufferImageCopy::builder()
                .buffer_row_length(256)
                .buffer_image_height(256)
                .image_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::PLANE_0,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .image_extent(vk::Extent3D {
                    width: 256,
                    height: 256,
                    depth: 1,
                })
                .build();

            let regions = [region];
            device.cmd_copy_image_to_buffer(
                self.copy_cb,
                texture.image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                self.copy_buffer.buffer,
                &regions,
            )
        }

        device.end_command_buffer(self.copy_cb)?;

        let submit_info = vk::SubmitInfo::builder()
            .command_buffers(&[self.copy_cb])
            .build();

        device.reset_fences(&[self.copy_fence])?;
        device.queue_submit(self.vk.present_queue.queue, &[submit_info], self.copy_fence)?;
        device.wait_for_fences(&[self.copy_fence], true, u64::MAX)?;
        Ok(())
    }
}
