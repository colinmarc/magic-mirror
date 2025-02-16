// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{mem::ManuallyDrop, sync::Arc};

use anyhow::anyhow;
use ash::vk;

mod composite;
mod convert;

use tracing::{instrument, trace, trace_span, warn};

use super::{
    compositor::{self, buffers::SyncobjTimelinePoint},
    DisplayParams, SessionHandle, VideoStreamParams,
};
use crate::{
    color::ColorSpace,
    encoder::{self},
    session::EPOCH,
    vulkan::*,
};

struct Sink(SessionHandle);

impl encoder::Sink for Sink {
    fn write_frame(
        &mut self,
        ts: std::time::Instant,
        frame: bytes::Bytes,
        hierarchical_layer: u32,
        is_keyframe: bool,
    ) {
        let pts = (ts - *EPOCH).as_millis() as u64;
        self.0
            .dispatch_video_frame(pts, frame, hierarchical_layer, is_keyframe);

        // Wake the compositor, so it can release buffers and send presentation
        // feedback.
        let _ = self.0.wake();
    }
}

pub struct SwapFrame {
    convert_ds: vk::DescriptorSet, // Should be dropped first.
    draws: Vec<(vk::ImageView, glam::Vec2, glam::Vec2)>,
    texture_semas: Vec<vk::Semaphore>, // Reused each frame.
    texture_semas_used: usize,

    /// An RGBA image to composite to.
    blend_image: VkImage,
    /// A YUV image we copy to before passing on to the encoder.
    encode_image: VkImage,
    plane_views: Vec<vk::ImageView>,

    staging_cb: vk::CommandBuffer,
    render_cb: vk::CommandBuffer,
    use_staging: bool,

    timeline: VkTimelineSemaphore,
    tp_staging_done: VkTimelinePoint,
    tp_render_done: VkTimelinePoint,
    tp_clear: VkTimelinePoint,

    // For tracing.
    staging_ts_pool: VkTimestampQueryPool,
    staging_span: Option<tracy_client::GpuSpan>,
    render_ts_pool: VkTimestampQueryPool,
    render_span: Option<tracy_client::GpuSpan>,
}

pub enum TextureSync {
    Explicit(SyncobjTimelinePoint),
    ImplicitInterop,
}

pub struct EncodePipeline {
    display_params: DisplayParams,
    streaming_params: VideoStreamParams,

    composite_pipeline: composite::CompositePipeline,
    convert_pipeline: convert::ConvertPipeline,
    encoder: ManuallyDrop<encoder::Encoder>,

    swap: [SwapFrame; 2],
    swap_idx: usize,

    vk: Arc<VkContext>,
}

impl EncodePipeline {
    #[instrument(level = "trace", skip_all)]
    pub fn new(
        vk: Arc<VkContext>,
        compositor_handle: SessionHandle,
        display_params: DisplayParams,
        streaming_params: VideoStreamParams,
    ) -> anyhow::Result<Self> {
        if streaming_params.width != display_params.width
            || streaming_params.height != display_params.height
        {
            trace!(
                ?streaming_params,
                ?display_params,
                "stream and display params differ"
            );

            // Superres is not implemented yet.
            unimplemented!()
        }

        let sink = Sink(compositor_handle);
        let mut encoder =
            encoder::Encoder::new(vk.clone(), streaming_params, display_params.framerate, sink)?;

        let encode_format = encoder.input_format();

        let composite_pipeline = composite::CompositePipeline::new(vk.clone())?;
        let convert_pipeline =
            convert::ConvertPipeline::new(vk.clone(), format_is_semiplanar(encode_format))?;

        let swap = [
            new_swapframe(vk.clone(), encoder.create_input_image()?, &convert_pipeline)?,
            new_swapframe(vk.clone(), encoder.create_input_image()?, &convert_pipeline)?,
        ];

        Ok(Self {
            display_params,
            streaming_params,

            composite_pipeline,
            convert_pipeline,
            encoder: ManuallyDrop::new(encoder),

            swap,
            swap_idx: 0,

            vk,
        })
    }

    // pub fn encode_single_surface(&mut self, surface: wl_surface::WlSurface) {
    //     todo!()
    // }

    #[instrument(level = "trace", skip_all)]
    pub unsafe fn begin(&mut self) -> anyhow::Result<bool> {
        let device = &self.vk.device;
        let frame = &mut self.swap[self.swap_idx];

        let ready = frame.tp_clear.poll()?;

        // If the previous frame isn't ready, drop this one to let the app
        // catch up.
        if !ready {
            return Ok(false);
        }

        // Trace on on the GPU side.
        if let Some(ref ctx) = self.vk.graphics_queue.tracy_context {
            if let Some(span) = frame.staging_span.take() {
                let timestamps = frame.staging_ts_pool.fetch_results(device)?;
                span.upload_timestamp(timestamps[0], timestamps[1]);
            }

            if let Some(span) = frame.render_span.take() {
                let timestamps = frame.render_ts_pool.fetch_results(device)?;
                span.upload_timestamp(timestamps[0], timestamps[1]);
            }

            // We conditionally create the staging span, below. Rendering always happens.
            frame.render_span = Some(ctx.span(tracy_client::span_location!("render"))?);
        }

        frame.texture_semas_used = 0;
        frame.tp_staging_done += 10;
        frame.tp_render_done = &frame.tp_staging_done + 1;
        frame.tp_clear = &frame.tp_render_done + 1;

        frame.use_staging = false;

        begin_command_buffer(device, frame.staging_cb)?;
        begin_command_buffer(device, frame.render_cb)?;

        // Record the start timestamp.
        frame.render_ts_pool.cmd_reset(device, frame.render_cb);
        device.cmd_write_timestamp(
            frame.render_cb,
            vk::PipelineStageFlags::TOP_OF_PIPE,
            frame.render_ts_pool.pool,
            0,
        );

        // Transition the blend image to be writable.
        insert_image_barrier(
            device,
            frame.render_cb,
            frame.blend_image.image,
            None,
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            vk::PipelineStageFlags2::NONE,
            vk::AccessFlags2::NONE,
            vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
        );

        Ok(true)
    }

    /// Adds a surface to be drawn. Returns the timeline point when the texture
    /// will no longer be in use. A return value of None indicates the texture
    /// is already safe to reuse.
    #[instrument(level = "trace", skip_all)]
    pub unsafe fn composite_surface(
        &mut self,
        texture: &compositor::buffers::Buffer,
        sync: Option<TextureSync>,
        dest: compositor::surface::SurfaceConfiguration,
    ) -> anyhow::Result<Option<VkTimelinePoint>> {
        let device = &self.vk.device;
        let frame = &mut self.swap[self.swap_idx];

        let (view, release) = match &texture.backing {
            compositor::buffers::BufferBacking::Shm {
                dirty,
                staging_buffer,
                image,
                format,
                ..
            } => {
                if *dirty {
                    // We only set up tracing for the staging command buffer if
                    // we're actually going to use it.
                    if !frame.use_staging {
                        if let Some(ref ctx) = self.vk.graphics_queue.tracy_context {
                            frame.staging_span = Some(ctx.span(tracy_client::span_location!())?);
                        }

                        // Record the start timestamp.
                        frame.staging_ts_pool.cmd_reset(device, frame.staging_cb);
                        device.cmd_write_timestamp(
                            frame.staging_cb,
                            vk::PipelineStageFlags::TOP_OF_PIPE,
                            frame.staging_ts_pool.pool,
                            0,
                        );
                    }

                    frame.use_staging = true;

                    // Transfer the image to be writable. The upload happens
                    // in the staging command buffer.
                    insert_image_barrier(
                        device,
                        frame.staging_cb,
                        image.image,
                        None,
                        vk::ImageLayout::UNDEFINED,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        vk::PipelineStageFlags2::NONE,
                        vk::AccessFlags2::NONE,
                        vk::PipelineStageFlags2::TRANSFER,
                        vk::AccessFlags2::TRANSFER_WRITE,
                    );

                    // Upload from the staging buffer to the texture.
                    cmd_upload_shm(
                        device,
                        frame.staging_cb,
                        staging_buffer,
                        image,
                        format.stride / format.bpp as u32,
                        format.height,
                    );
                }

                // Transition the image to be readable (in the second command buffer).
                insert_image_barrier(
                    device,
                    frame.render_cb,
                    image.image,
                    None,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::PipelineStageFlags2::TRANSFER,
                    vk::AccessFlags2::TRANSFER_WRITE,
                    vk::PipelineStageFlags2::FRAGMENT_SHADER,
                    vk::AccessFlags2::SHADER_READ,
                );

                assert!(sync.is_none());

                (image.view, None)
            }
            compositor::buffers::BufferBacking::Dmabuf { image, fd, .. } => {
                // Transition the image to be readable. A special queue,
                // EXTERNAL, is used in a queue transfer to indicate
                // acquiring the texture from the wayland client.
                insert_image_barrier(
                    device,
                    frame.render_cb,
                    image.image,
                    Some((vk::QUEUE_FAMILY_FOREIGN_EXT, self.vk.graphics_queue.family)),
                    vk::ImageLayout::GENERAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::PipelineStageFlags2::NONE,
                    vk::AccessFlags2::NONE,
                    vk::PipelineStageFlags2::FRAGMENT_SHADER,
                    vk::AccessFlags2::SHADER_READ,
                );

                // Release the image at the end.
                insert_image_barrier(
                    device,
                    frame.render_cb,
                    image.image,
                    Some((self.vk.graphics_queue.family, vk::QUEUE_FAMILY_FOREIGN_EXT)),
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::ImageLayout::GENERAL,
                    vk::PipelineStageFlags2::ALL_GRAPHICS,
                    vk::AccessFlags2::SHADER_READ,
                    vk::PipelineStageFlags2::NONE,
                    vk::AccessFlags2::NONE,
                );

                if let Some(sync) = sync {
                    let sema = allocate_texture_semaphore(self.vk.clone(), frame)?;

                    match sync {
                        TextureSync::Explicit(syncobj) => {
                            syncobj.import_as_semaphore(sema)?;
                        }
                        TextureSync::ImplicitInterop => {
                            compositor::buffers::import_dmabuf_fence_as_semaphore(
                                self.vk.clone(),
                                sema,
                                fd,
                            )?;
                        }
                    }
                }

                (image.view, Some(frame.tp_render_done.clone()))
            }
        };

        // Convert the destination rect into clip coordinates.
        let display_size: glam::UVec2 =
            (self.display_params.width, self.display_params.height).into();
        let dst_pos = (dest.topleft.as_vec2() / display_size.as_vec2() * 2.0) - 1.0;
        let dst_size = dest.size.as_vec2() / display_size.as_vec2() * 2.0;

        // Draw.
        frame.draws.push((view, dst_pos, dst_size));

        Ok(release)
    }

    /// End the current frame and submit it to the GPU. Returns the timeline
    /// point indicating when rendering and encoding have both completed.
    #[instrument(skip_all)]
    pub unsafe fn end_and_submit(&mut self) -> anyhow::Result<VkTimelinePoint> {
        let device = &self.vk.device;
        let frame = &mut self.swap[self.swap_idx];

        // Collate draw calls. We don't do this as we go because we need to do
        // all the sync outside of a dynamic rendering pass.
        self.composite_pipeline
            .begin_compositing(frame.render_cb, &frame.blend_image);

        for (view, dst_pos, dst_size) in frame.draws.drain(..) {
            self.composite_pipeline
                .composite_surface(frame.render_cb, view, dst_pos, dst_size)?;
        }

        self.composite_pipeline.end_compositing(frame.render_cb);

        // Transition the blend image to be readable.
        insert_image_barrier(
            device,
            frame.render_cb,
            frame.blend_image.image,
            None,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            vk::ImageLayout::GENERAL,
            vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
            vk::PipelineStageFlags2::COMPUTE_SHADER,
            vk::AccessFlags2::SHADER_STORAGE_READ,
        );

        // Acquire the encode image from the encode queue, but not for the
        // first frame.
        if frame.tp_clear.value() > 20 {
            let src_queue_family = self.vk.encode_queue.as_ref().unwrap().family;

            insert_image_barrier(
                device,
                frame.render_cb,
                frame.encode_image.image,
                Some((src_queue_family, self.vk.graphics_queue.family)),
                vk::ImageLayout::VIDEO_ENCODE_SRC_KHR,
                vk::ImageLayout::GENERAL,
                vk::PipelineStageFlags2::NONE,
                vk::AccessFlags2::NONE,
                vk::PipelineStageFlags2::COMPUTE_SHADER,
                vk::AccessFlags2::SHADER_STORAGE_WRITE,
            );
        } else {
            // Otherwise, just transition the image to be writable.
            insert_image_barrier(
                device,
                frame.render_cb,
                frame.encode_image.image,
                None,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::GENERAL,
                vk::PipelineStageFlags2::NONE,
                vk::AccessFlags2::NONE,
                vk::PipelineStageFlags2::COMPUTE_SHADER,
                vk::AccessFlags2::SHADER_STORAGE_WRITE,
            );
        }

        // We're converting the blend image, which is scRGB.
        let input_color_space = ColorSpace::LinearExtSrgb;

        self.convert_pipeline.cmd_convert(
            frame.render_cb,
            frame.blend_image.width,
            frame.blend_image.height,
            frame.convert_ds,
            input_color_space,
            self.streaming_params.profile,
        );

        // Transfer to the encode queue.
        let dst_queue_family = self.vk.encode_queue.as_ref().unwrap().family;
        insert_image_barrier(
            device,
            frame.render_cb,
            frame.encode_image.image,
            Some((self.vk.graphics_queue.family, dst_queue_family)),
            vk::ImageLayout::GENERAL,
            vk::ImageLayout::VIDEO_ENCODE_SRC_KHR,
            vk::PipelineStageFlags2::COMPUTE_SHADER,
            vk::AccessFlags2::SHADER_STORAGE_WRITE,
            vk::PipelineStageFlags2::empty(),
            vk::AccessFlags2::empty(),
        );

        let mut submits = Vec::new();

        let staging_cb_infos =
            [vk::CommandBufferSubmitInfoKHR::default().command_buffer(frame.staging_cb)];

        let staging_signal_infos = [vk::SemaphoreSubmitInfo::default()
            .semaphore(frame.timeline.as_semaphore())
            .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .value(frame.tp_staging_done.value())];

        let staging_submit_info = vk::SubmitInfo2::default()
            .command_buffer_infos(&staging_cb_infos)
            .signal_semaphore_infos(&staging_signal_infos);

        // Only submit the staging cb if we actually recorded commands to it.
        if frame.use_staging {
            // Record the end timestamp.
            device.cmd_write_timestamp(
                frame.staging_cb,
                vk::PipelineStageFlags::ALL_COMMANDS,
                frame.staging_ts_pool.pool,
                1,
            );

            if let Some(span) = &mut frame.staging_span {
                span.end_zone();
            }

            device.end_command_buffer(frame.staging_cb)?;
            submits.push(staging_submit_info);
        } else {
            frame.tp_staging_done.signal()?;
        }

        // Record the end timestamp.
        device.cmd_write_timestamp(
            frame.render_cb,
            vk::PipelineStageFlags::ALL_COMMANDS,
            frame.render_ts_pool.pool,
            1,
        );

        if let Some(span) = &mut frame.render_span {
            span.end_zone();
        }

        device.end_command_buffer(frame.render_cb)?;

        let render_cb_infos =
            [vk::CommandBufferSubmitInfoKHR::default().command_buffer(frame.render_cb)];
        let mut render_wait_infos = vec![vk::SemaphoreSubmitInfo::default()
            .semaphore(frame.timeline.as_semaphore())
            .stage_mask(vk::PipelineStageFlags2::FRAGMENT_SHADER)
            .value(frame.tp_staging_done.value())];
        let render_signal_infos = [vk::SemaphoreSubmitInfo::default()
            .semaphore(frame.timeline.as_semaphore())
            .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .value(frame.tp_render_done.value())];

        for sema in &frame.texture_semas[0..frame.texture_semas_used] {
            render_wait_infos.push(
                vk::SemaphoreSubmitInfo::default()
                    .semaphore(*sema)
                    .stage_mask(vk::PipelineStageFlags2::FRAGMENT_SHADER),
            );
        }

        let render_submit_info = vk::SubmitInfo2::default()
            .command_buffer_infos(&render_cb_infos)
            .wait_semaphore_infos(&render_wait_infos)
            .signal_semaphore_infos(&render_signal_infos);

        submits.push(render_submit_info);

        trace_span!("queue_submit2").in_scope(|| {
            device.queue_submit2(self.vk.graphics_queue.queue, &submits, vk::Fence::null())
        })?;

        // Trigger encode.
        self.encoder.submit_encode(
            &frame.encode_image,
            frame.tp_render_done.clone(),
            frame.tp_clear.clone(),
        )?;

        // Wait for uploads to finish before returning, so that writes to the
        // staging buffers are synchronized.
        trace_span!("tp_staging_done.wait").in_scope(|| frame.tp_staging_done.wait())?;
        let tp_clear = frame.tp_clear.clone();

        let swap_len = self.swap.len();
        self.swap_idx = (self.swap_idx + 1) % swap_len;

        Ok(tp_clear)
    }

    pub fn request_refresh(&mut self) {
        self.encoder.request_refresh()
    }
}

impl Drop for EncodePipeline {
    fn drop(&mut self) {
        let device = &self.vk.device;

        // Drop the encoder, since it consumes some of the shared resources below.
        unsafe {
            ManuallyDrop::drop(&mut self.encoder);
        }

        unsafe {
            device.device_wait_idle().unwrap();

            for frame in self.swap.iter() {
                device.free_command_buffers(
                    self.vk.graphics_queue.command_pool,
                    &[frame.staging_cb, frame.render_cb],
                );

                for view in &frame.plane_views {
                    device.destroy_image_view(*view, None);
                }

                for sema in &frame.texture_semas {
                    device.destroy_semaphore(*sema, None);
                }

                device.destroy_query_pool(frame.render_ts_pool.pool, None);
                device.destroy_query_pool(frame.staging_ts_pool.pool, None);
            }
        }
    }
}

fn new_swapframe(
    vk: Arc<VkContext>,
    encode_image: VkImage,
    convert_pipeline: &convert::ConvertPipeline,
) -> anyhow::Result<SwapFrame> {
    let blend_image = VkImage::new(
        vk.clone(),
        composite::BLEND_FORMAT,
        false,
        encode_image.width,
        encode_image.height,
        vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::SAMPLED,
        vk::SharingMode::EXCLUSIVE,
        vk::ImageCreateFlags::empty(),
    )?;

    let mut plane_views = Vec::new();
    let (single_plane_format, double_plane_format) = disjoint_plane_formats(encode_image.format)
        .ok_or(anyhow!(
            "couldn't find a disjoint plane formats for {:?}",
            encode_image.format
        ))?;

    let disjoint_formats = if format_is_semiplanar(encode_image.format) {
        vec![
            single_plane_format, // Y
            double_plane_format, // UV
        ]
    } else {
        vec![
            single_plane_format, // Y
            single_plane_format, // U
            single_plane_format, // V
        ]
    };

    let aspects = [
        vk::ImageAspectFlags::PLANE_0,
        vk::ImageAspectFlags::PLANE_1,
        vk::ImageAspectFlags::PLANE_2,
    ];

    for (idx, format) in disjoint_formats.into_iter().enumerate() {
        let mut usage_info =
            vk::ImageViewUsageCreateInfo::default().usage(vk::ImageUsageFlags::STORAGE);
        let create_info = vk::ImageViewCreateInfo::default()
            .image(encode_image.image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(format)
            .components(vk::ComponentMapping {
                r: vk::ComponentSwizzle::IDENTITY,
                g: vk::ComponentSwizzle::IDENTITY,
                b: vk::ComponentSwizzle::IDENTITY,
                a: vk::ComponentSwizzle::IDENTITY,
            })
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: aspects[idx],
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            })
            .push_next(&mut usage_info);

        let view = unsafe { vk.device.create_image_view(&create_info, None)? };
        plane_views.push(view);
    }

    let convert_ds = convert_pipeline.ds_for_conversion(&blend_image, &plane_views)?;

    let staging_ts_pool = create_timestamp_query_pool(&vk.device, 2)?;
    let render_ts_pool = create_timestamp_query_pool(&vk.device, 2)?;

    let timeline = VkTimelineSemaphore::new(vk.clone(), 0)?;

    Ok(SwapFrame {
        convert_ds,
        texture_semas: Vec::new(),
        texture_semas_used: 0,
        draws: Vec::new(),
        blend_image,
        encode_image,
        plane_views,
        staging_cb: allocate_command_buffer(&vk.device, vk.graphics_queue.command_pool)?,
        render_cb: allocate_command_buffer(&vk.device, vk.graphics_queue.command_pool)?,
        use_staging: false,
        timeline: timeline.clone(),
        tp_staging_done: timeline.new_point(0),
        tp_render_done: timeline.new_point(0),
        tp_clear: timeline.new_point(0),

        staging_ts_pool,
        staging_span: None,
        render_ts_pool,
        render_span: None,
    })
}

fn allocate_texture_semaphore(
    vk: Arc<VkContext>,
    frame: &mut SwapFrame,
) -> anyhow::Result<vk::Semaphore> {
    let idx = frame.texture_semas_used;
    frame.texture_semas_used += 1;

    if frame.texture_semas_used <= frame.texture_semas.len() {
        return Ok(frame.texture_semas[idx]);
    }

    let sema = unsafe {
        vk.device
            .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?
    };

    frame.texture_semas.push(sema);
    Ok(sema)
}

fn format_is_semiplanar(format: vk::Format) -> bool {
    // grep for 2PLANE in the vulkan spec.
    matches!(
        format,
        vk::Format::G8_B8R8_2PLANE_420_UNORM
            | vk::Format::G8_B8R8_2PLANE_422_UNORM
            | vk::Format::G8_B8R8_2PLANE_444_UNORM
            | vk::Format::G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16
            | vk::Format::G10X6_B10X6R10X6_2PLANE_422_UNORM_3PACK16
            | vk::Format::G10X6_B10X6R10X6_2PLANE_444_UNORM_3PACK16
            | vk::Format::G12X4_B12X4R12X4_2PLANE_420_UNORM_3PACK16
            | vk::Format::G12X4_B12X4R12X4_2PLANE_422_UNORM_3PACK16
            | vk::Format::G12X4_B12X4R12X4_2PLANE_444_UNORM_3PACK16
            | vk::Format::G16_B16R16_2PLANE_420_UNORM
            | vk::Format::G16_B16R16_2PLANE_422_UNORM
            | vk::Format::G16_B16R16_2PLANE_444_UNORM
    )
}

pub unsafe fn cmd_upload_shm(
    device: &ash::Device,
    cb: vk::CommandBuffer,
    buffer: &VkHostBuffer,
    image: &VkImage,
    stride: u32, // In texels.
    height: u32, // In texels.
) {
    let region = vk::BufferImageCopy::default()
        .buffer_row_length(stride)
        .buffer_image_height(height)
        .image_subresource(vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            mip_level: 0,
            base_array_layer: 0,
            layer_count: 1,
        })
        .image_extent(vk::Extent3D {
            width: image.width,
            height: image.height,
            depth: 1,
        });

    let regions = [region];
    device.cmd_copy_buffer_to_image(
        cb,
        buffer.buffer,
        image.image,
        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
        &regions,
    );
}

fn disjoint_plane_formats(format: vk::Format) -> Option<(vk::Format, vk::Format)> {
    match format {
        vk::Format::G8_B8R8_2PLANE_420_UNORM
        | vk::Format::G8_B8R8_2PLANE_422_UNORM
        | vk::Format::G8_B8R8_2PLANE_444_UNORM
        | vk::Format::G8_B8_R8_3PLANE_420_UNORM
        | vk::Format::G8_B8_R8_3PLANE_422_UNORM
        | vk::Format::G8_B8_R8_3PLANE_444_UNORM => {
            Some((vk::Format::R8_UNORM, vk::Format::R8G8_UNORM))
        }
        vk::Format::G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16
        | vk::Format::G10X6_B10X6R10X6_2PLANE_422_UNORM_3PACK16
        | vk::Format::G10X6_B10X6R10X6_2PLANE_444_UNORM_3PACK16
        | vk::Format::G10X6_B10X6_R10X6_3PLANE_420_UNORM_3PACK16
        | vk::Format::G10X6_B10X6_R10X6_3PLANE_422_UNORM_3PACK16
        | vk::Format::G10X6_B10X6_R10X6_3PLANE_444_UNORM_3PACK16 => Some((
            vk::Format::R10X6_UNORM_PACK16,
            vk::Format::R10X6G10X6_UNORM_2PACK16,
        )),
        vk::Format::G12X4_B12X4R12X4_2PLANE_420_UNORM_3PACK16
        | vk::Format::G12X4_B12X4R12X4_2PLANE_422_UNORM_3PACK16
        | vk::Format::G12X4_B12X4R12X4_2PLANE_444_UNORM_3PACK16
        | vk::Format::G12X4_B12X4_R12X4_3PLANE_420_UNORM_3PACK16
        | vk::Format::G12X4_B12X4_R12X4_3PLANE_422_UNORM_3PACK16
        | vk::Format::G12X4_B12X4_R12X4_3PLANE_444_UNORM_3PACK16 => Some((
            vk::Format::R12X4_UNORM_PACK16,
            vk::Format::R12X4G12X4_UNORM_2PACK16,
        )),
        vk::Format::G16_B16R16_2PLANE_420_UNORM
        | vk::Format::G16_B16R16_2PLANE_422_UNORM
        | vk::Format::G16_B16R16_2PLANE_444_UNORM
        | vk::Format::G16_B16_R16_3PLANE_420_UNORM
        | vk::Format::G16_B16_R16_3PLANE_422_UNORM
        | vk::Format::G16_B16_R16_3PLANE_444_UNORM => {
            Some((vk::Format::R16_UNORM, vk::Format::R16G16_UNORM))
        }
        _ => None,
    }
}
