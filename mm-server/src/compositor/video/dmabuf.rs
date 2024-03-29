// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{
    os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd},
    sync::Arc,
};

use anyhow::{bail, Context};
use ash::vk;
use drm_fourcc::{DrmFormat, DrmFourcc};
use smithay::{backend::allocator::dmabuf::Dmabuf, wayland::dmabuf};
use tracing::{debug, trace};

use crate::vulkan::{create_image_view, select_memory_type, VkContext, VkImage};

// Note that Mesa will throw out a format if either the opaque or alpha version
// is missing. For example, Argb8888 requires Xrgb8888, and vice versa.
const SUPPORTED_DRM_FORMATS: &[(DrmFourcc, vk::Format, bool)] = &[
    (DrmFourcc::Argb8888, vk::Format::B8G8R8A8_UNORM, false),
    (DrmFourcc::Xrgb8888, vk::Format::B8G8R8A8_UNORM, true),
    (DrmFourcc::Abgr8888, vk::Format::R8G8B8A8_UNORM, false),
    (DrmFourcc::Xbgr8888, vk::Format::R8G8B8A8_UNORM, true),
    (
        DrmFourcc::Argb16161616f,
        vk::Format::R16G16B16A16_SFLOAT,
        false,
    ),
    (
        DrmFourcc::Xrgb16161616f,
        vk::Format::R16G16B16A16_SFLOAT,
        true,
    ),
    (
        DrmFourcc::Abgr16161616f,
        vk::Format::R16G16B16A16_SFLOAT,
        false,
    ),
    (
        DrmFourcc::Xbgr16161616f,
        vk::Format::R16G16B16A16_SFLOAT,
        true,
    ),
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
        let mut fd_props = vk::MemoryFdPropertiesKHR::default();

        unsafe {
            vk.external_memory_api.get_memory_fd_properties(
                vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
                fd.as_raw_fd(),
                &mut fd_props,
            )?;
        };

        let image_memory_req = unsafe { vk.device.get_image_memory_requirements(image) };
        let memory_type_index = select_memory_type(
            &vk.device_info.memory_props,
            vk::MemoryPropertyFlags::empty(),
            Some(image_memory_req.memory_type_bits & fd_props.memory_type_bits),
        );

        trace!(
            ?fd_props,
            ?memory_type_index,
            ?image_memory_req,
            "memory import for dmabuf"
        );

        let mut external_mem_info = vk::ImportMemoryFdInfoKHR::default()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .fd(fd.into_raw_fd()); // Vulkan owns the fd now.

        let image_allocate_info = vk::MemoryAllocateInfo::default()
            .allocation_size(image_memory_req.size)
            .push_next(&mut external_mem_info);

        unsafe { vk.device.allocate_memory(&image_allocate_info, None)? }
    };

    unsafe {
        vk.device.bind_image_memory(image, memory, 0)?;
    }

    let view = unsafe { create_image_view(&vk.device, image, format, ignore_alpha)? };

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

#[allow(dead_code)]
mod ioctl {
    pub(super) const DMA_BUF_SYNC_READ: u32 = 1 << 0;
    pub(super) const DMA_BUF_SYNC_WRITE: u32 = 1 << 1;

    #[repr(C)]
    #[allow(non_camel_case_types)]
    pub(super) struct dma_buf_export_sync_file {
        pub flags: u32,
        pub fd: i32,
    }

    #[repr(C)]
    #[allow(non_camel_case_types)]
    pub(super) struct dma_buf_import_sync_file {
        pub flags: u32,
        pub fd: i32,
    }

    nix::ioctl_readwrite!(export_sync_file, b'b', 2, dma_buf_export_sync_file);
    nix::ioctl_write_ptr!(import_sync_file, b'b', 3, dma_buf_import_sync_file);
}

/// Retrieves a dmabuf fence, and uses it to set a semaphore. The semaphore will
/// be triggered when the dmabuf texture is safe to read. Note that the spec
/// insists that the semaphore must be waited on once set this way.
pub fn import_dmabuf_fence_as_semaphore(
    vk: Arc<VkContext>,
    semaphore: vk::Semaphore,
    dmabuf: Dmabuf,
) -> anyhow::Result<()> {
    assert_eq!(dmabuf.num_planes(), 1);

    let fd = dmabuf.handles().next().unwrap();
    let sync_fd = export_sync_file(fd, ioctl::DMA_BUF_SYNC_READ)?;

    let import_info = vk::ImportSemaphoreFdInfoKHR::default()
        .semaphore(semaphore)
        .handle_type(vk::ExternalSemaphoreHandleTypeFlags::SYNC_FD)
        .flags(vk::SemaphoreImportFlags::TEMPORARY)
        .fd(sync_fd.into_raw_fd()); // Vulkan owns the fd now.

    unsafe {
        vk.external_semaphore_api
            .import_semaphore_fd(&import_info)?;
    }

    Ok(())
}

/// Retrieves the fd of a sync file for a dmabuf.
pub fn export_sync_file(dmabuf: impl AsRawFd, flags: u32) -> anyhow::Result<OwnedFd> {
    let mut data = ioctl::dma_buf_export_sync_file { flags, fd: -1 };

    let res = unsafe {
        ioctl::export_sync_file(dmabuf.as_raw_fd(), &mut data)
            .context("error in dma_buf_export_sync_file ioctl")?
    };

    if res != 0 {
        bail!("ioctl dma_buf_export_sync_file failed: {}", res);
    } else {
        let fd = unsafe { OwnedFd::from_raw_fd(data.fd) };
        Ok(fd)
    }
}

/// Attaches a sync file to a dmabuf.
// TODO: the kernel docs and online resources state that we need to use this to
// attach a "render finished" semaphore back onto the client buffers once we
// start rendering. I think that's unecessary as long as we wait to call
// `wl_buffer.release` until long after we're done compositing, which we do as
// of this writing.
#[allow(dead_code)]
pub fn attach_sync_file(
    dmabuf: impl AsRawFd,
    flags: u32,
    sync_file: OwnedFd,
) -> anyhow::Result<()> {
    let data = ioctl::dma_buf_import_sync_file {
        flags,
        fd: sync_file.as_raw_fd(),
    };

    let res = unsafe {
        ioctl::import_sync_file(dmabuf.as_raw_fd(), &data)
            .context("error in dma_buf_import_sync_file ioctl")?
    };

    if res != 0 {
        bail!("ioctl dma_buf_import_sync_file failed: {}", res);
    } else {
        Ok(())
    }
}
