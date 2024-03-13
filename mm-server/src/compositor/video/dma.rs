// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{os::fd::IntoRawFd, sync::Arc};

use anyhow::bail;
use ash::vk;
use drm_fourcc::{DrmFormat, DrmFourcc};
use smithay::{backend::allocator::dmabuf::Dmabuf, wayland::dmabuf};
use tracing::{debug, trace};

use crate::vulkan::{create_image_view, VkContext, VkImage};

// Note that Mesa will throw out a format if either the opaque or alpha version
// is missing. For example, Argb8888 requires Xrgb8888, and vice versa.
const SUPPORTED_DRM_FORMATS: &[(DrmFourcc, vk::Format, bool)] = &[
    (DrmFourcc::Argb8888, vk::Format::B8G8R8A8_UNORM, false),
    (DrmFourcc::Xrgb8888, vk::Format::B8G8R8A8_UNORM, true),
    (DrmFourcc::Abgr8888, vk::Format::R8G8B8A8_UNORM, false),
    (DrmFourcc::Xbgr8888, vk::Format::R8G8B8A8_UNORM, true),
];

pub fn fourcc_to_vk(fourcc: DrmFourcc) -> Option<(vk::Format, bool)> {
    SUPPORTED_DRM_FORMATS
        .iter()
        .find(|(f, _, _)| *f == fourcc)
        .map(|(_, vk, ignore_alpha)| (*vk, *ignore_alpha))
}

pub fn dmabuf_feedback(vk: Arc<VkContext>) -> anyhow::Result<dmabuf::DmabufFeedback> {
    let drm_formats = unsafe {
        SUPPORTED_DRM_FORMATS
            .iter()
            .flat_map(|(fourcc, format, _)| {
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

    Ok(dmabuf::DmabufFeedbackBuilder::new(vk.device_info.drm_node, drm_formats).build()?)
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
            debug!("format not supported for dma import: {:?}", format);
            return false;
        }
    }

    drm_props
        .external_memory_properties
        .compatible_handle_types
        .contains(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
}

pub fn import_dma_texture(
    vk: Arc<VkContext>,
    buffer: &Dmabuf,
    usage: vk::ImageUsageFlags,
) -> anyhow::Result<VkImage> {
    use smithay::backend::allocator::Buffer;
    use std::os::fd::AsRawFd;

    assert_eq!(buffer.num_planes(), 1);

    let (format, ignore_alpha) = match fourcc_to_vk(buffer.format().code) {
        Some(format) => format,
        None => bail!("unsupported dmabuf format: {:?}", buffer.format().code),
    };

    unsafe {
        if !verify_dmabuf_support(vk.clone(), format, buffer.format().modifier, usage) {
            bail!("unsupported dmabuf format: {:?}", format);
        }
    }

    let (width, height) = buffer.size().into();

    let fd = buffer.handles().next().unwrap();

    trace!(
        fourcc = ?buffer.format().code,
        ?format, width, height,
        fd = fd.as_raw_fd(),
        ?buffer,
        "importing dmabuf texture"
    );

    let offset = buffer.offsets().next().unwrap();
    assert_eq!(offset, 0);

    // Vulkan wants to own the file descriptor, so we create a dup'd one just for the driver.
    let fd = fd.try_clone_to_owned()?;

    let image = {
        let plane_layouts = [vk::SubresourceLayout {
            offset: 0,
            size: 0, // Must be zero, according to the spec.
            row_pitch: buffer.strides().next().unwrap() as u64,
            ..Default::default()
        }];

        let mut format_modifier_info = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
            .drm_format_modifier(buffer.format().modifier.into())
            .plane_layouts(&plane_layouts);

        let mut external_memory_info = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

        let create_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D {
                width: width as u32,
                height: height as u32,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            .samples(vk::SampleCountFlags::TYPE_1)
            .usage(vk::ImageUsageFlags::SAMPLED)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut external_memory_info)
            .push_next(&mut format_modifier_info);

        unsafe { vk.device.create_image(&create_info, None).unwrap() }
    };

    let memory = {
        // let fd_props = unsafe {
        //     vk.external_mem_loader
        //         .get_memory_fd_properties(
        //             vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
        //             fd.as_raw_fd(),
        //         )
        //         .unwrap()
        // };

        let image_memory_req = unsafe { vk.device.get_image_memory_requirements(image) };

        let mut external_mem_info = vk::ImportMemoryFdInfoKHR::default()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .fd(fd.into_raw_fd()); // Vulkan owns the fd now.

        // TODO: explicit memory type index

        let image_allocate_info = vk::MemoryAllocateInfo::default()
            .allocation_size(image_memory_req.size)
            .push_next(&mut external_mem_info);

        unsafe { vk.device.allocate_memory(&image_allocate_info, None)? }
    };

    unsafe {
        vk.device.bind_image_memory(image, memory, 0)?;
    }

    let view = unsafe { create_image_view(&vk.device, image, format, ignore_alpha, None)? };

    Ok(VkImage::wrap(
        vk.clone(),
        image,
        view,
        memory,
        format,
        width as u32,
        height as u32,
    ))
}
