// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

#![allow(clippy::missing_safety_doc)]

use anyhow::{anyhow, Context, Result};
use ash::vk;
use cstr::cstr;
use imgui_rs_vulkan_renderer as imgui_vulkan;
use std::sync::Arc;
use std::time;
use tracing::debug;
use tracing::instrument;
use tracing::trace;
use tracing::trace_span;
use tracing::warn;
use tracy_client::span_location;

use crate::font;
use crate::video::*;
use crate::vulkan::*;

const FONT_SIZE: f32 = 8.0;

#[derive(Copy, Clone, Debug)]
#[repr(C)]
struct PushConstants {
    aspect: glam::Vec2,
}

pub struct Renderer {
    width: u32,
    height: u32,
    scale_factor: f64,

    imgui: imgui::Context,
    imgui_platform: imgui_winit_support::WinitPlatform,
    imgui_font: font_kit::font::Font,
    imgui_fontid_big: imgui::FontId,
    imgui_time: time::Instant,

    swapchain: Option<Swapchain>,
    swapchain_dirty: bool,

    video_texture: Option<VideoTexture>,

    vk: Arc<VkContext>,
    window: Arc<winit::window::Window>,
}

struct Swapchain {
    swapchain: vk::SwapchainKHR,
    frames: Vec<InFlightFrame>,
    present_images: Vec<SwapImage>,
    current_frame: usize,

    sampler_conversion: vk::SamplerYcbcrConversion,
    sampler: vk::Sampler,
    bound_video_texture: Option<(Arc<VkImage>, vk::ImageView)>,
    /// The normalized relationship between the output and the video texture,
    /// after scaling. For example, a 500x500 video texture in a 1000x500
    /// swapchain would have the aspect (2.0, 1.0), as would a 250x250 texture.
    aspect: (f64, f64),
    descriptor_set_layout: vk::DescriptorSetLayout,
    descriptor_pool: vk::DescriptorPool,
    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,

    imgui_renderer: imgui_vulkan::Renderer,
}

struct InFlightFrame {
    render_cb: vk::CommandBuffer,
    render_fence: vk::Fence,
    image_acquired_sema: vk::Semaphore,
    render_complete_sema: vk::Semaphore,
    descriptor_set: vk::DescriptorSet,
    ts_pool: VkTimestampQueryPool,
    tracy_span: Option<tracy_client::GpuSpan>,
}

struct SwapImage {
    image: vk::Image,
    view: vk::ImageView,
}

struct VideoTexture {
    params: VideoStreamParams,
    texture: Arc<VkImage>,
}

impl Renderer {
    pub fn new(vk: Arc<VkContext>, window: Arc<winit::window::Window>) -> Result<Self> {
        let window_size = window.inner_size();
        let scale_factor = window.scale_factor();

        let mut imgui = imgui::Context::create();
        imgui.set_ini_filename(None);

        let mut imgui_platform = imgui_winit_support::WinitPlatform::init(&mut imgui);
        imgui_platform.attach_window(
            imgui.io_mut(),
            &window,
            imgui_winit_support::HiDpiMode::Default,
        );

        let imgui_font = font::load_ui_font()?;
        let imgui_fontid_big = import_imgui_font(&mut imgui, &imgui_font, FONT_SIZE, scale_factor)?;

        let mut renderer = Self {
            width: window_size.width,
            height: window_size.height,
            scale_factor,
            window,
            imgui,
            imgui_platform,
            imgui_font,
            imgui_fontid_big,
            imgui_time: time::Instant::now(),
            swapchain: None,
            swapchain_dirty: false,
            video_texture: None,
            vk,
        };

        unsafe { renderer.recreate_swapchain()? };

        Ok(renderer)
    }

    #[instrument(skip_all, level = "trace")]
    unsafe fn recreate_swapchain(&mut self) -> Result<()> {
        let start = time::Instant::now();
        let device = &self.vk.device;

        let surface_format = select_surface_format(self.vk.clone())?;
        trace!(?surface_format, "surface format");

        let surface_capabilities = self
            .vk
            .surface_loader
            .get_physical_device_surface_capabilities(self.vk.pdevice, self.vk.surface)
            .unwrap();
        let mut desired_image_count = surface_capabilities.min_image_count + 1;
        if surface_capabilities.max_image_count > 0
            && desired_image_count > surface_capabilities.max_image_count
        {
            desired_image_count = surface_capabilities.max_image_count;
        }

        let surface_resolution = match surface_capabilities.current_extent.width {
            std::u32::MAX => vk::Extent2D {
                width: self.width,
                height: self.height,
            },
            _ => surface_capabilities.current_extent,
        };

        let pre_transform = if surface_capabilities
            .supported_transforms
            .contains(vk::SurfaceTransformFlagsKHR::IDENTITY)
        {
            vk::SurfaceTransformFlagsKHR::IDENTITY
        } else {
            surface_capabilities.current_transform
        };

        let present_modes = self
            .vk
            .surface_loader
            .get_physical_device_surface_present_modes(self.vk.pdevice, self.vk.surface)
            .unwrap();

        let mut present_modes = present_modes.clone();
        present_modes.sort_by_key(|&mode| match mode {
            vk::PresentModeKHR::MAILBOX => 0,
            vk::PresentModeKHR::FIFO => 1,
            vk::PresentModeKHR::IMMEDIATE => 2,
            _ => 4,
        });

        let present_mode = present_modes.first().unwrap();
        if *present_mode != vk::PresentModeKHR::MAILBOX {
            warn!(
                "present mode MAILBOX not available, using {:?} (available: {:?})",
                present_mode, present_modes
            );
        }

        let mut swapchain_create_info = vk::SwapchainCreateInfoKHR::builder()
            .surface(self.vk.surface)
            .min_image_count(desired_image_count)
            .image_color_space(surface_format.color_space)
            .image_format(surface_format.format)
            .image_extent(surface_resolution)
            .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(pre_transform)
            .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
            .present_mode(*present_mode)
            .clipped(true)
            .image_array_layers(1);

        if let Some(old_swapchain) = self.swapchain.as_ref() {
            swapchain_create_info = swapchain_create_info.old_swapchain(old_swapchain.swapchain);
        }

        let swapchain = self
            .vk
            .swapchain_loader
            .create_swapchain(&swapchain_create_info, None)?;
        let swapchain_images = self.vk.swapchain_loader.get_swapchain_images(swapchain)?;

        // TODO: rather than recreate the swapchain if the video texture
        // changes, we can just recreate the pipeline. This is tricky because
        // we create a descriptor set for each SwapFrame, which refers to the
        // layout, which includes the immutable sampler.

        // We need to create a sampler, even if we don't have a video stream yet
        // and don't know what the fields should be.
        let video_params = match self.video_texture.as_ref() {
            Some(tex) => tex.params,
            None => VideoStreamParams::default(),
        };

        let sampler_conversion = sampler_conversion(device, &video_params)?;

        let sampler = {
            let mut conversion_info = vk::SamplerYcbcrConversionInfo::builder()
                .conversion(sampler_conversion)
                .build();

            let create_info = vk::SamplerCreateInfo::builder()
                .mag_filter(vk::Filter::LINEAR)
                .min_filter(vk::Filter::LINEAR)
                .compare_enable(true)
                .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .push_next(&mut conversion_info);

            unsafe { device.create_sampler(&create_info, None)? }
        };

        let bound_video_texture = if let Some(tex) = self.video_texture.as_ref() {
            let view = create_image_view(
                &self.vk.device,
                tex.texture.image,
                tex.texture.format,
                Some(sampler_conversion),
            )?;

            // Increment the reference count on the texture.
            Some((tex.texture.clone(), view))
        } else {
            None
        };

        let aspect = if let Some((tex, _)) = bound_video_texture.as_ref() {
            calculate_aspect(self.width, self.height, tex.width, tex.height)
        } else {
            (1.0, 1.0)
        };

        let descriptor_set_layout = {
            // We're required to use an immutable sampler for YCbCr conversion
            // by the vulkan spec.
            let samplers = [sampler];
            let binding = vk::DescriptorSetLayoutBinding::builder()
                .binding(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT)
                .immutable_samplers(&samplers);

            let bindings = [binding.build()];
            let create_info = vk::DescriptorSetLayoutCreateInfo::builder().bindings(&bindings);
            unsafe { device.create_descriptor_set_layout(&create_info, None)? }
        };

        let descriptor_pool = {
            let sampler_size = vk::DescriptorPoolSize::builder()
                .ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(swapchain_images.len() as u32);

            let pool_sizes = &[sampler_size.build()];
            let info = vk::DescriptorPoolCreateInfo::builder()
                .pool_sizes(pool_sizes)
                .max_sets(swapchain_images.len() as u32);

            unsafe { device.create_descriptor_pool(&info, None)? }
        };

        let pipeline_layout = {
            let pc_ranges = [vk::PushConstantRange::builder()
                .stage_flags(vk::ShaderStageFlags::VERTEX)
                .offset(0)
                .size(std::mem::size_of::<PushConstants>() as u32)
                .build()];
            let set_layouts = [descriptor_set_layout];
            let create_info = vk::PipelineLayoutCreateInfo::builder()
                .set_layouts(&set_layouts)
                .push_constant_ranges(&pc_ranges);

            unsafe { device.create_pipeline_layout(&create_info, None)? }
        };

        let pipeline = {
            let vert_bytes = include_bytes!(concat!(env!("OUT_DIR"), "/shaders/vert.spv"));
            let frag_bytes = include_bytes!(concat!(env!("OUT_DIR"), "/shaders/frag.spv"));
            let vert_shader = load_shader(device, vert_bytes).context("loading vert.spv")?;
            let frag_shader = load_shader(device, frag_bytes).context("loading frag.spv")?;

            let vert_stage = vk::PipelineShaderStageCreateInfo::builder()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(vert_shader)
                .name(cstr!("main"));

            let frag_stage = vk::PipelineShaderStageCreateInfo::builder()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(frag_shader)
                .name(cstr!("main"));

            let vertex_input_state = vk::PipelineVertexInputStateCreateInfo::builder();

            let input_assembly_state = vk::PipelineInputAssemblyStateCreateInfo::builder()
                .topology(vk::PrimitiveTopology::TRIANGLE_STRIP)
                .primitive_restart_enable(false);

            let viewport = vk::Viewport::builder()
                .x(0.0)
                .y(0.0)
                .width(self.width as f32)
                .height(self.height as f32)
                .min_depth(0.0)
                .max_depth(1.0);

            let scissor = vk::Rect2D::builder().extent(vk::Extent2D {
                width: self.width,
                height: self.height,
            });

            let viewports = [viewport.build()];
            let scissors = [scissor.build()];
            let viewport_state = vk::PipelineViewportStateCreateInfo::builder()
                .viewports(&viewports)
                .scissors(&scissors);

            let rasterization_state = vk::PipelineRasterizationStateCreateInfo::builder()
                .depth_clamp_enable(false)
                .rasterizer_discard_enable(false)
                .polygon_mode(vk::PolygonMode::FILL)
                .line_width(1.0)
                .depth_bias_enable(false)
                // Per https://www.saschawillems.de/blog/2016/08/13/vulkan-tutorial-on-rendering-a-fullscreen-quad-without-buffers
                .cull_mode(vk::CullModeFlags::FRONT)
                .front_face(vk::FrontFace::COUNTER_CLOCKWISE);

            let multisample_state = vk::PipelineMultisampleStateCreateInfo::builder()
                .sample_shading_enable(false)
                .rasterization_samples(vk::SampleCountFlags::TYPE_1);

            let attachment = vk::PipelineColorBlendAttachmentState::builder()
                .color_write_mask(vk::ColorComponentFlags::RGBA)
                .blend_enable(true)
                .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
                .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
                .color_blend_op(vk::BlendOp::ADD)
                .src_alpha_blend_factor(vk::BlendFactor::ONE)
                .dst_alpha_blend_factor(vk::BlendFactor::ZERO)
                .alpha_blend_op(vk::BlendOp::ADD);

            let attachments = [attachment.build()];
            let color_blend_state = vk::PipelineColorBlendStateCreateInfo::builder()
                .logic_op_enable(false)
                .attachments(&attachments);

            let formats = [surface_format.format];
            let mut pipeline_rendering = vk::PipelineRenderingCreateInfo::builder()
                .color_attachment_formats(&formats)
                .build();

            let stages = [vert_stage.build(), frag_stage.build()];
            let create_info = vk::GraphicsPipelineCreateInfo::builder()
                .stages(&stages)
                .vertex_input_state(&vertex_input_state)
                .input_assembly_state(&input_assembly_state)
                .viewport_state(&viewport_state)
                .rasterization_state(&rasterization_state)
                .multisample_state(&multisample_state)
                .color_blend_state(&color_blend_state)
                .layout(pipeline_layout)
                .push_next(&mut pipeline_rendering);

            unsafe {
                let pipeline = match device.create_graphics_pipelines(
                    vk::PipelineCache::null(),
                    &[create_info.build()],
                    None,
                ) {
                    Ok(pipelines) => Ok(pipelines[0]),
                    Err((_, e)) => Err(e),
                }?;

                device.destroy_shader_module(vert_shader, None);
                device.destroy_shader_module(frag_shader, None);
                pipeline
            }
        };

        let create_frame = || -> Result<InFlightFrame> {
            let render_cb = {
                let create_info = vk::CommandBufferAllocateInfo::builder()
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_pool(self.vk.present_queue.command_pool)
                    .command_buffer_count(1);

                let cbs = device
                    .allocate_command_buffers(&create_info)
                    .context("failed to allocate render command buffer")?;

                cbs[0]
            };

            let descriptor_set = {
                let layouts = &[descriptor_set_layout];
                let create_info = vk::DescriptorSetAllocateInfo::builder()
                    .descriptor_pool(descriptor_pool)
                    .set_layouts(layouts);

                let ds = device
                    .allocate_descriptor_sets(&create_info)?
                    .pop()
                    .unwrap();

                // TODO: do the write in bind_video_texture?
                if let Some((_, view)) = bound_video_texture.as_ref() {
                    let info = vk::DescriptorImageInfo::builder()
                        .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                        .image_view(*view);

                    let image_info = &[info.build()];
                    let sampler_write = vk::WriteDescriptorSet::builder()
                        .dst_set(ds)
                        .dst_binding(0)
                        .dst_array_element(0)
                        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                        .image_info(image_info);

                    device.update_descriptor_sets(&[sampler_write.build()], &[]);
                }

                ds
            };

            let render_fence = create_fence(device, true)?;
            let image_acquired_sema = create_semaphore(device)?;
            let render_complete_sema = create_semaphore(device)?;

            let ts_pool = create_timestamp_query_pool(device, 2)?;

            Ok(InFlightFrame {
                render_cb,
                render_fence,
                image_acquired_sema,
                render_complete_sema,
                descriptor_set,
                ts_pool,
                tracy_span: None,
            })
        };

        let frames = (0..swapchain_images.len())
            .map(|_| create_frame())
            .collect::<Result<Vec<_>>>()?;

        let swapchain_images = swapchain_images
            .into_iter()
            .map(|image| {
                let image_view = create_image_view(device, image, surface_format.format, None)?;

                Ok(SwapImage {
                    image,
                    view: image_view,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let mut imgui_renderer = imgui_vulkan::Renderer::with_default_allocator(
            &self.vk.instance,
            self.vk.pdevice,
            self.vk.device.clone(),
            self.vk.present_queue.queue,
            self.vk.present_queue.command_pool,
            imgui_vulkan::DynamicRendering {
                color_attachment_format: surface_format.format,
                depth_attachment_format: None,
            },
            &mut self.imgui,
            Some(imgui_vulkan::Options {
                in_flight_frames: frames.len(),
                ..Default::default()
            }),
        )?;

        imgui_renderer.update_fonts_texture(
            self.vk.present_queue.queue,
            self.vk.present_queue.command_pool,
            &mut self.imgui,
        )?;

        let swapchain = Swapchain {
            swapchain,
            frames,
            present_images: swapchain_images,
            current_frame: 0,

            descriptor_pool,
            descriptor_set_layout,
            sampler_conversion,
            sampler,
            bound_video_texture,
            aspect,
            pipeline_layout,
            pipeline,

            imgui_renderer,
        };

        debug!("recreated swapchain in {:?}", start.elapsed());

        if let Some(old_swapchain) = self.swapchain.replace(swapchain) {
            self.destroy_swapchain(old_swapchain);
        };

        Ok(())
    }

    pub fn handle_event(&mut self, event: &winit::event::WindowEvent) -> anyhow::Result<()> {
        let now = time::Instant::now();
        self.imgui.io_mut().update_delta_time(now - self.imgui_time);
        self.imgui_time = now;

        let wrapped: winit::event::Event<()> = winit::event::Event::WindowEvent {
            window_id: self.window.id(),
            event: event.clone(),
        };

        self.imgui_platform
            .handle_event(self.imgui.io_mut(), self.window.as_ref(), &wrapped);

        match event {
            winit::event::WindowEvent::Resized(size) => {
                self.resize(size.width, size.height);
            }
            winit::event::WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.scale_factor_changed(*scale_factor)?;
            }
            _ => (),
        }

        Ok(())
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if self.width == width && self.height == height {
            return;
        }

        self.width = width;
        self.height = height;
        self.swapchain_dirty = true;
    }

    fn scale_factor_changed(&mut self, scale_factor: f64) -> anyhow::Result<()> {
        if self.scale_factor == scale_factor {
            return Ok(());
        }

        // Resize fonts.
        self.imgui_fontid_big =
            import_imgui_font(&mut self.imgui, &self.imgui_font, FONT_SIZE, scale_factor)?;

        self.scale_factor = scale_factor;
        Ok(())
    }

    pub fn bind_video_texture(
        &mut self,
        texture: Arc<VkImage>,
        params: VideoStreamParams,
    ) -> Result<()> {
        // TODO: no need to recreate the sampler if the params match.
        self.video_texture = Some(VideoTexture { params, texture });
        self.swapchain_dirty = true;
        Ok(())
    }

    // Returns the normalized relationship between the output dimensions and the
    // video texture dimensions, after scaling. For example, if the video
    // texture is 250x250 and the output is 1000x500, the aspect would be (2.0,
    // 1.0).
    pub fn get_texture_aspect(&self) -> Option<(f64, f64)> {
        if let Some(Swapchain {
            bound_video_texture: Some((_, _)),
            aspect,
            ..
        }) = self.swapchain.as_ref()
        {
            Some(*aspect)
        } else {
            None
        }
    }

    #[instrument(skip_all, level = "trace")]
    pub unsafe fn render<F>(&mut self, ui_builder: F) -> Result<()>
    where
        F: FnOnce(&imgui::Ui) -> anyhow::Result<()>,
    {
        if self.swapchain_dirty || self.swapchain.is_none() {
            self.recreate_swapchain()?;
            self.swapchain_dirty = false;
        }

        let device = &self.vk.device;
        let swapchain = self.swapchain.as_mut().unwrap();
        let num_frames = swapchain.frames.len();

        let frame = &mut swapchain.frames[swapchain.current_frame];
        swapchain.current_frame = (swapchain.current_frame + 1) % num_frames;

        // Wait for the gpu to catch up.
        device.wait_for_fences(&[frame.render_fence], true, u64::MAX)?;

        // Trace the frame on the GPU side.
        if let Some(ctx) = &self.vk.tracy_context {
            if let Some(span) = frame.tracy_span.take() {
                let timestamps = frame.ts_pool.fetch_results(device)?;
                span.upload_timestamp(timestamps[0], timestamps[1]);
            }

            frame.tracy_span = Some(ctx.span(span_location!())?);
        }

        let result = self.vk.swapchain_loader.acquire_next_image(
            swapchain.swapchain,
            u64::MAX,
            frame.image_acquired_sema,
            vk::Fence::null(),
        );

        let swapchain_index = match result {
            Ok((image_index, _)) => image_index,
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                // Recreate and try again.
                self.swapchain_dirty = true;
                return self.render(ui_builder);
            }
            Err(e) => return Err(e.into()),
        };

        let present_image = swapchain
            .present_images
            .get(swapchain_index as usize)
            .unwrap();

        // Reset the command buffer.
        device.reset_command_buffer(frame.render_cb, vk::CommandBufferResetFlags::empty())?;

        // Begin the command buffer.
        {
            let begin_info = vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);

            device.begin_command_buffer(frame.render_cb, &begin_info)?;
        }

        // Record the start timestamp.
        frame.ts_pool.cmd_reset(device, frame.render_cb);
        device.cmd_write_timestamp(
            frame.render_cb,
            vk::PipelineStageFlags::TOP_OF_PIPE,
            frame.ts_pool.pool,
            0,
        );

        // Transition the present image to be writable.
        cmd_image_barrier(
            device,
            frame.render_cb,
            present_image.image,
            vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::AccessFlags::empty(),
            vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        );

        // Begin rendering.
        {
            let rect: vk::Rect2D = vk::Rect2D::builder()
                .extent(vk::Extent2D {
                    width: self.width,
                    height: self.height,
                })
                .build();

            let clear_value = vk::ClearValue {
                color: vk::ClearColorValue {
                    float32: [0.0, 0.0, 0.0, 1.0],
                },
            };

            let color_attachment = vk::RenderingAttachmentInfo::builder()
                .image_view(present_image.view)
                .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .load_op(vk::AttachmentLoadOp::CLEAR)
                .store_op(vk::AttachmentStoreOp::STORE)
                .clear_value(clear_value)
                .build();

            let color_attachments = [color_attachment];
            let rendering_info = vk::RenderingInfo::builder()
                .render_area(rect)
                .color_attachments(&color_attachments)
                .layer_count(1);

            self.vk
                .dynamic_rendering_loader
                .cmd_begin_rendering(frame.render_cb, &rendering_info);
            device.cmd_bind_pipeline(
                frame.render_cb,
                vk::PipelineBindPoint::GRAPHICS,
                swapchain.pipeline,
            );
        }

        if self.video_texture.is_none() || swapchain.aspect != (1.0, 1.0) {
            // TODO Draw the background
            // https://www.toptal.com/designers/subtlepatterns/prism/
        }

        // Draw the video texture.
        if let Some((_texture, _)) = &swapchain.bound_video_texture {
            let pc = PushConstants {
                aspect: glam::Vec2::new(swapchain.aspect.0 as f32, swapchain.aspect.1 as f32),
            };

            device.cmd_push_constants(
                frame.render_cb,
                swapchain.pipeline_layout,
                vk::ShaderStageFlags::VERTEX,
                0,
                std::slice::from_raw_parts(
                    &pc as *const _ as *const u8,
                    std::mem::size_of::<PushConstants>(),
                ),
            );

            device.cmd_bind_descriptor_sets(
                frame.render_cb,
                vk::PipelineBindPoint::GRAPHICS,
                swapchain.pipeline_layout,
                0,
                &[frame.descriptor_set],
                &[],
            );

            // Draw the video texture.
            device.cmd_draw(frame.render_cb, 4, 1, 0, 0);
        }
        // Draw the overlay.
        {
            self.imgui_platform
                .prepare_frame(self.imgui.io_mut(), &self.window)?;

            {
                let ui = self.imgui.new_frame();

                let _font_stack = ui.push_font(self.imgui_fontid_big);
                ui_builder(ui)?;

                self.imgui_platform.prepare_render(ui, &self.window);
            }

            swapchain
                .imgui_renderer
                .cmd_draw(frame.render_cb, self.imgui.render())?;
        };

        // Done rendereng.
        self.vk
            .dynamic_rendering_loader
            .cmd_end_rendering(frame.render_cb);

        // Transition the present image to be presentable.
        cmd_image_barrier(
            device,
            frame.render_cb,
            present_image.image,
            vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            vk::PipelineStageFlags::BOTTOM_OF_PIPE,
            vk::AccessFlags::empty(),
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            vk::ImageLayout::PRESENT_SRC_KHR,
        );

        // Record the end timestamp.
        device.cmd_write_timestamp(
            frame.render_cb,
            vk::PipelineStageFlags::BOTTOM_OF_PIPE,
            frame.ts_pool.pool,
            1,
        );

        if let Some(span) = &mut frame.tracy_span {
            span.end_zone();
        }

        // Submit and present!
        {
            let present_queue = self.vk.present_queue.queue;

            device.end_command_buffer(frame.render_cb)?;
            device.reset_fences(&[frame.render_fence])?;

            let cbs = [frame.render_cb];
            let wait_semas = [frame.image_acquired_sema];
            let signal_semas = [frame.render_complete_sema];
            let wait_stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
            let submit_info = vk::SubmitInfo::builder()
                .command_buffers(&cbs)
                .wait_semaphores(&wait_semas)
                .wait_dst_stage_mask(&wait_stages)
                .signal_semaphores(&signal_semas);

            trace!(queue = ?present_queue, "queue submit for render");

            let submits = [submit_info.build()];
            device.queue_submit(present_queue, &submits, frame.render_fence)?;

            // This "helps winit [with stuff]". It also seems to increase latency.
            self.window.pre_present_notify();

            trace!(queue = ?present_queue, index = swapchain_index, "queue present");

            let wait_semas = [frame.render_complete_sema];
            let swapchains = [swapchain.swapchain];
            let image_indices = [swapchain_index];
            let present_info = vk::PresentInfoKHR::builder()
                .wait_semaphores(&wait_semas)
                .swapchains(&swapchains)
                .image_indices(&image_indices);

            let res = trace_span!("render.queue_present").in_scope(|| {
                self.vk
                    .swapchain_loader
                    .queue_present(present_queue, &present_info)
            });

            self.swapchain_dirty = match res {
                Ok(false) => false,
                Ok(true) => true,
                Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => true,
                Err(e) => return Err(e.into()),
            };
        }

        tracy_client::frame_mark();

        Ok(())
    }

    unsafe fn destroy_swapchain(&mut self, mut swapchain: Swapchain) {
        let device = &self.vk.device;
        device.device_wait_idle().unwrap();

        for frame in swapchain.frames.drain(..) {
            device.free_command_buffers(self.vk.present_queue.command_pool, &[frame.render_cb]);
            device.destroy_fence(frame.render_fence, None);
            device.destroy_semaphore(frame.image_acquired_sema, None);
            device.destroy_semaphore(frame.render_complete_sema, None);
            device.destroy_query_pool(frame.ts_pool.pool, None);
        }

        for swap_img in swapchain.present_images.drain(..) {
            // Destroying the swapchain does this.
            // device.destroy_image(swap_img.image, None);
            device.destroy_image_view(swap_img.view, None);
        }

        device.destroy_pipeline_layout(swapchain.pipeline_layout, None);
        device.destroy_descriptor_pool(swapchain.descriptor_pool, None);
        device.destroy_descriptor_set_layout(swapchain.descriptor_set_layout, None);
        device.destroy_sampler(swapchain.sampler, None);
        device.destroy_sampler_ycbcr_conversion(swapchain.sampler_conversion, None);

        if let Some((_img, view)) = swapchain.bound_video_texture.take() {
            device.destroy_image_view(view, None);
            // We probably drop the last reference to the image here, which then
            // gets destroyed.
        }

        device.destroy_pipeline(swapchain.pipeline, None);
        self.vk
            .swapchain_loader
            .destroy_swapchain(swapchain.swapchain, None)
    }
}

fn select_surface_format(vk: Arc<VkContext>) -> Result<vk::SurfaceFormatKHR, vk::Result> {
    let surface_formats = unsafe {
        vk.surface_loader
            .get_physical_device_surface_formats(vk.pdevice, vk.surface)?
    };

    let preferred_formats = [
        vk::Format::R16G16B16A16_SFLOAT,
        vk::Format::R8G8B8A8_UNORM,
        vk::Format::B8G8R8A8_UNORM,
    ];

    for preferred_format in &preferred_formats {
        for surface_format in &surface_formats {
            if surface_format.format == *preferred_format {
                return Ok(*surface_format);
            }
        }
    }

    // Just pick the first format.
    Ok(surface_formats[0])
}

impl Drop for Renderer {
    fn drop(&mut self) {
        if let Some(swapchain) = self.swapchain.take() {
            unsafe {
                self.destroy_swapchain(swapchain);
            };
        }
    }
}

fn import_imgui_font(
    imgui: &mut imgui::Context,
    font: &font_kit::font::Font,
    size: f32,
    scale_factor: f64,
) -> anyhow::Result<imgui::FontId> {
    let font_size = size * scale_factor as f32;
    imgui.io_mut().font_global_scale = (1.0 / scale_factor) as f32;

    let data = match font.copy_font_data() {
        Some(data) => data,
        None => return Err(anyhow!("failed to load font data for {:?}", font)),
    };

    let id = imgui.fonts().add_font(&[imgui::FontSource::TtfData {
        size_pixels: font_size,
        data: &data,
        config: Some(imgui::FontConfig {
            pixel_snap_h: true,
            oversample_h: 4,
            oversample_v: 4,
            ..imgui::FontConfig::default()
        }),
    }]);

    Ok(id)
}

fn calculate_aspect(width: u32, height: u32, tex_width: u32, tex_height: u32) -> (f64, f64) {
    let width = width as f64;
    let height = height as f64;
    let tex_width = tex_width as f64;
    let tex_height = tex_height as f64;

    let window_aspect = width / height;
    let texture_aspect = tex_width / tex_height;
    if window_aspect > texture_aspect {
        // Screen too wide.
        let scale = height / tex_height;
        (width / (tex_width * scale), 1.0)
    } else if window_aspect < texture_aspect {
        // Screen too tall.
        let scale = width / tex_width;
        (1.0, height / (tex_height * scale))
    } else {
        (1.0, 1.0)
    }
}
