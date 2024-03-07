// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use ash::vk;
use hashbrown::HashMap;

use crate::vulkan::*;

#[derive(Debug, Copy, Clone)]
pub struct DpbPicture {
    pub picture_resource_info: vk::VideoPictureResourceInfoKHR<'static>,
    pub index: usize,
    pub currently_active: bool,

    free: bool,
}

/// A DPB pool using one layer for each picture. Guaranteed to be supported,
/// where distinct images are not, but otherwise unoptimal and awkward.
pub struct DpbLayerBuffer {
    pub image: VkImage,
    slots: Vec<DpbPicture>,
    ids: HashMap<u32, usize>,
}

impl DpbLayerBuffer {
    pub fn new(
        vk: Arc<VkContext>,
        format: vk::Format,
        width: u32,
        height: u32,
        profile: &mut vk::VideoProfileInfoKHR,
        size: usize,
    ) -> anyhow::Result<Self> {
        let image = create_dpb_image(vk.clone(), profile, format, width, height, size as u32)?;

        // Each array layer of the image is used as a separate slot, with a
        // one-to-one correspondence between the layer index and the slot index.
        let mut slots = Vec::with_capacity(size);
        for i in 0..size {
            slots.push(DpbPicture {
                picture_resource_info: vk::VideoPictureResourceInfoKHR::default()
                    .image_view_binding(image.view)
                    .coded_extent(vk::Extent2D { width, height })
                    .base_array_layer(i as u32),
                index: i,
                currently_active: false,
                free: true,
            });
        }

        Ok(Self {
            image,
            slots,
            ids: HashMap::new(),
        })
    }

    /// Returns the index of a free slot and the backing picture resource for
    /// it. Note that this does not mark the slot as active, or retain an
    /// association between a picture ID and the slot. After the setup pic is
    /// used in an encode operation, it should be marked as active if the
    /// picture is a reference with `mark_active`.
    pub fn setup_pic(&self) -> DpbPicture {
        for slot in &self.slots {
            if slot.free {
                return *slot;
            }
        }

        panic!("no free slots in the dpb");
    }

    /// Retrieves the picture, along with its slot index, for a picture ID that
    /// was previously passed to `mark_active`.
    pub fn get_pic(&self, id: u32) -> Option<DpbPicture> {
        self.ids.get(&id).map(|&slot| self.slots[slot])
    }

    /// Marks a slot as active, with the picture referenced by `id` stored in
    /// it. Active slots are reserved until marked inactive, and will
    /// not be overwritten.
    ///
    /// The pool maintains a mapping of IDs to slots, so that the slot can be
    /// retrieved by ID. If an ID is reused, the previous slot is automatically
    /// marked as free for re-use.
    pub fn mark_active(&mut self, slot: usize, id: u32) {
        self.slots[slot].currently_active = true;
        self.slots[slot].free = false;
        if let Some(old_slot) = self.ids.insert(id, slot) {
            self.slots[old_slot].free = true;
        }
    }

    /// Mark a slot as inactive. Inactive slots are always considered free.
    pub fn mark_inactive(&mut self, slot: usize) {
        self.slots[slot].currently_active = false;
        self.slots[slot].free = true;
    }

    /// Mark all slots inactive.
    pub fn clear(&mut self) {
        self.ids.clear();
        for slot in &mut self.slots {
            slot.currently_active = false;
            slot.free = true;
        }
    }
}

fn create_dpb_image(
    vk: Arc<VkContext>,
    profile: &mut vk::VideoProfileInfoKHR,
    format: vk::Format,
    width: u32,
    height: u32,
    layers: u32,
) -> anyhow::Result<VkImage> {
    let image = {
        let mut profile_list_info = super::single_profile_list_info(profile);
        let create_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(layers)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::VIDEO_ENCODE_DPB_KHR)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut profile_list_info);

        unsafe { vk.device.create_image(&create_info, None)? }
    };

    let memory = unsafe { bind_memory_for_image(&vk.device, &vk.device_info.memory_props, image)? };

    let view = {
        let create_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D_ARRAY)
            .format(format)
            .components(vk::ComponentMapping {
                r: vk::ComponentSwizzle::IDENTITY,
                g: vk::ComponentSwizzle::IDENTITY,
                b: vk::ComponentSwizzle::IDENTITY,
                a: vk::ComponentSwizzle::IDENTITY,
            })
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: vk::REMAINING_MIP_LEVELS,
                base_array_layer: 0,
                layer_count: vk::REMAINING_ARRAY_LAYERS,
            });

        unsafe { vk.device.create_image_view(&create_info, None)? }
    };

    Ok(VkImage::wrap(
        vk.clone(),
        image,
        view,
        memory,
        format,
        width,
        height,
    ))
}
