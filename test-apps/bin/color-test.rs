// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use std::{
    ffi::{c_void, CStr, CString},
    rc::Rc,
    str::FromStr,
    time,
};

use anyhow::{anyhow, bail, Context};
use ash::{
    extensions::{
        ext::DebugUtils as DebugUtilsExt, khr::DynamicRendering as DynamicRenderingKhr,
        khr::Surface as SurfaceKhr, khr::Swapchain as SwapchainKhr,
    },
    vk,
};
use clap::Parser;
use raw_window_handle::{HasRawDisplayHandle, HasRawWindowHandle};
use winit::{
    event::{ElementState, Event, KeyEvent, WindowEvent},
    event_loop::EventLoop,
    keyboard::{KeyCode, PhysicalKey},
    window::WindowBuilder,
};

#[derive(Debug, Parser)]
#[command(name = "color-test")]
#[command(about = "The Magic Mirror color test app", long_about = None)]
struct Cli {
    /// Select a specific vk::Format.
    #[arg(long)]
    format: Option<VkF>,
    /// Select a specific vk::ColorSpaceKHR.
    #[arg(long)]
    color_space: Option<VkCs>,
}

#[derive(Debug, Clone)]
struct VkF(vk::Format);

#[derive(Debug, Clone)]
struct VkCs(vk::ColorSpaceKHR);

impl FromStr for VkF {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "R4G4_UNORM_PACK8" => Ok(VkF(vk::Format::R4G4_UNORM_PACK8)),
            "R4G4B4A4_UNORM_PACK16" => Ok(VkF(vk::Format::R4G4B4A4_UNORM_PACK16)),
            "B4G4R4A4_UNORM_PACK16" => Ok(VkF(vk::Format::B4G4R4A4_UNORM_PACK16)),
            "R5G6B5_UNORM_PACK16" => Ok(VkF(vk::Format::R5G6B5_UNORM_PACK16)),
            "B5G6R5_UNORM_PACK16" => Ok(VkF(vk::Format::B5G6R5_UNORM_PACK16)),
            "R5G5B5A1_UNORM_PACK16" => Ok(VkF(vk::Format::R5G5B5A1_UNORM_PACK16)),
            "B5G5R5A1_UNORM_PACK16" => Ok(VkF(vk::Format::B5G5R5A1_UNORM_PACK16)),
            "A1R5G5B5_UNORM_PACK16" => Ok(VkF(vk::Format::A1R5G5B5_UNORM_PACK16)),
            "R8_UNORM" => Ok(VkF(vk::Format::R8_UNORM)),
            "R8_SNORM" => Ok(VkF(vk::Format::R8_SNORM)),
            "R8_USCALED" => Ok(VkF(vk::Format::R8_USCALED)),
            "R8_SSCALED" => Ok(VkF(vk::Format::R8_SSCALED)),
            "R8_UINT" => Ok(VkF(vk::Format::R8_UINT)),
            "R8_SINT" => Ok(VkF(vk::Format::R8_SINT)),
            "R8_SRGB" => Ok(VkF(vk::Format::R8_SRGB)),
            "R8G8_UNORM" => Ok(VkF(vk::Format::R8G8_UNORM)),
            "R8G8_SNORM" => Ok(VkF(vk::Format::R8G8_SNORM)),
            "R8G8_USCALED" => Ok(VkF(vk::Format::R8G8_USCALED)),
            "R8G8_SSCALED" => Ok(VkF(vk::Format::R8G8_SSCALED)),
            "R8G8_UINT" => Ok(VkF(vk::Format::R8G8_UINT)),
            "R8G8_SINT" => Ok(VkF(vk::Format::R8G8_SINT)),
            "R8G8_SRGB" => Ok(VkF(vk::Format::R8G8_SRGB)),
            "R8G8B8_UNORM" => Ok(VkF(vk::Format::R8G8B8_UNORM)),
            "R8G8B8_SNORM" => Ok(VkF(vk::Format::R8G8B8_SNORM)),
            "R8G8B8_USCALED" => Ok(VkF(vk::Format::R8G8B8_USCALED)),
            "R8G8B8_SSCALED" => Ok(VkF(vk::Format::R8G8B8_SSCALED)),
            "R8G8B8_UINT" => Ok(VkF(vk::Format::R8G8B8_UINT)),
            "R8G8B8_SINT" => Ok(VkF(vk::Format::R8G8B8_SINT)),
            "R8G8B8_SRGB" => Ok(VkF(vk::Format::R8G8B8_SRGB)),
            "B8G8R8_UNORM" => Ok(VkF(vk::Format::B8G8R8_UNORM)),
            "B8G8R8_SNORM" => Ok(VkF(vk::Format::B8G8R8_SNORM)),
            "B8G8R8_USCALED" => Ok(VkF(vk::Format::B8G8R8_USCALED)),
            "B8G8R8_SSCALED" => Ok(VkF(vk::Format::B8G8R8_SSCALED)),
            "B8G8R8_UINT" => Ok(VkF(vk::Format::B8G8R8_UINT)),
            "B8G8R8_SINT" => Ok(VkF(vk::Format::B8G8R8_SINT)),
            "B8G8R8_SRGB" => Ok(VkF(vk::Format::B8G8R8_SRGB)),
            "R8G8B8A8_UNORM" => Ok(VkF(vk::Format::R8G8B8A8_UNORM)),
            "R8G8B8A8_SNORM" => Ok(VkF(vk::Format::R8G8B8A8_SNORM)),
            "R8G8B8A8_USCALED" => Ok(VkF(vk::Format::R8G8B8A8_USCALED)),
            "R8G8B8A8_SSCALED" => Ok(VkF(vk::Format::R8G8B8A8_SSCALED)),
            "R8G8B8A8_UINT" => Ok(VkF(vk::Format::R8G8B8A8_UINT)),
            "R8G8B8A8_SINT" => Ok(VkF(vk::Format::R8G8B8A8_SINT)),
            "R8G8B8A8_SRGB" => Ok(VkF(vk::Format::R8G8B8A8_SRGB)),
            "B8G8R8A8_UNORM" => Ok(VkF(vk::Format::B8G8R8A8_UNORM)),
            "B8G8R8A8_SNORM" => Ok(VkF(vk::Format::B8G8R8A8_SNORM)),
            "B8G8R8A8_USCALED" => Ok(VkF(vk::Format::B8G8R8A8_USCALED)),
            "B8G8R8A8_SSCALED" => Ok(VkF(vk::Format::B8G8R8A8_SSCALED)),
            "B8G8R8A8_UINT" => Ok(VkF(vk::Format::B8G8R8A8_UINT)),
            "B8G8R8A8_SINT" => Ok(VkF(vk::Format::B8G8R8A8_SINT)),
            "B8G8R8A8_SRGB" => Ok(VkF(vk::Format::B8G8R8A8_SRGB)),
            "A8B8G8R8_UNORM_PACK32" => Ok(VkF(vk::Format::A8B8G8R8_UNORM_PACK32)),
            "A8B8G8R8_SNORM_PACK32" => Ok(VkF(vk::Format::A8B8G8R8_SNORM_PACK32)),
            "A8B8G8R8_USCALED_PACK32" => Ok(VkF(vk::Format::A8B8G8R8_USCALED_PACK32)),
            "A8B8G8R8_SSCALED_PACK32" => Ok(VkF(vk::Format::A8B8G8R8_SSCALED_PACK32)),
            "A8B8G8R8_UINT_PACK32" => Ok(VkF(vk::Format::A8B8G8R8_UINT_PACK32)),
            "A8B8G8R8_SINT_PACK32" => Ok(VkF(vk::Format::A8B8G8R8_SINT_PACK32)),
            "A8B8G8R8_SRGB_PACK32" => Ok(VkF(vk::Format::A8B8G8R8_SRGB_PACK32)),
            "A2R10G10B10_UNORM_PACK32" => Ok(VkF(vk::Format::A2R10G10B10_UNORM_PACK32)),
            "A2R10G10B10_SNORM_PACK32" => Ok(VkF(vk::Format::A2R10G10B10_SNORM_PACK32)),
            "A2R10G10B10_USCALED_PACK32" => Ok(VkF(vk::Format::A2R10G10B10_USCALED_PACK32)),
            "A2R10G10B10_SSCALED_PACK32" => Ok(VkF(vk::Format::A2R10G10B10_SSCALED_PACK32)),
            "A2R10G10B10_UINT_PACK32" => Ok(VkF(vk::Format::A2R10G10B10_UINT_PACK32)),
            "A2R10G10B10_SINT_PACK32" => Ok(VkF(vk::Format::A2R10G10B10_SINT_PACK32)),
            "A2B10G10R10_UNORM_PACK32" => Ok(VkF(vk::Format::A2B10G10R10_UNORM_PACK32)),
            "A2B10G10R10_SNORM_PACK32" => Ok(VkF(vk::Format::A2B10G10R10_SNORM_PACK32)),
            "A2B10G10R10_USCALED_PACK32" => Ok(VkF(vk::Format::A2B10G10R10_USCALED_PACK32)),
            "A2B10G10R10_SSCALED_PACK32" => Ok(VkF(vk::Format::A2B10G10R10_SSCALED_PACK32)),
            "A2B10G10R10_UINT_PACK32" => Ok(VkF(vk::Format::A2B10G10R10_UINT_PACK32)),
            "A2B10G10R10_SINT_PACK32" => Ok(VkF(vk::Format::A2B10G10R10_SINT_PACK32)),
            "R16_UNORM" => Ok(VkF(vk::Format::R16_UNORM)),
            "R16_SNORM" => Ok(VkF(vk::Format::R16_SNORM)),
            "R16_USCALED" => Ok(VkF(vk::Format::R16_USCALED)),
            "R16_SSCALED" => Ok(VkF(vk::Format::R16_SSCALED)),
            "R16_UINT" => Ok(VkF(vk::Format::R16_UINT)),
            "R16_SINT" => Ok(VkF(vk::Format::R16_SINT)),
            "R16_SFLOAT" => Ok(VkF(vk::Format::R16_SFLOAT)),
            "R16G16_UNORM" => Ok(VkF(vk::Format::R16G16_UNORM)),
            "R16G16_SNORM" => Ok(VkF(vk::Format::R16G16_SNORM)),
            "R16G16_USCALED" => Ok(VkF(vk::Format::R16G16_USCALED)),
            "R16G16_SSCALED" => Ok(VkF(vk::Format::R16G16_SSCALED)),
            "R16G16_UINT" => Ok(VkF(vk::Format::R16G16_UINT)),
            "R16G16_SINT" => Ok(VkF(vk::Format::R16G16_SINT)),
            "R16G16_SFLOAT" => Ok(VkF(vk::Format::R16G16_SFLOAT)),
            "R16G16B16_UNORM" => Ok(VkF(vk::Format::R16G16B16_UNORM)),
            "R16G16B16_SNORM" => Ok(VkF(vk::Format::R16G16B16_SNORM)),
            "R16G16B16_USCALED" => Ok(VkF(vk::Format::R16G16B16_USCALED)),
            "R16G16B16_SSCALED" => Ok(VkF(vk::Format::R16G16B16_SSCALED)),
            "R16G16B16_UINT" => Ok(VkF(vk::Format::R16G16B16_UINT)),
            "R16G16B16_SINT" => Ok(VkF(vk::Format::R16G16B16_SINT)),
            "R16G16B16_SFLOAT" => Ok(VkF(vk::Format::R16G16B16_SFLOAT)),
            "R16G16B16A16_UNORM" => Ok(VkF(vk::Format::R16G16B16A16_UNORM)),
            "R16G16B16A16_SNORM" => Ok(VkF(vk::Format::R16G16B16A16_SNORM)),
            "R16G16B16A16_USCALED" => Ok(VkF(vk::Format::R16G16B16A16_USCALED)),
            "R16G16B16A16_SSCALED" => Ok(VkF(vk::Format::R16G16B16A16_SSCALED)),
            "R16G16B16A16_UINT" => Ok(VkF(vk::Format::R16G16B16A16_UINT)),
            "R16G16B16A16_SINT" => Ok(VkF(vk::Format::R16G16B16A16_SINT)),
            "R16G16B16A16_SFLOAT" => Ok(VkF(vk::Format::R16G16B16A16_SFLOAT)),
            "R32_UINT" => Ok(VkF(vk::Format::R32_UINT)),
            "R32_SINT" => Ok(VkF(vk::Format::R32_SINT)),
            "R32_SFLOAT" => Ok(VkF(vk::Format::R32_SFLOAT)),
            "R32G32_UINT" => Ok(VkF(vk::Format::R32G32_UINT)),
            "R32G32_SINT" => Ok(VkF(vk::Format::R32G32_SINT)),
            "R32G32_SFLOAT" => Ok(VkF(vk::Format::R32G32_SFLOAT)),
            "R32G32B32_UINT" => Ok(VkF(vk::Format::R32G32B32_UINT)),
            "R32G32B32_SINT" => Ok(VkF(vk::Format::R32G32B32_SINT)),
            "R32G32B32_SFLOAT" => Ok(VkF(vk::Format::R32G32B32_SFLOAT)),
            "R32G32B32A32_UINT" => Ok(VkF(vk::Format::R32G32B32A32_UINT)),
            "R32G32B32A32_SINT" => Ok(VkF(vk::Format::R32G32B32A32_SINT)),
            "R32G32B32A32_SFLOAT" => Ok(VkF(vk::Format::R32G32B32A32_SFLOAT)),
            "R64_UINT" => Ok(VkF(vk::Format::R64_UINT)),
            "R64_SINT" => Ok(VkF(vk::Format::R64_SINT)),
            "R64_SFLOAT" => Ok(VkF(vk::Format::R64_SFLOAT)),
            "R64G64_UINT" => Ok(VkF(vk::Format::R64G64_UINT)),
            "R64G64_SINT" => Ok(VkF(vk::Format::R64G64_SINT)),
            "R64G64_SFLOAT" => Ok(VkF(vk::Format::R64G64_SFLOAT)),
            "R64G64B64_UINT" => Ok(VkF(vk::Format::R64G64B64_UINT)),
            "R64G64B64_SINT" => Ok(VkF(vk::Format::R64G64B64_SINT)),
            "R64G64B64_SFLOAT" => Ok(VkF(vk::Format::R64G64B64_SFLOAT)),
            "R64G64B64A64_UINT" => Ok(VkF(vk::Format::R64G64B64A64_UINT)),
            "R64G64B64A64_SINT" => Ok(VkF(vk::Format::R64G64B64A64_SINT)),
            "R64G64B64A64_SFLOAT" => Ok(VkF(vk::Format::R64G64B64A64_SFLOAT)),
            "B10G11R11_UFLOAT_PACK32" => Ok(VkF(vk::Format::B10G11R11_UFLOAT_PACK32)),
            "E5B9G9R9_UFLOAT_PACK32" => Ok(VkF(vk::Format::E5B9G9R9_UFLOAT_PACK32)),
            "D16_UNORM" => Ok(VkF(vk::Format::D16_UNORM)),
            "X8_D24_UNORM_PACK32" => Ok(VkF(vk::Format::X8_D24_UNORM_PACK32)),
            "D32_SFLOAT" => Ok(VkF(vk::Format::D32_SFLOAT)),
            "S8_UINT" => Ok(VkF(vk::Format::S8_UINT)),
            "D16_UNORM_S8_UINT" => Ok(VkF(vk::Format::D16_UNORM_S8_UINT)),
            "D24_UNORM_S8_UINT" => Ok(VkF(vk::Format::D24_UNORM_S8_UINT)),
            "D32_SFLOAT_S8_UINT" => Ok(VkF(vk::Format::D32_SFLOAT_S8_UINT)),
            "BC1_RGB_UNORM_BLOCK" => Ok(VkF(vk::Format::BC1_RGB_UNORM_BLOCK)),
            "BC1_RGB_SRGB_BLOCK" => Ok(VkF(vk::Format::BC1_RGB_SRGB_BLOCK)),
            "BC1_RGBA_UNORM_BLOCK" => Ok(VkF(vk::Format::BC1_RGBA_UNORM_BLOCK)),
            "BC1_RGBA_SRGB_BLOCK" => Ok(VkF(vk::Format::BC1_RGBA_SRGB_BLOCK)),
            "BC2_UNORM_BLOCK" => Ok(VkF(vk::Format::BC2_UNORM_BLOCK)),
            "BC2_SRGB_BLOCK" => Ok(VkF(vk::Format::BC2_SRGB_BLOCK)),
            "BC3_UNORM_BLOCK" => Ok(VkF(vk::Format::BC3_UNORM_BLOCK)),
            "BC3_SRGB_BLOCK" => Ok(VkF(vk::Format::BC3_SRGB_BLOCK)),
            "BC4_UNORM_BLOCK" => Ok(VkF(vk::Format::BC4_UNORM_BLOCK)),
            "BC4_SNORM_BLOCK" => Ok(VkF(vk::Format::BC4_SNORM_BLOCK)),
            "BC5_UNORM_BLOCK" => Ok(VkF(vk::Format::BC5_UNORM_BLOCK)),
            "BC5_SNORM_BLOCK" => Ok(VkF(vk::Format::BC5_SNORM_BLOCK)),
            "BC6H_UFLOAT_BLOCK" => Ok(VkF(vk::Format::BC6H_UFLOAT_BLOCK)),
            "BC6H_SFLOAT_BLOCK" => Ok(VkF(vk::Format::BC6H_SFLOAT_BLOCK)),
            "BC7_UNORM_BLOCK" => Ok(VkF(vk::Format::BC7_UNORM_BLOCK)),
            "BC7_SRGB_BLOCK" => Ok(VkF(vk::Format::BC7_SRGB_BLOCK)),
            "ETC2_R8G8B8_UNORM_BLOCK" => Ok(VkF(vk::Format::ETC2_R8G8B8_UNORM_BLOCK)),
            "ETC2_R8G8B8_SRGB_BLOCK" => Ok(VkF(vk::Format::ETC2_R8G8B8_SRGB_BLOCK)),
            "ETC2_R8G8B8A1_UNORM_BLOCK" => Ok(VkF(vk::Format::ETC2_R8G8B8A1_UNORM_BLOCK)),
            "ETC2_R8G8B8A1_SRGB_BLOCK" => Ok(VkF(vk::Format::ETC2_R8G8B8A1_SRGB_BLOCK)),
            "ETC2_R8G8B8A8_UNORM_BLOCK" => Ok(VkF(vk::Format::ETC2_R8G8B8A8_UNORM_BLOCK)),
            "ETC2_R8G8B8A8_SRGB_BLOCK" => Ok(VkF(vk::Format::ETC2_R8G8B8A8_SRGB_BLOCK)),
            "EAC_R11_UNORM_BLOCK" => Ok(VkF(vk::Format::EAC_R11_UNORM_BLOCK)),
            "EAC_R11_SNORM_BLOCK" => Ok(VkF(vk::Format::EAC_R11_SNORM_BLOCK)),
            "EAC_R11G11_UNORM_BLOCK" => Ok(VkF(vk::Format::EAC_R11G11_UNORM_BLOCK)),
            "EAC_R11G11_SNORM_BLOCK" => Ok(VkF(vk::Format::EAC_R11G11_SNORM_BLOCK)),
            "ASTC_4X4_UNORM_BLOCK" => Ok(VkF(vk::Format::ASTC_4X4_UNORM_BLOCK)),
            "ASTC_4X4_SRGB_BLOCK" => Ok(VkF(vk::Format::ASTC_4X4_SRGB_BLOCK)),
            "ASTC_5X4_UNORM_BLOCK" => Ok(VkF(vk::Format::ASTC_5X4_UNORM_BLOCK)),
            "ASTC_5X4_SRGB_BLOCK" => Ok(VkF(vk::Format::ASTC_5X4_SRGB_BLOCK)),
            "ASTC_5X5_UNORM_BLOCK" => Ok(VkF(vk::Format::ASTC_5X5_UNORM_BLOCK)),
            "ASTC_5X5_SRGB_BLOCK" => Ok(VkF(vk::Format::ASTC_5X5_SRGB_BLOCK)),
            "ASTC_6X5_UNORM_BLOCK" => Ok(VkF(vk::Format::ASTC_6X5_UNORM_BLOCK)),
            "ASTC_6X5_SRGB_BLOCK" => Ok(VkF(vk::Format::ASTC_6X5_SRGB_BLOCK)),
            "ASTC_6X6_UNORM_BLOCK" => Ok(VkF(vk::Format::ASTC_6X6_UNORM_BLOCK)),
            "ASTC_6X6_SRGB_BLOCK" => Ok(VkF(vk::Format::ASTC_6X6_SRGB_BLOCK)),
            "ASTC_8X5_UNORM_BLOCK" => Ok(VkF(vk::Format::ASTC_8X5_UNORM_BLOCK)),
            "ASTC_8X5_SRGB_BLOCK" => Ok(VkF(vk::Format::ASTC_8X5_SRGB_BLOCK)),
            "ASTC_8X6_UNORM_BLOCK" => Ok(VkF(vk::Format::ASTC_8X6_UNORM_BLOCK)),
            "ASTC_8X6_SRGB_BLOCK" => Ok(VkF(vk::Format::ASTC_8X6_SRGB_BLOCK)),
            "ASTC_8X8_UNORM_BLOCK" => Ok(VkF(vk::Format::ASTC_8X8_UNORM_BLOCK)),
            "ASTC_8X8_SRGB_BLOCK" => Ok(VkF(vk::Format::ASTC_8X8_SRGB_BLOCK)),
            "ASTC_10X5_UNORM_BLOCK" => Ok(VkF(vk::Format::ASTC_10X5_UNORM_BLOCK)),
            "ASTC_10X5_SRGB_BLOCK" => Ok(VkF(vk::Format::ASTC_10X5_SRGB_BLOCK)),
            "ASTC_10X6_UNORM_BLOCK" => Ok(VkF(vk::Format::ASTC_10X6_UNORM_BLOCK)),
            "ASTC_10X6_SRGB_BLOCK" => Ok(VkF(vk::Format::ASTC_10X6_SRGB_BLOCK)),
            "ASTC_10X8_UNORM_BLOCK" => Ok(VkF(vk::Format::ASTC_10X8_UNORM_BLOCK)),
            "ASTC_10X8_SRGB_BLOCK" => Ok(VkF(vk::Format::ASTC_10X8_SRGB_BLOCK)),
            "ASTC_10X10_UNORM_BLOCK" => Ok(VkF(vk::Format::ASTC_10X10_UNORM_BLOCK)),
            "ASTC_10X10_SRGB_BLOCK" => Ok(VkF(vk::Format::ASTC_10X10_SRGB_BLOCK)),
            "ASTC_12X10_UNORM_BLOCK" => Ok(VkF(vk::Format::ASTC_12X10_UNORM_BLOCK)),
            "ASTC_12X10_SRGB_BLOCK" => Ok(VkF(vk::Format::ASTC_12X10_SRGB_BLOCK)),
            "ASTC_12X12_UNORM_BLOCK" => Ok(VkF(vk::Format::ASTC_12X12_UNORM_BLOCK)),
            "ASTC_12X12_SRGB_BLOCK" => Ok(VkF(vk::Format::ASTC_12X12_SRGB_BLOCK)),
            _ => Err(anyhow::anyhow!("Unknown format: {}", s)),
        }
    }
}

impl FromStr for VkCs {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "SRGB_NONLINEAR" => Ok(VkCs(vk::ColorSpaceKHR::SRGB_NONLINEAR)),
            "DISPLAY_P3_NONLINEAR_EXT" => Ok(VkCs(vk::ColorSpaceKHR::DISPLAY_P3_NONLINEAR_EXT)),
            "EXTENDED_SRGB_LINEAR_EXT" => Ok(VkCs(vk::ColorSpaceKHR::EXTENDED_SRGB_LINEAR_EXT)),
            "DISPLAY_P3_LINEAR_EXT" => Ok(VkCs(vk::ColorSpaceKHR::DISPLAY_P3_LINEAR_EXT)),
            "DCI_P3_NONLINEAR_EXT" => Ok(VkCs(vk::ColorSpaceKHR::DCI_P3_NONLINEAR_EXT)),
            "BT709_LINEAR_EXT" => Ok(VkCs(vk::ColorSpaceKHR::BT709_LINEAR_EXT)),
            "BT709_NONLINEAR_EXT" => Ok(VkCs(vk::ColorSpaceKHR::BT709_NONLINEAR_EXT)),
            "BT2020_LINEAR_EXT" => Ok(VkCs(vk::ColorSpaceKHR::BT2020_LINEAR_EXT)),
            "HDR10_ST2084_EXT" => Ok(VkCs(vk::ColorSpaceKHR::HDR10_ST2084_EXT)),
            "DOLBYVISION_EXT" => Ok(VkCs(vk::ColorSpaceKHR::DOLBYVISION_EXT)),
            "HDR10_HLG_EXT" => Ok(VkCs(vk::ColorSpaceKHR::HDR10_HLG_EXT)),
            "ADOBERGB_LINEAR_EXT" => Ok(VkCs(vk::ColorSpaceKHR::ADOBERGB_LINEAR_EXT)),
            "ADOBERGB_NONLINEAR_EXT" => Ok(VkCs(vk::ColorSpaceKHR::ADOBERGB_NONLINEAR_EXT)),
            "PASS_THROUGH_EXT" => Ok(VkCs(vk::ColorSpaceKHR::PASS_THROUGH_EXT)),
            "EXTENDED_SRGB_NONLINEAR_EXT" => {
                Ok(VkCs(vk::ColorSpaceKHR::EXTENDED_SRGB_NONLINEAR_EXT))
            }
            _ => Err(anyhow::anyhow!("Unknown color space: {}", s)),
        }
    }
}

#[derive(Copy, Clone, Debug)]
#[repr(C)]
struct PushConstants {
    colors: glam::Vec4,
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
    surface_format_idx: usize,
    pc: PushConstants,
    present_queue: VkQueue,
    width: u32,
    height: u32,

    window: Rc<winit::window::Window>,

    swapchain: Option<Swapchain>,
    swapchain_dirty: bool,
}

struct Swapchain {
    swapchain: vk::SwapchainKHR,
    frames: Vec<InFlightFrame>,
    present_images: Vec<SwapImage>,
    current_frame: usize,

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
    fn new(
        window: Rc<winit::window::Window>,
        debug: bool,
        format: Option<vk::Format>,
        color_space: Option<vk::ColorSpaceKHR>,
    ) -> anyhow::Result<Self> {
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

        devices.sort_by_key(|(_, _, info)| {
            let score = match info.device_type {
                vk::PhysicalDeviceType::DISCRETE_GPU => 0,
                vk::PhysicalDeviceType::INTEGRATED_GPU => 1,
                _ => 2,
            };
            score
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

            let selected_extensions = vec![
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

        let surface_format = surface_formats.iter().position(|sf| {
            (Some(sf.format) == format || format.is_none())
                && (Some(sf.color_space) == color_space || color_space.is_none())
        });

        let surface_format_idx = match surface_format {
            Some(idx) => idx,
            None if format.is_some() || color_space.is_some() => bail!(
                "no matching surface format found for {:?} / {:?}",
                format,
                color_space
            ),
            None => 0,
        };

        let swapchain_loader = SwapchainKhr::new(&instance, &device);
        let dynamic_rendering_loader = DynamicRenderingKhr::new(&instance, &device);

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
            surface_format_idx,
            pc: PushConstants {
                colors: glam::Vec4::new(1.0, 1.0, 1.0, 1.0),
            },
            present_queue,
            width: window_size.width,
            height: window_size.height,
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

        let surface_format = self.surface_formats[self.surface_format_idx];

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
                .stage_flags(vk::ShaderStageFlags::VERTEX)
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
            let vert_bytes = include_bytes!(concat!(env!("OUT_DIR"), "/shaders/vert.spv"));
            let frag_bytes = include_bytes!(concat!(env!("OUT_DIR"), "/shaders/frag.spv"));
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

        let swapchain = Swapchain {
            swapchain,
            frames,
            present_images: swapchain_images,
            current_frame: 0,

            descriptor_pool,
            descriptor_set_layout,
            pipeline_layout,
            pipeline,
        };

        eprintln!("recreated swapchain in {:?}", start.elapsed());

        if let Some(old_swapchain) = self.swapchain.replace(swapchain) {
            self.destroy_swapchain(old_swapchain);
        };

        Ok(())
    }

    fn handle_event<T>(&mut self, event: &winit::event::Event<T>) -> anyhow::Result<()> {
        match event {
            winit::event::Event::WindowEvent { window_id, event }
                if *window_id == self.window.id() =>
            {
                match event {
                    winit::event::WindowEvent::Resized(size) => {
                        self.resize(size.width, size.height);
                    }
                    _ => (),
                }
            }
            _ => (),
        }

        Ok(())
    }

    fn next_format(&mut self) -> vk::SurfaceFormatKHR {
        self.surface_format_idx = (self.surface_format_idx + 1) % self.surface_formats.len();
        self.swapchain_dirty = true;
        self.surface_formats[self.surface_format_idx]
    }

    fn prev_format(&mut self) -> vk::SurfaceFormatKHR {
        self.surface_format_idx =
            (self.surface_format_idx + self.surface_formats.len() - 1) % self.surface_formats.len();
        self.swapchain_dirty = true;
        self.surface_formats[self.surface_format_idx]
    }

    fn toggle_color(&mut self, idx: usize) {
        self.pc.colors[idx] = if self.pc.colors[idx] != 0.0 { 0.0 } else { 1.0 };
        self.swapchain_dirty = true;
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
            vk::ShaderStageFlags::VERTEX,
            0,
            std::slice::from_raw_parts(
                &self.pc as *const _ as *const u8,
                std::mem::size_of::<PushConstants>(),
            ),
        );

        // Draw the triangle.
        device.cmd_draw(frame.render_cb, 3, 1, 0, 0);

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
                Ok(false) => false,
                Ok(true) => true,
                Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => true,
                Err(e) => return Err(e.into()),
            };
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

            self.surface_loader.destroy_surface(self.surface, None);
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

fn main() -> anyhow::Result<()> {
    let args = Cli::parse();

    let event_loop = EventLoop::new()?;
    let window = WindowBuilder::new()
        .with_title("Colorful Triangle")
        .with_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0))
        .build(&event_loop)
        .unwrap();

    let window = Rc::new(window);
    let mut renderer = Renderer::new(
        window.clone(),
        cfg!(debug_assertions),
        args.format.map(|f| f.0),
        args.color_space.map(|c| c.0),
    )?;

    let f = renderer.surface_formats[renderer.surface_format_idx];
    window.set_title(&format!(
        "Colorful Triangle ({:?} / {:?})",
        f.format, f.color_space
    ));

    event_loop.run(move |event, el| {
        renderer.handle_event(&event).expect("resize failed");

        match event {
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
                    WindowEvent::KeyboardInput {
                        event:
                            KeyEvent {
                                state: ElementState::Pressed,
                                physical_key,
                                ..
                            },
                        ..
                    } => {
                        let f = match physical_key {
                            PhysicalKey::Code(KeyCode::ArrowRight) => Some(renderer.next_format()),
                            PhysicalKey::Code(KeyCode::ArrowLeft) => Some(renderer.prev_format()),
                            PhysicalKey::Code(KeyCode::Digit1) => {
                                renderer.toggle_color(0);
                                None
                            }
                            PhysicalKey::Code(KeyCode::Digit2) => {
                                renderer.toggle_color(1);
                                None
                            }
                            PhysicalKey::Code(KeyCode::Digit3) => {
                                renderer.toggle_color(2);
                                None
                            }
                            _ => None,
                        };

                        if let Some(f) = f {
                            window.set_title(&format!(
                                "Colorful Triangle ({:?} / {:?})",
                                f.format, f.color_space
                            ));
                        }

                        window.request_redraw();
                    }
                    WindowEvent::RedrawRequested => unsafe {
                        renderer.render().expect("render failed")
                    },
                    _ => (),
                };
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
