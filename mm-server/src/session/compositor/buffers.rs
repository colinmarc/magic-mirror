// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

mod modifiers;

use std::{
    os::fd::{AsFd, AsRawFd, FromRawFd as _, IntoRawFd as _, OwnedFd},
    sync::{Arc, RwLock},
};

use anyhow::bail;
use ash::vk;
use drm_fourcc::DrmModifier;
use hashbrown::HashSet;
pub use modifiers::*;
use tracing::trace;
use wayland_protocols::wp::linux_drm_syncobj::v1::server::wp_linux_drm_syncobj_timeline_v1;
use wayland_server::{protocol::wl_buffer, Resource as _};

use crate::{
    session::compositor::{shm::Pool, Compositor},
    vulkan::{
        create_image_view, select_memory_type, VkContext, VkHostBuffer, VkImage, VkTimelinePoint,
        VkTimelineSemaphore,
    },
};

slotmap::new_key_type! { pub struct BufferKey; }

pub struct Buffer {
    pub wl_buffer: wl_buffer::WlBuffer,
    pub backing: BufferBacking,

    /// The client is waiting for us to release this buffer.
    pub needs_release: bool,

    /// If set, we should wait on this timeline point before releasing the
    /// buffer.
    pub release_wait: Option<VkTimelinePoint>,

    /// If set, we should signal this timeline point when we're done with
    /// the buffer (instead of using the normal wl_buffer.release signal).
    pub release_signal: Option<VkTimelinePoint>,

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

impl Drop for Buffer {
    fn drop(&mut self) {
        if let BufferBacking::Dmabuf {
            vk, interop_sema, ..
        } = &self.backing
        {
            // This should be safe, since we would've waited on it before
            // releasing the buffer.
            unsafe {
                vk.device.destroy_semaphore(*interop_sema, None);
            }
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

        /// Used for implicit-explicit sync interop, where we use an ioctl to
        /// get an FD and use that to set a binary semaphore.
        interop_sema: vk::Semaphore,
        interop_sema_tripped: bool,

        vk: Arc<VkContext>,
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

slotmap::new_key_type! { pub struct BufferTimelineKey; }

pub struct BufferTimeline {
    pub _wp_syncobj_timeline: wp_linux_drm_syncobj_timeline_v1::WpLinuxDrmSyncobjTimelineV1,
    pub sema: VkTimelineSemaphore,
}

impl Compositor {
    pub fn release_buffers(&mut self) -> anyhow::Result<()> {
        let mut used_buffers = HashSet::new();
        used_buffers.extend(
            self.surfaces
                .iter()
                .flat_map(|(_, s)| s.content.as_ref())
                .map(|c| c.buffer),
        );

        let mut to_destroy = HashSet::new();
        for (id, buffer) in self.buffers.iter_mut().filter(|(_, b)| b.needs_release) {
            if used_buffers.contains(&id) {
                continue;
            }

            if let Some(tp) = &buffer.release_wait {
                if unsafe { !tp.poll()? } {
                    continue;
                }
            }

            trace!(
                wl_buffer = buffer.wl_buffer.id().protocol_id(),
                "releasing buffer"
            );

            if let Some(tp) = &buffer.release_signal.take() {
                unsafe {
                    tp.signal()?;
                }
            } else {
                buffer.wl_buffer.release();
            }

            buffer.needs_release = false;
            buffer.release_wait = None;
            if buffer.needs_destruction {
                to_destroy.insert(id);
            }
        }

        for id in to_destroy {
            let buf = self.buffers.remove(id).unwrap();
            assert!(!buf.wl_buffer.is_alive());

            trace!(
                wl_buffer = buf.wl_buffer.id().protocol_id(),
                "destroying buffer"
            );
        }

        Ok(())
    }
}

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
        needs_release: false,
        release_wait: None,
        release_signal: None,
        needs_destruction: false,
    })
}

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

    let interop_sema = unsafe {
        vk.device
            .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?
    };

    Ok(Buffer {
        wl_buffer,
        backing: BufferBacking::Dmabuf {
            format,
            fd,
            image,
            interop_sema,
            interop_sema_tripped: false,

            vk,
        },
        needs_release: false,
        release_wait: None,
        release_signal: None,
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

    use rustix::{io::Errno, ioctl::Opcode};

    pub(super) const DMA_BUF_SYNC_READ: u32 = 1 << 0;
    pub(super) const DMA_BUF_SYNC_WRITE: u32 = 1 << 1;

    // Opcode::write::<dma_buf_import_sync_file>(b'b', 3);

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

        const OPCODE: Opcode = Opcode::read_write::<dma_buf_export_sync_file>(b'b', 2);
        const IS_MUTATING: bool = true;

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

        const OPCODE: Opcode = Opcode::write::<dma_buf_import_sync_file>(b'b', 3);
        const IS_MUTATING: bool = true;

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
pub fn import_dmabuf_fence_as_semaphore(
    vk: Arc<VkContext>,
    semaphore: vk::Semaphore,
    fd: impl AsFd,
) -> anyhow::Result<()> {
    let fd = fd.as_fd();
    let sync_fd = unsafe { export_sync_file(fd, ioctl::DMA_BUF_SYNC_READ)? };

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
pub unsafe fn export_sync_file(dmabuf: impl AsFd, flags: u32) -> anyhow::Result<OwnedFd> {
    let raw_fd = rustix::ioctl::ioctl(dmabuf, ioctl::ExportSyncFile::new(flags))?;
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
    )?;

    Ok(())
}
