// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

import color;

// Should match the definition in render.rs
enum TextureColorSpace
{
    Bt709 = 0,
    Bt2020Pq = 1,
}

struct PushConstants
{
    float2 aspect;
    TextureColorSpace texture_color_space;
    int vk_color_space;
};

static const int VK_COLOR_SPACE_SRGB_NONLINEAR_EXT = 0;
static const int VK_COLOR_SPACE_EXTENDED_SRGB_LINEAR_EXT = 1_000_104_002;
// static const int VK_COLOR_SPACE_DISPLAY_P3_NONLINEAR_EXT = 1_000_104_001;
static const int VK_COLOR_SPACE_BT709_NONLINEAR_EXT = 1_000_104_006;
static const int VK_COLOR_SPACE_HDR10_ST2084_EXT = 1_000_104_008;

[[vk::push_constant]]
const PushConstants pc;

const Sampler2D texture;

struct VertOutput
{
    float2 uv : TextureCoord;
    float4 position : SV_Position;
};

[shader("vertex")]
VertOutput vert(uint vertexID: SV_VertexID)
{
    VertOutput output;

    let uv = float2((vertexID << 1) & 2, vertexID & 2) / 2.0;
    output.uv = uv;
    output.position = float4((uv * 2.0f - 1.0f) / pc.aspect, 0.0f, 1.0f);
    return output;
}

float3 bt709_to_display(float3 color, int vk_color_space)
{
    if (vk_color_space == VK_COLOR_SPACE_BT709_NONLINEAR_EXT)
    {
        return color;
    }

    let linear = bt709_eotf(color);
    switch (vk_color_space)
    {
    case VK_COLOR_SPACE_SRGB_NONLINEAR_EXT:
        return srgb_inverse_eotf(linear);
    case VK_COLOR_SPACE_EXTENDED_SRGB_LINEAR_EXT:
        return linear;
    // case VK_COLOR_SPACE_DISPLAY_P3_NONLINEAR_EXT:
    //     return srgb_inverse_eotf(transform(color, Primaries::BT709, Primaries::P3))
    case VK_COLOR_SPACE_HDR10_ST2084_EXT:
        return pq_inverse_eotf(transform(linear, Primaries::BT709, Primaries::BT2020));
    default:
        return srgb_inverse_eotf(linear);
    }
}

float3 bt2020_pq_to_display(float3 color, int vk_color_space)
{
    if (vk_color_space == VK_COLOR_SPACE_HDR10_ST2084_EXT)
    {
        return color;
    }

    let linear = transform(pq_eotf(color) * PQ_MAX_WHITE / SDR_REFERENCE_WHITE, Primaries::BT2020, Primaries::BT709);
    switch (vk_color_space)
    {
    case VK_COLOR_SPACE_SRGB_NONLINEAR_EXT:
        return srgb_inverse_eotf(linear);
    case VK_COLOR_SPACE_BT709_NONLINEAR_EXT:
        return bt709_inverse_eotf(clamp(linear, 0.0, 1.0));
    case VK_COLOR_SPACE_EXTENDED_SRGB_LINEAR_EXT:
        return linear;
    // case VK_COLOR_SPACE_DISPLAY_P3_NONLINEAR_EXT:
    //     return srgb_inverse_eotf(transform(color, Primaries::BT2020, Primaries::P3))
    default:
        return srgb_inverse_eotf(linear);
    }
}

[shader("fragment")]
float4 frag(float2 uv: TextureCoord)
    : SV_Target
{
    float4 color = clamp(texture.Sample(uv), 0.0, 1.0);

    // When sampling the video texture, vulkan does the matrix multiplication
    // for us, but doesn't apply any transfer function, so the values are
    // still nonlinear in either BT.709 or BT.2020/ST2048.
    switch (pc.texture_color_space)
    {
    case TextureColorSpace::Bt709:
        return float4(bt709_to_display(color.rgb, pc.vk_color_space), 1.0);
    case TextureColorSpace::Bt2020Pq:
        return float4(bt2020_pq_to_display(color.rgb, pc.vk_color_space), 1.0);
    default:
        return float4(0.0, 0.5, 1.0, 1.0);
    }
}
