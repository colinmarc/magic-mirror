// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::sync::{Arc, RwLock};

use ash::vk;
use slotmap::SecondaryMap;
use wayland_server::protocol::wl_buffer;

use crate::vulkan::{VkContext, VkHostBuffer, VkImage};

use super::shm::Pool;

slotmap::new_key_type! { pub struct BufferKey; }

pub struct Buffer {
    pub wl_buffer: wl_buffer::WlBuffer,
    pub backing: BufferBacking,

    /// The client is waiting for us to release this buffer.
    pub needs_release: bool,

    /// Next time we release this buffer, we should destroy it as well.
    pub needs_destruction: bool,
}

impl Buffer {
    pub fn dimensions(&self) -> glam::UVec2 {
        match self.backing {
            BufferBacking::Shm { format, .. } => (format.width, format.height).into(),
        }
    }
}

pub enum BufferBacking {
    Shm {
        format: PlaneMetadata,
        pool: Arc<RwLock<Pool>>,
        offset: u32,
    },
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct PlaneMetadata {
    pub format: drm_fourcc::DrmFourcc,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
}

pub enum Texture {
    Shm {
        staging_buffer: VkHostBuffer,
        format: PlaneMetadata,
        image: VkImage,
        /// Indicates that staging_buffer has been written to and needs to
        /// be uploaded to the image.
        dirty: bool,
    },
}

pub fn import_buffer(
    vk: Arc<VkContext>,
    id: BufferKey,
    buffer: &mut Buffer,
    cache: &mut SecondaryMap<BufferKey, Texture>,
) -> anyhow::Result<()> {
    let texture = cache.get_mut(id);

    match &buffer.backing {
        BufferBacking::Shm {
            format,
            pool,
            offset,
            ..
        } => {
            let len = (format.stride * format.height) as usize;
            let pool = pool.read().unwrap();
            let contents = pool.data(*offset as usize, len);

            // Check if we already imported this content update, or if
            // we previously created a texture for which all the
            // parameters match.
            match texture {
                Some(Texture::Shm {
                    ref mut staging_buffer,
                    format: dst_format,
                    dirty,
                    ..
                }) if format == dst_format => {
                    // Everything matches, just do a copy.
                    staging_buffer.copy_from_slice(contents);
                    *dirty = true;
                }
                _ => {
                    cache.insert(id, create_shm_texture(vk.clone(), *format, contents)?);
                }
            }

            // We just copied the contents, so we can already release the buffer.
            if buffer.needs_release {
                buffer.needs_release = false;
                buffer.wl_buffer.release();
            }
        }
    }

    Ok(())
}
fn create_shm_texture(
    vk: Arc<VkContext>,
    format: PlaneMetadata,
    contents: &[u8],
) -> anyhow::Result<Texture> {
    let (vk_format, ignore_alpha) = match format.format {
        drm_fourcc::DrmFourcc::Argb8888 => (vk::Format::B8G8R8A8_UNORM, false),
        drm_fourcc::DrmFourcc::Xrgb8888 => (vk::Format::B8G8R8A8_UNORM, true),
        _ => unreachable!(),
    };

    let mut staging_buffer = VkHostBuffer::new(
        vk.clone(),
        vk.device_info.host_visible_mem_type_index,
        vk::BufferUsageFlags::TRANSFER_SRC,
        contents.len(),
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

    staging_buffer.copy_from_slice(contents);

    Ok(Texture::Shm {
        staging_buffer,
        image,
        format,
        dirty: true,
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
