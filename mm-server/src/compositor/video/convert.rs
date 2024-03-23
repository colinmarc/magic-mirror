// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use ash::vk;
use tracing::instrument;

use crate::vulkan::*;

use super::VkPlaneView;

// GLSL requires vec4s for alignment.
const COLORSPACE_BT709: [[f32; 4]; 3] = [
    [0.2126, 0.7152, 0.0722, 0.0],
    [-0.1146, -0.3854, 0.5, 0.0],
    [0.5, -0.4542, -0.0458, 0.0],
];

#[repr(C)]
#[derive(Copy, Clone, Debug)]
struct ColorspacePC {
    color_space: [[f32; 4]; 3],
}

pub struct ConvertPipeline {
    semiplanar: bool,
    descriptor_set_layout: vk::DescriptorSetLayout,
    sampler: vk::Sampler,
    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    vk: Arc<VkContext>,
}

impl ConvertPipeline {
    #[instrument(level = "trace", name = "ConvertPipeline::new", skip_all)]
    pub fn new(vk: Arc<VkContext>, semiplanar: bool) -> anyhow::Result<Self> {
        let shader = if semiplanar {
            load_shader(
                &vk.device,
                include_bytes!(concat!(env!("OUT_DIR"), "/shaders/convert_semiplanar.spv")),
            )?
        } else {
            load_shader(
                &vk.device,
                include_bytes!(concat!(env!("OUT_DIR"), "/shaders/convert_multiplanar.spv")),
            )?
        };

        let sampler = {
            let create_info = vk::SamplerCreateInfo::default()
                .mag_filter(vk::Filter::LINEAR)
                .min_filter(vk::Filter::LINEAR)
                .address_mode_u(vk::SamplerAddressMode::REPEAT)
                .address_mode_v(vk::SamplerAddressMode::REPEAT)
                .address_mode_w(vk::SamplerAddressMode::REPEAT);

            unsafe { vk.device.create_sampler(&create_info, None)? }
        };

        let descriptor_set_layout = unsafe {
            let samplers = [sampler];
            let mut bindings = vec![
                vk::DescriptorSetLayoutBinding::default()
                    .binding(0)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::COMPUTE)
                    .immutable_samplers(&samplers),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(1)
                    .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::COMPUTE),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(2)
                    .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::COMPUTE),
            ];

            if !semiplanar {
                bindings.push(
                    vk::DescriptorSetLayoutBinding::default()
                        .binding(3)
                        .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                        .descriptor_count(1)
                        .stage_flags(vk::ShaderStageFlags::COMPUTE),
                );
            }

            vk.device.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings),
                None,
            )?
        };

        let pipeline_layout = {
            let ranges = [vk::PushConstantRange::default()
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
                .offset(0)
                .size(std::mem::size_of::<ColorspacePC>() as u32)];

            let set_layouts = [descriptor_set_layout];
            let create_info = vk::PipelineLayoutCreateInfo::default()
                .set_layouts(&set_layouts)
                .push_constant_ranges(&ranges);

            unsafe { vk.device.create_pipeline_layout(&create_info, None)? }
        };

        let pipeline = unsafe {
            let entry_point = std::ffi::CString::new("main")?;
            let stage = vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::COMPUTE)
                .module(shader)
                .name(&entry_point);

            let create_info = vk::ComputePipelineCreateInfo::default()
                .stage(stage)
                .layout(pipeline_layout);

            let pipeline = match vk.device.create_compute_pipelines(
                vk::PipelineCache::null(),
                &[create_info],
                None,
            ) {
                Ok(pipelines) => pipelines[0],
                Err((_, e)) => return Err(e.into()),
            };

            vk.device.destroy_shader_module(shader, None);
            pipeline
        };

        Ok(Self {
            semiplanar,
            descriptor_set_layout,
            sampler,
            pipeline_layout,
            pipeline,
            vk,
        })
    }

    pub unsafe fn cmd_convert(
        &self,
        cb: vk::CommandBuffer,
        width: u32,
        height: u32,
        descriptor_set: vk::DescriptorSet,
    ) {
        self.vk
            .device
            .cmd_bind_pipeline(cb, vk::PipelineBindPoint::COMPUTE, self.pipeline);

        self.vk.device.cmd_bind_descriptor_sets(
            cb,
            vk::PipelineBindPoint::COMPUTE,
            self.pipeline_layout,
            0,
            &[descriptor_set],
            &[],
        );

        let pc = ColorspacePC {
            color_space: COLORSPACE_BT709, // TODO
        };

        self.vk.device.cmd_push_constants(
            cb,
            self.pipeline_layout,
            vk::ShaderStageFlags::COMPUTE,
            0,
            std::slice::from_raw_parts(
                &pc as *const _ as *const u8,
                std::mem::size_of::<ColorspacePC>(),
            ),
        );

        // Each workgroup has 16x16 invocations, covering a 32x32 area.
        let group_count_x = (width + 31) / 32;
        let group_count_y = (height + 31) / 32;

        self.vk
            .device
            .cmd_dispatch(cb, group_count_x, group_count_y, 1);
    }

    pub unsafe fn ds_for_conversion(
        &self,
        blend_image: &VkImage,
        planes: &[VkPlaneView],
        pool: vk::DescriptorPool,
    ) -> anyhow::Result<vk::DescriptorSet> {
        let set_layouts = [self.descriptor_set_layout];
        let allocate_info = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(pool)
            .set_layouts(&set_layouts);

        let ds = self
            .vk
            .device
            .allocate_descriptor_sets(&allocate_info)?
            .pop()
            .unwrap();

        let blend_image_infos = [vk::DescriptorImageInfo::default()
            .image_layout(vk::ImageLayout::GENERAL)
            .image_view(blend_image.view)];
        let blend_write = vk::WriteDescriptorSet::default()
            .dst_set(ds)
            .dst_binding(0)
            .dst_array_element(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&blend_image_infos);

        let y_image_infos = [vk::DescriptorImageInfo::default()
            .image_layout(vk::ImageLayout::GENERAL)
            .image_view(planes[0].view)];
        let y_write = vk::WriteDescriptorSet::default()
            .dst_set(ds)
            .dst_binding(1)
            .dst_array_element(0)
            .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
            .image_info(&y_image_infos);

        if self.semiplanar {
            let uv_image_infos = [vk::DescriptorImageInfo::default()
                .image_layout(vk::ImageLayout::GENERAL)
                .image_view(planes[1].view)];
            let uv_write = vk::WriteDescriptorSet::default()
                .dst_set(ds)
                .dst_binding(2)
                .dst_array_element(0)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(&uv_image_infos);

            let writes = [blend_write, y_write, uv_write];
            unsafe {
                self.vk.device.update_descriptor_sets(&writes, &[]);
            }
        } else {
            let u_image_infos = [vk::DescriptorImageInfo::default()
                .image_layout(vk::ImageLayout::GENERAL)
                .image_view(planes[1].view)];
            let u_write = vk::WriteDescriptorSet::default()
                .dst_set(ds)
                .dst_binding(2)
                .dst_array_element(0)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(&u_image_infos);

            let v_image_infos = [vk::DescriptorImageInfo::default()
                .image_layout(vk::ImageLayout::GENERAL)
                .image_view(planes[2].view)];
            let v_write = vk::WriteDescriptorSet::default()
                .dst_set(ds)
                .dst_binding(3)
                .dst_array_element(0)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(&v_image_infos);

            let writes = [blend_write, y_write, u_write, v_write];
            unsafe {
                self.vk.device.update_descriptor_sets(&writes, &[]);
            }
        }

        Ok(ds)
    }
}

impl Drop for ConvertPipeline {
    fn drop(&mut self) {
        let device = &self.vk.device;

        unsafe {
            device
                .queue_wait_idle(self.vk.graphics_queue.queue)
                .unwrap();

            device.destroy_sampler(self.sampler, None);
            device.destroy_pipeline(self.pipeline, None);
            device.destroy_pipeline_layout(self.pipeline_layout, None);
            device.destroy_descriptor_set_layout(self.descriptor_set_layout, None);
        }
    }
}
