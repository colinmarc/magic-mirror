// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{os::fd::AsFd as _, sync::Arc};

use ash::vk;
use cstr::cstr;
use drm_fourcc::{DrmFormat, DrmFourcc};
use tracing::{debug, trace};
use wayland_protocols::wp::linux_dmabuf::zv1::server::zwp_linux_dmabuf_feedback_v1;

use crate::{
    session::compositor::{sealed::SealedFile, Compositor},
    vulkan::VkContext,
};

// Note that Mesa will throw out a format if either the opaque or alpha version
// is missing. For example, Argb8888 requires Xrgb8888, and vice versa.
/// (Fourcc, VkFormat, alpha, bpp)
pub const SUPPORTED_DRM_FORMATS: &[(DrmFourcc, vk::Format, bool, usize)] = &[
    (DrmFourcc::Argb8888, vk::Format::B8G8R8A8_UNORM, false, 4),
    (DrmFourcc::Xrgb8888, vk::Format::B8G8R8A8_UNORM, true, 4),
    (DrmFourcc::Abgr8888, vk::Format::R8G8B8A8_UNORM, false, 4),
    (DrmFourcc::Xbgr8888, vk::Format::R8G8B8A8_UNORM, true, 4),
    (
        DrmFourcc::Argb16161616f,
        vk::Format::R16G16B16A16_SFLOAT,
        false,
        8,
    ),
    (
        DrmFourcc::Xrgb16161616f,
        vk::Format::R16G16B16A16_SFLOAT,
        true,
        8,
    ),
    (
        DrmFourcc::Abgr16161616f,
        vk::Format::R16G16B16A16_SFLOAT,
        false,
        8,
    ),
    (
        DrmFourcc::Xbgr16161616f,
        vk::Format::R16G16B16A16_SFLOAT,
        true,
        8,
    ),
];

pub fn fourcc_to_vk(fourcc: DrmFourcc) -> Option<(vk::Format, bool)> {
    SUPPORTED_DRM_FORMATS
        .iter()
        .find(|(f, _, _, _)| *f == fourcc)
        .map(|(_, vk, ignore_alpha, _)| (*vk, *ignore_alpha))
}

pub fn fourcc_bpp(fourcc: DrmFourcc) -> Option<usize> {
    SUPPORTED_DRM_FORMATS
        .iter()
        .find(|(f, _, _, _)| *f == fourcc)
        .map(|(_, _, _, bpp)| *bpp)
}

pub struct CachedDmabufFeedback {
    drm_node: u64,
    formats: Vec<DrmFormat>,
    table: SealedFile,
}

impl CachedDmabufFeedback {
    pub fn contains(&self, modifier: u64) -> bool {
        self.formats
            .iter()
            .any(|format| format.modifier == modifier)
    }

    pub fn new(vk: Arc<VkContext>) -> anyhow::Result<Self> {
        let formats = unsafe {
            SUPPORTED_DRM_FORMATS
                .iter()
                .flat_map(|(fourcc, format, _, _)| {
                    let mods =
                        query_drm_format_modifiers(&vk.instance, vk.device_info.pdevice, *format);

                    mods.into_iter().filter_map(|props| {
                        if props.drm_format_modifier_plane_count == 1 {
                            let modifier = props.drm_format_modifier.into();
                            assert!(verify_dmabuf_support(
                                vk.clone(),
                                *format,
                                modifier,
                                vk::ImageUsageFlags::SAMPLED,
                            ));

                            Some(DrmFormat {
                                code: *fourcc,
                                modifier,
                            })
                        } else {
                            None
                        }
                    })
                })
                .collect::<Vec<_>>()
        };

        let mut table = vec![0_u8; 16 * formats.len()];
        for (idx, format) in formats.iter().enumerate() {
            let off = idx * 16;
            let modifier: u64 = format.modifier.into();
            let code = format.code as u32;
            trace!(idx, code = ?format.code, code, modifier, "adding format to table");

            table[off..off + 4].copy_from_slice(&code.to_ne_bytes());
            table[off + 8..off + 16].copy_from_slice(&modifier.to_ne_bytes());
        }

        Ok(Self {
            formats,
            drm_node: vk.device_info.drm_node,
            table: SealedFile::new(cstr!("dmabuf_formats"), &table)?,
        })
    }
}

impl Compositor {
    pub fn emit_dmabuf_feedback(
        &self,
        feedback: &zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1,
    ) {
        let fb = &self.cached_dmabuf_feedback;
        let dev = fb.drm_node.to_ne_bytes().to_vec();
        feedback.main_device(dev.clone());
        feedback.format_table(fb.table.as_fd(), fb.table.size() as u32);
        feedback.tranche_target_device(dev.clone());
        feedback.tranche_flags(zwp_linux_dmabuf_feedback_v1::TrancheFlags::empty());

        let indices = (0..(fb.formats.len() as u16))
            .flat_map(|i| i.to_ne_bytes())
            .collect::<Vec<_>>();
        feedback.tranche_formats(indices);
        feedback.tranche_done();
        feedback.done();
    }
}

unsafe fn query_drm_format_modifiers(
    instance: &ash::Instance,
    device: vk::PhysicalDevice,
    format: vk::Format,
) -> Vec<vk::DrmFormatModifierPropertiesEXT> {
    let count = {
        let mut modifiers = vk::DrmFormatModifierPropertiesListEXT::default();
        let mut format_props = vk::FormatProperties2::default().push_next(&mut modifiers);

        instance.get_physical_device_format_properties2(device, format, &mut format_props);
        modifiers.drm_format_modifier_count
    };

    let mut res = vec![vk::DrmFormatModifierPropertiesEXT::default(); count as usize];
    let mut modifiers =
        vk::DrmFormatModifierPropertiesListEXT::default().drm_format_modifier_properties(&mut res);
    let mut format_props = vk::FormatProperties2::default().push_next(&mut modifiers);
    instance.get_physical_device_format_properties2(device, format, &mut format_props);

    res
}

pub unsafe fn verify_dmabuf_support(
    vk: Arc<VkContext>,
    format: vk::Format,
    modifier: drm_fourcc::DrmModifier,
    usage: vk::ImageUsageFlags,
) -> bool {
    let mut drm_props = vk::ExternalImageFormatProperties::default();
    let mut props = vk::ImageFormatProperties2::default().push_next(&mut drm_props);

    let mut modifier_info = vk::PhysicalDeviceImageDrmFormatModifierInfoEXT::default()
        .drm_format_modifier(modifier.into());

    let mut external_format_info = vk::PhysicalDeviceExternalImageFormatInfo::default()
        .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

    let format_info = vk::PhysicalDeviceImageFormatInfo2::default()
        .format(format)
        .ty(vk::ImageType::TYPE_2D)
        .usage(usage)
        .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
        .push_next(&mut external_format_info)
        .push_next(&mut modifier_info);

    match vk.instance.get_physical_device_image_format_properties2(
        vk.device_info.pdevice,
        &format_info,
        &mut props,
    ) {
        Ok(_) => (),
        Err(_) => {
            debug!(?format, ?modifier, "format not supported for dma import");
            return false;
        }
    }

    drm_props
        .external_memory_properties
        .compatible_handle_types
        .contains(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
}
