// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

#[cfg(feature = "svt_encode")]
mod svt;

#[cfg(feature = "ffmpeg_encode")]
mod ffmpeg;

use std::sync::Arc;

use crate::{
    codec::VideoCodec,
    compositor::{
        video::format_is_semiplanar, CompositorEvent, CompositorHandle, VideoStreamParams, EPOCH,
    },
    vulkan::*,
};

use anyhow::{bail, Context};
use ash::vk;
use bytes::BytesMut;
use crossbeam_channel as crossbeam;
use tracing::{error, instrument, trace, trace_span};

use super::{begin_cb, timebase::Timebase};

const DEFAULT_INPUT_FORMAT: vk::Format = vk::Format::G8_B8_R8_3PLANE_420_UNORM;

struct VkExtMemoryFrame {
    buffer: VkHostBuffer,
    width: u32,
    height: u32,
    strides: [usize; 3],
    offsets: [usize; 3],
    copy_cb: vk::CommandBuffer,
    copy_fence: vk::Fence,
    vk: Arc<VkContext>,
}

// SAFETY: we synchronize access to the buffer using channels.
unsafe impl Send for VkExtMemoryFrame {}

impl VkExtMemoryFrame {
    pub fn new(vk: Arc<VkContext>, width: u32, height: u32) -> anyhow::Result<Self> {
        let y_stride = (width as usize).next_multiple_of(32);
        let uv_stride = (width as usize / 2).next_multiple_of(32);

        let y_size = y_stride * height as usize;
        let uv_size = uv_stride * height as usize / 2;
        let buffer_size = y_size + uv_size + uv_size;
        let buffer_offsets = [0, y_size, y_size + uv_size];

        let buffer = VkHostBuffer::new(
            vk.clone(),
            vk.device_info.host_visible_mem_type_index,
            vk::BufferUsageFlags::TRANSFER_DST,
            buffer_size,
        )?;

        let copy_cb = allocate_command_buffer(&vk.device, vk.graphics_queue.command_pool)?;
        let copy_fence = create_fence(&vk.device, true)?;

        Ok(Self {
            width,
            height,
            buffer,
            strides: [y_stride, uv_stride, uv_stride],
            offsets: buffer_offsets,
            copy_cb,
            copy_fence,
            vk,
        })
    }
}

impl Drop for VkExtMemoryFrame {
    fn drop(&mut self) {
        unsafe {
            self.vk
                .device
                .queue_wait_idle(self.vk.graphics_queue.queue)
                .unwrap();

            self.vk
                .device
                .free_command_buffers(self.vk.graphics_queue.command_pool, &[self.copy_cb]);
            self.vk.device.destroy_fence(self.copy_fence, None);
        }
    }
}

trait EncoderInner {
    type Packet: AsRef<[u8]> + std::fmt::Debug;

    fn send_picture(&mut self, frame: &VkExtMemoryFrame, pts: u64) -> anyhow::Result<()>;
    fn receive_packet(&mut self) -> anyhow::Result<Option<Self::Packet>>;
    fn flush(&mut self) -> anyhow::Result<()>;
}

pub struct CpuEncoder {
    width: u32,
    height: u32,

    encode_thread_handle: Option<std::thread::JoinHandle<anyhow::Result<()>>>,
    input_frames: Option<crossbeam::Sender<(VkTimelinePoint, VkExtMemoryFrame)>>,
    done_frames: crossbeam::Receiver<VkExtMemoryFrame>,
    graphics_queue: VkQueue,
    vk: Arc<VkContext>,
}

impl CpuEncoder {
    #[instrument(level = "trace", skip_all)]
    pub fn new(
        vk: Arc<VkContext>,
        compositor: CompositorHandle,
        stream_seq: u64,
        params: VideoStreamParams,
        framerate: u32,
    ) -> anyhow::Result<Self> {
        let video_timebase = Timebase::new(600, *EPOCH);

        let (input_frames_tx, input_frames_rx) = crossbeam::unbounded();

        // Use a "swapchain" of three frames.
        let (done_frames_tx, done_frames_rx) = crossbeam::bounded::<VkExtMemoryFrame>(3);
        for _ in 0..3 {
            done_frames_tx
                .send(VkExtMemoryFrame::new(
                    vk.clone(),
                    params.width,
                    params.height,
                )?)
                .unwrap();
        }

        let thread = std::thread::Builder::new().name("encoder".to_string());
        let handle = match params.codec {
            #[cfg(feature = "ffmpeg_encode")]
            VideoCodec::H264 | VideoCodec::H265 if ffmpeg::probe_codec(params.codec) => thread
                .spawn(move || {
                    let encoder = trace_span!("ffmpeg_new_encoder")
                        .in_scope(|| ffmpeg::new_encoder(params, framerate, video_timebase))?;

                    encode_thread(
                        encoder,
                        input_frames_rx,
                        done_frames_tx,
                        compositor,
                        stream_seq,
                        video_timebase,
                    )
                })?,
            #[cfg(feature = "svt_encode")]
            VideoCodec::H265 => thread.spawn(move || {
                let encoder = trace_span!("svt_new_hevc")
                    .in_scope(|| svt::new_hevc(params, framerate))
                    .context("failed to create encoder")?;

                encode_thread(
                    encoder,
                    input_frames_rx,
                    done_frames_tx,
                    compositor,
                    stream_seq,
                    video_timebase,
                )
            })?,
            #[cfg(feature = "svt_encode")]
            VideoCodec::Av1 => thread.spawn(move || {
                let encoder = trace_span!("svt_new_av1")
                    .in_scope(|| svt::new_av1(params, framerate))
                    .context("failed to create encoder")?;

                encode_thread(
                    encoder,
                    input_frames_rx,
                    done_frames_tx,
                    compositor,
                    stream_seq,
                    video_timebase,
                )
            })?,
            _ => bail!("no encoder available for codec {:?}", params.codec),
        };

        Ok(Self {
            width: params.width,
            height: params.height,
            encode_thread_handle: Some(handle),
            input_frames: Some(input_frames_tx),
            done_frames: done_frames_rx,
            graphics_queue: vk.graphics_queue.clone(),
            vk,
        })
    }

    pub fn input_format(&self) -> vk::Format {
        DEFAULT_INPUT_FORMAT
    }

    pub fn create_input_image(&self) -> anyhow::Result<VkImage> {
        let image = {
            let create_info = vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(DEFAULT_INPUT_FORMAT)
                .extent(vk::Extent3D {
                    width: self.width,
                    height: self.height,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::STORAGE)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED)
                .flags(vk::ImageCreateFlags::EXTENDED_USAGE | vk::ImageCreateFlags::MUTABLE_FORMAT);

            unsafe {
                self.vk
                    .device
                    .create_image(&create_info, None)
                    .context("VkCreateImage")?
            }
        };

        let memory = unsafe {
            bind_memory_for_image(&self.vk.device, &self.vk.device_info.memory_props, image)?
        };

        Ok(VkImage::wrap(
            self.vk.clone(),
            image,
            // No view into the combined image (and we can't create one because
            // of the UsageFlags).
            vk::ImageView::null(),
            memory,
            DEFAULT_INPUT_FORMAT,
            self.width,
            self.height,
        ))
    }

    #[instrument(level = "trace", skip_all)]
    pub unsafe fn submit_encode(
        &mut self,
        image: &VkImage,
        tp_acquire: VkTimelinePoint,
        tp_release: VkTimelinePoint,
    ) -> anyhow::Result<()> {
        // "Acquire" a frame to copy to. This provides backpressure if the
        // encoder can't keep up.
        let frame = match self.done_frames.recv() {
            Ok(frame) => frame,
            Err(_) => {
                bail!("encoder thread died");
            }
        };

        let device = &self.vk.device;

        trace_span!("wait_for_fences")
            .in_scope(|| device.wait_for_fences(&[frame.copy_fence], true, 1_000_000_000))?;

        // Reset command buffer.
        begin_cb(device, frame.copy_cb)?;

        // Transition the image to be copyable and the buffer to be writable.
        {
            let read_barrier = vk::ImageMemoryBarrier2::default()
                .image(image.image)
                .src_stage_mask(vk::PipelineStageFlags2::TOP_OF_PIPE)
                .dst_stage_mask(vk::PipelineStageFlags2::COPY)
                .dst_access_mask(vk::AccessFlags2::TRANSFER_READ)
                .old_layout(vk::ImageLayout::GENERAL)
                .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });

            let write_barrier = vk::BufferMemoryBarrier2::default()
                .buffer(frame.buffer.buffer)
                .src_stage_mask(vk::PipelineStageFlags2::TOP_OF_PIPE)
                .dst_stage_mask(vk::PipelineStageFlags2::TRANSFER)
                .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                .offset(0)
                .size(vk::WHOLE_SIZE);

            let image_barriers = [read_barrier];
            let buffer_barriers = [write_barrier];
            let read_dependency_info = vk::DependencyInfo::default()
                .image_memory_barriers(&image_barriers)
                .buffer_memory_barriers(&buffer_barriers);

            unsafe { device.cmd_pipeline_barrier2(frame.copy_cb, &read_dependency_info) };
        }

        // Copy the image to the staging buffers.
        {
            // We require fully multiplanar input images for this encoder.
            debug_assert!(!format_is_semiplanar(image.format));
            let plane_aspects = [
                vk::ImageAspectFlags::PLANE_0,
                vk::ImageAspectFlags::PLANE_1,
                vk::ImageAspectFlags::PLANE_2,
            ];

            let mut regions = Vec::new();
            for (idx, plane) in plane_aspects.iter().enumerate() {
                let (width, height) = if idx == 0 {
                    (self.width, self.height)
                } else {
                    (self.width / 2, self.height / 2)
                };

                regions.push(
                    vk::BufferImageCopy::default()
                        .buffer_offset(frame.offsets[idx] as u64)
                        // In texels, but YUV has 1bpp for each plane.
                        .buffer_row_length(frame.strides[idx] as u32)
                        .image_subresource(vk::ImageSubresourceLayers {
                            aspect_mask: *plane,
                            mip_level: 0,
                            base_array_layer: 0,
                            layer_count: 1,
                        })
                        .image_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
                        .image_extent(vk::Extent3D {
                            width,
                            height,
                            depth: 1,
                        }),
                );
            }

            unsafe {
                device.cmd_copy_image_to_buffer(
                    frame.copy_cb,
                    image.image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    frame.buffer.buffer,
                    &regions,
                );
            }
        }

        // Transition the buffer to be readable.
        {
            let read_barrier = vk::BufferMemoryBarrier2::default()
                .buffer(frame.buffer.buffer)
                .src_stage_mask(vk::PipelineStageFlags2::TRANSFER)
                .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::HOST)
                .dst_access_mask(vk::AccessFlags2::HOST_READ)
                .offset(0)
                .size(vk::WHOLE_SIZE);

            let buffer_barriers = [read_barrier];
            let read_dependency_info =
                vk::DependencyInfo::default().buffer_memory_barriers(&buffer_barriers);

            device.cmd_pipeline_barrier2(frame.copy_cb, &read_dependency_info);
        }

        // Submit!
        {
            device.end_command_buffer(frame.copy_cb)?;
            device.reset_fences(&[frame.copy_fence])?;
            let wait_semas = [tp_acquire.timeline().as_semaphore()];
            let wait_vals = [tp_acquire.value()];
            let signal_semas = [tp_release.timeline().as_semaphore()];
            let signal_vals = [tp_release.value()];
            let mut timeline_sema_info = vk::TimelineSemaphoreSubmitInfo::default()
                .wait_semaphore_values(&wait_vals)
                .signal_semaphore_values(&signal_vals);

            let cbs = [frame.copy_cb];
            let submit_info = vk::SubmitInfo::default()
                .wait_semaphores(&wait_semas)
                .wait_dst_stage_mask(&[vk::PipelineStageFlags::ALL_COMMANDS])
                .command_buffers(&cbs)
                .signal_semaphores(&signal_semas)
                .push_next(&mut timeline_sema_info);

            device.queue_submit(self.graphics_queue.queue, &[submit_info], frame.copy_fence)?;
        }

        match self
            .input_frames
            .as_ref()
            .unwrap()
            .send((tp_release, frame))
        {
            Ok(_) => (),
            Err(crossbeam::SendError(_)) => {
                bail!("encoder thread died");
            }
        }

        Ok(())
    }
}

impl Drop for CpuEncoder {
    #[instrument(level = "trace", name = "CpuEncoder::drop", skip(self))]
    fn drop(&mut self) {
        drop(self.input_frames.take());
        for _ in self.done_frames.iter() {
            // Flush.
        }

        if let Some(handle) = self.encode_thread_handle.take() {
            match handle.join() {
                Ok(Ok(())) => (),
                Ok(Err(e)) => error!("encoder thread exited with error: {:#}", e),
                Err(_) => error!("encoder thread panicked"),
            }
        }
    }
}

fn encode_thread(
    mut encoder: impl EncoderInner,
    input_frames: crossbeam::Receiver<(VkTimelinePoint, VkExtMemoryFrame)>,
    done_frames: crossbeam::Sender<VkExtMemoryFrame>,
    compositor: CompositorHandle,
    stream_seq: u64,
    video_timebase: Timebase,
) -> anyhow::Result<()> {
    tracy_client::set_thread_name!("CPU Encode");

    let mut packet_slab = BytesMut::new();
    let mut seq = 0;

    for (tp, frame) in input_frames {
        let _tracy_frame = tracy_client::non_continuous_frame!("cpu encode");
        let span = trace_span!("encode_loop");
        let _guard = span.enter();

        let capture_ts = EPOCH.elapsed().as_millis() as u64;

        // Wait for the copy operation to finish.
        unsafe { tp.wait() }?;

        // Wake the compositor, so it can release buffers and send presentation
        // feedback.
        compositor.wake()?;

        let pts = video_timebase.now();

        encoder
            .send_picture(&frame, pts)
            .context("error in send_frame")?;

        while let Some(packet) = encoder.receive_packet().context("encoding error")? {
            packet_slab.extend_from_slice(packet.as_ref());
            let pkt = packet_slab.split().freeze();

            trace!(len = pkt.len(), "encoded video packet");

            compositor.dispatch(CompositorEvent::VideoFrame {
                stream_seq,
                seq,
                ts: capture_ts,
                frame: pkt,
                _hierarchical_layer: 0, // TODO
            });
            seq += 1;
        }

        done_frames.send(frame).ok();
    }

    // Flush.
    trace!("flushing encoder");

    encoder.flush().context("error in flush")?;
    loop {
        let packet = match encoder.receive_packet()? {
            Some(p) => p,
            None => break,
        };

        packet_slab.extend_from_slice(packet.as_ref());
        let pkt = packet_slab.split().freeze();

        trace!("encoded video packet ({} bytes)", pkt.len());

        compositor.dispatch(CompositorEvent::VideoFrame {
            stream_seq,
            seq,
            ts: EPOCH.elapsed().as_millis() as u64,
            frame: pkt,
            _hierarchical_layer: 0, // TODO
        });
        seq += 1;
    }

    Ok(())
}
