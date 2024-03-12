// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use std::{
    sync::{mpsc, Arc},
    time,
};

use anyhow::{anyhow, Context};
use ash::vk;
use bytes::{Bytes, BytesMut};
use ffmpeg_next as ffmpeg;
use ffmpeg_sys_next as ffmpeg_sys;
use tracing::{debug, error, instrument, trace, trace_span, warn};

use crate::{
    packet_ring::{Packet as Undecoded, PacketRing},
    stats::STATS,
    vulkan::*,
};
use mm_protocol as protocol;

const DECODER_INIT_TIMEOUT: time::Duration = time::Duration::from_secs(5);

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FrameMetadata {
    pub attachment_id: u64,
    pub stream_seq: u64,
    pub seq: u64,
    pub pts: u64,
}

struct YUVPicture {
    y: Bytes,
    u: Bytes,
    v: Bytes,
    info: FrameMetadata,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoStreamParams {
    pub width: u32,
    pub height: u32,
    pub pixel_format: ffmpeg::format::Pixel,
    pub color_space: ffmpeg::color::Space,
    pub color_range: ffmpeg::color::Range,
}

impl Default for VideoStreamParams {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            pixel_format: ffmpeg::format::Pixel::YUV420P,
            color_space: ffmpeg::color::Space::BT709,
            color_range: ffmpeg::color::Range::MPEG,
        }
    }
}

pub enum VideoStreamEvent {
    VideoStreamReady(VkImage, VideoStreamParams),
    VideoFrameAvailable,
}

enum StreamState {
    Empty,
    Initializing(DecoderInit),
    Streaming(CPUDecoder),
    Restarting(CPUDecoder, DecoderInit),
}

impl std::fmt::Debug for StreamState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StreamState::Empty => write!(f, "Empty"),
            StreamState::Initializing(init) => write!(f, "Initializing({})", init.stream_seq),
            StreamState::Streaming(dec) => write!(f, "Streaming({})", dec.stream_seq),
            StreamState::Restarting(dec, init) => write!(
                f,
                "RestartingStream({} -> {})",
                dec.stream_seq, init.stream_seq
            ),
        }
    }
}

pub struct VideoStream<T: From<VideoStreamEvent> + Send + 'static> {
    state: StreamState,
    ring: PacketRing,

    proxy: winit::event_loop::EventLoopProxy<T>,
    vk: Arc<VkContext>,
}

impl<T: From<VideoStreamEvent> + Send + 'static> VideoStream<T> {
    pub fn new(vk: Arc<VkContext>, proxy: winit::event_loop::EventLoopProxy<T>) -> Self {
        Self {
            state: StreamState::Empty,
            ring: PacketRing::new(),

            proxy,
            vk,
        }
    }

    pub fn reset(
        &mut self,
        attachment_id: u64,
        stream_seq: u64,
        width: u32,
        height: u32,
        codec: protocol::VideoCodec,
    ) -> anyhow::Result<()> {
        debug!(
            stream_seq,
            width,
            height,
            ?codec,
            "starting or restarting video stream"
        );

        let init = DecoderInit::new(
            self.vk.clone(),
            attachment_id,
            stream_seq,
            codec,
            width,
            height,
        )?;

        use StreamState::*;
        let state = std::mem::replace(&mut self.state, Empty);
        self.state = match state {
            Empty | Initializing(_) => Initializing(init),
            Streaming(renderer) | Restarting(renderer, _) => Restarting(renderer, init),
        };

        trace!(state = ?self.state, "video stream updated");
        Ok(())
    }

    pub fn recv_chunk(&mut self, chunk: protocol::VideoChunk) -> anyhow::Result<()> {
        use StreamState::*;

        trace!(
            stream_seq = chunk.stream_seq,
            seq = chunk.seq,
            chunk = chunk.chunk,
            num_chunks = chunk.num_chunks,
            len = chunk.data.len(),
            "received video chunk",
        );

        STATS.frame_chunk_received(chunk.stream_seq, chunk.seq, chunk.data.len());
        self.ring.recv_chunk(chunk)?;

        // Feed the existing stream.
        if let Streaming(ref mut dec) | Restarting(ref mut dec, _) = self.state {
            for pkt in self.ring.drain_completed(dec.stream_seq) {
                let len = bytes::Buf::remaining(&pkt);
                trace!(
                    stream_seq = dec.stream_seq,
                    seq = pkt.seq,
                    pts = pkt.pts,
                    len,
                    "received full video packet",
                );

                STATS.full_frame_received(dec.stream_seq, pkt.seq, len);
                dec.send_packet(pkt)?;
            }
        }

        // Feed the new stream, if there is one.
        let new_stream_ready = match self.state {
            Empty | Streaming(_) => false,
            Initializing(ref mut init) | Restarting(_, ref mut init) => {
                let mut success = false;
                for pkt in self.ring.drain_completed(init.stream_seq) {
                    trace!(
                        stream_seq = init.stream_seq,
                        seq = pkt.seq,
                        len = bytes::Buf::remaining(&pkt),
                        "received full video packet for initializing stream",
                    );

                    if init.send_packet(pkt)? {
                        success = true;
                        break;
                    }
                }

                success
            }
        };

        if new_stream_ready {
            // N.B. An error here puts us into an invalid state.
            let (dec, texture, params) = match std::mem::replace(&mut self.state, Empty) {
                Initializing(init) | Restarting(_, init) => {
                    init.into_decoder(self.proxy.clone())?
                }
                Streaming(_) | Empty => unreachable!(),
            };

            self.proxy
                .send_event(VideoStreamEvent::VideoStreamReady(texture, params).into())
                .ok();

            self.ring.discard(dec.stream_seq - 1);
            self.state = Streaming(dec);
            trace!(state = ?self.state, "video stream updated")
        }

        Ok(())
    }

    pub fn prepare_frame(&mut self) -> anyhow::Result<bool> {
        match self.state {
            StreamState::Streaming(ref mut dec) | StreamState::Restarting(ref mut dec, _) => {
                dec.prepare_frame()
            }
            StreamState::Empty | StreamState::Initializing(_) => Ok(false),
        }
    }

    pub fn mark_frame_rendered(&mut self) {
        match self.state {
            StreamState::Streaming(ref mut dec) | StreamState::Restarting(ref mut dec, _) => {
                dec.mark_frame_rendered()
            }
            StreamState::Empty | StreamState::Initializing(_) => (),
        }
    }

    pub fn is_ready(&self) -> bool {
        match self.state {
            StreamState::Empty | StreamState::Initializing(_) => false,
            StreamState::Streaming(_) | StreamState::Restarting(_, _) => true,
        }
    }
}

struct CPUDecoder {
    stream_seq: u64,
    prepared_frame_info: Option<FrameMetadata>,

    staging_buffer: VkHostBuffer,
    yuv_buffer_offsets: [usize; 3],
    yuv_buffer_strides: [usize; 3],
    video_texture: VkImage,
    sampler_conversion: vk::SamplerYcbcrConversion,
    texture_width: u32,
    texture_height: u32,

    upload_cb: vk::CommandBuffer,
    upload_fence: vk::Fence,
    upload_ts_pool: VkTimestampQueryPool,
    tracy_upload_span: Option<tracy_client::GpuSpan>,

    undecoded_send: mpsc::Sender<Undecoded>,
    decoded_recv: mpsc::Receiver<YUVPicture>,
    decoder_thread_handle: Option<std::thread::JoinHandle<anyhow::Result<()>>>,

    vk: Arc<VkContext>,
}

/// A temporary struct that receives video packets until it has enough metadata
/// to start decoding and recieves a single frame. It also handles timing out
/// if it never receives any metadata in the (otherwise valid) video stream.
struct DecoderInit {
    attachment_id: u64,
    stream_seq: u64,
    width: u32,
    height: u32,
    started: time::Instant,
    decoder: ffmpeg::decoder::Video,
    packet: ffmpeg::Packet,
    first_frame: Option<(ffmpeg::frame::Video, FrameMetadata)>,
    vk: Arc<VkContext>,
}

impl DecoderInit {
    fn new(
        vk: Arc<VkContext>,
        attachment_id: u64,
        stream_seq: u64,
        codec: protocol::VideoCodec,
        width: u32,
        height: u32,
    ) -> anyhow::Result<Self> {
        let codec = {
            let id = match codec {
                protocol::VideoCodec::H264 => ffmpeg::codec::Id::H264,
                protocol::VideoCodec::H265 => ffmpeg::codec::Id::H265,
                protocol::VideoCodec::Av1 => ffmpeg::codec::Id::AV1,
                _ => {
                    error!("unexpected codec: {:?}", codec);
                    unimplemented!();
                }
            };

            ffmpeg::decoder::find(id).ok_or(anyhow::anyhow!("codec not found"))?
        };

        let dec_ctx = unsafe {
            let ptr = ffmpeg_sys::avcodec_alloc_context3(codec.as_ptr());
            (*ptr).width = width as i32;
            (*ptr).height = height as i32;

            // On macOS/iOS, we can use the ASIC decoder.
            if cfg!(any(target_os = "macos", target_os = "ios"))
                && vk.device_info.is_integrated()
                && (codec.id() == ffmpeg::codec::Id::H264
                    || codec.id() == ffmpeg::codec::Id::HEVC
                    || codec.id() == ffmpeg::codec::Id::H265)
            {
                let mut hw_ctx: *mut _ = std::ptr::null_mut();
                let res = ffmpeg_sys::av_hwdevice_ctx_create(
                    &mut hw_ctx,
                    ffmpeg_sys::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    0,
                );

                if res < 0 {
                    warn!("VideoToolbox setup failed, falling back to CPU decoder");
                } else {
                    (*ptr).hw_device_ctx = hw_ctx;
                    (*ptr).get_format = Some(get_hw_format_videotoolbox);
                }
            }

            ffmpeg::codec::context::Context::wrap(ptr, None)
        };

        let mut decoder = dec_ctx.decoder();
        decoder.set_flags(ffmpeg::codec::Flags::LOW_DELAY);

        let decoder = decoder.open()?.video()?;
        let packet = ffmpeg::Packet::empty();

        Ok(Self {
            attachment_id,
            stream_seq,
            width,
            height,
            started: time::Instant::now(),
            decoder,
            packet,
            first_frame: None,
            vk,
        })
    }

    /// Feed a packet into the decoder. Returns true if the paramaters of the
    /// stream have been recovered and it's safe to call into_decoder. Returns
    /// an error only on timeout.
    fn send_packet(&mut self, buf: Undecoded) -> anyhow::Result<bool> {
        let info = FrameMetadata {
            attachment_id: self.attachment_id,
            stream_seq: self.stream_seq,
            seq: buf.seq,
            pts: buf.pts,
        };

        if self.started.elapsed() > DECODER_INIT_TIMEOUT {
            return Err(anyhow!("timed out waiting for video stream metadata"));
        }

        copy_packet(&mut self.packet, buf)?;
        match self.decoder.send_packet(&self.packet) {
            Ok(()) => {}
            Err(ffmpeg::Error::Other {
                errno: ffmpeg::error::EAGAIN,
            }) => return Err(anyhow!("decoder already read initial packets")),
            Err(_) => return Ok(false),
        }

        let mut frame = ffmpeg::frame::Video::empty();
        match self.decoder.receive_frame(&mut frame) {
            Ok(()) => {
                self.first_frame = match frame.format() {
                    ffmpeg::format::Pixel::YUV420P => Some((frame, info)),
                    ffmpeg::format::Pixel::VIDEOTOOLBOX => {
                        let mut sw_frame = ffmpeg::frame::Video::new(
                            ffmpeg::format::Pixel::YUV420P,
                            self.decoder.width(),
                            self.decoder.height(),
                        );

                        unsafe {
                            let res = ffmpeg_sys::av_hwframe_transfer_data(
                                sw_frame.as_mut_ptr(),
                                frame.as_ptr(),
                                0,
                            );

                            if res < 0 {
                                return Err(anyhow!("call to av_hwframe_transfer_data failed"));
                            }

                            Some((sw_frame, info))
                        }
                    }
                    f => return Err(anyhow!("unexpected stream format: {:?}", f)),
                };

                Ok(true)
            }
            Err(ffmpeg::Error::Other {
                errno: ffmpeg::error::EAGAIN,
            }) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Consumes the DecoderInit, returning a CPUDecoder capable of uploading
    /// frames to the GPU.
    fn into_decoder<T>(
        self,
        proxy: winit::event_loop::EventLoopProxy<T>,
    ) -> anyhow::Result<(CPUDecoder, VkImage, VideoStreamParams)>
    where
        T: From<VideoStreamEvent> + Send,
    {
        let width = self.decoder.width();
        let height = self.decoder.height();

        let decoder_format = self.decoder.format();
        let first_frame = match self.first_frame {
            Some(f) => f,
            None => return Err(anyhow!("no frames received yet")),
        };

        // debug_assert_eq!(self.decoder.color_space(), ffmpeg::color::Space::BT709);

        let output_format = first_frame.0.format();
        assert_eq!(output_format, ffmpeg::format::Pixel::YUV420P);

        // If we're using VideoToolbox, create a "hardware" frame to use with
        // receive_frame.
        let ((mut frame, info), mut hw_frame) = match decoder_format {
            ffmpeg::format::Pixel::YUV420P => (first_frame, None),
            ffmpeg::format::Pixel::VIDEOTOOLBOX => {
                let hw_frame =
                    ffmpeg::frame::Video::new(ffmpeg::format::Pixel::VIDEOTOOLBOX, width, height);

                (first_frame, Some(hw_frame))
            }
            _ => return Err(anyhow!("unexpected stream format: {:?}", decoder_format)),
        };

        let texture_format = vk::Format::G8_B8_R8_3PLANE_420_UNORM;

        debug_assert_eq!(frame.width(), width);
        debug_assert_eq!(frame.height(), height);

        if width != self.width || height != self.height {
            return Err(anyhow!(
                "unexpected video stream dimensions: {}x{} (expected {}x{})",
                width,
                height,
                self.width,
                self.height
            ));
        }

        let y_stride = frame.stride(0);
        let u_stride = frame.stride(1);
        let v_stride = frame.stride(2);

        let y_len = y_stride * frame.plane_height(0) as usize;
        let u_len = u_stride * frame.plane_height(1) as usize;
        let v_len = v_stride * frame.plane_height(2) as usize;

        // This is vanishingly unlikely, and I have no idea how the pixels
        // would be layed out in that case.
        debug_assert_eq!(y_len % 4, 0);

        // Precalculate the offsets into the buffer for each plane.
        let buffer_size = y_len + u_len + v_len;
        let buffer_offsets = [0, y_len, y_len + u_len];
        let buffer_strides = [y_stride, u_stride, v_stride];

        let staging_buffer = create_host_buffer(
            &self.vk.device,
            self.vk.device_info.host_visible_mem_type_index,
            vk::BufferUsageFlags::TRANSFER_SRC,
            buffer_size,
        )?;

        let color_space = match self.decoder.color_space() {
            ffmpeg::color::Space::BT709 => ffmpeg::color::Space::BT709,
            ffmpeg::color::Space::BT2020NCL => ffmpeg::color::Space::BT2020NCL,
            cs => {
                warn!("unexpected color space: {:?}", cs);
                ffmpeg::color::Space::BT709
            }
        };

        let color_range = match self.decoder.color_range() {
            ffmpeg::color::Range::MPEG => ffmpeg::color::Range::MPEG,
            ffmpeg::color::Range::JPEG => ffmpeg::color::Range::JPEG,
            cr => {
                warn!("unexpected color range: {:?}", cr);
                ffmpeg::color::Range::MPEG
            }
        };

        let sampler_conversion = {
            let ycbcr_model = match color_space {
                ffmpeg::color::Space::BT709 => vk::SamplerYcbcrModelConversion::YCBCR_709,
                ffmpeg::color::Space::BT2020NCL => vk::SamplerYcbcrModelConversion::YCBCR_2020,
                _ => unreachable!(),
            };

            let ycbcr_range = match color_range {
                ffmpeg::color::Range::MPEG => vk::SamplerYcbcrRange::ITU_NARROW,
                ffmpeg::color::Range::JPEG => vk::SamplerYcbcrRange::ITU_FULL,
                _ => unreachable!(),
            };

            let create_info = vk::SamplerYcbcrConversionCreateInfo::builder()
                .format(texture_format)
                .ycbcr_model(ycbcr_model)
                .ycbcr_range(ycbcr_range)
                .chroma_filter(vk::Filter::LINEAR)
                .x_chroma_offset(vk::ChromaLocation::MIDPOINT)
                .y_chroma_offset(vk::ChromaLocation::MIDPOINT);

            unsafe {
                self.vk
                    .device
                    .create_sampler_ycbcr_conversion(&create_info, None)?
            }
        };

        let video_texture = create_image(
            &self.vk.device,
            texture_format,
            width,
            height,
            vk::ImageUsageFlags::TRANSFER_DST
                | vk::ImageUsageFlags::SAMPLED
                | vk::ImageUsageFlags::TRANSFER_SRC,
            vk::SharingMode::EXCLUSIVE,
        )?;

        // Uploads happen on the present queue.
        let upload_cb = create_command_buffer(&self.vk.device, self.vk.present_queue.command_pool)?;
        let upload_fence = create_fence(&self.vk.device, true)?;
        let upload_ts_pool = create_timestamp_query_pool(&self.vk.device, 2)?;

        let (undecoded_send, undecoded_recv) = mpsc::channel::<Undecoded>();
        let (decoded_send, decoded_recv) = mpsc::channel::<YUVPicture>();

        // Send the frame we have from before.
        decoded_send
            .send(copy_frame(&mut frame, &mut BytesMut::new(), info))
            .unwrap();

        // Spawn another thread that receives packets on one channel and sends
        // completed pictures on another.
        let attachment_id = self.attachment_id;
        let stream_seq = self.stream_seq;
        let mut decoder = self.decoder;
        let mut packet = self.packet;
        let decoder_thread_handle = std::thread::Builder::new()
            .name("CPU decoder".to_string())
            .spawn(move || -> anyhow::Result<()> {
                tracy_client::set_thread_name!("CPU decoder");

                // This should have enough capacity for four pictures (YUV420 has
                // a bpp of 1.5). It will also resize dynamically, of course.
                let mut scratch = BytesMut::with_capacity((width * height * 6) as usize);

                for buf in undecoded_recv {
                    let _tracy_frame = tracy_client::non_continuous_frame!("decode");
                    let span = trace_span!("decode_loop");
                    let _guard = span.enter();

                    let info = FrameMetadata {
                        attachment_id,
                        stream_seq,
                        seq: buf.seq,
                        pts: buf.pts,
                    };

                    copy_packet(&mut packet, buf)?;

                    // Send the packet to the decoder.
                    if trace_span!("send_packet")
                        .in_scope(|| decoder.send_packet(&packet))
                        .is_err()
                    {
                        continue;
                    }

                    // Receive frames until we get EAGAIN.
                    loop {
                        match receive_frame(&mut decoder, &mut frame, hw_frame.as_mut()) {
                            Ok(()) => {
                                let pic = copy_frame(&mut frame, &mut scratch, info);

                                debug_assert_eq!(pic.y.len(), y_len);
                                debug_assert_eq!(pic.u.len(), u_len);
                                debug_assert_eq!(pic.v.len(), v_len);

                                let span = trace_span!("send");
                                let _guard = span.enter();

                                match decoded_send.send(pic) {
                                    Ok(()) => {}
                                    Err(mpsc::SendError(_)) => return Ok(()),
                                }

                                match proxy.send_event(VideoStreamEvent::VideoFrameAvailable.into())
                                {
                                    Ok(()) => {}
                                    Err(_) => return Ok(()),
                                }
                            }
                            Err(ffmpeg::Error::Other {
                                errno: ffmpeg::error::EAGAIN,
                            }) => break,
                            Err(e) => {
                                debug!("receive_frame failed: {:?}", e);
                                return Err(e.into());
                            }
                        }
                    }
                }

                Ok(())
            })?;

        let dec = CPUDecoder {
            stream_seq: self.stream_seq,
            prepared_frame_info: None,

            staging_buffer,
            yuv_buffer_offsets: buffer_offsets,
            yuv_buffer_strides: buffer_strides,
            video_texture,
            sampler_conversion,
            texture_width: width,
            texture_height: height,
            upload_cb,
            upload_fence,
            upload_ts_pool,
            tracy_upload_span: None,

            undecoded_send,
            decoded_recv,
            decoder_thread_handle: Some(decoder_thread_handle),
            vk: self.vk,
        };

        unsafe { dec.prerecord_upload()? };

        let params = VideoStreamParams {
            width,
            height,
            pixel_format: output_format,
            color_space,
            color_range,
        };

        Ok((dec, video_texture, params))
    }
}

impl CPUDecoder {
    fn send_packet(&mut self, buf: Undecoded) -> anyhow::Result<()> {
        let exit = match self.undecoded_send.send(buf) {
            Ok(_) => return Ok(()),
            Err(mpsc::SendError(_)) => match self.decoder_thread_handle.take() {
                Some(h) => h.join(),
                None => unreachable!(),
            },
        };

        match exit {
            Ok(Ok(())) => Err(anyhow!("decoding thread exited unexpectedly")),
            Ok(Err(e)) => Err(e).context("decoding exited with error"),
            Err(v) => Err(anyhow!("decoding thread panicked: {:?}", v)),
        }
    }

    pub fn prepare_frame(&mut self) -> anyhow::Result<bool> {
        // If multiple frames are ready, only grab the last one.
        let mut iterator = self.decoded_recv.try_iter().peekable();
        while let Some(pic) = iterator.next() {
            if iterator.peek().is_some() {
                STATS.frame_discarded(pic.info.stream_seq, pic.info.seq);

                debug!(
                    stream_seq = pic.info.stream_seq,
                    seq = pic.info.seq,
                    "discarding frame"
                );
            } else {
                let pic_info = pic.info;
                unsafe {
                    self.upload(pic).context("uploading frame to GPU")?;
                }

                if let Some(old) = self.prepared_frame_info.replace(pic_info) {
                    debug!(
                        stream_seq = old.stream_seq,
                        seq = old.seq,
                        "overwriting uploaded frame"
                    );

                    STATS.frame_discarded(old.stream_seq, old.seq);
                }

                return Ok(true);
            }
        }

        Ok(false)
    }

    pub fn mark_frame_rendered(&mut self) {
        if let Some(info) = self.prepared_frame_info.take() {
            STATS.frame_rendered(info.stream_seq, info.seq);
        }
    }

    unsafe fn upload(&mut self, pic: YUVPicture) -> anyhow::Result<()> {
        // Wait for the previous upload to complete.
        let device = &self.vk.device;
        device.wait_for_fences(&[self.upload_fence], true, u64::MAX)?;

        // Copy data into the staging buffer.
        self.yuv_buffer_offsets
            .iter()
            .zip([pic.y, pic.u, pic.v])
            .for_each(|(offset, src)| {
                let dst = std::slice::from_raw_parts_mut(
                    (self.staging_buffer.access as *mut u8).add(*offset),
                    src.len(),
                );

                dst.copy_from_slice(&src);
            });

        // Trace the upload, including loading timestamps for the previous upload.
        if let Some(ctx) = &self.vk.tracy_context {
            if let Some(prev_span) = self.tracy_upload_span.take() {
                let timestamps = self.upload_ts_pool.fetch_results(&self.vk.device)?;
                prev_span.upload_timestamp(timestamps[0], timestamps[1]);
            }

            self.tracy_upload_span = Some(ctx.span(tracy_client::span_location!())?);
        }

        // The command buffer was prerecorded, so we can directly submit it.
        {
            let cbs = [self.upload_cb];
            let submit_info = vk::SubmitInfo::builder().command_buffers(&cbs).build();

            self.vk.device.reset_fences(&[self.upload_fence])?;

            trace!(queue = ?self.vk.present_queue.queue, "queue submit for upload");

            let submits = [submit_info];
            device.queue_submit(self.vk.present_queue.queue, &submits, self.upload_fence)?;
        }

        if let Some(span) = self.tracy_upload_span.as_mut() {
            span.end_zone();
        }

        Ok(())
    }

    unsafe fn prerecord_upload(&self) -> anyhow::Result<()> {
        let device = &self.vk.device;

        // Reset the command buffer.
        device.reset_command_buffer(self.upload_cb, vk::CommandBufferResetFlags::empty())?;

        // Begin the command buffer.
        {
            let begin_info = vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::SIMULTANEOUS_USE);

            device.begin_command_buffer(self.upload_cb, &begin_info)?;
        }

        // Record the start timestamp.
        self.upload_ts_pool.cmd_reset(device, self.upload_cb);
        device.cmd_write_timestamp(
            self.upload_cb,
            vk::PipelineStageFlags::TOP_OF_PIPE,
            self.upload_ts_pool.pool,
            0,
        );

        // Transfer the image to be writable.
        cmd_image_barrier(
            device,
            self.upload_cb,
            self.video_texture.image,
            vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::AccessFlags::empty(),
            vk::PipelineStageFlags::TRANSFER,
            vk::AccessFlags::TRANSFER_WRITE,
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
        );

        // Upload from the staging buffer to the texture.
        {
            let regions = [
                vk::ImageAspectFlags::PLANE_0,
                vk::ImageAspectFlags::PLANE_1,
                vk::ImageAspectFlags::PLANE_2,
            ]
            .into_iter()
            .enumerate()
            .map(|(plane, plane_aspect_mask)| {
                // Vulkan considers the image width/height to be 1/2 the size
                // for the U and V planes.
                let (width, height) = if plane == 0 {
                    (self.texture_width, self.texture_height)
                } else {
                    (self.texture_width / 2, self.texture_height / 2)
                };

                vk::BufferImageCopy::builder()
                    .buffer_offset(self.yuv_buffer_offsets[plane] as u64)
                    // This is actually in texels, but each plane uses 1bpp.
                    .buffer_row_length(self.yuv_buffer_strides[plane] as u32)
                    .image_subresource(vk::ImageSubresourceLayers {
                        aspect_mask: plane_aspect_mask,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .image_extent(vk::Extent3D {
                        width,
                        height,
                        depth: 1,
                    })
                    .build()
            })
            .collect::<Vec<_>>();

            device.cmd_copy_buffer_to_image(
                self.upload_cb,
                self.staging_buffer.buffer,
                self.video_texture.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &regions,
            );
        }

        // Transfer the image back to be readable.
        cmd_image_barrier(
            device,
            self.upload_cb,
            self.video_texture.image,
            vk::PipelineStageFlags::TRANSFER,
            vk::AccessFlags::TRANSFER_WRITE,
            vk::PipelineStageFlags::FRAGMENT_SHADER,
            vk::AccessFlags::SHADER_READ,
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        );

        // Record the end timestamp.
        device.cmd_write_timestamp(
            self.upload_cb,
            vk::PipelineStageFlags::BOTTOM_OF_PIPE,
            self.upload_ts_pool.pool,
            1,
        );

        device.end_command_buffer(self.upload_cb)?;
        Ok(())
    }
}

impl Drop for CPUDecoder {
    fn drop(&mut self) {
        let device = &self.vk.device;

        unsafe {
            device.queue_wait_idle(self.vk.present_queue.queue).ok();

            device.destroy_sampler_ycbcr_conversion(self.sampler_conversion, None);
            destroy_image(device, &self.video_texture);
            destroy_host_buffer(device, &self.staging_buffer);
            device.destroy_fence(self.upload_fence, None);
            device.destroy_query_pool(self.upload_ts_pool.pool, None);
            device.free_command_buffers(self.vk.present_queue.command_pool, &[self.upload_cb]);
        }
    }
}

#[instrument(skip_all)]
fn receive_frame(
    dec: &mut ffmpeg::decoder::Video,
    frame: &mut ffmpeg::frame::Video,
    hw_frame: Option<&mut ffmpeg::frame::Video>,
) -> Result<(), ffmpeg::Error> {
    match hw_frame {
        Some(f) => {
            dec.receive_frame(f)?;

            unsafe {
                let res = ffmpeg_sys::av_hwframe_transfer_data(frame.as_mut_ptr(), f.as_ptr(), 0);
                if res < 0 {
                    error!("call to av_hwframe_transfer_data failed");
                    Err(ffmpeg::Error::Other { errno: res })
                } else {
                    Ok(())
                }
            }
        }
        None => dec.receive_frame(frame),
    }
}

#[instrument(skip_all)]
fn copy_packet(pkt: &mut ffmpeg::Packet, buf: Undecoded) -> anyhow::Result<()> {
    use bytes::Buf;
    use std::io::Read;

    // It's necessary to reset the packet metadata for each NAL.
    unsafe {
        use ffmpeg::packet::Mut;
        ffmpeg_sys::av_init_packet(pkt.as_mut_ptr());
    }

    // Copy into data.
    match pkt.size().cmp(&buf.remaining()) {
        std::cmp::Ordering::Less => {
            pkt.grow(buf.remaining() - pkt.size());
        }
        std::cmp::Ordering::Greater => {
            // This is infuriatingly inconsistent.
            pkt.shrink(buf.remaining());
        }
        std::cmp::Ordering::Equal => {}
    };

    buf.reader()
        .read_exact(pkt.data_mut().unwrap())
        .context("copying into packet buffer")?;

    Ok(())
}

#[instrument(skip_all)]
fn copy_frame(
    frame: &mut ffmpeg::frame::Video,
    scratch: &mut BytesMut,
    info: FrameMetadata,
) -> YUVPicture {
    scratch.truncate(0);

    scratch.extend_from_slice(frame.data(0));
    let y = scratch.split().freeze();

    scratch.extend_from_slice(frame.data(1));
    let u = scratch.split().freeze();

    scratch.extend_from_slice(frame.data(2));
    let v = scratch.split().freeze();

    YUVPicture { y, u, v, info }
}

#[no_mangle]
unsafe extern "C" fn get_hw_format_videotoolbox(
    ctx: *mut ffmpeg_sys::AVCodecContext,
    mut formats: *const ffmpeg_sys::AVPixelFormat,
) -> ffmpeg_sys::AVPixelFormat {
    use ffmpeg_sys::AVPixelFormat::*;

    while *formats != AV_PIX_FMT_NONE {
        if *formats == AV_PIX_FMT_VIDEOTOOLBOX {
            let frames_ctx_ref = ffmpeg_sys::av_hwframe_ctx_alloc((*ctx).hw_device_ctx);
            if frames_ctx_ref.is_null() {
                error!("call to av_hwframe_ctx_alloc failed");
                break;
            }

            let frames_ctx = (*frames_ctx_ref).data as *mut ffmpeg_sys::AVHWFramesContext;
            (*frames_ctx).width = (*ctx).width;
            (*frames_ctx).height = (*ctx).height;
            (*frames_ctx).format = AV_PIX_FMT_VIDEOTOOLBOX;
            (*frames_ctx).sw_format = AV_PIX_FMT_YUV420P;

            let res = ffmpeg_sys::av_hwframe_ctx_init(frames_ctx_ref);
            if res < 0 {
                error!("call to av_hwframe_ctx_init failed");
                break;
            }

            debug!("using VideoToolbox hardware encoder");
            (*ctx).hw_frames_ctx = frames_ctx_ref;
            return *formats;
        }

        formats = formats.add(1);
    }

    warn!("VideoToolbox setup failed, falling back to CPU decoder");
    AV_PIX_FMT_YUV420P
}
