// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use std::{sync::Arc, time};

use anyhow::{bail, Context as _};
use ash::vk;
use clap::Parser;
use mm_client::{
    delegate::{AttachmentEvent, AttachmentProxy},
    video::*,
    vulkan::*,
};
use mm_client_common as client;
use mm_protocol as protocol;
use pollster::FutureExt as _;
use tracing::{debug, error, warn};
use winit::event_loop::EventLoop;

const APP_DIMENSION: u32 = 256;
const DEFAULT_TIMEOUT: time::Duration = time::Duration::from_secs(1);

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
    VideoStreamReady(Arc<VkImage>, VideoStreamParams),
    VideoFrameAvailable,
    AttachmentEvent(AttachmentEvent),
}

impl std::fmt::Debug for AppEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use AppEvent::*;

        match self {
            VideoStreamReady(_, params) => write!(f, "VideoStreamReady({params:?})"),
            VideoFrameAvailable => write!(f, "VideoFrameAvailable"),
            AttachmentEvent(ev) => std::fmt::Debug::fmt(ev, f),
        }
    }
}

impl From<AttachmentEvent> for AppEvent {
    fn from(event: AttachmentEvent) -> Self {
        Self::AttachmentEvent(event)
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

struct App {
    client: client::Client,
    args: Cli,
    proxy: winit::event_loop::EventLoopProxy<AppEvent>,
    win: Option<LatencyTest>,
}

struct LatencyTest {
    attachment: client::Attachment,
    session_id: u64,
    stream: VideoStream<AppEvent>,
    video_texture: Option<Arc<VkImage>>,

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
    init_logging()?;

    let args = Cli::parse();

    // Invisible window.
    let event_loop: EventLoop<AppEvent> = EventLoop::with_user_event().build()?;
    let proxy = event_loop.create_proxy();

    let client = client::Client::new(&args.host, "latency-test", time::Duration::from_secs(1))
        .block_on()
        .context("failed to connect")?;

    let mut app = App {
        client,
        args,
        proxy,
        win: None,
    };

    event_loop.run_app(&mut app)?;

    if let Some(win) = app.win.take() {
        drop(win.stream);
        unsafe {
            win.vk
                .device
                .free_command_buffers(win.vk.present_queue.command_pool, &[win.copy_cb]);
            win.vk.device.destroy_fence(win.copy_fence, None);
            destroy_host_buffer(&win.vk.device, &win.copy_buffer);
        }

        println!("{}", win.histogram);

        if let Some(first_frame_recvd) = win.first_frame_recvd {
            println!(
                "transfer rate: {:.2} mpbs ({:.2}kb per frame)",
                win.total_video_bytes as f64 * 8.0
                    / 1_000_000.0
                    / first_frame_recvd.elapsed().as_secs_f64(),
                win.total_video_bytes as f64 / 1_000.0 / win.frames_recvd as f64
            );
        }
    }

    Ok(())
}

impl winit::application::ApplicationHandler<AppEvent> for App {
    fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        if self.win.is_some() {
            return;
        }

        match start_test(&self.args, &self.client, event_loop, self.proxy.clone()) {
            Ok(w) => {
                self.win = Some(w);
            }
            Err(e) => {
                error!("failed to start test: {:#}", e);
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        _event_loop: &winit::event_loop::ActiveEventLoop,
        _window_id: winit::window::WindowId,
        _event: winit::event::WindowEvent,
    ) {
    }

    fn about_to_wait(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        let Some(win) = &self.win else {
            return;
        };

        if win.block_started.elapsed() > time::Duration::from_secs(3) {
            error!("timed out waiting for block");
            event_loop.exit();
        }
    }

    fn user_event(&mut self, event_loop: &winit::event_loop::ActiveEventLoop, event: AppEvent) {
        let Some(win) = &mut self.win else {
            return;
        };

        match win.event(event) {
            Ok(true) => (),
            Ok(false) => event_loop.exit(),
            Err(e) => {
                error!("error: {}", e);
                event_loop.exit();
            }
        }
    }

    fn exiting(&mut self, _event_loop: &winit::event_loop::ActiveEventLoop) {
        let Some(win) = &self.win else {
            return;
        };

        let _ = win.attachment.detach().block_on();
        let _ = self
            .client
            .end_session(win.session_id, DEFAULT_TIMEOUT)
            .block_on();
    }
}

impl LatencyTest {
    fn event(&mut self, event: AppEvent) -> anyhow::Result<bool> {
        match event {
            AppEvent::AttachmentEvent(AttachmentEvent::VideoStreamStart(stream_seq, params)) => {
                assert_eq!(params.width, APP_DIMENSION);
                assert_eq!(params.height, APP_DIMENSION);

                self.stream
                    .reset(stream_seq, APP_DIMENSION, APP_DIMENSION, params.codec)?;
            }
            AppEvent::AttachmentEvent(AttachmentEvent::VideoPacket(packet)) => {
                if self.first_frame_recvd.is_none() {
                    self.first_frame_recvd = Some(time::Instant::now());
                }

                self.total_video_bytes += packet.len();
                self.stream.recv_packet(packet)?;
            }
            AppEvent::AttachmentEvent(AttachmentEvent::AttachmentEnded) => {
                bail!("server closed connection");
            }
            AppEvent::AttachmentEvent(_) => (),
            AppEvent::VideoStreamReady(tex, params) => {
                assert_eq!(params.width, APP_DIMENSION);
                assert_eq!(params.height, APP_DIMENSION);

                self.video_texture = Some(tex);
            }
            AppEvent::VideoFrameAvailable => {
                if self.stream.prepare_frame()?.is_some() {
                    self.frames_recvd += 1;

                    match self.frames_recvd.cmp(&100) {
                        std::cmp::Ordering::Less => (),
                        std::cmp::Ordering::Equal => {
                            debug!("starting test...");
                            self.send_space();
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
        }

        Ok(true)
    }

    fn send_space(&mut self) {
        debug!("sending space");

        self.attachment.keyboard_input(
            client::input::Key::Space,
            client::input::KeyState::Pressed,
            0,
        );

        self.attachment.keyboard_input(
            client::input::Key::Space,
            client::input::KeyState::Released,
            0,
        );
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
            self.send_space();
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
        let texture = self.video_texture.as_ref().unwrap();

        // Reset the command buffer.
        device.reset_command_buffer(self.copy_cb, vk::CommandBufferResetFlags::empty())?;

        // Begin the command buffer.
        {
            let begin_info = vk::CommandBufferBeginInfo::default()
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
            let region = vk::BufferImageCopy::default()
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
                });

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

        device.reset_fences(&[self.copy_fence])?;
        device.queue_submit(
            self.vk.present_queue.queue,
            &[vk::SubmitInfo::default().command_buffers(&[self.copy_cb])],
            self.copy_fence,
        )?;
        device.wait_for_fences(&[self.copy_fence], true, u64::MAX)?;
        Ok(())
    }
}

fn start_test(
    args: &Cli,
    client: &client::Client,
    event_loop: &winit::event_loop::ActiveEventLoop,
    proxy: winit::event_loop::EventLoopProxy<AppEvent>,
) -> anyhow::Result<LatencyTest> {
    let attr = winit::window::Window::default_attributes().with_visible(false);

    let window = Arc::new(event_loop.create_window(attr)?);
    let vk = unsafe { Arc::new(VkContext::new(window.clone(), cfg!(debug_assertions))?) };

    let codec = match args.codec.as_deref() {
        Some("h264") => protocol::VideoCodec::H264,
        Some("h265") | None => protocol::VideoCodec::H265,
        Some("av1") => protocol::VideoCodec::Av1,
        Some(v) => bail!("invalid codec: {:?}", v),
    };

    // Create session, attach
    let sess = client
        .launch_session(
            "latency-test".to_string(),
            client::display_params::DisplayParams {
                width: APP_DIMENSION,
                height: APP_DIMENSION,
                framerate: args.framerate.unwrap_or(60),
                ui_scale: client::pixel_scale::PixelScale::ONE,
            },
            vec![],
            DEFAULT_TIMEOUT,
        )
        .block_on()
        .context("failed to create session")?;

    let config = client::AttachmentConfig {
        width: APP_DIMENSION,
        height: APP_DIMENSION,
        video_codec: codec.into(),
        video_profile: None,
        quality_preset: Some(6),
        audio_codec: None,
        sample_rate: None,
        channels: vec![],
        video_stream_seq_offset: 0,
        audio_stream_seq_offset: 0,
    };

    let delegate = Arc::new(AttachmentProxy::new(proxy.clone()));
    let attachment = client
        .attach_session(sess.id, config, delegate, DEFAULT_TIMEOUT)
        .block_on()
        .context("failed to attach")?;

    // Just big enough for the Y plane.
    let copy_buffer = create_host_buffer(
        &vk.device,
        vk.device_info.host_visible_mem_type_index,
        vk::BufferUsageFlags::TRANSFER_DST,
        (APP_DIMENSION * APP_DIMENSION) as usize,
    )?;

    let copy_cb = create_command_buffer(&vk.device, vk.present_queue.command_pool)?;
    let copy_fence = create_fence(&vk.device, false)?;

    Ok(LatencyTest {
        attachment,
        session_id: sess.id,

        stream: VideoStream::new(vk.clone(), proxy.clone()),
        video_texture: None,
        frames_recvd: 0,

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
    })
}

fn init_logging() -> anyhow::Result<()> {
    if let Ok(env_filter) = tracing_subscriber::EnvFilter::try_from_default_env() {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    } else {
        let filter = tracing_subscriber::EnvFilter::builder()
            .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
            .from_env()?
            .add_directive("mm_client=info".parse()?)
            .add_directive("mm_client_common=info".parse()?);
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }

    Ok(())
}
