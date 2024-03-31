// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use anyhow::Context;
use ash::vk;
use cstr::cstr;
use tracing::trace;

use crate::{color::ColorSpace, vulkan::*};

use super::SurfaceTexture;

pub const BLEND_FORMAT: vk::Format = vk::Format::R16G16B16A16_SFLOAT;

// Also defined in composite.slang.
#[repr(u32)]
#[derive(Copy, Clone, Debug)]
enum SurfaceColorSpace {
    Srgb = 0,
    LinearExtSrgb = 1,
    Hdr10 = 2,
}

impl From<ColorSpace> for SurfaceColorSpace {
    fn from(cs: ColorSpace) -> Self {
        match cs {
            ColorSpace::Srgb => SurfaceColorSpace::Srgb,
            ColorSpace::LinearExtSrgb => SurfaceColorSpace::LinearExtSrgb,
            ColorSpace::Hdr10 => SurfaceColorSpace::Hdr10,
        }
    }
}

#[derive(Copy, Clone, Debug)]
#[repr(C)]
#[allow(dead_code)]
struct SurfacePC {
    // Should be in texture coords: [0, 1].
    src_pos: glam::Vec2,
    src_size: glam::Vec2,
    // Should be in clip coords: [-1, 1].
    // TODO: suck it up and use a matrix transform (mat3) to support rotations.
    dst_pos: glam::Vec2,
    dst_size: glam::Vec2,
    color_space: SurfaceColorSpace,
}

/// Composites surfaces into a blend image.
pub struct CompositePipeline {
    descriptor_set_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    sampler: vk::Sampler,
    vk: Arc<VkContext>,
}

impl CompositePipeline {
    pub fn new(vk: Arc<VkContext>) -> anyhow::Result<Self> {
        let sampler = {
            let create_info = vk::SamplerCreateInfo::default()
                .mag_filter(vk::Filter::LINEAR)
                .min_filter(vk::Filter::LINEAR)
                .address_mode_u(vk::SamplerAddressMode::REPEAT)
                .address_mode_v(vk::SamplerAddressMode::REPEAT)
                .address_mode_w(vk::SamplerAddressMode::REPEAT);

            unsafe { vk.device.create_sampler(&create_info, None)? }
        };

        let descriptor_set_layout = {
            let samplers = [sampler];
            let binding = vk::DescriptorSetLayoutBinding::default()
                .binding(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT)
                .immutable_samplers(&samplers);

            let bindings = [binding];
            let create_info = vk::DescriptorSetLayoutCreateInfo::default()
                .bindings(&bindings)
                .flags(vk::DescriptorSetLayoutCreateFlags::PUSH_DESCRIPTOR_KHR);

            unsafe { vk.device.create_descriptor_set_layout(&create_info, None)? }
        };

        let pipeline_layout = {
            let ranges = [vk::PushConstantRange::default()
                .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
                .offset(0)
                .size(std::mem::size_of::<SurfacePC>() as u32)];
            let set_layouts = [descriptor_set_layout];
            let create_info = vk::PipelineLayoutCreateInfo::default()
                .push_constant_ranges(&ranges)
                .set_layouts(&set_layouts);

            unsafe { vk.device.create_pipeline_layout(&create_info, None)? }
        };

        let pipeline = {
            let vert_bytes =
                include_bytes!(concat!(env!("OUT_DIR"), "/shaders/composite_vert.spv"));
            let frag_bytes =
                include_bytes!(concat!(env!("OUT_DIR"), "/shaders/composite_frag.spv"));

            let vert_shader = load_shader(&vk.device, vert_bytes).context("loading vert.spv")?;
            let frag_shader = load_shader(&vk.device, frag_bytes).context("loading frag.spv")?;

            let vert_stage = vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(vert_shader)
                .name(cstr!("main"));

            let frag_stage = vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(frag_shader)
                .name(cstr!("main"));

            let vertex_input_state = vk::PipelineVertexInputStateCreateInfo::default();

            let input_assembly_state = vk::PipelineInputAssemblyStateCreateInfo::default()
                .topology(vk::PrimitiveTopology::TRIANGLE_STRIP)
                .primitive_restart_enable(false);

            let dynamic_state = vk::PipelineDynamicStateCreateInfo::default()
                .dynamic_states(&[vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR]);

            let viewport_state = vk::PipelineViewportStateCreateInfo::default()
                .viewport_count(1)
                .scissor_count(1);

            let rasterization_state = vk::PipelineRasterizationStateCreateInfo::default()
                .depth_clamp_enable(false)
                .rasterizer_discard_enable(false)
                .polygon_mode(vk::PolygonMode::FILL)
                .line_width(1.0)
                .cull_mode(vk::CullModeFlags::NONE)
                .front_face(vk::FrontFace::CLOCKWISE)
                .depth_bias_enable(false);

            let multisample_state = vk::PipelineMultisampleStateCreateInfo::default()
                .sample_shading_enable(false)
                .rasterization_samples(vk::SampleCountFlags::TYPE_1);

            let attachment = vk::PipelineColorBlendAttachmentState::default()
                .color_write_mask(vk::ColorComponentFlags::RGBA)
                .blend_enable(true)
                .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
                .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
                .color_blend_op(vk::BlendOp::ADD)
                .src_alpha_blend_factor(vk::BlendFactor::ONE)
                .dst_alpha_blend_factor(vk::BlendFactor::ZERO)
                .alpha_blend_op(vk::BlendOp::ADD);

            let attachments = [attachment];
            let color_blend_state = vk::PipelineColorBlendStateCreateInfo::default()
                .logic_op_enable(false)
                .attachments(&attachments);

            let formats = [BLEND_FORMAT];
            let mut pipeline_rendering =
                vk::PipelineRenderingCreateInfo::default().color_attachment_formats(&formats);

            let stages = [vert_stage, frag_stage];
            let create_info = vk::GraphicsPipelineCreateInfo::default()
                .stages(&stages)
                .vertex_input_state(&vertex_input_state)
                .input_assembly_state(&input_assembly_state)
                .dynamic_state(&dynamic_state)
                .viewport_state(&viewport_state)
                .rasterization_state(&rasterization_state)
                .multisample_state(&multisample_state)
                .color_blend_state(&color_blend_state)
                .layout(pipeline_layout)
                .push_next(&mut pipeline_rendering);

            unsafe {
                let pipeline = match vk.device.create_graphics_pipelines(
                    vk::PipelineCache::null(),
                    &[create_info],
                    None,
                ) {
                    Ok(pipelines) => Ok(pipelines[0]),
                    Err((_, e)) => Err(e),
                }?;

                vk.device.destroy_shader_module(vert_shader, None);
                vk.device.destroy_shader_module(frag_shader, None);
                pipeline
            }
        };

        Ok(Self {
            descriptor_set_layout,
            pipeline_layout,
            pipeline,
            sampler,
            vk,
        })
    }

    pub unsafe fn begin_compositing(&self, cb: vk::CommandBuffer, render_target: &VkImage) {
        let device = &self.vk.device;

        // Set the viewport and scissor.
        let rect = render_target.rect();
        {
            let viewport = vk::Viewport::default()
                .x(0.0)
                .y(0.0)
                .width(render_target.width as f32)
                .height(render_target.height as f32)
                .min_depth(0.0)
                .max_depth(1.0);

            device.cmd_set_viewport(cb, 0, &[viewport]);
            device.cmd_set_scissor(cb, 0, &[rect]);
        }

        // Attach the render target.
        let clear_value = vk::ClearValue {
            color: vk::ClearColorValue {
                #[cfg(debug_assertions)]
                float32: [0.0, 0.3, 1.0, 1.0], // Blue for debug.
                #[cfg(not(debug_assertions))]
                float32: [0.0, 0.0, 0.0, 1.0],
            },
        };

        let color_attachment = vk::RenderingAttachmentInfo::default()
            .image_view(render_target.view)
            .image_layout(vk::ImageLayout::ATTACHMENT_OPTIMAL)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::STORE)
            .clear_value(clear_value);

        let color_attachments = [color_attachment];
        let rendering_info = vk::RenderingInfo::default()
            .render_area(rect)
            .color_attachments(&color_attachments)
            .layer_count(1);

        device.cmd_begin_rendering(cb, &rendering_info);
        device.cmd_bind_pipeline(cb, vk::PipelineBindPoint::GRAPHICS, self.pipeline);
    }

    /// Draws the surface texture to the output. The texture should already
    /// be in the correct layout.
    pub unsafe fn composite_surface(
        &self,
        cb: vk::CommandBuffer,
        tex: &SurfaceTexture,
        // In clip coordinates.
        // TODO: mat3 transform
        dst_pos: glam::Vec2,
        dst_size: glam::Vec2,
    ) -> anyhow::Result<()> {
        let device = &self.vk.device;

        let color_space = match tex {
            SurfaceTexture::Uploaded { .. } => ColorSpace::Srgb,
            SurfaceTexture::Imported { color_space, .. } => *color_space,
        };

        trace!(?color_space, ?dst_pos, ?dst_size, "compositing surface");

        let pc = SurfacePC {
            src_pos: glam::Vec2::ZERO,
            src_size: glam::Vec2::ONE,
            dst_pos,
            dst_size,
            color_space: color_space.into(),
        };

        // Push the texture.
        {
            let view = match tex {
                SurfaceTexture::Imported { image, .. } => image.view,
                SurfaceTexture::Uploaded { image, .. } => image.view,
            };

            let image_info = vk::DescriptorImageInfo::default()
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .image_view(view);

            let image_infos = [image_info];
            let write = vk::WriteDescriptorSet::default()
                .dst_set(vk::DescriptorSet::null())
                .dst_binding(0)
                .dst_array_element(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&image_infos);

            let writes = [write];
            unsafe {
                self.vk.push_ds_api.cmd_push_descriptor_set(
                    cb,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.pipeline_layout,
                    0,
                    &writes,
                );
            }
        }

        device.cmd_push_constants(
            cb,
            self.pipeline_layout,
            vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
            0,
            std::slice::from_raw_parts(
                &pc as *const _ as *const u8,
                std::mem::size_of::<SurfacePC>(),
            ),
        );

        device.cmd_draw(cb, 4, 1, 0, 0);

        Ok(())
    }

    pub unsafe fn end_compositing(&self, cb: vk::CommandBuffer) {
        self.vk.device.cmd_end_rendering(cb);
    }
}

impl Drop for CompositePipeline {
    fn drop(&mut self) {
        let device = &self.vk.device;

        unsafe {
            device
                .queue_wait_idle(self.vk.graphics_queue.queue)
                .unwrap();

            device.destroy_pipeline(self.pipeline, None);
            device.destroy_descriptor_set_layout(self.descriptor_set_layout, None);
            device.destroy_pipeline_layout(self.pipeline_layout, None);
            device.destroy_sampler(self.sampler, None);
        }
    }
}
