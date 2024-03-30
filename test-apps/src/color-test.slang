// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

struct PushConstants
{
    float2 size;
    float2 mouse;
    float mul;
    int color_space;
};

static const int VK_COLOR_SPACE_SRGB_NONLINEAR_EXT = 0;
static const int VK_COLOR_SPACE_DISPLAY_P3_NONLINEAR_EXT = 1_000_104_001;
static const int VK_COLOR_SPACE_EXTENDED_SRGB_LINEAR_EXT = 1_000_104_002;
static const int VK_COLOR_SPACE_DISPLAY_P3_LINEAR_EXT = 1_000_104_003;
static const int VK_COLOR_SPACE_DCI_P3_NONLINEAR_EXT = 1_000_104_004;
static const int VK_COLOR_SPACE_BT709_LINEAR_EXT = 1_000_104_005;
static const int VK_COLOR_SPACE_BT709_NONLINEAR_EXT = 1_000_104_006;
static const int VK_COLOR_SPACE_BT2020_LINEAR_EXT = 1_000_104_007;
static const int VK_COLOR_SPACE_HDR10_ST2084_EXT = 1_000_104_008;
static const int VK_COLOR_SPACE_HDR10_HLG_EXT = 1_000_104_010;
static const int VK_COLOR_SPACE_EXTENDED_SRGB_NONLINEAR_EXT = 1_000_104_014;

[[vk::push_constant]]
PushConstants pc;

struct VertOutput
{
    float2 uv : TextureCoord;
    float2 mouse : MouseCoord;
    float4 position : SV_Position;
};

[shader("vertex")]
VertOutput vert(uint vertexID: SV_VertexID)
{
    VertOutput output;

    float2 aspect;
    if (pc.size.x > pc.size.y)
    {
        aspect = float2(pc.size.x / pc.size.y, 1.0);
    }
    else
    {
        aspect = float2(1.0, pc.size.y / pc.size.x);
    }

    let uv = float2((vertexID << 1) & 2, vertexID & 2);
    output.uv = uv;
    output.mouse = pc.mouse * aspect;
    output.position = float4((uv * 2.0 - 1.0) / aspect, 0.0, 1.0);
    return output;
}

// Adapted from "Color Wheel", by GoldenCrystal:
// https://www.shadertoy.com/view/MsXXzX
static const float M_PI = 3.14159265358979323846;
static const float AA = 250;

float3 hue(float2 pos)
{
    float theta = 3.0 + 3.0 * atan2(pos.x, pos.y) / M_PI;
    float3 color = float3(0.0);
    return clamp(abs(((theta + float3(0.0, 4.0, 2.0)) % 6.0) - 3.0) - 1.0, 0.0, 1.0);
}

float4 color_wheel(float2 coord, float2 mouse)
{
    float l = length(coord);
    float m = length(mouse);

    float4 color = float4(0.0);

    if (l >= 0.75 && l <= 1.0)
    {
        l = 1.0 - abs((l - 0.875) * 8.0);
        l = clamp(l * AA * 0.0625, 0.0, 1.0); // Antialiasing approximation

        color = float4(l * hue(coord), l);
    }
    else if (l < 0.75)
    {
        float3 picked;

        if (m < 0.75 || m > 1.0)
        {
            mouse = float2(0.0, -1.0);
            picked = float3(1.0, 0.0, 0.0);
        }
        else
        {
            picked = hue(mouse);
        }

        coord = coord / 0.75;
        mouse = normalize(mouse);

        float sat = 1.5 - (dot(coord, mouse) + 0.5); // [0.0,1.5]

        if (sat < 1.5)
        {
            float h = sat / sqrt(3.0);
            float2 om = float2(cross(float3(mouse, 0.0), float3(0.0, 0.0, 1.0)).xy);
            float lum = dot(coord, om);

            if (abs(lum) <= h)
            {
                let l = clamp((h - abs(lum)) * AA * 0.5, 0.0, 1.0) * clamp((1.5 - sat) / 1.5 * AA * 0.5, 0.0, 1.0); // Fake antialiasing
                return float4(l * lerp(picked, float3(0.5 * (lum + h) / h), sat / 1.5), l);
            }
        }
    }

    return color;
}

[shader("fragment")]
float4 frag(
    float2 uv: TextureCoord,
    float2 mouse: MouseCoord)
    : SV_Target
{
    float2 uv = uv.xy * 2.0 - 1.0;
    float2 mouse = mouse * 2.0;

    uv = uv / 0.75;
    mouse = mouse / 0.75;

    let color =  color_wheel(uv, mouse) * pc.mul;
    switch (pc.color_space)
    {
    case VK_COLOR_SPACE_SRGB_NONLINEAR_EXT:
        return delinearize_srgb(color);
    case VK_COLOR_SPACE_DISPLAY_P3_NONLINEAR_EXT:
        return delinearize_srgb(color);
    case VK_COLOR_SPACE_BT709_NONLINEAR_EXT:
        return delinearize_bt709(color);
    case VK_COLOR_SPACE_DCI_P3_NONLINEAR_EXT:
        return delinearize_dci_p3(color);
    case VK_COLOR_SPACE_HDR10_ST2084_EXT:
        return delinearize_pq(color);
    default:
        return color;
    }
}

float4 delinearize_srgb(float4 color)
{
    return float4(
        delinearize_srgb(color.r),
        delinearize_srgb(color.g),
        delinearize_srgb(color.b),
        color.a);
}

float delinearize_srgb(float channel)
{
     return channel > 0.0031308 ? 1.055 * pow(channel, 1.0 / 2.4) - 0.055 : 12.92 * channel;
}

float4 delinearize_bt709(float4 color)
{
    return float4(
        delinearize_bt709(color.r),
        delinearize_bt709(color.g),
        delinearize_bt709(color.b),
        color.a);
}

float delinearize_bt709(float channel)
{
    return channel >= 0.018 ? 1.099 * pow(channel, 1.0 / 2.2) - 0.099 : 4.5 * channel;
}

float4 delinearize_dci_p3(float4 color)
{
    return float4(
        delinearize_dci_p3(color.r),
        delinearize_dci_p3(color.g),
        delinearize_dci_p3(color.b),
        color.a);
}

float delinearize_dci_p3(float channel)
{
    return pow(channel, 1.0/2.6);
}

static const float PQ_M1 = 0.1593017578125;
static const float PQ_M2 = 78.84375;
static const float PQ_C1 = 0.8359375;
static const float PQ_C2 = 18.8515625;
static const float PQ_C3 = 18.6875;

static const float REFERENCE_WHITE = 203.0;

float4 delinearize_pq(float4 color)
{
    return float4(
        delinearize_pq(color.r),
        delinearize_pq(color.g),
        delinearize_pq(color.b),
        color.a);
}

float delinearize_pq(float channel)
{
        let c = pow(channel * REFERENCE_WHITE / 10000.0, PQ_M1);
    return pow(
        (PQ_C1 + PQ_C2 * c) / (1.0 + PQ_C3 * c),
        PQ_M2);
}
