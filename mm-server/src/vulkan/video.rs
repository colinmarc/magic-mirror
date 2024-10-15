// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use ash::prelude::*;
use ash::vk;
use ash::RawPtr;

pub struct VideoQueueExt {
    handle: vk::Device,
    fp: vk::KhrVideoQueueFn,
}

#[allow(dead_code)]
impl VideoQueueExt {
    pub fn new(entry: &ash::Entry, instance: &ash::Instance, device: &ash::Device) -> Self {
        let handle = device.handle();
        let fp = vk::KhrVideoQueueFn::load(|name| unsafe {
            std::mem::transmute(entry.get_instance_proc_addr(instance.handle(), name.as_ptr()))
        });

        Self { handle, fp }
    }

    #[inline]
    pub fn name() -> &'static std::ffi::CStr {
        vk::KhrVideoDecodeQueueFn::NAME
    }

    #[inline]
    /// <https://registry.khronos.org/vulkan/specs/1.3-extensions/man/html/vkBindVideoSessionMemoryKHR.html>
    pub unsafe fn bind_video_session_memory(
        &self,
        device: &ash::Device,
        video_session: vk::VideoSessionKHR,
        bind_session_memory_infos: &[vk::BindVideoSessionMemoryInfoKHR],
    ) -> VkResult<()> {
        (self.fp.bind_video_session_memory_khr)(
            device.handle(),
            video_session,
            bind_session_memory_infos.len() as u32,
            bind_session_memory_infos.as_ptr(),
        )
        .result()
    }

    #[inline]
    /// <https://registry.khronos.org/vulkan/specs/1.3-extensions/man/html/vkCmdBeginVideoCodingKHR.html>
    pub unsafe fn cmd_begin_video_coding(
        &self,
        command_buffer: vk::CommandBuffer,
        begin_info: &vk::VideoBeginCodingInfoKHR,
    ) {
        (self.fp.cmd_begin_video_coding_khr)(command_buffer, begin_info);
    }

    #[inline]
    /// <https://registry.khronos.org/vulkan/specs/1.3-extensions/man/html/vkCmdControlVideoCodingKHR.html>
    pub unsafe fn cmd_control_video_coding(
        &self,
        command_buffer: vk::CommandBuffer,
        coding_control_info: &vk::VideoCodingControlInfoKHR,
    ) {
        (self.fp.cmd_control_video_coding_khr)(command_buffer, coding_control_info);
    }

    #[inline]
    /// <https://registry.khronos.org/vulkan/specs/1.3-extensions/man/html/vkCmdEndVideoCodingKHR.html>
    pub unsafe fn cmd_end_video_coding(
        &self,
        command_buffer: vk::CommandBuffer,
        end_coding_info: &vk::VideoEndCodingInfoKHR,
    ) {
        (self.fp.cmd_end_video_coding_khr)(command_buffer, end_coding_info);
    }

    #[inline]
    /// <https://registry.khronos.org/vulkan/specs/1.3-extensions/man/html/vkCreateVideoSessionKHR.html>
    pub unsafe fn create_video_session(
        &self,
        create_info: &vk::VideoSessionCreateInfoKHR,
        allocation_callbacks: Option<&vk::AllocationCallbacks>,
    ) -> VkResult<vk::VideoSessionKHR> {
        let mut video_session = std::mem::zeroed();
        (self.fp.create_video_session_khr)(
            self.handle,
            create_info,
            allocation_callbacks.as_raw_ptr(),
            &mut video_session,
        )
        .result_with_success(video_session)
    }

    #[inline]
    /// <https://registry.khronos.org/vulkan/specs/1.3-extensions/man/html/vkCreateVideoSessionParametersKHR.html>
    pub unsafe fn create_video_session_parameters(
        &self,
        create_info: &vk::VideoSessionParametersCreateInfoKHR,
        allocation_callbacks: Option<&vk::AllocationCallbacks>,
    ) -> VkResult<vk::VideoSessionParametersKHR> {
        let mut video_session_parameters = std::mem::zeroed();
        (self.fp.create_video_session_parameters_khr)(
            self.handle,
            create_info,
            allocation_callbacks.as_raw_ptr(),
            &mut video_session_parameters,
        )
        .result_with_success(video_session_parameters)
    }

    #[inline]
    /// <https://registry.khronos.org/vulkan/specs/1.3-extensions/man/html/vkDestroyVideoSessionKHR.html>
    pub unsafe fn destroy_video_session(
        &self,
        video_session: vk::VideoSessionKHR,
        allocation_callbacks: Option<&vk::AllocationCallbacks>,
    ) {
        (self.fp.destroy_video_session_khr)(
            self.handle,
            video_session,
            allocation_callbacks.as_raw_ptr(),
        );
    }

    #[inline]
    /// <https://registry.khronos.org/vulkan/specs/1.3-extensions/man/html/vkDestroyVideoSessionParametersKHR.html>
    pub unsafe fn destroy_video_session_parameters(
        &self,
        video_session_parameters: vk::VideoSessionParametersKHR,
        allocation_callbacks: Option<&vk::AllocationCallbacks>,
    ) {
        (self.fp.destroy_video_session_parameters_khr)(
            self.handle,
            video_session_parameters,
            allocation_callbacks.as_raw_ptr(),
        );
    }

    #[inline]
    /// <https://registry.khronos.org/vulkan/specs/1.3-extensions/man/html/vkGetPhysicalDeviceVideoCapabilitiesKHR.html>
    pub unsafe fn get_physical_device_video_capabilities(
        &self,
        physical_device: vk::PhysicalDevice,
        video_profile: &vk::VideoProfileInfoKHR,
        capabilities: &mut vk::VideoCapabilitiesKHR,
    ) -> VkResult<()> {
        (self.fp.get_physical_device_video_capabilities_khr)(
            physical_device,
            video_profile,
            capabilities,
        )
        .result()
    }

    #[inline]
    /// <https://registry.khronos.org/vulkan/specs/1.3-extensions/man/html/vkGetPhysicalDeviceVideoFormatPropertiesKHR.html>
    pub unsafe fn get_physical_device_video_format_properties(
        &self,
        physical_device: vk::PhysicalDevice,
        video_format_info: &vk::PhysicalDeviceVideoFormatInfoKHR,
    ) -> VkResult<Vec<vk::VideoFormatPropertiesKHR>> {
        read_into_defaulted_vector(|count, data| {
            (self.fp.get_physical_device_video_format_properties_khr)(
                physical_device,
                video_format_info,
                count,
                data,
            )
        })
    }

    #[inline]
    /// <https://registry.khronos.org/vulkan/specs/1.3-extensions/man/html/vkGetVideoSessionMemoryRequirementsKHR.html>
    pub unsafe fn get_video_session_memory_requirements(
        &self,
        video_session: vk::VideoSessionKHR,
    ) -> VkResult<Vec<vk::VideoSessionMemoryRequirementsKHR>> {
        read_into_defaulted_vector(|count, data| {
            (self.fp.get_video_session_memory_requirements_khr)(
                self.handle,
                video_session,
                count,
                data,
            )
        })
    }

    #[inline]
    /// <https://registry.khronos.org/vulkan/specs/1.3-extensions/man/html/vkUpdateVideoSessionParametersKHR.html>
    pub unsafe fn update_video_session_parameters(
        &self,
        video_session_parameters: vk::VideoSessionParametersKHR,
        update_info: &vk::VideoSessionParametersUpdateInfoKHR,
    ) -> VkResult<()> {
        (self.fp.update_video_session_parameters_khr)(
            self.handle,
            video_session_parameters,
            update_info,
        )
        .result()
    }
}

pub struct VideoDecodeQueueExt {
    fp: vk::KhrVideoDecodeQueueFn,
}

#[allow(dead_code)]
impl VideoDecodeQueueExt {
    pub fn new(entry: &ash::Entry, instance: &ash::Instance) -> Self {
        let fp = vk::KhrVideoDecodeQueueFn::load(|name| unsafe {
            std::mem::transmute(entry.get_instance_proc_addr(instance.handle(), name.as_ptr()))
        });

        Self { fp }
    }

    #[inline]
    /// <https://registry.khronos.org/vulkan/specs/1.3-extensions/man/html/vkCmdDecodeVideoKHR.html>
    pub unsafe fn cmd_decode_video(
        &self,
        command_buffer: vk::CommandBuffer,
        decode_info: &vk::VideoDecodeInfoKHR,
    ) {
        (self.fp.cmd_decode_video_khr)(command_buffer, decode_info);
    }
}

pub struct VideoEncodeQueueExt {
    handle: vk::Device,
    fp: vk::KhrVideoEncodeQueueFn,
}

#[allow(dead_code)]
impl VideoEncodeQueueExt {
    pub fn new(entry: &ash::Entry, instance: &ash::Instance, device: &ash::Device) -> Self {
        let handle = device.handle();
        let fp = vk::KhrVideoEncodeQueueFn::load(|name| unsafe {
            std::mem::transmute(entry.get_instance_proc_addr(instance.handle(), name.as_ptr()))
        });

        Self { handle, fp }
    }

    #[inline]
    /// <https://registry.khronos.org/vulkan/specs/1.3-extensions/man/html/vkGetPhysicalDeviceVideoEncodeQualityLevelPropertiesKHR.html>
    pub unsafe fn get_physical_device_video_encode_quality_level_properties(
        &self,
        physical_device: vk::PhysicalDevice,
        quality_level_info: &vk::PhysicalDeviceVideoEncodeQualityLevelInfoKHR,
        quality_level_properties: &mut vk::VideoEncodeQualityLevelPropertiesKHR,
    ) -> VkResult<()> {
        (self
            .fp
            .get_physical_device_video_encode_quality_level_properties_khr)(
            physical_device,
            quality_level_info,
            quality_level_properties,
        )
        .result()
    }

    #[inline]
    /// <https://registry.khronos.org/vulkan/specs/1.3-extensions/man/html/vkCmdEncodeVideoKHR.html>
    pub unsafe fn cmd_encode_video(
        &self,
        command_buffer: vk::CommandBuffer,
        encode_info: &vk::VideoEncodeInfoKHR,
    ) {
        (self.fp.cmd_encode_video_khr)(command_buffer, encode_info);
    }

    #[inline]
    /// <https://registry.khronos.org/vulkan/specs/1.3-extensions/man/html/vkGetEncodedVideoSessionParametersKHR.html>
    pub unsafe fn get_encoded_video_session_parameters(
        &self,
        session_parameters_info: &vk::VideoEncodeSessionParametersGetInfoKHR,
        info: &mut vk::VideoEncodeSessionParametersFeedbackInfoKHR,
    ) -> VkResult<Vec<u8>> {
        let ptr = info as *mut _;
        read_into_uninitialized_vector(|count, data: *mut u8| {
            (self.fp.get_encoded_video_session_parameters_khr)(
                self.handle,
                session_parameters_info,
                ptr,
                count,
                data.cast(),
            )
        })
    }
}

// Copied from ash.
/// Repeatedly calls `f` until it does not return [`vk::Result::INCOMPLETE`]
/// anymore, ensuring all available data has been read into the vector.
///
/// See for example [`vkEnumerateInstanceExtensionProperties`]: the number of
/// available items may change between calls; [`vk::Result::INCOMPLETE`] is
/// returned when the count increased (and the vector is not large enough after
/// querying the initial size), requiring Ash to try again.
///
/// [`vkEnumerateInstanceExtensionProperties`]: https://www.khronos.org/registry/vulkan/specs/1.3-extensions/man/html/vkEnumerateInstanceExtensionProperties.html
pub(crate) unsafe fn read_into_uninitialized_vector<N: Copy + Default + TryInto<usize>, T>(
    f: impl Fn(&mut N, *mut T) -> vk::Result,
) -> VkResult<Vec<T>>
where
    <N as TryInto<usize>>::Error: std::fmt::Debug,
{
    loop {
        let mut count = N::default();
        f(&mut count, std::ptr::null_mut()).result()?;
        let mut data =
            Vec::with_capacity(count.try_into().expect("`N` failed to convert to `usize`"));

        let err_code = f(&mut count, data.as_mut_ptr());
        if err_code != vk::Result::INCOMPLETE {
            break err_code.set_vec_len_on_success(
                data,
                count.try_into().expect("`N` failed to convert to `usize`"),
            );
        }
    }
}

/// Repeatedly calls `f` until it does not return [`vk::Result::INCOMPLETE`]
/// anymore, ensuring all available data has been read into the vector.
///
/// Items in the target vector are [`default()`][Default::default()]-initialized
/// which is required for [`vk::BaseOutStructure`]-like structs where
/// [`vk::BaseOutStructure::s_type`] needs to be a valid type and
/// [`vk::BaseOutStructure::p_next`] a valid or [`null`][std::ptr::null_mut()]
/// pointer.
///
/// See for example [`vkEnumerateInstanceExtensionProperties`]: the number of
/// available items may change between calls; [`vk::Result::INCOMPLETE`] is
/// returned when the count increased (and the vector is not large enough after
/// querying the initial size), requiring Ash to try again.
///
/// [`vkEnumerateInstanceExtensionProperties`]: https://www.khronos.org/registry/vulkan/specs/1.3-extensions/man/html/vkEnumerateInstanceExtensionProperties.html
pub(crate) unsafe fn read_into_defaulted_vector<
    N: Copy + Default + TryInto<usize>,
    T: Default + Clone,
>(
    f: impl Fn(&mut N, *mut T) -> vk::Result,
) -> VkResult<Vec<T>>
where
    <N as TryInto<usize>>::Error: std::fmt::Debug,
{
    loop {
        let mut count = N::default();
        f(&mut count, std::ptr::null_mut()).result()?;
        let mut data =
            vec![Default::default(); count.try_into().expect("`N` failed to convert to `usize`")];

        let err_code = f(&mut count, data.as_mut_ptr());
        if err_code != vk::Result::INCOMPLETE {
            data.set_len(count.try_into().expect("`N` failed to convert to `usize`"));
            break err_code.result_with_success(data);
        }
    }
}
