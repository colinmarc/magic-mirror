// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

// It's not me, it's vulkan.
#![allow(clippy::too_many_arguments)]

use std::sync::Arc;
use std::time;

use anyhow::{anyhow, bail, Context};
use ash::vk;
use bytes::Bytes;
use crossbeam_channel as crossbeam;
use tracing::{debug, error, instrument, trace, trace_span};

use self::gop_structure::HierarchicalP;
use crate::codec::VideoCodec;
use crate::session::control::VideoStreamParams;
use crate::vulkan::video::VideoQueueExt;
use crate::vulkan::*;

mod dpb;
mod gop_structure;
mod rate_control;
mod stats;

mod h264;
use h264::H264Encoder;

mod h265;
use h265::H265Encoder;

pub enum Encoder {
    H264(H264Encoder),
    H265(H265Encoder),
}

impl Encoder {
    pub fn new(
        vk: Arc<VkContext>,
        params: VideoStreamParams,
        framerate: u32,
        sink: impl Sink,
    ) -> anyhow::Result<Self> {
        match params.codec {
            VideoCodec::H264 => Ok(Self::H264(H264Encoder::new(vk, params, framerate, sink)?)),
            VideoCodec::H265 => Ok(Self::H265(H265Encoder::new(vk, params, framerate, sink)?)),
            _ => bail!("unsupported codec"),
        }
    }

    pub unsafe fn submit_encode(
        &mut self,
        image: &VkImage,
        acquire: VkTimelinePoint,
        release: VkTimelinePoint,
    ) -> anyhow::Result<()> {
        match self {
            Self::H264(encoder) => encoder.submit_encode(image, acquire, release),
            Self::H265(encoder) => encoder.submit_encode(image, acquire, release),
        }
    }

    pub fn input_format(&self) -> vk::Format {
        match self {
            Self::H264(encoder) => encoder.input_format(),
            Self::H265(encoder) => encoder.input_format(),
        }
    }

    pub fn create_input_image(&mut self) -> anyhow::Result<VkImage> {
        match self {
            Self::H264(encoder) => encoder.create_input_image(),
            Self::H265(encoder) => encoder.create_input_image(),
        }
    }

    pub fn request_refresh(&mut self) {
        match self {
            Encoder::H264(encoder) => encoder.request_refresh(),
            Encoder::H265(encoder) => encoder.request_refresh(),
        }
    }
}

struct EncoderInner {
    session: vk::VideoSessionKHR,
    session_memory: Vec<vk::DeviceMemory>,

    session_params: vk::VideoSessionParametersKHR,

    writer_thread_handle: Option<std::thread::JoinHandle<anyhow::Result<()>>>,
    submitted_frames: Option<crossbeam::Sender<WriterInput>>,
    done_frames: crossbeam::Receiver<EncoderOutputFrame>,

    dpb: dpb::DpbPool,

    width: u32,
    height: u32,
    framerate: u32,
    input_format: vk::Format,

    stats: stats::EncodeStats,

    vk: Arc<VkContext>,
}

impl EncoderInner {
    pub fn new(
        vk: Arc<VkContext>,
        width: u32,
        height: u32,
        framerate: u32,
        required_dpb_size: usize,
        profile: &mut vk::VideoProfileInfoKHR,
        capabilities: vk::VideoCapabilitiesKHR,
        session_params: &mut impl vk::ExtendsVideoSessionParametersCreateInfoKHR,
        sink: impl Sink,
    ) -> anyhow::Result<Self> {
        if vk.encode_queue.is_none() {
            bail!("no vulkan video support")
        }

        let (video_loader, _encode_loader) = vk.video_apis.as_ref().unwrap();
        let encode_family = vk.device_info.encode_family.unwrap();

        if capabilities.max_coded_extent.width < width
            || capabilities.max_coded_extent.height < height
        {
            bail!(
                "video resolution too large: (max {}x{})",
                capabilities.max_coded_extent.width,
                capabilities.max_coded_extent.height
            );
        }

        let format_info = list_format_props(
            video_loader,
            vk.device_info.pdevice,
            profile,
            vk::ImageUsageFlags::VIDEO_ENCODE_SRC_KHR,
        )?;

        for format in &format_info {
            trace!(?format, "available input format");
        }

        let input_format = match format_info.first() {
            Some(format) => format.format,
            None => bail!("unable to determine supported ENCODE_SRC format"),
        };

        trace!(?input_format, width, height, "using input format");

        let buffer_size_alignment = capabilities.min_bitstream_buffer_size_alignment as usize;

        let session = {
            let create_info = vk::VideoSessionCreateInfoKHR::default()
                .queue_family_index(encode_family)
                .flags(vk::VideoSessionCreateFlagsKHR::ALLOW_ENCODE_PARAMETER_OPTIMIZATIONS)
                .video_profile(profile)
                .picture_format(input_format)
                .reference_picture_format(input_format)
                .max_coded_extent(capabilities.max_coded_extent)
                .max_dpb_slots(capabilities.max_dpb_slots)
                .max_active_reference_pictures(capabilities.max_active_reference_pictures)
                .std_header_version(&capabilities.std_header_version);

            unsafe {
                video_loader
                    .create_video_session(&create_info, None)
                    .context("vkCreateVideoSessionKHR")?
            }
        };

        let session_memory =
            bind_session_memory(video_loader, &vk.device, &vk.device_info, session)?;

        let session_params = {
            let create_info = vk::VideoSessionParametersCreateInfoKHR::default()
                .video_session(session)
                .push_next(session_params);

            unsafe {
                video_loader
                    .create_video_session_parameters(&create_info, None)
                    .context("vkCreateVideoSessionParametersKHR")?
            }
        };

        let dpb = if capabilities
            .flags
            .contains(vk::VideoCapabilityFlagsKHR::SEPARATE_REFERENCE_IMAGES)
        {
            trace!("using separate images for DPB pool");

            dpb::DpbPool::new_separate_images(
                vk.clone(),
                input_format,
                width.next_multiple_of(capabilities.picture_access_granularity.width),
                height.next_multiple_of(capabilities.picture_access_granularity.height),
                profile,
                required_dpb_size,
            )?
        } else {
            trace!("using shared image for DPB pool");

            dpb::DpbPool::new(
                vk.clone(),
                input_format,
                width.next_multiple_of(capabilities.picture_access_granularity.width),
                height.next_multiple_of(capabilities.picture_access_granularity.height),
                profile,
                required_dpb_size,
            )?
        };

        let stats = stats::EncodeStats::default();

        let (submitted_frames_tx, submitted_frames_rx) = crossbeam::bounded(1);
        let (done_frames_tx, done_frames_rx) = crossbeam::unbounded();

        for _ in 0..1 {
            done_frames_tx
                .send(EncoderOutputFrame::new(
                    vk.clone(),
                    width,
                    height,
                    buffer_size_alignment,
                    profile,
                )?)
                .unwrap();
        }

        let vk_clone = vk.clone();
        let stats_clone = stats.clone();
        let handle = std::thread::Builder::new()
            .name("encoder writer".to_owned())
            .spawn(move || {
                writer_thread(
                    vk_clone,
                    submitted_frames_rx,
                    done_frames_tx,
                    sink,
                    stats_clone,
                )
            })?;

        Ok(Self {
            session,
            session_params,
            session_memory,

            writer_thread_handle: Some(handle),
            submitted_frames: Some(submitted_frames_tx),
            done_frames: done_frames_rx,

            dpb,

            width,
            height,
            framerate,
            input_format,

            stats,

            vk,
        })
    }

    fn create_input_image(&self, profile: &mut vk::VideoProfileInfoKHR) -> anyhow::Result<VkImage> {
        let image = {
            let mut profile_list_info = single_profile_list_info(profile);

            let create_info = vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(self.input_format)
                .extent(vk::Extent3D {
                    width: self.width,
                    height: self.height,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::VIDEO_ENCODE_SRC_KHR | vk::ImageUsageFlags::STORAGE)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED)
                .flags(vk::ImageCreateFlags::MUTABLE_FORMAT | vk::ImageCreateFlags::EXTENDED_USAGE)
                .push_next(&mut profile_list_info);

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

        let view = unsafe {
            let mut usage_info = vk::ImageViewUsageCreateInfo::default()
                .usage(vk::ImageUsageFlags::VIDEO_ENCODE_SRC_KHR);

            let create_info = vk::ImageViewCreateInfo::default()
                .image(image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(self.input_format)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .push_next(&mut usage_info);

            self.vk
                .device
                .create_image_view(&create_info, None)
                .context("VkCreateImageView")?
        };

        Ok(VkImage::wrap(
            self.vk.clone(),
            image,
            view,
            memory,
            self.input_format,
            self.width,
            self.height,
        ))
    }

    #[instrument(skip_all)]
    pub unsafe fn submit_encode(
        &mut self,
        input: &VkImage,
        tp_acquire: VkTimelinePoint,
        tp_release: VkTimelinePoint,
        frame_state: &gop_structure::GopFrame,
        rc_info: &mut (impl vk::ExtendsVideoBeginCodingInfoKHR + vk::ExtendsVideoCodingControlInfoKHR),
        codec_pic_info: &mut impl vk::ExtendsVideoEncodeInfoKHR,
        codec_setup_info: &mut impl vk::ExtendsVideoReferenceSlotInfoKHR,
        codec_ref_info: &mut [impl vk::ExtendsVideoReferenceSlotInfoKHR],
        insert: Option<Bytes>,
    ) -> anyhow::Result<()> {
        use ash::vk::Handle;
        if self.session_params.is_null() {
            bail!("session parameters not yet created");
        }

        let (video_loader, encode_loader) = self.vk.video_apis.as_ref().unwrap();
        let encode_queue = self.vk.encode_queue.as_ref().unwrap();

        // "Acquire" a buffer to copy to. This provides backpressure if the
        // encoder can't keep up.
        let res = trace_span!("wait_prev_frame").in_scope(|| self.done_frames.recv());
        let mut frame = match res {
            Ok(frame) => frame,
            Err(_) => {
                bail!("copy thread died");
            }
        };

        #[cfg(feature = "tracy")]
        {
            frame.tracy_frame = Some(tracy_client::non_continuous_frame!("encode"));
            if let Some(ref ctx) = encode_queue.tracy_context {
                frame.encode_span = Some(ctx.span(tracy_client::span_location!("encode"))?);
            }
        }

        begin_command_buffer(&self.vk.device, frame.encode_cb)?;

        // Record the start timestamp.
        #[cfg(feature = "tracy")]
        if let Some(encode_ts_pool) = &mut frame.encode_ts_pool {
            encode_ts_pool.cmd_reset(&self.vk.device, frame.encode_cb);
            self.vk.device.cmd_write_timestamp(
                frame.encode_cb,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                encode_ts_pool.pool,
                0,
            );
        }

        // Acquire the image from the graphics queue.
        insert_image_barrier(
            &self.vk.device,
            frame.encode_cb,
            input.image,
            Some((self.vk.graphics_queue.family, encode_queue.family)),
            vk::ImageLayout::GENERAL,
            vk::ImageLayout::VIDEO_ENCODE_SRC_KHR,
            vk::PipelineStageFlags2::NONE,
            vk::AccessFlags2::NONE,
            vk::PipelineStageFlags2::VIDEO_ENCODE_KHR,
            vk::AccessFlags2::VIDEO_ENCODE_READ_KHR,
        );

        // Bind the setup picture and any reference pictures.
        let setup_pic = self.dpb.setup_pic();
        let ref_pics = frame_state
            .ref_ids
            .iter()
            .map(|id| {
                self.dpb
                    .get_pic(*id)
                    .ok_or(anyhow!("ref pic {id} missing from dpb"))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        let mut bound_pics = vec![vk::VideoReferenceSlotInfoKHR::default()
            .slot_index(if setup_pic.currently_active {
                setup_pic.index as i32
            } else {
                -1
            })
            .picture_resource(&setup_pic.picture_resource_info)];

        for ref_pic in &ref_pics {
            assert!(ref_pic.currently_active);
            bound_pics.push(
                vk::VideoReferenceSlotInfoKHR::default()
                    .slot_index(ref_pic.index as i32)
                    .picture_resource(&ref_pic.picture_resource_info),
            );
        }

        trace!(
            ref_ids = ?frame_state.ref_ids,
            ref_slots = ?ref_pics.iter().map(|p| p.index).collect::<Vec<_>>(),
            setup_id = frame_state.id,
            setup_slot = setup_pic.index,
            gop_position = frame_state.gop_position,
            is_keyframe = frame_state.is_keyframe,
            forward_ref_count = frame_state.forward_ref_count,
            input_image = ?input.image,
            "encoding frame"
        );

        // Bind the session.
        {
            let mut begin_info = vk::VideoBeginCodingInfoKHR::default()
                .flags(vk::VideoBeginCodingFlagsKHR::empty())
                .video_session(self.session)
                .video_session_parameters(self.session_params)
                .reference_slots(&bound_pics);

            // Vulkan wants us to inform it of the current rate control, which
            // is unset on the first frame.
            if frame_state.stream_position != 0 {
                begin_info = begin_info.push_next(rc_info)
            }

            unsafe {
                video_loader.cmd_begin_video_coding(frame.encode_cb, &begin_info);
            };
        }

        // Reset on keyframes.
        if frame_state.is_keyframe {
            let ctrl_info = vk::VideoCodingControlInfoKHR::default()
                .flags(
                    vk::VideoCodingControlFlagsKHR::RESET
                        | vk::VideoCodingControlFlagsKHR::ENCODE_RATE_CONTROL,
                )
                .push_next(rc_info);

            unsafe {
                video_loader.cmd_control_video_coding(frame.encode_cb, &ctrl_info);
            };
        }

        // Encode.
        self.vk.device.cmd_begin_query(
            frame.encode_cb,
            frame.query_pool,
            0,
            vk::QueryControlFlags::empty(),
        );

        {
            // The input picture.
            let src_pic_resource = vk::VideoPictureResourceInfoKHR::default()
                .coded_extent(vk::Extent2D {
                    width: self.width,
                    height: self.height,
                })
                .image_view_binding(input.view);

            // The slot we're writing to.
            let setup_reference_slot = vk::VideoReferenceSlotInfoKHR::default()
                .slot_index(setup_pic.index as i32)
                .picture_resource(&setup_pic.picture_resource_info)
                .push_next(codec_setup_info);

            // The slots we're referencing.
            let reference_slots = ref_pics
                .iter()
                .zip(codec_ref_info.iter_mut())
                .map(|(ref_pic, codec_ref_info)| {
                    vk::VideoReferenceSlotInfoKHR::default()
                        .slot_index(ref_pic.index as i32)
                        .picture_resource(&ref_pic.picture_resource_info)
                        .push_next(codec_ref_info)
                })
                .collect::<Vec<_>>();

            let encode_info = vk::VideoEncodeInfoKHR::default()
                .flags(vk::VideoEncodeFlagsKHR::empty())
                .dst_buffer(frame.copy_buffer.buffer)
                .dst_buffer_range(frame.copy_buffer.len as u64)
                .src_picture_resource(src_pic_resource)
                .setup_reference_slot(&setup_reference_slot)
                .reference_slots(&reference_slots)
                .push_next(codec_pic_info);

            // Transition the DPB images/layers we need.
            let mut dpb_barriers = Vec::new();
            for pic in &ref_pics {
                dpb_barriers.push(
                    vk::ImageMemoryBarrier2::default()
                        .src_stage_mask(vk::PipelineStageFlags2::NONE)
                        .src_access_mask(vk::AccessFlags2::NONE)
                        .dst_stage_mask(vk::PipelineStageFlags2::VIDEO_ENCODE_KHR)
                        .dst_access_mask(vk::AccessFlags2::VIDEO_ENCODE_READ_KHR)
                        .old_layout(vk::ImageLayout::VIDEO_ENCODE_DPB_KHR)
                        .new_layout(vk::ImageLayout::VIDEO_ENCODE_DPB_KHR)
                        .image(pic.image)
                        .subresource_range(vk::ImageSubresourceRange {
                            aspect_mask: vk::ImageAspectFlags::COLOR,
                            base_mip_level: 0,
                            level_count: vk::REMAINING_MIP_LEVELS,
                            // For multiple-layers-in-one-image DPB, just the layer referenced.
                            base_array_layer: pic.picture_resource_info.base_array_layer,
                            layer_count: 1,
                        }),
                );
            }

            dpb_barriers.push(
                vk::ImageMemoryBarrier2::default()
                    .src_stage_mask(vk::PipelineStageFlags2::NONE)
                    .src_access_mask(vk::AccessFlags2::NONE)
                    .dst_stage_mask(vk::PipelineStageFlags2::VIDEO_ENCODE_KHR)
                    .dst_access_mask(
                        vk::AccessFlags2::VIDEO_ENCODE_WRITE_KHR
                            | vk::AccessFlags2::VIDEO_ENCODE_READ_KHR,
                    )
                    .old_layout(vk::ImageLayout::VIDEO_ENCODE_DPB_KHR)
                    .new_layout(vk::ImageLayout::VIDEO_ENCODE_DPB_KHR)
                    .image(setup_pic.image)
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        base_mip_level: 0,
                        level_count: vk::REMAINING_MIP_LEVELS,
                        base_array_layer: setup_pic.picture_resource_info.base_array_layer,
                        layer_count: 1,
                    }),
            );

            self.vk.device.cmd_pipeline_barrier2(
                frame.encode_cb,
                &vk::DependencyInfo::default().image_memory_barriers(&dpb_barriers),
            );

            // Update state as if the operation succeeded.
            if frame_state.forward_ref_count > 0 {
                // Keyframes clear all dpb slots.
                if frame_state.is_keyframe {
                    self.dpb.clear();
                }

                self.dpb.mark_active(setup_pic.index, frame_state.id);
            } else {
                self.dpb.mark_inactive(setup_pic.index);
            }

            unsafe {
                encode_loader.cmd_encode_video(frame.encode_cb, &encode_info);
            };
        }

        self.vk
            .device
            .cmd_end_query(frame.encode_cb, frame.query_pool, 0);

        // Unbind the session.
        {
            let end_info =
                vk::VideoEndCodingInfoKHR::default().flags(vk::VideoEndCodingFlagsKHR::empty());

            unsafe {
                video_loader.cmd_end_video_coding(frame.encode_cb, &end_info);
            };
        }

        // Release the input picture back to the graphics queue.
        insert_image_barrier(
            &self.vk.device,
            frame.encode_cb,
            input.image,
            Some((encode_queue.family, self.vk.graphics_queue.family)),
            vk::ImageLayout::VIDEO_ENCODE_SRC_KHR,
            vk::ImageLayout::GENERAL,
            vk::PipelineStageFlags2::VIDEO_ENCODE_KHR,
            vk::AccessFlags2::VIDEO_ENCODE_WRITE_KHR,
            vk::PipelineStageFlags2::empty(),
            vk::AccessFlags2::empty(),
        );

        // Record the end timestamp.
        #[cfg(feature = "tracy")]
        if let Some(encode_ts_pool) = &frame.encode_ts_pool {
            self.vk.device.cmd_write_timestamp(
                frame.encode_cb,
                vk::PipelineStageFlags::ALL_COMMANDS,
                encode_ts_pool.pool,
                1,
            );
        }

        #[cfg(feature = "tracy")]
        if let Some(span) = &mut frame.encode_span {
            span.end_zone();
        }

        // Wait for the output buffer to be clear of the previous copy
        // operation, then establish new timeline points.
        frame.tp_copied.wait()?;
        frame.tp_encoded += 10;
        frame.tp_copied = &frame.tp_encoded + 1;

        // Submit!
        {
            self.vk.device.end_command_buffer(frame.encode_cb)?;

            let cb_infos = [vk::CommandBufferSubmitInfo::default().command_buffer(frame.encode_cb)];

            let wait_infos = [vk::SemaphoreSubmitInfo::default()
                .semaphore(tp_acquire.timeline().as_semaphore())
                .value(tp_acquire.into())
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];

            let signal_infos = [
                vk::SemaphoreSubmitInfo::default()
                    .semaphore(frame.timeline.as_semaphore())
                    .value(frame.tp_encoded.value())
                    .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS),
                vk::SemaphoreSubmitInfo::default()
                    .semaphore(tp_release.timeline().as_semaphore())
                    .value(tp_release.value())
                    .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS),
            ];

            let submit_info = vk::SubmitInfo2::default()
                .wait_semaphore_infos(&wait_infos)
                .signal_semaphore_infos(&signal_infos)
                .command_buffer_infos(&cb_infos);

            let encode_queue = self.vk.encode_queue.as_ref().unwrap();
            self.vk
                .device
                .queue_submit2(encode_queue.queue, &[submit_info], vk::Fence::null())
                .context("vkQueueSubmit")?;
        }

        frame.hierarchical_layer = frame_state.id;
        frame.is_keyframe = frame_state.is_keyframe;
        if let Some(submitted_frames) = &self.submitted_frames {
            // Tell the other thread to copy out the finished packet when it's
            // finished. Optionally insert headers.
            if let Some(buf) = insert {
                submitted_frames
                    .send(WriterInput::InsertBytes(buf))
                    .map_err(|_| anyhow::anyhow!("writer thread died"))?;
            }

            submitted_frames
                .send(WriterInput::SubmittedFrame(frame))
                .map_err(|_| anyhow::anyhow!("writer thread died"))?;
        }

        Ok(())
    }
}

impl Drop for EncoderInner {
    fn drop(&mut self) {
        drop(self.submitted_frames.take());
        for done in self.done_frames.iter() {
            drop(done)
        }

        if let Some(handle) = self.writer_thread_handle.take() {
            match handle.join() {
                Ok(Ok(())) => (),
                Ok(Err(e)) => error!("copy thread exited with error: {:#}", e),
                Err(_) => error!("copy thread panicked"),
            }
        }

        debug!("stream stats: \n{:#?}", self.stats);

        let (video_loader, _) = self.vk.video_apis.as_ref().unwrap();

        unsafe {
            self.vk
                .device
                .queue_wait_idle(self.vk.encode_queue.as_ref().unwrap().queue)
                .unwrap();

            video_loader.destroy_video_session(self.session, None);
            video_loader.destroy_video_session_parameters(self.session_params, None);

            for memory in self.session_memory.drain(..) {
                self.vk.device.free_memory(memory, None);
            }
        }
    }
}

/// A synchronized buffer for writing encoded frames to. Passed back and forth
/// between the submission thread and the copy thread.
struct EncoderOutputFrame {
    encode_cb: vk::CommandBuffer,
    copy_buffer: VkHostBuffer,
    query_pool: vk::QueryPool,

    hierarchical_layer: u32,
    is_keyframe: bool,

    timeline: VkTimelineSemaphore,
    tp_encoded: VkTimelinePoint,
    tp_copied: VkTimelinePoint,

    #[cfg(feature = "tracy")]
    tracy_frame: Option<tracy_client::Frame>,

    #[cfg(feature = "tracy")]
    encode_span: Option<tracy_client::GpuSpan>,

    #[cfg(feature = "tracy")]
    encode_ts_pool: Option<VkTimestampQueryPool>,

    vk: Arc<VkContext>,
}

impl EncoderOutputFrame {
    pub fn new(
        vk: Arc<VkContext>,
        width: u32,
        height: u32,
        buffer_size_alignment: usize,
        profile: &mut vk::VideoProfileInfoKHR,
    ) -> anyhow::Result<Self> {
        let buffer_size = (width * height * 3).next_multiple_of(buffer_size_alignment as u32);

        let mut profile_list_info = single_profile_list_info(profile);

        let copy_buffer = {
            let buf = {
                let create_info = vk::BufferCreateInfo::default()
                    .size(buffer_size as u64)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .usage(vk::BufferUsageFlags::VIDEO_ENCODE_DST_KHR)
                    .push_next(&mut profile_list_info);

                unsafe { vk.device.create_buffer(&create_info, None)? }
            };

            let requirements = unsafe { vk.device.get_buffer_memory_requirements(buf) };

            let alloc_info = vk::MemoryAllocateInfo::default()
                .allocation_size(requirements.size)
                .memory_type_index(vk.device_info.host_visible_mem_type_index);

            let memory = unsafe { vk.device.allocate_memory(&alloc_info, None)? };

            unsafe {
                vk.device
                    .bind_buffer_memory(buf, memory, 0)
                    .context("vkBindBufferMemory")?
            };

            VkHostBuffer::wrap(vk.clone(), buf, memory, buffer_size as usize)
        };

        let encode_queue = vk.encode_queue.as_ref().unwrap();
        let encode_cb = allocate_command_buffer(&vk.device, encode_queue.command_pool)?;

        let query_pool = {
            let mut video_pool_info = vk::QueryPoolVideoEncodeFeedbackCreateInfoKHR::default()
                .encode_feedback_flags(
                    vk::VideoEncodeFeedbackFlagsKHR::BITSTREAM_BUFFER_OFFSET
                        | vk::VideoEncodeFeedbackFlagsKHR::BITSTREAM_BYTES_WRITTEN,
                );

            let create_info = vk::QueryPoolCreateInfo::default()
                .query_type(vk::QueryType::VIDEO_ENCODE_FEEDBACK_KHR)
                .query_count(1)
                .push_next(profile)
                .push_next(&mut video_pool_info);

            unsafe {
                let query_pool = vk
                    .device
                    .create_query_pool(&create_info, None)
                    .context("vkCreateQueryPool")?;
                vk.device.reset_query_pool(query_pool, 0, 1);

                query_pool
            }
        };

        let timeline = VkTimelineSemaphore::new(vk.clone(), 0)?;

        #[cfg(feature = "tracy")]
        let encode_ts_pool = if matches!(
            vk.device_info.driver_version,
            DriverVersion::MesaRadv { .. }
        ) {
            // RADV offers support for timestamp queries, but then has an
            // assertion at timestamp write time.
            None
        } else {
            create_timestamp_query_pool(&vk.device, 2).ok()
        };

        Ok(EncoderOutputFrame {
            encode_cb,
            copy_buffer,
            query_pool,

            hierarchical_layer: 0,
            is_keyframe: false,

            tp_encoded: timeline.new_point(0),
            tp_copied: timeline.new_point(0),
            timeline,

            #[cfg(feature = "tracy")]
            tracy_frame: None,
            #[cfg(feature = "tracy")]
            encode_span: None,
            #[cfg(feature = "tracy")]
            encode_ts_pool,

            vk,
        })
    }
}

impl Drop for EncoderOutputFrame {
    fn drop(&mut self) {
        unsafe {
            let device = &self.vk.device;
            let encode_queue = self.vk.encode_queue.as_ref().unwrap();

            device.queue_wait_idle(encode_queue.queue).unwrap();
            device.free_command_buffers(encode_queue.command_pool, &[self.encode_cb]);
            device.destroy_query_pool(self.query_pool, None);

            #[cfg(feature = "tracy")]
            if let Some(pool) = self.encode_ts_pool.take() {
                device.destroy_query_pool(pool.pool, None);
            }
        }
    }
}

// SAFETY: the contained pointers are nothing fancy.
unsafe impl Send for EncoderOutputFrame {}

/// Allows the caller to decide where to sink the frames.
pub trait Sink: Send + 'static {
    fn write_frame(&mut self, ts: time::Instant, frame: Bytes, hierarchical_layer: u32);
}

/// Allows us to intersperse arbitrary headers or other data into the bitstream.
enum WriterInput {
    InsertBytes(Bytes),
    SubmittedFrame(EncoderOutputFrame),
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct QueryResults {
    offset: i32,
    size: i32,
    result: i32,
}

/// Responsible for copying encoded frames from the output buffer and
/// dispatching them to the client. Passes instances of `EncodedOutputFrame`
/// back and forth with the main thread.
fn writer_thread(
    vk: Arc<VkContext>,
    input: crossbeam::Receiver<WriterInput>,
    done: crossbeam::Sender<EncoderOutputFrame>,
    mut sink: impl Sink,
    stats: stats::EncodeStats,
) -> anyhow::Result<()> {
    let device = &vk.device;

    let mut capture_ts = time::Instant::now();

    for frame in input {
        #[allow(unused_mut)]
        let mut frame = match frame {
            WriterInput::InsertBytes(header) => {
                sink.write_frame(time::Instant::now(), header, 0);
                continue;
            }
            WriterInput::SubmittedFrame(frame) => frame,
        };

        let dur = capture_ts.elapsed();
        capture_ts = time::Instant::now();

        // Wait for the frame to finish encoding.
        unsafe {
            frame.tp_encoded.wait()?;
        }

        #[cfg(feature = "tracy")]
        {
            frame.tracy_frame.take();
            if let Some(span) = frame.encode_span.take() {
                if let Some(pool) = &mut frame.encode_ts_pool {
                    let timestamps = pool.fetch_results(device)?;
                    span.upload_timestamp(timestamps[0], timestamps[1])
                }
            }
        }

        // Get the buffer offsets for the encoded data.
        let mut results = [QueryResults::default()];
        unsafe {
            device
                .get_query_pool_results(
                    frame.query_pool,
                    0,
                    &mut results,
                    vk::QueryResultFlags::WITH_STATUS_KHR,
                )
                .context("vkGetQueryPoolResults")?;
            device.reset_query_pool(frame.query_pool, 0, 1)
        }

        let res = vk::QueryResultStatusKHR::from_raw(results[0].result);
        if res != vk::QueryResultStatusKHR::COMPLETE {
            bail!("encode failed: {:?}", res);
        }

        trace!(len = results[0].size, ?dur, "encoded packet");
        stats.record_frame_size(
            frame.is_keyframe,
            frame.hierarchical_layer,
            results[0].size as usize,
        );

        let packet = unsafe {
            let ptr = frame.copy_buffer.access as *const u8;
            std::slice::from_raw_parts(
                ptr.add(results[0].offset as usize),
                results[0].size as usize,
            )
        };

        let data = Bytes::copy_from_slice(packet);
        unsafe {
            frame.tp_copied.signal()?;
        }

        sink.write_frame(capture_ts, data, frame.hierarchical_layer);
        done.send(frame).ok();
    }

    Ok(())
}

fn list_format_props<'a>(
    video_loader: &'a VideoQueueExt,
    pdevice: vk::PhysicalDevice,
    profile: &mut vk::VideoProfileInfoKHR,
    usage: vk::ImageUsageFlags,
) -> anyhow::Result<Vec<vk::VideoFormatPropertiesKHR<'a>>> {
    let mut profile_list_info = single_profile_list_info(profile);
    let format_info = vk::PhysicalDeviceVideoFormatInfoKHR::default()
        .image_usage(usage)
        .push_next(&mut profile_list_info);

    let props = unsafe {
        video_loader
            .get_physical_device_video_format_properties(pdevice, &format_info)
            .context("vkGetVideoFormatPropertiesKHR")?
    };

    Ok(props)
}

fn bind_session_memory(
    video_loader: &VideoQueueExt,
    device: &ash::Device,
    device_info: &VkDeviceInfo,
    session: vk::VideoSessionKHR,
) -> anyhow::Result<Vec<vk::DeviceMemory>> {
    let mut session_memory = Vec::new();
    let reqs = unsafe { video_loader.get_video_session_memory_requirements(session)? };

    let mut binds = Vec::new();
    for req in reqs.into_iter() {
        let memory = {
            let mut alloc_info =
                vk::MemoryAllocateInfo::default().allocation_size(req.memory_requirements.size);

            let mem_type_idx = select_memory_type(
                &device_info.memory_props,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
                Some(req.memory_requirements.memory_type_bits),
            )
            .or_else(|| {
                select_memory_type(
                    &device_info.memory_props,
                    vk::MemoryPropertyFlags::empty(),
                    Some(req.memory_requirements.memory_type_bits),
                )
            });

            if mem_type_idx.is_none() {
                bail!("no suitable memory type for video session");
            }

            alloc_info = alloc_info.memory_type_index(mem_type_idx.unwrap());

            unsafe {
                device
                    .allocate_memory(&alloc_info, None)
                    .context("vkAllocateMemory")?
            }
        };

        session_memory.push(memory);
        binds.push(
            vk::BindVideoSessionMemoryInfoKHR::default()
                .memory_bind_index(req.memory_bind_index)
                .memory(memory)
                .memory_size(req.memory_requirements.size),
        );
    }

    unsafe {
        video_loader
            .bind_video_session_memory(device, session, &binds)
            .context("vkBindVideoSessionMemory")?
    }

    Ok(session_memory)
}

fn default_profile(op: vk::VideoCodecOperationFlagsKHR) -> vk::VideoProfileInfoKHR<'static> {
    vk::VideoProfileInfoKHR::default()
        .video_codec_operation(op)
        .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::TYPE_420)
        .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
        .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
}

fn default_hdr10_profile(op: vk::VideoCodecOperationFlagsKHR) -> vk::VideoProfileInfoKHR<'static> {
    vk::VideoProfileInfoKHR::default()
        .video_codec_operation(op)
        .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::TYPE_420)
        .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_10)
        .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_10)
}

fn default_encode_usage() -> vk::VideoEncodeUsageInfoKHR<'static> {
    let tuning_mode = vk::VideoEncodeTuningModeKHR::ULTRA_LOW_LATENCY;

    vk::VideoEncodeUsageInfoKHR::default()
        .video_usage_hints(vk::VideoEncodeUsageFlagsKHR::STREAMING)
        .video_content_hints(vk::VideoEncodeContentFlagsKHR::RENDERED)
        .tuning_mode(tuning_mode)
}

fn single_profile_list_info<'a>(
    profile: &'a mut vk::VideoProfileInfoKHR,
) -> vk::VideoProfileListInfoKHR<'a> {
    vk::VideoProfileListInfoKHR {
        p_profiles: <*const _>::cast(profile),
        profile_count: 1,
        ..Default::default()
    }
}

fn default_structure(
    codec: VideoCodec,
    max_codec_layers: u32,
    max_dpb_slots: u32,
) -> anyhow::Result<HierarchicalP> {
    const MAX_LAYERS: u32 = 4;
    const DEFAULT_GOP_SIZE: u32 = 256;

    // Disable hierarchical coding on H264, because it's broken.
    let mut layers = if codec == VideoCodec::H264 {
        1
    } else {
        std::cmp::min(MAX_LAYERS, max_codec_layers)
    };

    let mut structure = HierarchicalP::new(layers, DEFAULT_GOP_SIZE);
    while structure.required_dpb_size() as u32 > max_dpb_slots {
        layers -= 1;
        if layers == 0 {
            bail!("max_dpb_slots too low");
        }

        structure = HierarchicalP::new(layers, DEFAULT_GOP_SIZE);
    }

    Ok(structure)
}
