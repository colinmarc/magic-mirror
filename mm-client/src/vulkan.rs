#![allow(clippy::missing_safety_doc)]

// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use std::{
    ffi::{c_void, CStr, CString},
    sync::Arc,
};

use anyhow::{anyhow, bail, Context};
use ash::{
    extensions::{
        ext::DebugUtils,
        khr::{
            DynamicRendering as DynamicRenderingExt, Surface as SurfaceExt,
            Swapchain as SwapchainExt,
        },
    },
    vk,
};
use cstr::cstr;
use raw_window_handle::{HasRawDisplayHandle, HasRawWindowHandle};
use tracing::{debug, error, info, warn};

use crate::video::ColorSpace;

pub struct VkDebugContext {
    debug: DebugUtils,
    messenger: vk::DebugUtilsMessengerEXT,
}

pub struct VkQueue {
    pub queue: vk::Queue,
    pub command_pool: vk::CommandPool,
}

pub struct VkDeviceInfo {
    pub device_name: CString,
    pub device_type: vk::PhysicalDeviceType,
    pub limits: vk::PhysicalDeviceLimits,
    pub present_family: u32,
    pub decode_family: Option<u32>,
    pub supports_h264: bool,
    pub supports_h265: bool,
    pub supports_av1: bool,
    pub memory_props: vk::PhysicalDeviceMemoryProperties,
    pub host_visible_mem_type_index: u32,
    pub host_mem_is_cached: bool,
    pub selected_extensions: Vec<CString>,
}

pub struct VkContext {
    pub entry: ash::Entry,
    pub instance: ash::Instance,
    pub swapchain_loader: SwapchainExt,
    pub surface_loader: SurfaceExt,
    pub dynamic_rendering_loader: DynamicRenderingExt,

    pub surface: vk::SurfaceKHR,
    pub pdevice: vk::PhysicalDevice,
    pub device: ash::Device,
    pub device_info: VkDeviceInfo,
    pub present_queue: VkQueue,
    pub decode_queue: Option<VkQueue>,
    pub debug: Option<VkDebugContext>,

    pub tracy_context: Option<tracy_client::GpuContext>,

    // Hold on to a reference to the window, so that it gets dropped last.
    _window: Arc<winit::window::Window>,
}

impl VkDeviceInfo {
    fn query(
        instance: &ash::Instance,
        surface_loader: &SurfaceExt,
        surface: vk::SurfaceKHR,
        device: vk::PhysicalDevice,
    ) -> anyhow::Result<Self> {
        let props = unsafe { instance.get_physical_device_properties(device) };
        let device_type = props.device_type;
        let device_name = unsafe { CStr::from_ptr(props.device_name.as_ptr()).to_owned() };

        let queue_families = unsafe {
            instance
                .get_physical_device_queue_family_properties(device)
                .into_iter()
                .collect::<Vec<_>>()
        };

        let present_family = queue_families
            .iter()
            .enumerate()
            .find(|(idx, properties)| {
                properties.queue_flags.contains(vk::QueueFlags::GRAPHICS)
                    && properties.queue_flags.contains(vk::QueueFlags::COMPUTE)
                    && unsafe {
                        surface_loader
                            .get_physical_device_surface_support(device, *idx as u32, surface)
                            .unwrap_or(false)
                    }
            })
            .map(|(index, _)| index as u32)
            .to_owned()
            .ok_or_else(|| anyhow::anyhow!("no graphics queue found"))?;

        let decode_family = queue_families
            .iter()
            .enumerate()
            .find(|(_, properties)| {
                properties
                    .queue_flags
                    .contains(vk::QueueFlags::VIDEO_DECODE_KHR)
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

        let ext_swapchain = SwapchainExt::name();
        if !contains(&available_extensions, ext_swapchain) {
            return Err(anyhow::anyhow!("swapchain extension not available"));
        }

        let mut selected_extensions = vec![
            ext_swapchain.to_owned(),
            vk::KhrDynamicRenderingFn::name().to_owned(),
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            vk::KhrPortabilitySubsetFn::name().to_owned(),
        ];

        let ext_video_queue = vk::KhrVideoQueueFn::name();
        let ext_video_decode_queue = vk::KhrVideoDecodeQueueFn::name();
        let ext_h264 = vk::KhrVideoDecodeH264Fn::name();
        let ext_h265 = vk::KhrVideoDecodeH265Fn::name();
        let ext_av1 = cstr!("VK_EXT_video_decode_av1"); // This doesn't exist yet.

        let mut supports_h264 = false;
        let mut supports_h265 = false;
        let mut supports_av1 = false;
        if decode_family.is_some()
            && contains(&available_extensions, ext_video_queue)
            && contains(&available_extensions, ext_video_decode_queue)
        {
            selected_extensions.push(ext_video_decode_queue.to_owned());
            selected_extensions.push(ext_video_queue.to_owned());

            if contains(&available_extensions, ext_h264) {
                supports_h264 = true;
                selected_extensions.push(ext_h264.to_owned());
            }

            if contains(&available_extensions, ext_h265) {
                supports_h265 = true;
                selected_extensions.push(ext_h265.to_owned());
            }

            // This doesn't actually exist yet.
            if contains(&available_extensions, ext_av1) {
                supports_av1 = true;
                selected_extensions.push(ext_av1.to_owned());
            }
        }

        // We want HOST_CACHED | HOST_COHERENT, but we can make do with just
        // HOST_VISIBLE.
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
            device_name,
            device_type,
            limits: props.limits,
            present_family,
            decode_family,
            supports_h264,
            supports_h265,
            supports_av1,
            memory_props,
            host_visible_mem_type_index,
            host_mem_is_cached,
            selected_extensions,
        })
    }

    pub fn is_integrated(&self) -> bool {
        self.device_type == vk::PhysicalDeviceType::INTEGRATED_GPU
    }
}

impl VkContext {
    pub fn new(window: Arc<winit::window::Window>, debug: bool) -> anyhow::Result<Self> {
        // MoltenVK is very noisy.
        #[cfg(target_os = "macos")]
        std::env::set_var(
            "MVK_CONFIG_LOG_LEVEL",
            std::env::var("MVK_CONFIG_LOG_LEVEL").unwrap_or("0".to_string()),
        );

        #[cfg(all(target_os = "macos", feature = "moltenvk_static"))]
        let entry = ash_molten::load();

        #[cfg(not(all(target_os = "macos", feature = "moltenvk_static")))]
        let entry = unsafe { ash::Entry::load().context("failed to load vulkan libraries!") }?;

        debug!("creating vulkan instance");

        let (major, minor) = match entry.try_enumerate_instance_version()? {
            // Vulkan 1.1+
            Some(version) => (
                vk::api_version_major(version),
                vk::api_version_minor(version),
            ),
            // Vulkan 1.0
            None => (1, 0),
        };

        if major < 1 || (major == 1 && minor < 2) {
            return Err(anyhow::anyhow!("vulkan 1.2 or higher is required"));
        }

        // MoltenVK doesn't actually support 1.3.
        let (major, minor) = if cfg!(any(target_os = "macos")) {
            (1, 2)
        } else {
            (major, minor)
        };

        let app_info = vk::ApplicationInfo::builder()
            .application_name(cstr!("Magic Mirror"))
            .application_version(vk::make_api_version(0, 0, 1, 0))
            .engine_name(cstr!("No Engine"))
            .engine_version(vk::make_api_version(0, 0, 1, 0))
            .api_version(vk::make_api_version(0, major, minor, 0));

        let mut extensions =
            ash_window::enumerate_required_extensions(window.raw_display_handle())?.to_vec();

        let mut layers = Vec::new();

        #[cfg(all(target_os = "macos", not(feature = "moltenvk_static")))]
        {
            extensions.push(vk::KhrPortabilityEnumerationFn::name().as_ptr());
            // Enabling this extension is a requirement when using
            // `VK_KHR_portability_subset`
            extensions.push(vk::KhrGetPhysicalDeviceProperties2Fn::name().as_ptr());
        }

        if debug {
            let props = entry.enumerate_instance_extension_properties(None)?;
            let available_extensions = props
                .into_iter()
                .map(|properties| unsafe {
                    CStr::from_ptr(&properties.extension_name as *const _).to_owned()
                })
                .collect::<Vec<_>>();

            if !available_extensions
                .iter()
                .any(|ext| ext.as_c_str() == DebugUtils::name())
            {
                return Err(anyhow::anyhow!(
                    "debug utils extension requested, but not available"
                ));
            }

            warn!("vulkan debug tooling enabled");
            extensions.push(DebugUtils::name().as_ptr());

            let validation_layer = cstr!("VK_LAYER_KHRONOS_validation");
            let layer_props = entry.enumerate_instance_layer_properties()?;
            if layer_props
                .into_iter()
                .map(|properties| unsafe { CStr::from_ptr(&properties.layer_name as *const _) })
                .any(|layer| layer == validation_layer)
            {
                layers.push(validation_layer.as_ptr());
            } else {
                warn!("validation layers requested, but not available!")
            }
        }

        let instance = {
            let flags = if cfg!(any(target_os = "macos", target_os = "ios")) {
                vk::InstanceCreateFlags::ENUMERATE_PORTABILITY_KHR
            } else {
                vk::InstanceCreateFlags::default()
            };

            let instance_create_info = vk::InstanceCreateInfo::builder()
                .flags(flags)
                .application_info(&app_info)
                .enabled_layer_names(&layers)
                .enabled_extension_names(&extensions);

            unsafe { entry.create_instance(&instance_create_info, None)? }
        };

        let surface_loader = SurfaceExt::new(&entry, &instance);
        let surface = unsafe {
            ash_window::create_surface(
                &entry,
                &instance,
                window.raw_display_handle(),
                window.raw_window_handle(),
                None,
            )?
        };

        let debug_utils = if debug {
            let debug_utils = DebugUtils::new(&entry, &instance);

            let create_info = vk::DebugUtilsMessengerCreateInfoEXT::builder()
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
            .flat_map(|(index, dev)| {
                match VkDeviceInfo::query(&instance, &surface_loader, surface, dev) {
                    Ok(info) => Some((index as u32, dev, info)),
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

                        warn!("gpu {device_name:?} ineligible: {err}");
                        None
                    }
                }
            })
            .collect::<Vec<_>>();

        devices.sort_by_key(|(_, _, info)| {
            let mut score = match info.device_type {
                vk::PhysicalDeviceType::DISCRETE_GPU => 0,
                vk::PhysicalDeviceType::INTEGRATED_GPU => 10,
                _ => 20,
            };

            score += info.decode_family.is_none() as u32;
            score += !info.supports_h264 as u32;
            score += !info.supports_h265 as u32;
            score += !info.supports_av1 as u32;
            score
        });

        if devices.is_empty() {
            return Err(anyhow!("no eligible GPU found!"));
        }

        let (index, pdevice, device_info) = devices.remove(0);
        info!("selected gpu: {:?} ({index})", device_info.device_name);

        let device = {
            let queue_priorities = &[1.0];
            let mut queue_indices = Vec::new();
            queue_indices.push(device_info.present_family);
            if let Some(idx) = device_info.decode_family {
                queue_indices.push(idx);
            }

            queue_indices.dedup();
            let queue_create_infos = queue_indices
                .iter()
                .map(|&index| {
                    vk::DeviceQueueCreateInfo::builder()
                        .queue_family_index(index)
                        .queue_priorities(queue_priorities)
                        .build()
                })
                .collect::<Vec<_>>();

            let mut enabled_1_1_features =
                vk::PhysicalDeviceVulkan11Features::builder().sampler_ycbcr_conversion(true);

            let mut dynamic_rendering_features =
                vk::PhysicalDeviceDynamicRenderingFeatures::builder().dynamic_rendering(true);

            let extension_names = device_info
                .selected_extensions
                .iter()
                .map(|v| v.as_c_str().as_ptr())
                .collect::<Vec<_>>();
            let device_create_info = vk::DeviceCreateInfo::builder()
                .queue_create_infos(&queue_create_infos)
                .enabled_extension_names(&extension_names)
                .push_next(&mut enabled_1_1_features)
                .push_next(&mut dynamic_rendering_features);

            unsafe { instance.create_device(pdevice, &device_create_info, None)? }
        };

        let present_queue = get_queue_with_command_pool(&device, device_info.present_family)?;
        let mut decode_queue = None;
        if device_info.decode_family.is_some() {
            info!(
                "vulkan video decode support: (h264: {}, h265: {}, av1: {})",
                device_info.supports_h264, device_info.supports_h265, device_info.supports_av1
            );

            decode_queue = Some(get_queue_with_command_pool(
                &device,
                device_info.decode_family.unwrap(),
            )?);
        } else {
            debug!("no vulkan video support found")
        }

        if !device_info.host_mem_is_cached {
            warn!("no cache-coherent memory type found on device!");
        }

        let swapchain_loader = SwapchainExt::new(&instance, &device);
        let dynamic_rendering_loader = DynamicRenderingExt::new(&instance, &device);

        let tracy_context = tracy_client::Client::running().and_then(|client| {
            match init_tracy_context(&device, &device_info, &present_queue, client) {
                Ok(ctx) => Some(ctx),
                Err(err) => {
                    error!("failed to initialize tracy GPU context: {err}");
                    None
                }
            }
        });

        Ok(Self {
            entry,
            instance,
            swapchain_loader,
            surface_loader,
            dynamic_rendering_loader,

            surface,
            pdevice,
            device,
            device_info,
            present_queue,
            decode_queue,
            debug: debug_utils,
            tracy_context,

            _window: window,
        })
    }
}

impl Drop for VkContext {
    fn drop(&mut self) {
        let device = &self.device;

        unsafe {
            device.destroy_command_pool(self.present_queue.command_pool, None);
            if let Some(decode_queue) = self.decode_queue.take() {
                device.destroy_command_pool(decode_queue.command_pool, None);
            }

            if let Some(debug) = self.debug.take() {
                debug
                    .debug
                    .destroy_debug_utils_messenger(debug.messenger, None);
            }

            self.surface_loader.destroy_surface(self.surface, None);
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

fn contains(list: &[CString], str: &'static CStr) -> bool {
    list.iter().any(|v| v.as_c_str() == str)
}

fn init_tracy_context(
    device: &ash::Device,
    pdevice: &VkDeviceInfo,
    present_queue: &VkQueue,
    client: tracy_client::Client,
) -> anyhow::Result<tracy_client::GpuContext> {
    // Query the timestamp once to calibrate the clocks.
    let cb = create_command_buffer(device, present_queue.command_pool)?;

    unsafe {
        device.reset_command_buffer(cb, vk::CommandBufferResetFlags::empty())?;

        let query_pool = create_timestamp_query_pool(device, 1)?;
        let fence = create_fence(device, false)?;

        // Begin the command buffer.
        device.begin_command_buffer(
            cb,
            &vk::CommandBufferBeginInfo::builder()
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

        let cbs = [cb];
        device.queue_submit(
            present_queue.queue,
            &[vk::SubmitInfo::builder().command_buffers(&cbs).build()],
            fence,
        )?;

        // Wait for the fence, fetch the timestamp.
        device.wait_for_fences(&[fence], true, u64::MAX)?;
        let ts = query_pool.fetch_results(device)?[0];

        let context = client.new_gpu_context(
            Some("present queue"),
            tracy_client::GpuContextType::Vulkan,
            ts as i64,
            pdevice.limits.timestamp_period,
        )?;

        // Cleanup.
        device.free_command_buffers(present_queue.command_pool, &[cb]);
        device.destroy_fence(fence, None);
        device.destroy_query_pool(query_pool.pool, None);

        Ok(context)
    }
}

pub fn select_memory_type(
    props: &vk::PhysicalDeviceMemoryProperties,
    flags: vk::MemoryPropertyFlags,
    req: Option<vk::MemoryRequirements>,
) -> Option<u32> {
    for i in 0..props.memory_type_count {
        if let Some(req) = req {
            if req.memory_type_bits & (1 << i) == 0 {
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

fn get_queue_with_command_pool(device: &ash::Device, idx: u32) -> Result<VkQueue, vk::Result> {
    let queue = unsafe { device.get_device_queue(idx, 0) };

    let command_pool = unsafe {
        let create_info = vk::CommandPoolCreateInfo::builder()
            .queue_family_index(idx)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);

        device.create_command_pool(&create_info, None)?
    };

    Ok(VkQueue {
        queue,
        command_pool,
    })
}

pub fn create_command_buffer(
    device: &ash::Device,
    pool: vk::CommandPool,
) -> anyhow::Result<vk::CommandBuffer> {
    let create_info = vk::CommandBufferAllocateInfo::builder()
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

pub struct VkImage {
    pub image: vk::Image,
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
        width: u32,
        height: u32,
        usage: vk::ImageUsageFlags,
        sharing_mode: vk::SharingMode,
        flags: vk::ImageCreateFlags,
    ) -> anyhow::Result<Self> {
        let image = {
            let create_info = vk::ImageCreateInfo::builder()
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

        Ok(Self {
            image,
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
        memory: vk::DeviceMemory,
        format: vk::Format,
        width: u32,
        height: u32,
    ) -> Self {
        Self {
            image,
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
        Some(image_memory_req),
    );

    if mem_type_index.is_none() {
        bail!(
            "no appropriate memory type found for reqs: {:?}",
            image_memory_req
        );
    }

    let memory = {
        let image_allocate_info = vk::MemoryAllocateInfo::builder()
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
    sampler_conversion: Option<vk::SamplerYcbcrConversion>,
) -> anyhow::Result<vk::ImageView> {
    let mut create_info = vk::ImageViewCreateInfo::builder()
        .image(image)
        .view_type(vk::ImageViewType::TYPE_2D)
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

    let mut sampler_conversion_info;
    if let Some(sampler_conversion) = sampler_conversion {
        sampler_conversion_info =
            vk::SamplerYcbcrConversionInfo::builder().conversion(sampler_conversion);
        create_info = create_info.push_next(&mut sampler_conversion_info);
    }

    device
        .create_image_view(&create_info, None)
        .context("VkCreateImageView")
}

#[derive(Copy, Clone)]
pub struct VkHostBuffer {
    pub buffer: vk::Buffer,
    pub memory: vk::DeviceMemory,
    pub access: *mut c_void,
}

pub fn create_host_buffer(
    device: &ash::Device,
    mem_type: u32,
    usage: vk::BufferUsageFlags,
    size: usize,
) -> Result<VkHostBuffer, vk::Result> {
    let buffer = {
        let create_info: vk::BufferCreateInfoBuilder<'_> = vk::BufferCreateInfo::builder()
            .size(size as u64)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);

        unsafe { device.create_buffer(&create_info, None)? }
    };

    let memory = {
        let requirements = unsafe { device.get_buffer_memory_requirements(buffer) };

        let alloc_info = vk::MemoryAllocateInfo::builder()
            .allocation_size(requirements.size)
            .memory_type_index(mem_type);

        unsafe { device.allocate_memory(&alloc_info, None)? }
    };

    unsafe { device.bind_buffer_memory(buffer, memory, 0)? };

    let access =
        { unsafe { device.map_memory(memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty())? } };

    Ok(VkHostBuffer {
        buffer,
        memory,
        access,
    })
}

pub unsafe fn destroy_host_buffer(device: &ash::Device, buffer: &VkHostBuffer) {
    device.unmap_memory(buffer.memory);
    device.destroy_buffer(buffer.buffer, None);
    device.free_memory(buffer.memory, None);
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
                .get_query_pool_results(
                    self.pool,
                    0,
                    self.num_timestamps,
                    &mut results,
                    vk::QueryResultFlags::empty(),
                )
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
    let create_info = vk::QueryPoolCreateInfo::builder()
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

pub fn create_fence(device: &ash::Device, signalled: bool) -> Result<vk::Fence, vk::Result> {
    let mut create_info = vk::FenceCreateInfo::builder();
    if signalled {
        create_info = create_info.flags(vk::FenceCreateFlags::SIGNALED);
    }

    let fence = unsafe { device.create_fence(&create_info, None)? };

    Ok(fence)
}

pub fn create_semaphore(device: &ash::Device) -> Result<vk::Semaphore, vk::Result> {
    let semaphore = unsafe { device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)? };
    Ok(semaphore)
}

pub fn load_shader(device: &ash::Device, bytes: &[u8]) -> anyhow::Result<vk::ShaderModule> {
    let code = ash::util::read_spv(&mut std::io::Cursor::new(bytes))?;
    let create_info = vk::ShaderModuleCreateInfo::builder().code(&code);

    let shader = unsafe { device.create_shader_module(&create_info, None)? };

    Ok(shader)
}

pub fn create_ycbcr_sampler_conversion(
    device: &ash::Device,
    format: vk::Format,
    params: &crate::video::VideoStreamParams,
) -> anyhow::Result<vk::SamplerYcbcrConversion> {
    let ycbcr_model = match params.color_space {
        ColorSpace::Bt709 => vk::SamplerYcbcrModelConversion::YCBCR_709,
        ColorSpace::Bt2020Pq => vk::SamplerYcbcrModelConversion::YCBCR_2020,
    };

    let ycbcr_range = if params.color_full_range {
        vk::SamplerYcbcrRange::ITU_FULL
    } else {
        vk::SamplerYcbcrRange::ITU_NARROW
    };

    let create_info = vk::SamplerYcbcrConversionCreateInfo::builder()
        .format(format)
        .ycbcr_model(ycbcr_model)
        .ycbcr_range(ycbcr_range)
        .chroma_filter(vk::Filter::LINEAR)
        .x_chroma_offset(vk::ChromaLocation::MIDPOINT)
        .y_chroma_offset(vk::ChromaLocation::MIDPOINT);

    let conversion = unsafe { device.create_sampler_ycbcr_conversion(&create_info, None)? };
    Ok(conversion)
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

#[allow(clippy::too_many_arguments)]
pub fn cmd_image_barrier(
    device: &ash::Device,
    command_buffer: vk::CommandBuffer,
    image: vk::Image,
    src_stage_mask: vk::PipelineStageFlags,
    src_access_mask: vk::AccessFlags,
    dst_stage_mask: vk::PipelineStageFlags,
    dst_access_mask: vk::AccessFlags,
    old_layout: vk::ImageLayout,
    new_layout: vk::ImageLayout,
) {
    let barrier = vk::ImageMemoryBarrier::builder()
        .src_access_mask(src_access_mask)
        .dst_access_mask(dst_access_mask)
        .old_layout(old_layout)
        .new_layout(new_layout)
        .image(image)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        })
        .build();

    unsafe {
        device.cmd_pipeline_barrier(
            command_buffer,
            src_stage_mask,
            dst_stage_mask,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[barrier],
        )
    };
}
