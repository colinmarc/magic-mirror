// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::too_many_arguments)]

mod chain;
pub(crate) use chain::*;

pub mod video;

use cstr::cstr;
use nix::libc;
use std::ffi::{c_void, CStr, CString};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use ash::extensions::{ext, khr::ExternalMemoryFd as ExternalMemoryFdExt};
use ash::vk;
use tracing::{debug, error, info, warn};

use self::video::{VideoEncodeQueueExt, VideoQueueExt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vendor {
    Amd,
    Nvidia,
    Other,
}

pub struct VkContext {
    pub entry: ash::Entry,
    pub external_mem_loader: ExternalMemoryFdExt,
    pub video_loaders: Option<(VideoQueueExt, VideoEncodeQueueExt)>,

    pub instance: ash::Instance,
    pub debug: Option<VkDebugContext>,
    pub device: ash::Device,
    pub device_info: VkDeviceInfo,
    pub graphics_queue: VkQueue,
    pub encode_queue: Option<VkQueue>,
}

pub struct VkDebugContext {
    debug: ext::DebugUtils,
    messenger: vk::DebugUtilsMessengerEXT,
}

#[derive(Clone)]
pub struct VkQueue {
    pub family: u32,
    pub queue: vk::Queue,
    pub command_pool: vk::CommandPool, // TODO: synchronize access

    #[allow(unused)]
    pub tracy_context: Option<tracy_client::GpuContext>,
}

impl VkQueue {
    pub fn new(
        device: &ash::Device,
        _pdevice: &VkDeviceInfo,
        family: u32,
        _name: &str,
    ) -> Result<Self> {
        let queue = unsafe { device.get_device_queue(family, 0) };

        let command_pool = unsafe {
            let create_info = vk::CommandPoolCreateInfo::default()
                .queue_family_index(family)
                .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);

            device.create_command_pool(&create_info, None)?
        };

        #[cfg(feature = "tracy")]
        let tracy_context = tracy_client::Client::running().and_then(|client| {
            match init_tracy_context(device, _pdevice, queue, command_pool, client, _name) {
                Ok(ctx) => Some(ctx),
                Err(err) => {
                    error!("failed to initialize tracy GPU context: {err}");
                    None
                }
            }
        });

        #[cfg(not(feature = "tracy"))]
        let tracy_context = None;

        Ok(Self {
            family,
            queue,
            command_pool,
            tracy_context,
        })
    }
}

pub struct VkDeviceInfo {
    pub pdevice: vk::PhysicalDevice,
    pub device_name: CString,
    pub device_type: vk::PhysicalDeviceType,
    pub device_vendor: Vendor,
    pub limits: vk::PhysicalDeviceLimits,
    pub drm_node: nix::libc::dev_t,
    pub graphics_family: u32,
    pub encode_family: Option<u32>,
    pub supports_h264: bool,
    pub supports_h265: bool,
    pub supports_av1: bool,
    pub memory_props: vk::PhysicalDeviceMemoryProperties,
    pub host_visible_mem_type_index: u32,
    pub host_mem_is_cached: bool,
    pub selected_extensions: Vec<CString>,
}

impl VkDeviceInfo {
    fn query(instance: &ash::Instance, device: vk::PhysicalDevice) -> Result<Self> {
        let mut drm_props = vk::PhysicalDeviceDrmPropertiesEXT::default();
        let mut host_mem_props = vk::PhysicalDeviceExternalMemoryHostPropertiesEXT::default();
        let mut props = vk::PhysicalDeviceProperties2::default()
            .push_next(&mut drm_props)
            .push_next(&mut host_mem_props);
        unsafe { instance.get_physical_device_properties2(device, &mut props) };

        let limits = props.properties.limits;
        let device_type = props.properties.device_type;
        let device_name =
            unsafe { CStr::from_ptr(props.properties.device_name.as_ptr()).to_owned() };
        let device_vendor = match props.properties.vendor_id {
            0x1002 => Vendor::Amd,
            0x10de => Vendor::Nvidia,
            _ => Vendor::Other,
        };

        if drm_props.render_major != 226 || drm_props.render_minor < 128 {
            bail!("device {:?} is not a render node", device_name);
        }

        let drm_node = libc::makedev(drm_props.render_major as u32, drm_props.render_minor as u32);

        let queue_families = unsafe {
            instance
                .get_physical_device_queue_family_properties(device)
                .into_iter()
                .collect::<Vec<_>>()
        };

        let graphics_family = queue_families
            .iter()
            .enumerate()
            .find(|(_, properties)| {
                properties.queue_flags.contains(vk::QueueFlags::GRAPHICS)
                    && properties.queue_flags.contains(vk::QueueFlags::COMPUTE)
            })
            .map(|(index, _)| index as u32)
            .to_owned()
            .ok_or_else(|| anyhow::anyhow!("no graphics queue found"))?;

        let encode_family = queue_families
            .iter()
            .enumerate()
            .find(|(_, properties)| {
                properties
                    .queue_flags
                    .contains(vk::QueueFlags::VIDEO_ENCODE_KHR)
            })
            .map(|(index, _)| index as u32);

        let available_extensions = unsafe {
            instance
                .enumerate_device_extension_properties(device)
                .unwrap()
                .into_iter()
                .map(|properties| CStr::from_ptr(&properties.extension_name as *const _).to_owned())
                .collect::<Vec<_>>()
        };

        let mut selected_extensions = vec![
            vk::KhrExternalMemoryFdFn::NAME.to_owned(),
            vk::ExtExternalMemoryDmaBufFn::NAME.to_owned(),
            vk::ExtImageDrmFormatModifierFn::NAME.to_owned(),
            vk::ExtPhysicalDeviceDrmFn::NAME.to_owned(),
        ];

        for ext in selected_extensions.iter() {
            if !contains_extension(&available_extensions, ext) {
                return Err(anyhow::anyhow!("extension {:?} not available", ext));
            }
        }

        let ext_video_queue = vk::KhrVideoQueueFn::NAME;
        let ext_video_encode_queue = vk::KhrVideoEncodeQueueFn::NAME;

        // TODO: ash hasn't picked up the promoted names yet.
        let ext_h264 = cstr!("VK_KHR_video_encode_h264");
        let ext_h265 = cstr!("VK_KHR_video_encode_h265");

        // This doesn't exist yet.
        let ext_av1 = cstr!("VK_EXT_video_encode_av1");

        let mut supports_h264 = false;
        let mut supports_h265 = false;
        let mut supports_av1 = false;
        if encode_family.is_some()
            && contains_extension(&available_extensions, ext_video_queue)
            && contains_extension(&available_extensions, ext_video_encode_queue)
        {
            selected_extensions.push(ext_video_encode_queue.to_owned());
            selected_extensions.push(ext_video_queue.to_owned());

            if contains_extension(&available_extensions, ext_h264) {
                supports_h264 = true;
                selected_extensions.push(ext_h264.to_owned());
            }

            if contains_extension(&available_extensions, ext_h265) {
                supports_h265 = true;
                selected_extensions.push(ext_h265.to_owned());
            }
            if contains_extension(&available_extensions, ext_av1) {
                supports_av1 = true;
                selected_extensions.push(ext_av1.to_owned());
            }
        }

        // We want HOST_CACHED | HOST_COHERENT, but we can make do with just HOST_VISIBLE.
        let memory_props = unsafe { instance.get_physical_device_memory_properties(device) };

        let (host_visible_mem_type_index, host_mem_is_cached) = {
            let mut cached = true;
            let mut idx = select_memory_type(
                &memory_props,
                vk::MemoryPropertyFlags::HOST_VISIBLE
                    | vk::MemoryPropertyFlags::HOST_CACHED
                    | vk::MemoryPropertyFlags::HOST_COHERENT,
                None,
            );

            if idx.is_none() {
                idx = select_memory_type(
                    &memory_props,
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                    None,
                );

                if idx.is_none() {
                    bail!("no host visible memory type found");
                }

                cached = false;
            }

            (idx.unwrap(), cached)
        };

        Ok(Self {
            pdevice: device,
            device_name,
            device_type,
            device_vendor,
            limits,
            drm_node,
            graphics_family,
            encode_family,
            supports_h264,
            supports_h265,
            supports_av1,
            memory_props,
            host_visible_mem_type_index,
            host_mem_is_cached,
            selected_extensions,
        })
    }
}

impl VkContext {
    pub fn new(enable_debug: bool) -> Result<Self> {
        let entry = unsafe { ash::Entry::load().context("failed to load vulkan libraries!") }?;
        debug!("creating vulkan instance");

        let (major, minor) = match unsafe { entry.try_enumerate_instance_version()? } {
            // Vulkan 1.1+
            Some(version) => (
                vk::api_version_major(version),
                vk::api_version_minor(version),
            ),
            // Vulkan 1.0
            None => (1, 0),
        };

        if major < 1 || (major == 1 && minor < 3) {
            return Err(anyhow::anyhow!("vulkan 1.3 or higher is required"));
        }

        let app_info = vk::ApplicationInfo::default()
            .application_name(cstr!("Magic Mirror"))
            .application_version(vk::make_api_version(0, 0, 1, 0))
            .engine_name(cstr!("No Engine"))
            .engine_version(vk::make_api_version(0, 0, 1, 0))
            .api_version(vk::make_api_version(0, major, minor, 0));

        let available_extensions = unsafe {
            entry
                .enumerate_instance_extension_properties(None)?
                .into_iter()
                .map(|properties| CStr::from_ptr(&properties.extension_name as *const _).to_owned())
                .collect::<Vec<_>>()
        };

        let mut extensions = Vec::new();
        let mut layers = Vec::new();

        if enable_debug {
            if !available_extensions
                .iter()
                .any(|ext| ext.as_c_str() == ext::DebugUtils::NAME)
            {
                return Err(anyhow::anyhow!(
                    "debug utils extension requested, but not available"
                ));
            }

            warn!("vulkan debug tooling enabled");
            extensions.push(ext::DebugUtils::NAME.as_ptr());

            unsafe {
                let validation_layer = cstr!("VK_LAYER_KHRONOS_validation");
                if entry
                    .enumerate_instance_layer_properties()?
                    .into_iter()
                    .map(|properties| CStr::from_ptr(&properties.layer_name as *const _))
                    .any(|layer| layer == validation_layer)
                {
                    layers.push(validation_layer.as_ptr());
                } else {
                    warn!("validation layers requested, but not available!")
                }
            }
        }

        let instance_create_info = vk::InstanceCreateInfo::default()
            .application_info(&app_info)
            .enabled_layer_names(&layers)
            .enabled_extension_names(&extensions);

        let instance = unsafe { entry.create_instance(&instance_create_info, None)? };

        // Enable validation layers and a debugging callback, if requested.
        let debug_utils = if enable_debug {
            let debug_utils = ext::DebugUtils::new(&entry, &instance);

            let create_info = vk::DebugUtilsMessengerCreateInfoEXT::default()
                .message_severity(
                    vk::DebugUtilsMessageSeverityFlagsEXT::WARNING
                        | vk::DebugUtilsMessageSeverityFlagsEXT::VERBOSE
                        | vk::DebugUtilsMessageSeverityFlagsEXT::INFO
                        | vk::DebugUtilsMessageSeverityFlagsEXT::ERROR,
                )
                .message_type(
                    vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                        | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE
                        | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION,
                )
                .pfn_user_callback(Some(vulkan_debug_utils_callback));

            let messenger =
                unsafe { debug_utils.create_debug_utils_messenger(&create_info, None) }?;

            Some(VkDebugContext {
                debug: debug_utils,
                messenger,
            })
        } else {
            None
        };

        // Select a device based on encoding support.
        let devices = unsafe { instance.enumerate_physical_devices()? };
        let mut devices = devices
            .into_iter()
            .enumerate()
            .flat_map(|(index, dev)| match VkDeviceInfo::query(&instance, dev) {
                Ok(device) => Some((index as u32, device)),
                Err(err) => {
                    let device_name = unsafe {
                        CStr::from_ptr(
                            instance
                                .get_physical_device_properties(dev)
                                .device_name
                                .as_ptr(),
                        )
                        .to_owned()
                    };

                    info!("gpu {device_name:?} ineligible: {err}");
                    None
                }
            })
            .collect::<Vec<_>>();

        if devices.is_empty() {
            return Err(anyhow::anyhow!("no suitable gpu found"));
        }

        devices.sort_by_key(|(_, dev)| {
            let mut score = match dev.device_type {
                vk::PhysicalDeviceType::DISCRETE_GPU => 0,
                vk::PhysicalDeviceType::INTEGRATED_GPU => 10,
                _ => 20,
            };

            score += dev.encode_family.is_none() as u32;
            score += !dev.supports_h264 as u32;
            score += !dev.supports_h265 as u32;
            score += !dev.supports_av1 as u32;
            score
        });

        let (index, device_info) = devices.remove(0);
        info!("selected gpu: {:?} ({index})", device_info.device_name);

        let device = {
            let queue_priorities = &[1.0];
            let mut queue_indices = Vec::new();
            queue_indices.push(device_info.graphics_family);
            if let Some(idx) = device_info.encode_family {
                queue_indices.push(idx);
            }

            queue_indices.dedup();
            let queue_create_infos = queue_indices
                .iter()
                .map(|&index| {
                    vk::DeviceQueueCreateInfo::default()
                        .queue_family_index(index)
                        .queue_priorities(queue_priorities)
                })
                .collect::<Vec<_>>();

            let mut enabled_1_1_features =
                vk::PhysicalDeviceVulkan11Features::default().sampler_ycbcr_conversion(true);

            let mut enabled_1_2_features = vk::PhysicalDeviceVulkan12Features::default()
                .timeline_semaphore(true)
                .host_query_reset(true);

            let mut enabled_1_3_features = vk::PhysicalDeviceVulkan13Features::default()
                .dynamic_rendering(true)
                .synchronization2(true);

            let extension_names = device_info
                .selected_extensions
                .iter()
                .map(|v| v.as_c_str().as_ptr())
                .collect::<Vec<_>>();
            let device_create_info = vk::DeviceCreateInfo::default()
                .queue_create_infos(&queue_create_infos)
                .enabled_extension_names(&extension_names)
                .push_next(&mut enabled_1_1_features)
                .push_next(&mut enabled_1_2_features)
                .push_next(&mut enabled_1_3_features);

            unsafe { instance.create_device(device_info.pdevice, &device_create_info, None)? }
        };

        let graphics_queue = VkQueue::new(
            &device,
            &device_info,
            device_info.graphics_family,
            "graphics",
        )?;
        let mut encode_queue = None;
        if device_info.encode_family.is_some() {
            info!(
                "hardware encoding support: (h264: {}, h265: {}, av1: {})",
                device_info.supports_h264, device_info.supports_h265, device_info.supports_av1
            );

            encode_queue = Some(VkQueue::new(
                &device,
                &device_info,
                device_info.encode_family.unwrap(),
                "encode",
            )?);
        } else {
            warn!("no hardware encoding support found!")
        }

        if !device_info.host_mem_is_cached {
            warn!("no cache-coherent memory type found on device!");
        }

        let external_mem_loader = ExternalMemoryFdExt::new(&instance, &device);

        let video_loaders = if device_info.encode_family.is_some() {
            let video_queue = VideoQueueExt::new(&entry, &instance, &device);
            let video_encode_queue = VideoEncodeQueueExt::new(&instance, &device);

            Some((video_queue, video_encode_queue))
        } else {
            None
        };

        Ok(Self {
            entry,
            external_mem_loader,
            video_loaders,
            instance,
            device,
            device_info,
            graphics_queue,
            encode_queue,
            debug: debug_utils,
        })
    }
}

unsafe extern "system" fn vulkan_debug_utils_callback(
    message_severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    message_type: vk::DebugUtilsMessageTypeFlagsEXT,
    p_callback_data: *const vk::DebugUtilsMessengerCallbackDataEXT,
    _userdata: *mut c_void,
) -> vk::Bool32 {
    let _ = std::panic::catch_unwind(|| {
        let message = unsafe { CStr::from_ptr((*p_callback_data).p_message) }.to_string_lossy();
        let ty = format!("{:?}", message_type).to_lowercase();

        // TODO: these should all be debug.
        match message_severity {
            vk::DebugUtilsMessageSeverityFlagsEXT::VERBOSE => {
                tracing::trace!(ty, "{}", message)
            }
            vk::DebugUtilsMessageSeverityFlagsEXT::INFO => info!(ty, "{}", message),
            vk::DebugUtilsMessageSeverityFlagsEXT::WARNING => warn!(ty, "{}", message),
            vk::DebugUtilsMessageSeverityFlagsEXT::ERROR => error!(ty, "{}", message),
            _ => (),
        }
    });

    // Must always return false.
    vk::FALSE
}

impl Drop for VkContext {
    fn drop(&mut self) {
        debug!("destroying vulkan instance");

        unsafe {
            if let Some(debug) = self.debug.as_ref() {
                debug
                    .debug
                    .destroy_debug_utils_messenger(debug.messenger, None);
            }

            self.device
                .destroy_command_pool(self.graphics_queue.command_pool, None);

            if let Some(encode_queue) = self.encode_queue.as_ref() {
                self.device
                    .destroy_command_pool(encode_queue.command_pool, None);
            }

            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

#[cfg(feature = "tracy")]
fn init_tracy_context(
    device: &ash::Device,
    pdevice: &VkDeviceInfo,
    queue: vk::Queue,
    command_pool: vk::CommandPool,
    client: tracy_client::Client,
    name: &str,
) -> anyhow::Result<tracy_client::GpuContext> {
    if pdevice.device_vendor == Vendor::Amd && name.contains("encode") {
        bail!("can't calibrate timestamp on encode queue on radv")
    }

    // Query the timestamp once to calibrate the clocks.
    let cb = allocate_command_buffer(device, command_pool)?;

    unsafe {
        device.reset_command_buffer(cb, vk::CommandBufferResetFlags::empty())?;

        let query_pool = create_timestamp_query_pool(device, 1)?;
        let fence = create_fence(device, false)?;

        // Begin the command buffer.
        device.begin_command_buffer(
            cb,
            &vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
        )?;

        // Write a timestamp.
        query_pool.cmd_reset(device, cb);
        device.cmd_write_timestamp(
            cb,
            vk::PipelineStageFlags::BOTTOM_OF_PIPE,
            query_pool.pool,
            0,
        );

        // Submit.
        device.end_command_buffer(cb)?;

        device.queue_submit(
            queue,
            &[vk::SubmitInfo::default().command_buffers(&[cb])],
            fence,
        )?;

        // Wait for the fence, fetch the timestamp.
        device.wait_for_fences(&[fence], true, u64::MAX)?;
        let ts = query_pool.fetch_results(device)?[0];

        let context = client.new_gpu_context(
            Some(name),
            tracy_client::GpuContextType::Vulkan,
            ts as i64,
            pdevice.limits.timestamp_period,
        )?;

        // Cleanup.
        device.free_command_buffers(command_pool, &[cb]);
        device.destroy_fence(fence, None);
        device.destroy_query_pool(query_pool.pool, None);

        Ok(context)
    }
}

pub fn select_memory_type(
    props: &vk::PhysicalDeviceMemoryProperties,
    flags: vk::MemoryPropertyFlags,
    memory_type_bits: Option<u32>,
) -> Option<u32> {
    for i in 0..props.memory_type_count {
        if let Some(mask) = memory_type_bits {
            if mask & (1 << i) == 0 {
                continue;
            }
        }

        if flags.is_empty()
            || props.memory_types[i as usize]
                .property_flags
                .contains(flags)
        {
            return Some(i);
        }
    }

    None
}

pub struct VkImage {
    pub image: vk::Image,
    pub view: vk::ImageView,
    pub memory: vk::DeviceMemory,
    pub format: vk::Format,
    pub width: u32,
    pub height: u32,
    vk: Arc<VkContext>,
}

impl VkImage {
    pub fn new(
        vk: Arc<VkContext>,
        format: vk::Format,
        ignore_alpha: bool,
        width: u32,
        height: u32,
        usage: vk::ImageUsageFlags,
        sharing_mode: vk::SharingMode,
        flags: vk::ImageCreateFlags,
    ) -> anyhow::Result<Self> {
        let image = {
            let create_info = vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(format)
                .extent(vk::Extent3D {
                    width,
                    height,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(usage)
                .sharing_mode(sharing_mode)
                .initial_layout(vk::ImageLayout::UNDEFINED)
                .flags(flags);

            unsafe {
                vk.device
                    .create_image(&create_info, None)
                    .context("VkCreateImage")?
            }
        };

        let memory =
            unsafe { bind_memory_for_image(&vk.device, &vk.device_info.memory_props, image)? };

        let view = unsafe { create_image_view(&vk.device, image, format, ignore_alpha, None)? };

        Ok(Self {
            image,
            view,
            memory,
            format,
            width,
            height,
            vk,
        })
    }

    pub fn wrap(
        vk: Arc<VkContext>,
        image: vk::Image,
        view: vk::ImageView,
        memory: vk::DeviceMemory,
        format: vk::Format,
        width: u32,
        height: u32,
    ) -> Self {
        Self {
            image,
            view,
            memory,
            format,
            width,
            height,
            vk,
        }
    }

    pub fn extent(&self) -> vk::Extent2D {
        vk::Extent2D {
            width: self.width,
            height: self.height,
        }
    }

    pub fn rect(&self) -> vk::Rect2D {
        vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: self.extent(),
        }
    }
}

impl Drop for VkImage {
    fn drop(&mut self) {
        unsafe {
            self.vk.device.destroy_image_view(self.view, None);
            self.vk.device.destroy_image(self.image, None);
            self.vk.device.free_memory(self.memory, None);
        }
    }
}

pub unsafe fn bind_memory_for_image(
    device: &ash::Device,
    props: &vk::PhysicalDeviceMemoryProperties,
    image: vk::Image,
) -> anyhow::Result<vk::DeviceMemory> {
    let image_memory_req = unsafe { device.get_image_memory_requirements(image) };

    let mem_type_index = select_memory_type(
        props,
        vk::MemoryPropertyFlags::DEVICE_LOCAL,
        Some(image_memory_req.memory_type_bits),
    );

    if mem_type_index.is_none() {
        bail!(
            "no appropriate memory type found for reqs: {:?}",
            image_memory_req
        );
    }

    let memory = {
        let image_allocate_info = vk::MemoryAllocateInfo::default()
            .allocation_size(image_memory_req.size)
            .memory_type_index(mem_type_index.unwrap());

        unsafe {
            device
                .allocate_memory(&image_allocate_info, None)
                .context("VkAllocateMemory")?
        }
    };

    unsafe {
        device
            .bind_image_memory(image, memory, 0)
            .context("VkBindImageMemory")?;
    }

    Ok(memory)
}

pub unsafe fn create_image_view(
    device: &ash::Device,
    image: vk::Image,
    format: vk::Format,
    ignore_alpha: bool,
    sampler_conversion: Option<vk::SamplerYcbcrConversion>,
) -> anyhow::Result<vk::ImageView> {
    let alpha_swizzle = if ignore_alpha {
        vk::ComponentSwizzle::ONE
    } else {
        vk::ComponentSwizzle::IDENTITY
    };

    let mut create_info = vk::ImageViewCreateInfo::default()
        .image(image)
        .view_type(vk::ImageViewType::TYPE_2D)
        .format(format)
        .components(vk::ComponentMapping {
            r: vk::ComponentSwizzle::IDENTITY,
            g: vk::ComponentSwizzle::IDENTITY,
            b: vk::ComponentSwizzle::IDENTITY,
            a: alpha_swizzle,
        })
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: vk::REMAINING_MIP_LEVELS,
            base_array_layer: 0,
            layer_count: vk::REMAINING_ARRAY_LAYERS,
        });

    let mut sampler_conversion_info;
    if let Some(sampler_conversion) = sampler_conversion {
        sampler_conversion_info =
            vk::SamplerYcbcrConversionInfo::default().conversion(sampler_conversion);
        create_info = create_info.push_next(&mut sampler_conversion_info);
    }

    device
        .create_image_view(&create_info, None)
        .context("VkCreateImageView")
}

pub struct VkHostBuffer {
    pub buffer: vk::Buffer,
    pub memory: vk::DeviceMemory,
    pub access: *mut c_void,
    pub size: usize,
    vk: Arc<VkContext>,
}

impl VkHostBuffer {
    pub fn new(
        vk: Arc<VkContext>,
        mem_type: u32,
        usage: vk::BufferUsageFlags,
        size: usize,
    ) -> anyhow::Result<Self> {
        let buffer = {
            let create_info = vk::BufferCreateInfo::default()
                .size(size as u64)
                .usage(usage)
                .sharing_mode(vk::SharingMode::EXCLUSIVE);

            unsafe {
                vk.device
                    .create_buffer(&create_info, None)
                    .context("VkCreateBuffer")?
            }
        };

        let requirements = unsafe { vk.device.get_buffer_memory_requirements(buffer) };

        let alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(requirements.size)
            .memory_type_index(mem_type);

        let memory = unsafe {
            vk.device
                .allocate_memory(&alloc_info, None)
                .context("VkAllocateMemory")?
        };

        unsafe {
            vk.device
                .bind_buffer_memory(buffer, memory, 0)
                .context("vkBindBufferMemory")?
        };

        let access = {
            unsafe {
                vk.device
                    .map_memory(memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty())
                    .context("VkMapMemory")?
            }
        };

        Ok(VkHostBuffer {
            buffer,
            memory,
            access,
            size,
            vk,
        })
    }

    pub(crate) fn wrap(
        vk: Arc<VkContext>,
        buf: vk::Buffer,
        memory: vk::DeviceMemory,
        buffer_size: usize,
    ) -> Self {
        let access = unsafe {
            vk.device
                .map_memory(memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty())
                .context("failed to map buffer memory")
                .unwrap()
        };

        Self {
            buffer: buf,
            memory,
            access,
            size: buffer_size,
            vk,
        }
    }
}

impl Drop for VkHostBuffer {
    fn drop(&mut self) {
        unsafe {
            self.vk.device.unmap_memory(self.memory);
            self.vk.device.destroy_buffer(self.buffer, None);
            self.vk.device.free_memory(self.memory, None);
        }
    }
}

pub struct VkTimestampQueryPool {
    pub pool: vk::QueryPool,
    num_timestamps: u32,
}

impl VkTimestampQueryPool {
    pub unsafe fn cmd_reset(&self, device: &ash::Device, command_buffer: vk::CommandBuffer) {
        device.cmd_reset_query_pool(command_buffer, self.pool, 0, self.num_timestamps);
    }

    pub fn fetch_results(&self, device: &ash::Device) -> anyhow::Result<Vec<i64>> {
        let mut results = vec![0_i64; self.num_timestamps as usize];
        unsafe {
            device
                .get_query_pool_results(self.pool, 0, &mut results, vk::QueryResultFlags::WAIT)
                .context("vkGetQueryPoolResults")?;
        }

        for v in &results {
            assert!(v > &0_i64, "invalid query pool results")
        }

        Ok(results)
    }
}

pub fn create_timestamp_query_pool(
    device: &ash::Device,
    num_timestamps: u32,
) -> anyhow::Result<VkTimestampQueryPool> {
    let create_info = vk::QueryPoolCreateInfo::default()
        .query_type(vk::QueryType::TIMESTAMP)
        .query_count(num_timestamps);

    let pool = unsafe {
        device
            .create_query_pool(&create_info, None)
            .context("vkCreateQueryPool")?
    };

    Ok(VkTimestampQueryPool {
        pool,
        num_timestamps,
    })
}

pub fn create_timeline_semaphore(
    device: &ash::Device,
    initial_value: u64,
) -> anyhow::Result<vk::Semaphore> {
    let sema = unsafe {
        device.create_semaphore(
            &vk::SemaphoreCreateInfo::default().push_next(
                &mut vk::SemaphoreTypeCreateInfo::default()
                    .semaphore_type(vk::SemaphoreType::TIMELINE)
                    .initial_value(initial_value),
            ),
            None,
        )?
    };

    Ok(sema)
}

pub fn create_fence(device: &ash::Device, signalled: bool) -> anyhow::Result<vk::Fence> {
    let mut create_info = vk::FenceCreateInfo::default();
    if signalled {
        create_info = create_info.flags(vk::FenceCreateFlags::SIGNALED);
    }

    let fence = unsafe { device.create_fence(&create_info, None)? };

    Ok(fence)
}

pub fn load_shader(device: &ash::Device, bytes: &[u8]) -> anyhow::Result<vk::ShaderModule> {
    let code = ash::util::read_spv(&mut std::io::Cursor::new(bytes))?;
    let create_info = vk::ShaderModuleCreateInfo::default().code(&code);

    let shader = unsafe { device.create_shader_module(&create_info, None)? };

    Ok(shader)
}

pub fn allocate_command_buffer(
    device: &ash::Device,
    pool: vk::CommandPool,
) -> anyhow::Result<vk::CommandBuffer> {
    let create_info = vk::CommandBufferAllocateInfo::default()
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_pool(pool)
        .command_buffer_count(1);

    let cb = unsafe {
        device
            .allocate_command_buffers(&create_info)
            .context("failed to allocate render command buffer")?
            .pop()
            .unwrap()
    };

    Ok(cb)
}

pub fn format_bpp(format: vk::Format) -> usize {
    match format {
        vk::Format::R8G8B8A8_UNORM => 4,
        vk::Format::B8G8R8A8_UNORM => 4,
        _ => unimplemented!(),
    }
}

pub fn insert_image_barrier(
    device: &ash::Device,
    cb: vk::CommandBuffer,
    image: vk::Image,
    queue_transfer: Option<(u32, u32)>,
    old_layout: vk::ImageLayout,
    new_layout: vk::ImageLayout,
    src_stage: vk::PipelineStageFlags2,
    src_access: vk::AccessFlags2,
    dst_stage: vk::PipelineStageFlags2,
    dst_access: vk::AccessFlags2,
) {
    let (src_family, dst_family) =
        queue_transfer.unwrap_or((vk::QUEUE_FAMILY_IGNORED, vk::QUEUE_FAMILY_IGNORED));

    let barriers = [vk::ImageMemoryBarrier2::default()
        .src_stage_mask(src_stage)
        .src_access_mask(src_access)
        .dst_stage_mask(dst_stage)
        .dst_access_mask(dst_access)
        .old_layout(old_layout)
        .new_layout(new_layout)
        .src_queue_family_index(src_family)
        .dst_queue_family_index(dst_family)
        .image(image)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        })];

    unsafe {
        device.cmd_pipeline_barrier2(
            cb,
            &vk::DependencyInfo::default().image_memory_barriers(&barriers),
        )
    };
}

fn contains_extension(list: &[CString], str: &CStr) -> bool {
    list.iter().any(|v| v.as_c_str() == str)
}
