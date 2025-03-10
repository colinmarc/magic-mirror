// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

mod modifiers;
mod syncobj_timeline;

use std::{
    collections::BTreeSet,
    os::fd::{AsFd, AsRawFd, FromRawFd as _, IntoRawFd as _, OwnedFd},
    sync::{Arc, RwLock},
};

use anyhow::{bail, Context as _};
use ash::vk;
use drm_fourcc::DrmModifier;
pub use modifiers::*;
pub use syncobj_timeline::*;
use tracing::{instrument, trace};
use wayland_server::{protocol::wl_buffer, Resource as _};

use crate::{
    session::compositor::{shm::Pool, Compositor},
    vulkan::{create_image_view, select_memory_type, VkContext, VkHostBuffer, VkImage},
};

slotmap::new_key_type! { pub struct BufferKey; }

pub struct Buffer {
    pub wl_buffer: wl_buffer::WlBuffer,
    pub backing: BufferBacking,

    /// Next time we release this buffer, we should destroy it as well.
    pub needs_destruction: bool,
}

impl Buffer {
    pub fn dimensions(&self) -> glam::UVec2 {
        match self.backing {
            BufferBacking::Shm { format, .. } => (format.width, format.height).into(),
            BufferBacking::Dmabuf { format, .. } => (format.width, format.height).into(),
        }
    }
}

pub enum BufferBacking {
    Shm {
        format: PlaneMetadata,
        pool: Arc<RwLock<Pool>>,
        staging_buffer: VkHostBuffer,
        image: VkImage,

        /// Indicates that staging_buffer has been written to and needs to
        /// be uploaded to the image.
        dirty: bool,
    },
    Dmabuf {
        format: PlaneMetadata,
        fd: OwnedFd,
        image: VkImage,
    },
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct PlaneMetadata {
    pub format: drm_fourcc::DrmFourcc,
    pub bpp: usize,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub offset: u32,
}

impl Compositor {
    #[instrument(skip_all)]
    pub fn release_buffers(&mut self) -> anyhow::Result<()> {
        // Check if any content updates have finished.
        let mut still_in_flight = Vec::new();
        for content in self.in_flight_buffers.drain(..) {
            if let Some(tp) = &content.tp_done {
                if unsafe { !tp.poll()? } {
                    // The frame using this content is still in-progress.
                    still_in_flight.push(content);
                    continue;
                }
            }

            if content.needs_release {
                let buffer = self
                    .buffers
                    .get(content.buffer)
                    .expect("buffer has no entry");

                trace!(
                    wl_buffer = buffer.wl_buffer.id().protocol_id(),
                    "explicitly releasing buffer"
                );

                buffer.wl_buffer.release();
            }

            if let Some((_, release)) = content.explicit_sync {
                release.signal()?;
            }

            // If we didn't move the presentation feedback into a separate queue,
            // that means we didn't use the content update and we should relate
            // that to the client.
            if let Some(feedback) = &content.wp_presentation_feedback {
                feedback.discarded();
            }
        }

        self.in_flight_buffers = still_in_flight;

        // A buffer is in use if it's either part of an in-flight frame, or if
        // we're holding on to it because the client hasn't committed a new one
        // yet, and we may need to display it again.
        let used_buffers: BTreeSet<BufferKey> = self
            .surfaces
            .values()
            .flat_map(|s| &s.content)
            .chain(self.in_flight_buffers.iter())
            .map(|c| c.buffer)
            .collect();

        self.buffers.retain(|id, buffer| {
            if !buffer.needs_destruction || used_buffers.contains(&id) {
                true
            } else {
                assert!(!buffer.wl_buffer.is_alive());
                trace!(
                    wl_buffer = buffer.wl_buffer.id().protocol_id(),
                    "destroying buffer"
                );

                false
            }
        });

        Ok(())
    }
}

#[instrument(skip_all)]
pub fn import_shm_buffer(
    vk: Arc<VkContext>,
    wl_buffer: wl_buffer::WlBuffer,
    pool: Arc<RwLock<Pool>>,
    format: PlaneMetadata,
) -> anyhow::Result<Buffer> {
    let (vk_format, ignore_alpha) = match format.format {
        drm_fourcc::DrmFourcc::Argb8888 => (vk::Format::B8G8R8A8_UNORM, false),
        drm_fourcc::DrmFourcc::Xrgb8888 => (vk::Format::B8G8R8A8_UNORM, true),
        _ => unreachable!(),
    };

    let len = format.stride * format.height;
    trace!(?format, len, "importing shm buffer");

    let staging_buffer = VkHostBuffer::new(
        vk.clone(),
        vk.device_info.host_visible_mem_type_index,
        vk::BufferUsageFlags::TRANSFER_SRC,
        len as usize,
    )?;

    let image = VkImage::new(
        vk.clone(),
        vk_format,
        ignore_alpha,
        format.width,
        format.height,
        vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED,
        vk::SharingMode::EXCLUSIVE,
        vk::ImageCreateFlags::empty(),
    )?;

    Ok(Buffer {
        wl_buffer,
        backing: BufferBacking::Shm {
            pool,
            staging_buffer,
            image,
            format,
            dirty: true,
        },
        needs_destruction: false,
    })
}

#[instrument(skip_all)]
pub fn import_dmabuf_buffer(
    vk: Arc<VkContext>,
    wl_buffer: wl_buffer::WlBuffer,
    format: PlaneMetadata,
    modifier: DrmModifier,
    fd: OwnedFd,
) -> anyhow::Result<Buffer> {
    let PlaneMetadata {
        format: fourcc,
        width,
        height,
        stride,
        offset,
        ..
    } = format;

    let (vk_format, ignore_alpha) = match modifiers::fourcc_to_vk(fourcc) {
        Some(format) => format,
        None => bail!("unsupported dmabuf format: {:?}", format),
    };

    unsafe {
        if !modifiers::verify_dmabuf_support(
            vk.clone(),
            vk_format,
            modifier,
            vk::ImageUsageFlags::SAMPLED,
        ) {
            bail!("unsupported dmabuf format: {:?}", vk_format);
        }
    }

    trace!(
        ?fourcc,
        ?vk_format,
        width,
        height,
        offset,
        stride,
        fd = fd.as_fd().as_raw_fd(),
        "importing dmabuf texture"
    );

    // Vulkan wants to own the file descriptor, so we create a dup'd one just for
    // the driver.
    let vk_fd = fd.as_fd().try_clone_to_owned()?;

    let image = {
        let plane_layouts = [vk::SubresourceLayout {
            offset: offset as u64,
            size: 0, // Must be zero, according to the spec.
            row_pitch: stride as u64,
            ..Default::default()
        }];

        let mut format_modifier_info = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
            .drm_format_modifier(modifier.into())
            .plane_layouts(&plane_layouts);

        let mut external_memory_info = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

        let create_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk_format)
            .extent(vk::Extent3D {
                width,
                height,
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
                vk_fd.as_raw_fd(),
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
            .fd(vk_fd.into_raw_fd()); // Vulkan owns the fd now.

        // Technically we can query whether this is required, but it doesn't
        // hurt anyways. It seems to be only required on some NVIDIA cards.
        let mut dedicated_memory_info = vk::MemoryDedicatedAllocateInfo::default().image(image);

        let image_allocate_info = vk::MemoryAllocateInfo::default()
            .allocation_size(image_memory_req.size)
            .push_next(&mut external_mem_info)
            .push_next(&mut dedicated_memory_info);

        unsafe { vk.device.allocate_memory(&image_allocate_info, None)? }
    };

    unsafe {
        vk.device.bind_image_memory(image, memory, 0)?;
    }

    let view = unsafe { create_image_view(&vk.device, image, vk_format, ignore_alpha)? };
    let image = VkImage::wrap(vk.clone(), image, view, memory, vk_format, width, height);

    Ok(Buffer {
        wl_buffer,
        backing: BufferBacking::Dmabuf { format, fd, image },
        needs_destruction: false,
    })
}

pub fn validate_buffer_parameters(
    offset: i32,
    width: i32,
    height: i32,
    stride: i32,
    bpp: usize,
) -> Result<(), String> {
    if offset < 0 {
        return Err("Negative offset.".to_string());
    }

    if width <= 0 || height <= 0 {
        return Err("Invalid height or width.".to_string());
    }

    if stride <= 0
        || stride.checked_div(bpp as i32).unwrap_or(0) < width
        || stride.checked_mul(height).is_none()
    {
        return Err("Invalid stride.".to_string());
    }

    if let Some(size) = stride.checked_mul(height) {
        if offset.checked_add(size).is_none() {
            return Err("Invalid offset.".to_string());
        }
    } else {
        return Err("Invalid total size.".to_string());
    }

    Ok(())
}

#[allow(dead_code)]
mod ioctl {
    use std::{ffi::c_void, os::fd::RawFd};

    use rustix::{
        io::Errno,
        ioctl::{opcode, Opcode},
    };

    pub(super) const DMA_BUF_SYNC_READ: u32 = 1 << 0;
    pub(super) const DMA_BUF_SYNC_WRITE: u32 = 1 << 1;

    #[repr(C)]
    #[allow(non_camel_case_types)]
    struct dma_buf_export_sync_file {
        pub flags: u32,
        pub fd: i32,
    }

    #[repr(C)]
    #[allow(non_camel_case_types)]
    struct dma_buf_import_sync_file {
        pub flags: u32,
        pub fd: i32,
    }

    pub(super) struct ExportSyncFile(dma_buf_export_sync_file);

    impl ExportSyncFile {
        pub(super) fn new(flags: u32) -> Self {
            Self(dma_buf_export_sync_file { flags, fd: -1 })
        }
    }

    pub(super) struct ImportSyncFile(dma_buf_import_sync_file);

    impl ImportSyncFile {
        pub(super) fn new(fd: RawFd, flags: u32) -> Self {
            Self(dma_buf_import_sync_file { flags, fd })
        }
    }

    unsafe impl rustix::ioctl::Ioctl for ExportSyncFile {
        type Output = RawFd;

        const IS_MUTATING: bool = true;

        fn opcode(&self) -> Opcode {
            opcode::read_write::<dma_buf_export_sync_file>(b'b', 2)
        }

        fn as_ptr(&mut self) -> *mut c_void {
            &mut self.0 as *mut dma_buf_export_sync_file as _
        }

        unsafe fn output_from_ptr(
            out: rustix::ioctl::IoctlOutput,
            extract_output: *mut c_void,
        ) -> rustix::io::Result<Self::Output> {
            let res: &mut dma_buf_export_sync_file = &mut *(extract_output as *mut _);
            if out != 0 {
                Err(rustix::io::Errno::from_raw_os_error(out))
            } else if res.fd <= 0 {
                Err(Errno::INVAL)
            } else {
                Ok(res.fd)
            }
        }
    }

    unsafe impl rustix::ioctl::Ioctl for ImportSyncFile {
        type Output = ();

        const IS_MUTATING: bool = true;

        fn opcode(&self) -> Opcode {
            opcode::write::<dma_buf_import_sync_file>(b'b', 3)
        }

        fn as_ptr(&mut self) -> *mut c_void {
            &mut self.0 as *mut dma_buf_import_sync_file as _
        }

        unsafe fn output_from_ptr(
            out: rustix::ioctl::IoctlOutput,
            _: *mut c_void,
        ) -> rustix::io::Result<Self::Output> {
            if out == 0 {
                Ok(())
            } else {
                Err(Errno::from_raw_os_error(out))
            }
        }
    }
}

/// Retrieves a dmabuf fence, and uses it to set a semaphore. The semaphore will
/// be triggered when the dmabuf texture is safe to read. Note that the spec
/// insists that the semaphore must be waited on once set this way.
#[instrument(skip_all)]
pub fn import_dmabuf_fence_as_semaphore(
    vk: Arc<VkContext>,
    semaphore: vk::Semaphore,
    fd: impl AsFd,
) -> anyhow::Result<()> {
    let fd = fd.as_fd();
    let sync_fd = unsafe { export_sync_file(fd, ioctl::DMA_BUF_SYNC_READ)? };

    unsafe { import_sync_file_as_semaphore(vk, sync_fd, semaphore) }
}

#[instrument(skip_all)]
pub unsafe fn import_sync_file_as_semaphore(
    vk: Arc<VkContext>,
    fd: OwnedFd,
    semaphore: vk::Semaphore,
) -> anyhow::Result<()> {
    let import_info = vk::ImportSemaphoreFdInfoKHR::default()
        .semaphore(semaphore)
        .handle_type(vk::ExternalSemaphoreHandleTypeFlags::SYNC_FD)
        .flags(vk::SemaphoreImportFlags::TEMPORARY)
        .fd(fd.into_raw_fd()); // Vulkan owns the fd now.

    vk.external_semaphore_api
        .import_semaphore_fd(&import_info)?;

    Ok(())
}

/// Retrieves the fd of a sync file for a dmabuf.
pub unsafe fn export_sync_file(dmabuf: impl AsFd, flags: u32) -> anyhow::Result<OwnedFd> {
    let raw_fd = rustix::ioctl::ioctl(dmabuf, ioctl::ExportSyncFile::new(flags))
        .context("DMA_BUF_IOCTL_EXPORT_SYNC_FILE")?;
    Ok(OwnedFd::from_raw_fd(raw_fd))
}

/// Attaches a sync file to a dmabuf.
// TODO: the kernel docs and online resources state that we need to use this to
// attach a "render finished" semaphore back onto the client buffers once we
// start rendering. I think that's unecessary as long as we wait to call
// `wl_buffer.release` until long after we're done compositing, which we do as
// of this writing.
#[allow(dead_code)]
pub unsafe fn attach_sync_file(
    dmabuf: impl AsFd,
    flags: u32,
    sync_file: OwnedFd, // Closed on return.
) -> anyhow::Result<()> {
    rustix::ioctl::ioctl(
        dmabuf,
        ioctl::ImportSyncFile::new(sync_file.as_raw_fd(), flags),
    )
    .context("DMA_BUF_IOCTL_IMPORT_SYNC_FILE")?;

    Ok(())
}
