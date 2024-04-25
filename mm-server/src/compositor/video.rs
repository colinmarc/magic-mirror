// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{mem::ManuallyDrop, sync::Arc};

use ash::vk;

mod composite;
mod convert;
mod cpu_encode;
mod dmabuf;
mod textures;
mod timebase;
mod vulkan_encode;

use cpu_encode::CpuEncoder;

use smithay::{
    reexports::wayland_server::{protocol::wl_surface, Resource},
    wayland::compositor,
};
use tracing::{error, instrument, trace, warn};

use crate::{codec::VideoCodec, vulkan::*};

use super::{AttachedClients, DisplayParams, VideoStreamParams};

pub use dmabuf::dmabuf_feedback;
use textures::*;
pub use textures::{texture_to_png, TextureManager};
use vulkan_encode::VulkanEncoder;

pub struct VkPlaneView {
    pub format: vk::Format,
    pub view: vk::ImageView,
    pub width: u32,
    pub height: u32,
}

pub enum Encoder {
    Cpu(CpuEncoder),
    Vulkan(VulkanEncoder),
}

impl Encoder {
    pub fn select(
        vk: Arc<VkContext>,
        attachments: AttachedClients,
        stream_seq: u64,
        params: VideoStreamParams,
        framerate: u32,
    ) -> anyhow::Result<Self> {
        let use_vulkan = if cfg!(feature = "vulkan_encode") {
            match params.codec {
                VideoCodec::H264 if vk.device_info.supports_h264 => true,
                VideoCodec::H265 if vk.device_info.supports_h265 => true,
                _ => false,
            }
        } else {
            false
        };

        if use_vulkan {
            match VulkanEncoder::new(vk.clone(), attachments.clone(), stream_seq, params) {
                Ok(enc) => return Ok(Self::Vulkan(enc)),
                Err(e) => {
                    error!("error creating vulkan encoder: {:#}", e);
                    warn!("falling back to CPU encoder");
                }
            }
        }

        Ok(Self::Cpu(CpuEncoder::new(
            vk,
            attachments,
            stream_seq,
            params,
            framerate,
        )?))
    }

    pub fn input_format(&self) -> vk::Format {
        match self {
            Self::Cpu(enc) => enc.input_format(),
            Self::Vulkan(enc) => enc.input_format(),
        }
    }

    pub fn create_input_image(&mut self) -> anyhow::Result<VkImage> {
        match self {
            Self::Cpu(enc) => enc.create_input_image(),
            Self::Vulkan(enc) => enc.create_input_image(),
        }
    }
}

pub struct SwapFrame {
    convert_ds: vk::DescriptorSet, // Should be dropped first.
    texture_semas: Vec<vk::Semaphore>,

    /// An RGBA image to composite to.
    blend_image: VkImage,
    /// A YUV image we copy to before passing on to the encoder.
    encode_image: VkImage,
    plane_views: Vec<VkPlaneView>,

    staging_cb: vk::CommandBuffer,
    render_cb: vk::CommandBuffer,

    timeline: vk::Semaphore,
    tp_staging_done: u64,
    tp_render_done: u64,
    tp_clear: u64,

    shm_uploads_queued: bool,

    // For tracing.
    staging_ts_pool: VkTimestampQueryPool,
    staging_span: Option<tracy_client::GpuSpan>,
    render_ts_pool: VkTimestampQueryPool,
    render_span: Option<tracy_client::GpuSpan>,
}

pub struct EncodePipeline {
    display_params: DisplayParams,

    composite_pipeline: composite::CompositePipeline,
    convert_pipeline: convert::ConvertPipeline,
    encoder: ManuallyDrop<Encoder>,

    swap: [SwapFrame; 2],
    swap_idx: usize,

    vk: Arc<VkContext>,
}

impl EncodePipeline {
    #[instrument(level = "trace", skip_all)]
    pub fn new(
        vk: Arc<VkContext>,
        stream_seq: u64,
        attachments: AttachedClients,
        display_params: DisplayParams,
        stream_params: VideoStreamParams,
    ) -> anyhow::Result<Self> {
        if stream_params.width != display_params.width
            || stream_params.height != display_params.height
        {
            // Superres is not implemented yet.
            unimplemented!()
        }

        let mut encoder = Encoder::select(
            vk.clone(),
            attachments.clone(),
            stream_seq,
            stream_params,
            display_params.framerate,
        )?;

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
    pub unsafe fn begin(&mut self, textures: &TextureManager) -> anyhow::Result<()> {
        let device = &self.vk.device;
        let frame = &mut self.swap[self.swap_idx];

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
            frame.render_span = Some(ctx.span(tracy_client::span_location!())?);
        }

        // Wait for the frame to no longer be in flight, and then establish new timeline points.
        timeline_wait(&self.vk.device, frame.timeline, frame.tp_clear)?;
        frame.shm_uploads_queued = false;
        frame.tp_staging_done += 10;
        frame.tp_render_done = frame.tp_staging_done + 1;
        frame.tp_clear = frame.tp_render_done + 1;

        begin_cb(device, frame.staging_cb)?;
        begin_cb(device, frame.render_cb)?;

        // Record the start timestamp.
        frame.render_ts_pool.cmd_reset(device, frame.render_cb);
        device.cmd_write_timestamp(
            frame.render_cb,
            vk::PipelineStageFlags::TOP_OF_PIPE,
            frame.render_ts_pool.pool,
            0,
        );

        let mut texture_semas = Vec::new();

        // Upload any updated shm textures, and transition all surface textures to be readable.
        for tex in textures.iter_surfaces() {
            match tex {
                SurfaceTexture::Uploaded {
                    dirty,
                    staging_buffer,
                    image,
                    ..
                } => {
                    if *dirty {
                        frame.shm_uploads_queued = true;

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
                        cmd_upload_shm(device, frame.staging_cb, staging_buffer, image);
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
                }
                SurfaceTexture::Imported {
                    dmabuf,
                    image,
                    semaphore,
                    ..
                } => {
                    // Grab a semaphore for explicit sync.
                    dmabuf::import_dmabuf_fence_as_semaphore(
                        self.vk.clone(),
                        *semaphore,
                        dmabuf.clone(),
                    )?;

                    texture_semas.push(semaphore);

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
                }
            }
        }

        if frame.shm_uploads_queued {
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

        self.composite_pipeline
            .begin_compositing(frame.render_cb, &frame.blend_image);

        Ok(())
    }

    #[instrument(level = "trace", skip_all)]
    pub unsafe fn composite_surface(
        &mut self,
        textures: &mut TextureManager,
        surface: &wl_surface::WlSurface,
        dest: smithay::utils::Rectangle<i32, smithay::utils::Physical>,
    ) -> anyhow::Result<()> {
        trace!(surface = surface.id().protocol_id(), "rendering surface");

        let frame = &mut self.swap[self.swap_idx];

        let tex = textures.get_mut(surface);
        if tex.is_none() {
            if !compositor::is_sync_subsurface(surface) {
                error!(
                    "trying to render surface @{} that hasn't be imported",
                    surface.id().protocol_id()
                );
            }

            return Ok(());
        }

        // Convert the destination rect into clip coordinates.
        let dst_pos = glam::Vec2::new(
            (dest.loc.x as f32 / self.display_params.width as f32 * 2.0) - 1.0,
            (dest.loc.y as f32 / self.display_params.height as f32 * 2.0) - 1.0,
        );

        let dst_size = glam::Vec2::new(
            dest.size.w as f32 / self.display_params.width as f32 * 2.0,
            dest.size.h as f32 / self.display_params.height as f32 * 2.0,
        );

        // Draw.
        self.composite_pipeline.composite_surface(
            frame.render_cb,
            tex.unwrap(),
            dst_pos,
            dst_size,
        )?;

        Ok(())
    }

    #[instrument(level = "trace", skip(self))]
    pub unsafe fn end_and_submit(&mut self) -> anyhow::Result<()> {
        let device = &self.vk.device;
        let frame = &mut self.swap[self.swap_idx];

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

        // For Vulkan encode, acquire the encode image from the encode queue.
        // But not for the first frame.
        if frame.tp_clear > 20 && matches!(*self.encoder, Encoder::Vulkan(_)) {
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

        self.convert_pipeline.cmd_convert(
            frame.render_cb,
            frame.blend_image.width,
            frame.blend_image.height,
            frame.convert_ds,
        );

        // Do a queue transfer for vulkan encode.
        if let Encoder::Vulkan(_) = *self.encoder {
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
        }

        let mut submits = Vec::new();

        let staging_cb_infos =
            [vk::CommandBufferSubmitInfoKHR::default().command_buffer(frame.staging_cb)];

        let staging_signal_infos = [vk::SemaphoreSubmitInfo::default()
            .semaphore(frame.timeline)
            .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .value(frame.tp_staging_done)];

        let staging_submit_info = vk::SubmitInfo2::default()
            .command_buffer_infos(&staging_cb_infos)
            .signal_semaphore_infos(&staging_signal_infos);

        // Only submit the staging cb if we actually recorded commands to it.
        if frame.shm_uploads_queued {
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
            timeline_signal(&self.vk.device, frame.timeline, frame.tp_staging_done)?;
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
            .semaphore(frame.timeline)
            .stage_mask(vk::PipelineStageFlags2::FRAGMENT_SHADER)
            .value(frame.tp_staging_done)];
        let render_signal_infos = [vk::SemaphoreSubmitInfo::default()
            .semaphore(frame.timeline)
            .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .value(frame.tp_render_done)];

        for sema in frame.texture_semas.drain(..) {
            render_wait_infos.push(
                vk::SemaphoreSubmitInfo::default()
                    .semaphore(sema)
                    .stage_mask(vk::PipelineStageFlags2::FRAGMENT_SHADER),
            );
        }

        let render_submit_info = vk::SubmitInfo2::default()
            .command_buffer_infos(&render_cb_infos)
            .wait_semaphore_infos(&render_wait_infos)
            .signal_semaphore_infos(&render_signal_infos);

        submits.push(render_submit_info);
        device.queue_submit2(self.vk.graphics_queue.queue, &submits, vk::Fence::null())?;

        // Trigger encode.
        match *self.encoder {
            Encoder::Cpu(ref mut enc) => enc.submit_encode(
                &frame.encode_image,
                frame.timeline,
                frame.tp_render_done,
                frame.tp_clear,
            )?,
            Encoder::Vulkan(ref mut vkenc) => vkenc.submit_encode(
                &frame.encode_image,
                frame.timeline,
                frame.tp_render_done,
                frame.tp_clear,
            )?,
        };

        // Wait for uploads to finish before returning, so that writes to the
        // staging buffers are synchronized.
        timeline_wait(&self.vk.device, frame.timeline, frame.tp_staging_done)?;

        let swap_len = self.swap.len();
        self.swap_idx = (self.swap_idx + 1) % swap_len;
        Ok(())
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
                    device.destroy_image_view(view.view, None);
                }

                device.destroy_semaphore(frame.timeline, None);
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
    let disjoint_formats = if format_is_semiplanar(encode_image.format) {
        vec![
            vk::Format::R8_UNORM,   // Y
            vk::Format::R8G8_UNORM, // UV
        ]
    } else {
        vec![
            vk::Format::R8_UNORM, // Y
            vk::Format::R8_UNORM, // U
            vk::Format::R8_UNORM, // V
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

        let (width, height) = if idx == 0 {
            (encode_image.width, encode_image.height)
        } else {
            (encode_image.width / 2, encode_image.height / 2)
        };

        plane_views.push(VkPlaneView {
            format,
            view,
            width,
            height,
        });
    }

    let convert_ds = convert_pipeline.ds_for_conversion(&blend_image, &plane_views)?;

    let staging_ts_pool = create_timestamp_query_pool(&vk.device, 2)?;
    let render_ts_pool = create_timestamp_query_pool(&vk.device, 2)?;

    let timeline = create_timeline_semaphore(&vk.device, 0)?;

    Ok(SwapFrame {
        convert_ds,
        texture_semas: Vec::new(),
        blend_image,
        encode_image,
        plane_views,
        staging_cb: allocate_command_buffer(&vk.device, vk.graphics_queue.command_pool)?,
        render_cb: allocate_command_buffer(&vk.device, vk.graphics_queue.command_pool)?,
        shm_uploads_queued: false,
        timeline,
        tp_staging_done: 0,
        tp_render_done: 0,
        tp_clear: 0,

        staging_ts_pool,
        staging_span: None,
        render_ts_pool,
        render_span: None,
    })
}

#[instrument(level = "trace", skip_all)]
unsafe fn timeline_wait(device: &ash::Device, sema: vk::Semaphore, tp: u64) -> anyhow::Result<()> {
    device.wait_semaphores(
        &vk::SemaphoreWaitInfo::default()
            .semaphores(&[sema])
            .values(&[tp]),
        1_000_000_000, // 1 second
    )?;

    Ok(())
}

#[instrument(level = "trace", skip_all)]
unsafe fn timeline_signal(
    device: &ash::Device,
    sema: vk::Semaphore,
    tp: u64,
) -> anyhow::Result<()> {
    device.signal_semaphore(&vk::SemaphoreSignalInfo::default().semaphore(sema).value(tp))?;

    Ok(())
}

unsafe fn begin_cb(device: &ash::Device, cb: vk::CommandBuffer) -> anyhow::Result<()> {
    device.reset_command_buffer(cb, vk::CommandBufferResetFlags::empty())?;
    device.begin_command_buffer(
        cb,
        &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
    )?;

    Ok(())
}

fn format_is_semiplanar(format: vk::Format) -> bool {
    // grep for 2PLANE in the vulkan spec.
    matches!(
        format,
        vk::Format::G8_B8R8_2PLANE_420_UNORM
            | vk::Format::G8_B8R8_2PLANE_422_UNORM
            | vk::Format::G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16
            | vk::Format::G10X6_B10X6R10X6_2PLANE_422_UNORM_3PACK16
            | vk::Format::G12X4_B12X4R12X4_2PLANE_420_UNORM_3PACK16
            | vk::Format::G12X4_B12X4R12X4_2PLANE_422_UNORM_3PACK16
            | vk::Format::G16_B16R16_2PLANE_420_UNORM
            | vk::Format::G16_B16R16_2PLANE_422_UNORM
            | vk::Format::G8_B8R8_2PLANE_444_UNORM
            | vk::Format::G10X6_B10X6R10X6_2PLANE_444_UNORM_3PACK16
            | vk::Format::G12X4_B12X4R12X4_2PLANE_444_UNORM_3PACK16
            | vk::Format::G16_B16R16_2PLANE_444_UNORM
    )
}
