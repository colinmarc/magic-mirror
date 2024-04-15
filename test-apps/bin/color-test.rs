// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use std::{
    ffi::{c_void, CStr, CString},
    rc::Rc,
    time,
};

use anyhow::{anyhow, Context};
use ash::{
    extensions::{
        ext::DebugUtils as DebugUtilsExt, khr::DynamicRendering as DynamicRenderingKhr,
        khr::Surface as SurfaceKhr, khr::Swapchain as SwapchainKhr,
    },
    vk,
};
use imgui_rs_vulkan_renderer as imgui_vulkan;
use raw_window_handle::{HasRawDisplayHandle, HasRawWindowHandle};
use winit::{
    event::{ElementState, Event, KeyEvent, MouseButton, WindowEvent},
    event_loop::EventLoop,
    keyboard::{KeyCode, PhysicalKey},
    window::WindowBuilder,
};

struct ImguiContext {
    imgui: imgui::Context,
    platform: imgui_winit_support::WinitPlatform,
}

#[derive(Copy, Clone, Debug)]
#[repr(C)]
struct PushConstants {
    size: glam::Vec2,
    mouse: glam::Vec2,
    color_mul: f32,
    color_space: vk::ColorSpaceKHR,
}

struct VkDebugContext {
    debug: DebugUtilsExt,
    messenger: vk::DebugUtilsMessengerEXT,
}

struct DeviceInfo {
    device_name: CString,
    device_type: vk::PhysicalDeviceType,
    present_family: u32,
}

pub struct VkQueue {
    pub queue: vk::Queue,
    pub command_pool: vk::CommandPool,
}

struct Renderer {
    _entry: ash::Entry,
    instance: ash::Instance,
    device: ash::Device,
    swapchain_loader: SwapchainKhr,
    surface_loader: SurfaceKhr,
    dynamic_rendering_loader: DynamicRenderingKhr,
    debug: Option<VkDebugContext>,

    pdevice: vk::PhysicalDevice,
    _device_info: DeviceInfo,

    surface: vk::SurfaceKHR,
    surface_formats: Vec<vk::SurfaceFormatKHR>,
    format: vk::Format,
    colorspace: vk::ColorSpaceKHR,

    pc: PushConstants,
    present_queue: VkQueue,
    width: u32,
    height: u32,

    imgui: Option<ImguiContext>,

    window: Rc<winit::window::Window>,

    swapchain: Option<Swapchain>,
    swapchain_dirty: bool,
}

struct Swapchain {
    swapchain: vk::SwapchainKHR,
    frames: Vec<InFlightFrame>,
    present_images: Vec<SwapImage>,
    current_frame: usize,

    imgui_renderer: Option<imgui_vulkan::Renderer>,

    descriptor_set_layout: vk::DescriptorSetLayout,
    descriptor_pool: vk::DescriptorPool,
    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
}

struct InFlightFrame {
    render_cb: vk::CommandBuffer,
    render_fence: vk::Fence,
    image_acquired_sema: vk::Semaphore,
    render_complete_sema: vk::Semaphore,
}

struct SwapImage {
    image: vk::Image,
    view: vk::ImageView,
}

impl Renderer {
    fn new(window: Rc<winit::window::Window>, debug: bool) -> anyhow::Result<Self> {
        let entry = unsafe { ash::Entry::load().context("failed to load vulkan libraries!") }?;
        eprintln!("creating vulkan instance");

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
        let (major, minor) = if cfg!(any(target_os = "macos", target_os = "ios")) {
            (1, 2)
        } else {
            (major, minor)
        };

        let app_info = vk::ApplicationInfo::builder()
            .application_name(c"c")
            .application_version(vk::make_api_version(0, 0, 1, 0))
            .engine_name(c"No Engine")
            .engine_version(vk::make_api_version(0, 0, 1, 0))
            .api_version(vk::make_api_version(0, major, minor, 0));

        let mut extensions =
            ash_window::enumerate_required_extensions(window.raw_display_handle())?.to_vec();

        let mut layers = Vec::new();

        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            extensions.push(vk::KhrPortabilityEnumerationFn::name().as_ptr());
            // Enabling this extension is a requirement when using `VK_KHR_portability_subset`
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
                .any(|ext| ext.as_c_str() == DebugUtilsExt::name())
            {
                return Err(anyhow::anyhow!(
                    "debug utils extension requested, but not available"
                ));
            }

            extensions.push(DebugUtilsExt::name().as_ptr());

            let validation_layer = c"VK_LAYER_KHRONOS_validation";
            let layer_props = entry.enumerate_instance_layer_properties()?;
            if layer_props
                .into_iter()
                .map(|properties| unsafe { CStr::from_ptr(&properties.layer_name as *const _) })
                .any(|layer| layer == validation_layer)
            {
                layers.push(validation_layer.as_ptr());
            } else {
                eprintln!("validation layers requested, but not available!")
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

        let debug_utils = if debug {
            let debug_utils = DebugUtilsExt::new(&entry, &instance);

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

        let surface_loader = SurfaceKhr::new(&entry, &instance);
        let surface = unsafe {
            ash_window::create_surface(
                &entry,
                &instance,
                window.raw_display_handle(),
                window.raw_window_handle(),
                None,
            )?
        };

        let devices = unsafe { instance.enumerate_physical_devices()? };
        let mut devices = devices
            .into_iter()
            .enumerate()
            .flat_map(
                |(index, dev)| match query_device(&instance, &surface_loader, surface, dev) {
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

                        eprintln!("gpu {device_name:?} ineligible: {err}");
                        None
                    }
                },
            )
            .collect::<Vec<_>>();

        devices.sort_by_key(|(_, _, info)| match info.device_type {
            vk::PhysicalDeviceType::DISCRETE_GPU => 0,
            vk::PhysicalDeviceType::INTEGRATED_GPU => 1,
            _ => 2,
        });

        if devices.is_empty() {
            return Err(anyhow!("no eligible GPU found!"));
        }

        let (index, pdevice, device_info) = devices.remove(0);
        eprintln!("selected gpu: {:?} ({index})", device_info.device_name);

        let device = {
            let queue_priorities = &[1.0];
            let mut queue_indices = Vec::new();
            queue_indices.push(device_info.present_family);

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

            let selected_extensions = [
                vk::KhrSwapchainFn::name().to_owned(),
                vk::KhrDynamicRenderingFn::name().to_owned(),
                #[cfg(any(target_os = "macos", target_os = "ios"))]
                vk::KhrPortabilitySubsetFn::name().to_owned(),
            ];

            let extension_names = selected_extensions
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
        let window_size = window.inner_size();

        let surface_formats =
            unsafe { surface_loader.get_physical_device_surface_formats(pdevice, surface)? };

        for surface_format in &surface_formats {
            eprintln!(
                "available surface format: {:?} ({}) -> {:?} ({})",
                surface_format.format,
                surface_format.format.as_raw(),
                surface_format.color_space,
                surface_format.color_space.as_raw()
            );
        }

        // Disable Vulkan's automatic sRGB conversion.
        let surface_formats = surface_formats
            .into_iter()
            .filter(|sf| !format_is_srgb(sf.format) && colorspace_supported(sf.color_space))
            .collect::<Vec<_>>();

        let surface_format = surface_formats[0];
        eprintln!(
            "using surface format: {:?} / {:?}",
            surface_format.format, surface_format.color_space,
        );

        let swapchain_loader = SwapchainKhr::new(&instance, &device);
        let dynamic_rendering_loader = DynamicRenderingKhr::new(&instance, &device);

        let mut imgui = imgui::Context::create();
        imgui.set_ini_filename(None);

        let mut imgui_platform = imgui_winit_support::WinitPlatform::init(&mut imgui);
        imgui_platform.attach_window(
            imgui.io_mut(),
            &window,
            imgui_winit_support::HiDpiMode::Default,
        );

        let mut renderer = Self {
            _entry: entry,
            instance,
            device,
            swapchain_loader,
            surface_loader,
            dynamic_rendering_loader,
            debug: debug_utils,

            pdevice,
            _device_info: device_info,

            surface,
            surface_formats,
            format: surface_format.format,
            colorspace: surface_format.color_space,

            pc: PushConstants {
                size: glam::Vec2::new(window_size.width as f32, window_size.height as f32),
                mouse: glam::Vec2::ZERO,
                color_mul: 1.0,
                color_space: surface_format.color_space,
            },
            present_queue,
            width: window_size.width,
            height: window_size.height,

            imgui: Some(ImguiContext {
                imgui,
                platform: imgui_platform,
            }),

            window,
            swapchain: None,

            swapchain_dirty: false,
        };

        unsafe { renderer.recreate_swapchain()? };

        Ok(renderer)
    }

    unsafe fn recreate_swapchain(&mut self) -> anyhow::Result<()> {
        let start = time::Instant::now();
        let device = &self.device;

        let surface_format = self
            .surface_formats
            .iter()
            .find(|sf| sf.format == self.format && sf.color_space == self.colorspace)
            .expect("invalid format / colorspace combination");
        eprintln!(
            "recreating swapchain with format {:?} / {:?}",
            surface_format.format, surface_format.color_space
        );

        self.pc.color_space = surface_format.color_space;

        let surface_capabilities = self
            .surface_loader
            .get_physical_device_surface_capabilities(self.pdevice, self.surface)
            .unwrap();
        let mut desired_image_count = surface_capabilities.min_image_count + 1;
        if surface_capabilities.max_image_count > 0
            && desired_image_count > surface_capabilities.max_image_count
        {
            desired_image_count = surface_capabilities.max_image_count;
        }

        let surface_resolution = match surface_capabilities.current_extent.width {
            std::u32::MAX => vk::Extent2D {
                width: self.width,
                height: self.height,
            },
            _ => surface_capabilities.current_extent,
        };

        self.pc.size = glam::Vec2::new(
            surface_resolution.width as f32,
            surface_resolution.height as f32,
        );

        let pre_transform = if surface_capabilities
            .supported_transforms
            .contains(vk::SurfaceTransformFlagsKHR::IDENTITY)
        {
            vk::SurfaceTransformFlagsKHR::IDENTITY
        } else {
            surface_capabilities.current_transform
        };

        let present_modes = self
            .surface_loader
            .get_physical_device_surface_present_modes(self.pdevice, self.surface)
            .unwrap();

        let mut present_modes = present_modes.clone();
        present_modes.sort_by_key(|&mode| match mode {
            vk::PresentModeKHR::MAILBOX => 0,
            vk::PresentModeKHR::IMMEDIATE => 1,
            vk::PresentModeKHR::FIFO => 2,
            _ => 4,
        });

        let present_mode = present_modes.first().unwrap();
        if *present_mode != vk::PresentModeKHR::MAILBOX {
            eprintln!(
                "present mode MAILBOX not available, using {:?} (available: {:?})",
                present_mode, present_modes
            );
        }

        let mut swapchain_create_info = vk::SwapchainCreateInfoKHR::builder()
            .surface(self.surface)
            .min_image_count(desired_image_count)
            .image_color_space(surface_format.color_space)
            .image_format(surface_format.format)
            .image_extent(surface_resolution)
            .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(pre_transform)
            .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
            .present_mode(*present_mode)
            .clipped(true)
            .image_array_layers(1);

        if let Some(old_swapchain) = self.swapchain.as_ref() {
            swapchain_create_info = swapchain_create_info.old_swapchain(old_swapchain.swapchain);
        }

        let swapchain = self
            .swapchain_loader
            .create_swapchain(&swapchain_create_info, None)?;
        let swapchain_images = self.swapchain_loader.get_swapchain_images(swapchain)?;

        let descriptor_set_layout = {
            let create_info = vk::DescriptorSetLayoutCreateInfo::builder();
            unsafe { device.create_descriptor_set_layout(&create_info, None)? }
        };

        let descriptor_pool = {
            let sampler_size = vk::DescriptorPoolSize::builder()
                .ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(swapchain_images.len() as u32);

            let pool_sizes = &[sampler_size.build()];
            let info = vk::DescriptorPoolCreateInfo::builder()
                .pool_sizes(pool_sizes)
                .max_sets(swapchain_images.len() as u32);

            unsafe { device.create_descriptor_pool(&info, None)? }
        };

        let pipeline_layout = {
            let pc_ranges = [vk::PushConstantRange::builder()
                .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
                .offset(0)
                .size(std::mem::size_of::<PushConstants>() as u32)
                .build()];
            let set_layouts = [descriptor_set_layout];
            let create_info = vk::PipelineLayoutCreateInfo::builder()
                .set_layouts(&set_layouts)
                .push_constant_ranges(&pc_ranges);

            unsafe { device.create_pipeline_layout(&create_info, None)? }
        };

        let pipeline = {
            let vert_bytes = include_bytes!(concat!(env!("OUT_DIR"), "/color-test/vert.spv"));
            let frag_bytes = include_bytes!(concat!(env!("OUT_DIR"), "/color-test/frag.spv"));
            let vert_shader = load_shader(device, vert_bytes).context("loading vert.spv")?;
            let frag_shader = load_shader(device, frag_bytes).context("loading frag.spv")?;

            let vert_stage = vk::PipelineShaderStageCreateInfo::builder()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(vert_shader)
                .name(c"main");

            let frag_stage = vk::PipelineShaderStageCreateInfo::builder()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(frag_shader)
                .name(c"main");

            let vertex_input_state = vk::PipelineVertexInputStateCreateInfo::builder();

            let input_assembly_state = vk::PipelineInputAssemblyStateCreateInfo::builder()
                .topology(vk::PrimitiveTopology::TRIANGLE_STRIP)
                .primitive_restart_enable(false);

            let viewport = vk::Viewport::builder()
                .x(0.0)
                .y(0.0)
                .width(self.width as f32)
                .height(self.height as f32)
                .min_depth(0.0)
                .max_depth(1.0);

            let scissor = vk::Rect2D::builder().extent(vk::Extent2D {
                width: self.width,
                height: self.height,
            });

            let viewports = [viewport.build()];
            let scissors = [scissor.build()];
            let viewport_state = vk::PipelineViewportStateCreateInfo::builder()
                .viewports(&viewports)
                .scissors(&scissors);

            let rasterization_state = vk::PipelineRasterizationStateCreateInfo::builder()
                .depth_clamp_enable(false)
                .rasterizer_discard_enable(false)
                .polygon_mode(vk::PolygonMode::FILL)
                .line_width(1.0)
                .depth_bias_enable(false)
                // Per https://www.saschawillems.de/blog/2016/08/13/vulkan-tutorial-on-rendering-a-fullscreen-quad-without-buffers
                .cull_mode(vk::CullModeFlags::FRONT)
                .front_face(vk::FrontFace::COUNTER_CLOCKWISE);

            let multisample_state = vk::PipelineMultisampleStateCreateInfo::builder()
                .sample_shading_enable(false)
                .rasterization_samples(vk::SampleCountFlags::TYPE_1);

            let attachment = vk::PipelineColorBlendAttachmentState::builder()
                .color_write_mask(vk::ColorComponentFlags::RGBA)
                .blend_enable(true)
                .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
                .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
                .color_blend_op(vk::BlendOp::ADD)
                .src_alpha_blend_factor(vk::BlendFactor::ONE)
                .dst_alpha_blend_factor(vk::BlendFactor::ZERO)
                .alpha_blend_op(vk::BlendOp::ADD);

            let attachments = [attachment.build()];
            let color_blend_state = vk::PipelineColorBlendStateCreateInfo::builder()
                .logic_op_enable(false)
                .attachments(&attachments);

            let formats = [surface_format.format];
            let mut pipeline_rendering = vk::PipelineRenderingCreateInfo::builder()
                .color_attachment_formats(&formats)
                .build();

            let stages = [vert_stage.build(), frag_stage.build()];
            let create_info = vk::GraphicsPipelineCreateInfo::builder()
                .stages(&stages)
                .vertex_input_state(&vertex_input_state)
                .input_assembly_state(&input_assembly_state)
                .viewport_state(&viewport_state)
                .rasterization_state(&rasterization_state)
                .multisample_state(&multisample_state)
                .color_blend_state(&color_blend_state)
                .layout(pipeline_layout)
                .push_next(&mut pipeline_rendering);

            unsafe {
                let pipeline = match device.create_graphics_pipelines(
                    vk::PipelineCache::null(),
                    &[create_info.build()],
                    None,
                ) {
                    Ok(pipelines) => Ok(pipelines[0]),
                    Err((_, e)) => Err(e),
                }?;

                device.destroy_shader_module(vert_shader, None);
                device.destroy_shader_module(frag_shader, None);
                pipeline
            }
        };

        let create_frame = || -> anyhow::Result<InFlightFrame> {
            let render_cb = {
                let create_info = vk::CommandBufferAllocateInfo::builder()
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_pool(self.present_queue.command_pool)
                    .command_buffer_count(1);

                let cbs = device
                    .allocate_command_buffers(&create_info)
                    .context("failed to allocate render command buffer")?;

                cbs[0]
            };

            let render_fence = create_fence(device, true)?;
            let image_acquired_sema = create_semaphore(device)?;
            let render_complete_sema = create_semaphore(device)?;

            Ok(InFlightFrame {
                render_cb,
                render_fence,
                image_acquired_sema,
                render_complete_sema,
            })
        };

        let frames = (0..swapchain_images.len())
            .map(|_| create_frame())
            .collect::<anyhow::Result<Vec<_>>>()?;

        let swapchain_images = swapchain_images
            .into_iter()
            .map(|image| {
                let create_info = vk::ImageViewCreateInfo::builder()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(surface_format.format)
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

                let image_view = device
                    .create_image_view(&create_info, None)
                    .context("vkCreateImageView")?;

                Ok(SwapImage {
                    image,
                    view: image_view,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        let imgui_renderer = if let Some(ImguiContext { imgui, .. }) = &mut self.imgui {
            Some(imgui_vulkan::Renderer::with_default_allocator(
                &self.instance,
                self.pdevice,
                device.clone(),
                self.present_queue.queue,
                self.present_queue.command_pool,
                imgui_vulkan::DynamicRendering {
                    color_attachment_format: surface_format.format,
                    depth_attachment_format: None,
                },
                imgui,
                Some(imgui_vulkan::Options {
                    in_flight_frames: frames.len(),
                    ..Default::default()
                }),
            )?)
        } else {
            None
        };

        let swapchain = Swapchain {
            swapchain,
            frames,
            present_images: swapchain_images,
            current_frame: 0,

            descriptor_pool,
            descriptor_set_layout,
            pipeline_layout,
            pipeline,

            imgui_renderer,
        };

        eprintln!("recreated swapchain in {:?}", start.elapsed());

        if let Some(old_swapchain) = self.swapchain.replace(swapchain) {
            self.destroy_swapchain(old_swapchain);
        };

        Ok(())
    }

    fn handle_event<T>(&mut self, event: &winit::event::Event<T>) -> anyhow::Result<()> {
        if let Some(ImguiContext {
            platform, imgui, ..
        }) = self.imgui.as_mut()
        {
            platform.handle_event(imgui.io_mut(), &self.window, event);
        }

        match event {
            winit::event::Event::WindowEvent {
                window_id,
                event: winit::event::WindowEvent::Resized(size),
            } if *window_id == self.window.id() => {
                self.resize(size.width, size.height);
            }
            _ => (),
        }

        Ok(())
    }

    fn resize(&mut self, width: u32, height: u32) {
        if self.width == width && self.height == height {
            return;
        }

        self.width = width;
        self.height = height;
        self.swapchain_dirty = true;
    }

    unsafe fn render(&mut self) -> anyhow::Result<()> {
        if self.swapchain_dirty || self.swapchain.is_none() {
            self.recreate_swapchain()?;
            self.swapchain_dirty = false;
        }

        let device = &self.device;
        let swapchain = self.swapchain.as_mut().unwrap();
        let num_frames = swapchain.frames.len();

        let frame = &mut swapchain.frames[swapchain.current_frame];
        swapchain.current_frame = (swapchain.current_frame + 1) % num_frames;

        // Wait for the gpu to catch up.
        device.wait_for_fences(&[frame.render_fence], true, u64::MAX)?;

        let result = self.swapchain_loader.acquire_next_image(
            swapchain.swapchain,
            u64::MAX,
            frame.image_acquired_sema,
            vk::Fence::null(),
        );

        let swapchain_index = match result {
            Ok((image_index, _)) => image_index,
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                // Recreate and try again.
                self.swapchain_dirty = true;
                return self.render();
            }
            Err(e) => return Err(e.into()),
        };

        let present_image = swapchain
            .present_images
            .get(swapchain_index as usize)
            .unwrap();

        // Reset the command buffer.
        device.reset_command_buffer(frame.render_cb, vk::CommandBufferResetFlags::empty())?;

        // Begin the command buffer.
        {
            let begin_info = vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);

            device.begin_command_buffer(frame.render_cb, &begin_info)?;
        }

        // Transition the present image to be writable.
        cmd_image_barrier(
            device,
            frame.render_cb,
            present_image.image,
            vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::AccessFlags::empty(),
            vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        );

        // Begin rendering.
        {
            let rect: vk::Rect2D = vk::Rect2D::builder()
                .extent(vk::Extent2D {
                    width: self.width,
                    height: self.height,
                })
                .build();

            let clear_value = vk::ClearValue {
                color: vk::ClearColorValue {
                    float32: [0.0, 0.0, 0.0, 1.0],
                },
            };

            let color_attachment = vk::RenderingAttachmentInfo::builder()
                .image_view(present_image.view)
                .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .load_op(vk::AttachmentLoadOp::CLEAR)
                .store_op(vk::AttachmentStoreOp::STORE)
                .clear_value(clear_value)
                .build();

            let color_attachments = [color_attachment];
            let rendering_info = vk::RenderingInfo::builder()
                .render_area(rect)
                .color_attachments(&color_attachments)
                .layer_count(1);

            self.dynamic_rendering_loader
                .cmd_begin_rendering(frame.render_cb, &rendering_info);
            device.cmd_bind_pipeline(
                frame.render_cb,
                vk::PipelineBindPoint::GRAPHICS,
                swapchain.pipeline,
            );
        }

        device.cmd_push_constants(
            frame.render_cb,
            swapchain.pipeline_layout,
            vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
            0,
            std::slice::from_raw_parts(
                &self.pc as *const _ as *const u8,
                std::mem::size_of::<PushConstants>(),
            ),
        );

        // Draw the triangle.
        device.cmd_draw(frame.render_cb, 3, 1, 0, 0);

        // Draw the overlay.
        if let Some(ImguiContext { platform, imgui }) = self.imgui.as_mut() {
            let mut formats = self
                .surface_formats
                .iter()
                .map(|sf| sf.format)
                .collect::<Vec<_>>();

            let mut colorspaces = self
                .surface_formats
                .iter()
                .map(|sf| sf.color_space)
                .collect::<Vec<_>>();

            formats.sort();
            formats.dedup();
            colorspaces.sort();
            colorspaces.dedup();

            let format_names = formats
                .iter()
                .map(|f| format!("{:?}", f))
                .collect::<Vec<_>>();

            let cs_names = colorspaces
                .iter()
                .map(|c| format!("{:?}", c))
                .collect::<Vec<_>>();

            let mut format_idx = formats.iter().position(|&f| f == self.format).unwrap() as i32;
            let mut cs_idx = colorspaces
                .iter()
                .position(|&c| c == self.colorspace)
                .unwrap() as i32;

            platform.prepare_frame(imgui.io_mut(), &self.window)?;

            {
                let ui = imgui.new_frame();

                let [width, _height] = ui.io().display_size;

                let _padding = ui.push_style_var(imgui::StyleVar::WindowPadding([8.0, 8.0]));
                let _rounding = ui.push_style_var(imgui::StyleVar::WindowRounding(4.0));
                let _frame_rounding = ui.push_style_var(imgui::StyleVar::FrameRounding(4.0));

                if let Some(_window) = ui
                    .window("controls")
                    .position([width - 16.0, 16.0], imgui::Condition::Always)
                    .position_pivot([1.0, 0.0])
                    .bg_alpha(0.8)
                    .size([250.0, 300.0], imgui::Condition::Always)
                    .begin()
                {
                    let _stretch = ui.push_item_width(-1.0);
                    ui.text("Format:");
                    ui.list_box(
                        "##format",
                        &mut format_idx,
                        &format_names.iter().map(|f| f.as_str()).collect::<Vec<_>>(),
                        4,
                    );

                    ui.text("Color Space:");
                    ui.list_box(
                        "##cs",
                        &mut cs_idx,
                        &cs_names.iter().map(|f| f.as_str()).collect::<Vec<_>>(),
                        4,
                    );

                    ui.text("Headroom:");
                    ui.slider("##headroom", 0.75, 4.0, &mut self.pc.color_mul);
                }

                platform.prepare_render(ui, &self.window);
            }

            let renderer = swapchain.imgui_renderer.as_mut().unwrap();
            renderer.cmd_draw(frame.render_cb, imgui.render())?;

            if formats[format_idx as usize] != self.format {
                self.format = formats[format_idx as usize];
                self.swapchain_dirty = true;
            }

            if colorspaces[cs_idx as usize] != self.colorspace {
                self.colorspace = colorspaces[cs_idx as usize];
                self.swapchain_dirty = true;
            }
        }

        // Done rendereng.
        self.dynamic_rendering_loader
            .cmd_end_rendering(frame.render_cb);

        // Transition the present image to be presentable.
        cmd_image_barrier(
            device,
            frame.render_cb,
            present_image.image,
            vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            vk::PipelineStageFlags::BOTTOM_OF_PIPE,
            vk::AccessFlags::empty(),
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            vk::ImageLayout::PRESENT_SRC_KHR,
        );

        // Submit and present!
        {
            let present_queue = self.present_queue.queue;

            device.end_command_buffer(frame.render_cb)?;
            device.reset_fences(&[frame.render_fence])?;

            let cbs = [frame.render_cb];
            let wait_semas = [frame.image_acquired_sema];
            let signal_semas = [frame.render_complete_sema];
            let wait_stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
            let submit_info = vk::SubmitInfo::builder()
                .command_buffers(&cbs)
                .wait_semaphores(&wait_semas)
                .wait_dst_stage_mask(&wait_stages)
                .signal_semaphores(&signal_semas);

            let submits = [submit_info.build()];
            device.queue_submit(present_queue, &submits, frame.render_fence)?;

            // This "helps winit [with stuff]". It also seems to increase latency.
            self.window.pre_present_notify();

            let wait_semas = [frame.render_complete_sema];
            let swapchains = [swapchain.swapchain];
            let image_indices = [swapchain_index];
            let present_info = vk::PresentInfoKHR::builder()
                .wait_semaphores(&wait_semas)
                .swapchains(&swapchains)
                .image_indices(&image_indices);

            self.swapchain_dirty = match self
                .swapchain_loader
                .queue_present(present_queue, &present_info)
            {
                Ok(false) => self.swapchain_dirty,
                Ok(true) => true,
                Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => true,
                Err(e) => return Err(e.into()),
            };
        }

        // Render again!
        if self.swapchain_dirty {
            return self.render();
        }

        Ok(())
    }

    unsafe fn destroy_swapchain(&mut self, mut swapchain: Swapchain) {
        let device = &self.device;
        device.device_wait_idle().unwrap();

        for frame in swapchain.frames.drain(..) {
            device.free_command_buffers(self.present_queue.command_pool, &[frame.render_cb]);
            device.destroy_fence(frame.render_fence, None);
            device.destroy_semaphore(frame.image_acquired_sema, None);
            device.destroy_semaphore(frame.render_complete_sema, None);
        }

        for swap_img in swapchain.present_images.drain(..) {
            // Destroying the swapchain does this.
            // device.destroy_image(swap_img.image, None);
            device.destroy_image_view(swap_img.view, None);
        }

        device.destroy_pipeline_layout(swapchain.pipeline_layout, None);
        device.destroy_descriptor_pool(swapchain.descriptor_pool, None);
        device.destroy_descriptor_set_layout(swapchain.descriptor_set_layout, None);

        device.destroy_pipeline(swapchain.pipeline, None);
        self.swapchain_loader
            .destroy_swapchain(swapchain.swapchain, None)
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe {
            if let Some(swapchain) = self.swapchain.take() {
                self.destroy_swapchain(swapchain);
            }

            self.device
                .destroy_command_pool(self.present_queue.command_pool, None);

            if let Some(debug) = self.debug.take() {
                debug
                    .debug
                    .destroy_debug_utils_messenger(debug.messenger, None);
            }

            if let Some(imgui) = self.imgui.take() {
                drop(imgui);
            }

            self.surface_loader.destroy_surface(self.surface, None);
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

fn main() -> anyhow::Result<()> {
    let event_loop = EventLoop::new()?;
    let window = WindowBuilder::new()
        .with_title("Colorful Triangle")
        .with_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0))
        .build(&event_loop)
        .unwrap();

    let window = Rc::new(window);
    let mut renderer = Renderer::new(window.clone(), cfg!(debug_assertions))?;

    let mut mouse_pressed = false;
    let mut mouse_pos = glam::Vec2::ZERO;

    event_loop.run(move |event, el| {
        renderer.handle_event(&event).expect("resize failed");

        match event {
            Event::AboutToWait { .. } => {
                window.request_redraw();
            }
            Event::WindowEvent { window_id, event } if window_id == window.id() => {
                match event {
                    WindowEvent::CloseRequested
                    | WindowEvent::KeyboardInput {
                        event:
                            KeyEvent {
                                state: ElementState::Pressed,
                                physical_key: PhysicalKey::Code(KeyCode::Escape),
                                ..
                            },
                        ..
                    } => el.exit(),
                    WindowEvent::MouseInput {
                        state,
                        button: MouseButton::Left,
                        ..
                    } => {
                        mouse_pressed = state == ElementState::Pressed;
                    }
                    WindowEvent::CursorMoved { position, .. } => {
                        let phys_size = window.inner_size();
                        let mouse_x = position.x as f32 / phys_size.width as f32 - 0.5;
                        let mouse_y = position.y as f32 / phys_size.height as f32 - 0.5;
                        mouse_pos = glam::Vec2::new(mouse_x, mouse_y);
                    }
                    WindowEvent::RedrawRequested => unsafe {
                        renderer.render().expect("render failed")
                    },
                    _ => (),
                };

                if mouse_pressed {
                    renderer.pc.mouse = mouse_pos;
                }
            }
            _ => (),
        }
    })?;

    Ok(())
}

fn query_device(
    instance: &ash::Instance,
    surface_loader: &SurfaceKhr,
    surface: vk::SurfaceKHR,
    device: vk::PhysicalDevice,
) -> anyhow::Result<DeviceInfo> {
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

    let available_extensions = unsafe {
        instance
            .enumerate_device_extension_properties(device)
            .unwrap()
            .into_iter()
            .map(|properties| CStr::from_ptr(&properties.extension_name as *const _).to_owned())
            .collect::<Vec<_>>()
    };

    let ext_swapchain = SwapchainKhr::name();
    if !available_extensions
        .iter()
        .any(|ext| **ext == *ext_swapchain)
    {
        return Err(anyhow::anyhow!("no swapchain extension found"));
    }

    Ok(DeviceInfo {
        device_name,
        device_type,
        present_family,
    })
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

fn create_fence(device: &ash::Device, signalled: bool) -> Result<vk::Fence, vk::Result> {
    let mut create_info = vk::FenceCreateInfo::builder();
    if signalled {
        create_info = create_info.flags(vk::FenceCreateFlags::SIGNALED);
    }

    let fence = unsafe { device.create_fence(&create_info, None)? };

    Ok(fence)
}

fn create_semaphore(device: &ash::Device) -> Result<vk::Semaphore, vk::Result> {
    let semaphore = unsafe { device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)? };
    Ok(semaphore)
}

#[allow(clippy::too_many_arguments)]
fn cmd_image_barrier(
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

fn load_shader(device: &ash::Device, bytes: &[u8]) -> anyhow::Result<vk::ShaderModule> {
    let code = ash::util::read_spv(&mut std::io::Cursor::new(bytes))?;
    let create_info = vk::ShaderModuleCreateInfo::builder().code(&code);

    let shader = unsafe { device.create_shader_module(&create_info, None)? };

    Ok(shader)
}

fn format_is_srgb(format: vk::Format) -> bool {
    matches!(
        format,
        vk::Format::R8_SRGB
            | vk::Format::R8G8_SRGB
            | vk::Format::R8G8B8_SRGB
            | vk::Format::B8G8R8_SRGB
            | vk::Format::R8G8B8A8_SRGB
            | vk::Format::B8G8R8A8_SRGB
            | vk::Format::A8B8G8R8_SRGB_PACK32
            | vk::Format::BC1_RGB_SRGB_BLOCK
            | vk::Format::BC1_RGBA_SRGB_BLOCK
            | vk::Format::BC2_SRGB_BLOCK
            | vk::Format::BC3_SRGB_BLOCK
            | vk::Format::BC7_SRGB_BLOCK
            | vk::Format::ETC2_R8G8B8_SRGB_BLOCK
            | vk::Format::ETC2_R8G8B8A1_SRGB_BLOCK
            | vk::Format::ETC2_R8G8B8A8_SRGB_BLOCK
            | vk::Format::ASTC_4X4_SRGB_BLOCK
            | vk::Format::ASTC_5X4_SRGB_BLOCK
            | vk::Format::ASTC_5X5_SRGB_BLOCK
            | vk::Format::ASTC_6X5_SRGB_BLOCK
            | vk::Format::ASTC_6X6_SRGB_BLOCK
            | vk::Format::ASTC_8X5_SRGB_BLOCK
            | vk::Format::ASTC_8X6_SRGB_BLOCK
            | vk::Format::ASTC_8X8_SRGB_BLOCK
            | vk::Format::ASTC_10X5_SRGB_BLOCK
            | vk::Format::ASTC_10X6_SRGB_BLOCK
            | vk::Format::ASTC_10X8_SRGB_BLOCK
            | vk::Format::ASTC_10X10_SRGB_BLOCK
            | vk::Format::ASTC_12X10_SRGB_BLOCK
            | vk::Format::ASTC_12X12_SRGB_BLOCK
    )
}

fn colorspace_supported(colorspace: vk::ColorSpaceKHR) -> bool {
    matches!(
        colorspace,
        vk::ColorSpaceKHR::SRGB_NONLINEAR
            | vk::ColorSpaceKHR::EXTENDED_SRGB_LINEAR_EXT
            | vk::ColorSpaceKHR::DISPLAY_P3_LINEAR_EXT
            | vk::ColorSpaceKHR::DISPLAY_P3_NONLINEAR_EXT
            | vk::ColorSpaceKHR::DCI_P3_NONLINEAR_EXT
            | vk::ColorSpaceKHR::BT709_LINEAR_EXT
            | vk::ColorSpaceKHR::BT709_NONLINEAR_EXT
            | vk::ColorSpaceKHR::HDR10_ST2084_EXT
    )
}

unsafe extern "system" fn vulkan_debug_utils_callback(
    _message_severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    message_type: vk::DebugUtilsMessageTypeFlagsEXT,
    p_callback_data: *const vk::DebugUtilsMessengerCallbackDataEXT,
    _userdata: *mut c_void,
) -> vk::Bool32 {
    let _ = std::panic::catch_unwind(|| {
        let message = unsafe { CStr::from_ptr((*p_callback_data).p_message) }.to_string_lossy();
        let ty = format!("{:?}", message_type).to_lowercase();

        eprintln!("VULKAN[{}]: {}", ty, message);
    });

    // Must always return false.
    vk::FALSE
}
