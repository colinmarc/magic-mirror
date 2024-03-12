// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::rc::Rc;

use anyhow::anyhow;
use ash::vk;
use hashbrown::{hash_map::Entry, HashMap};
use smithay::{
    backend::allocator::dmabuf,
    reexports::wayland_server::{
        protocol::{wl_buffer, wl_shm, wl_surface},
        Resource,
    },
    wayland::shm,
};
use tracing::{debug, error, trace, warn};

use crate::vulkan::*;

use super::{dma::import_dma_texture, EncodePipeline};

pub struct DmabufCache(
    HashMap<dmabuf::WeakDmabuf, (Rc<VkImage>, dmabuf::Dmabuf, vk::DescriptorSet)>,
);

impl DmabufCache {
    pub fn new() -> Self {
        Self(HashMap::new())
    }

    pub fn get(
        &self,
        dmabuf: &dmabuf::Dmabuf,
    ) -> Option<(Rc<VkImage>, dmabuf::Dmabuf, vk::DescriptorSet)> {
        self.0.get(&dmabuf.weak()).cloned()
    }

    pub fn insert(
        &mut self,
        dmabuf: &dmabuf::Dmabuf,
        image: Rc<VkImage>,
        descriptor_set: vk::DescriptorSet,
    ) -> Option<(Rc<VkImage>, dmabuf::Dmabuf, vk::DescriptorSet)> {
        self.0
            .insert(dmabuf.weak(), (image, dmabuf.clone(), descriptor_set))
    }

    pub fn remove(
        &mut self,
        dmabuf: &dmabuf::Dmabuf,
    ) -> Option<(Rc<VkImage>, dmabuf::Dmabuf, vk::DescriptorSet)> {
        self.0.remove(&dmabuf.weak())
    }

    fn contains_key(&self, buffer: &dmabuf::Dmabuf) -> bool {
        self.0.contains_key(&buffer.weak())
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ShmBufferParameters {
    pub format: vk::Format,
    pub bpp: usize,
    pub width: usize,
    pub height: usize,
    pub stride: usize,
}

/// A texture for a surface.
pub enum SurfaceTexture {
    Uploaded {
        staging_buffer: VkHostBuffer,
        buffer_params: ShmBufferParameters,
        image: VkImage,
        descriptor_set: vk::DescriptorSet,
        dirty: bool,
    },
    Imported {
        dmabuf: dmabuf::Dmabuf,
        buffer: wl_buffer::WlBuffer,
        image: Rc<VkImage>,
        descriptor_set: vk::DescriptorSet,
    },
}

impl EncodePipeline {
    /// Attaches a buffer to a new surface, or updates an existing surface. The
    /// staging buffer and texture images are recreated if the existing ones
    /// aren't compatible with the new buffer.
    pub fn import_and_attach_shm_buffer(
        &mut self,
        surface: &wl_surface::WlSurface,
        buffer: &wl_buffer::WlBuffer,
        contents: &[u8],
        metadata: &shm::BufferData,
        // TODO: buffer transform
    ) -> anyhow::Result<()> {
        trace!(
            surface = ?surface.id().protocol_id(),
            width = metadata.width,
            height = metadata.height,
            "importing shm buffer for surface"
        );

        let (format, ignore_alpha) = match metadata.format {
            wl_shm::Format::Argb8888 => (vk::Format::B8G8R8A8_UNORM, false),
            wl_shm::Format::Xrgb8888 => (vk::Format::B8G8R8A8_UNORM, true),
            _ => {
                error!("unsupported shm format {:?}", metadata.format);
                unimplemented!();
            }
        };

        let bpp = format_bpp(format);

        let params = ShmBufferParameters {
            format,
            bpp,
            width: metadata.width as usize,
            height: metadata.height as usize,
            stride: metadata.stride as usize,
        };

        let existing = self.committed_surfaces.entry(surface.clone());
        if let Entry::Occupied(mut ms) = existing {
            match ms.get_mut() {
                SurfaceTexture::Uploaded {
                    buffer_params,
                    staging_buffer,
                    dirty,
                    ..
                } if &params == buffer_params => {
                    // The existing staging buffer is fine. Update the viewport in
                    // case it changed, then do the copy.
                    *dirty = true;

                    unsafe {
                        copy_shm(staging_buffer, contents);
                    }

                    buffer.release();
                    return Ok(());
                }
                _ => (),
            }

            debug!("recreating staging buffer for surface {:?}", surface);

            // Drop the old texture.
            let tex = ms.remove();
            self.free_surface_texture(tex)?;
        }

        let buffer_size = params.stride * params.height;
        let mut staging_buffer = VkHostBuffer::new(
            self.vk.clone(),
            self.vk.device_info.host_visible_mem_type_index,
            vk::BufferUsageFlags::TRANSFER_SRC,
            buffer_size,
        )?;

        let image = VkImage::new(
            self.vk.clone(),
            params.format,
            ignore_alpha,
            params.width as u32,
            params.height as u32,
            vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED,
            vk::SharingMode::EXCLUSIVE,
            vk::ImageCreateFlags::empty(),
        )?;

        unsafe { copy_shm(&mut staging_buffer, contents) };

        let descriptor_set = self
            .composite_pipeline
            .ds_for_texture(&image, self.descriptor_pool)?;

        self.committed_surfaces.insert(
            surface.clone(),
            SurfaceTexture::Uploaded {
                staging_buffer,
                buffer_params: params,
                image,
                descriptor_set,
                dirty: true,
            },
        );

        // We're done with the buffer.
        buffer.release();

        Ok(())
    }

    pub fn import_dma_buffer(
        &mut self,
        _global: &smithay::wayland::dmabuf::DmabufGlobal,
        buffer: dmabuf::Dmabuf,
    ) -> anyhow::Result<()> {
        if !self.dmabuf_cache.contains_key(&buffer) {
            let texture =
                import_dma_texture(self.vk.clone(), &buffer, vk::ImageUsageFlags::SAMPLED)?;
            let descriptor_set = self
                .composite_pipeline
                .ds_for_texture(&texture, self.descriptor_pool)?;

            self.dmabuf_cache
                .insert(&buffer, Rc::new(texture), descriptor_set);
        }

        Ok(())
    }

    pub fn attach_dma_buffer(
        &mut self,
        surface: &wl_surface::WlSurface,
        buffer: &wl_buffer::WlBuffer,
        dmabuf: dmabuf::Dmabuf,
    ) -> anyhow::Result<()> {
        let (image, _, descriptor_set) = self
            .dmabuf_cache
            .get(&dmabuf)
            .ok_or(anyhow!("dmabuf not imported"))?;

        let old = self.committed_surfaces.insert(
            surface.clone(),
            SurfaceTexture::Imported {
                dmabuf,
                buffer: buffer.clone(),
                image,
                descriptor_set,
            },
        );

        if let Some(tex) = old {
            self.free_surface_texture(tex)?;
        }

        Ok(())
    }

    pub fn remove_surface(&mut self, surface: &wl_surface::WlSurface) -> anyhow::Result<()> {
        if let Some(tex) = self.committed_surfaces.remove(surface) {
            self.free_surface_texture(tex)?;
        }

        Ok(())
    }

    pub fn remove_dmabuf(&mut self, dmabuf: &dmabuf::Dmabuf) -> anyhow::Result<()> {
        let surf = self
            .committed_surfaces
            .iter()
            .find_map(|(surf, tex)| match tex {
                SurfaceTexture::Imported { dmabuf: d, .. } if d.weak() == dmabuf.weak() => {
                    Some(surf.clone())
                }
                _ => None,
            });

        if let Some(surf) = surf {
            // Keeping a reference to the texture seems worse than freeing
            // an in-use surface.
            warn!("destroying buffer for committed surface");
            let tex = self.committed_surfaces.remove(&surf).unwrap();
            self.free_surface_texture(tex)?;
        }

        if self.dmabuf_cache.remove(dmabuf).is_some() {
            use std::os::fd::AsRawFd;
            debug!(
                fd = dmabuf.handles().next().unwrap().as_raw_fd(),
                "destroying dmabuf",
            );
        }

        Ok(())
    }

    fn free_surface_texture(&mut self, tex: SurfaceTexture) -> anyhow::Result<()> {
        match tex {
            SurfaceTexture::Uploaded { descriptor_set, .. } => unsafe {
                // We have to make sure the descriptor set is not in use.
                self.vk
                    .device
                    .queue_wait_idle(self.vk.graphics_queue.queue)?;

                self.vk
                    .device
                    .free_descriptor_sets(self.descriptor_pool, &[descriptor_set])?;
            },
            SurfaceTexture::Imported {
                image,
                dmabuf,
                buffer,
                descriptor_set,
            } => {
                // TODO: is this the right place for this?
                buffer.release();

                // Put the image back in the cache.
                self.dmabuf_cache.insert(&dmabuf, image, descriptor_set);
            }
        }

        Ok(())
    }
}

pub unsafe fn cmd_upload_shm(
    device: &ash::Device,
    cb: vk::CommandBuffer,
    buffer: &VkHostBuffer,
    image: &VkImage,
) {
    let region = vk::BufferImageCopy::default()
        .image_subresource(vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            mip_level: 0,
            base_array_layer: 0,
            layer_count: 1,
        })
        .image_extent(vk::Extent3D {
            width: image.width,
            height: image.height,
            depth: 1,
        });

    let regions = [region];
    device.cmd_copy_buffer_to_image(
        cb,
        buffer.buffer,
        image.image,
        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
        &regions,
    );
}

unsafe fn copy_shm(dst: &mut VkHostBuffer, src: &[u8]) {
    let dst = std::slice::from_raw_parts_mut(dst.access as *mut u8, src.len());
    dst.copy_from_slice(src);
}
