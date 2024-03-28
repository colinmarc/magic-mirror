// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

struct PushConstants
{
    float2 aspect;
};

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

float srgb_unlinear(float s)
{
    if (s >= 0.0031)
        return 1.055 * pow(s, 1.0 / 2.4) - 0.055;
    else
        return s * 12.92;
}

float bt709_linearize(float s)
{
    if (s > 0.081)
        return pow((s + 0.099) / 1.099, 1.0 / 0.45);
    else
        return s / 4.5;
}

[shader("fragment")]
float4 frag(float2 uv: TextureCoord)
    : SV_Target
{
    float4 color = texture.Sample(uv);

    // When sampling the video texture, vulkan does the matrix multiplication
    // for us, but doesn't apply any transfer function. So we need to convert
    // from the BT.709 transfer function to the sRGB one.
    return float4(
        srgb_unlinear(bt709_linearize(color.r)),
        srgb_unlinear(bt709_linearize(color.g)),
        srgb_unlinear(bt709_linearize(color.b)),
        color.a);
}
