// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

import color;

const Sampler2D texture;

struct VertOutput
{
    float2 uv : TextureCoord;
    float4 position : SV_Position;
};

// This must match the enum in composite.rs.
enum InputTextureColorSpace
{
    SRGB = 1,
    LINEAR_EXTENDED_SRGB = 2,
    HDR10 = 3,
}

struct PushConstants
{
    float2 src_pos;
    float2 src_size;
    float2 dst_pos;
    float2 dst_size;

    InputTextureColorSpace color_space;
};

[[vk::push_constant]]
PushConstants pc;

[shader("vertex")]
VertOutput vert(uint vid: SV_VertexID)
{
    float2 corner;
    switch (vid % 4)
    {
    case 0:
        corner = float2(0.0, 0.0);
        break;
    case 1:
        corner = float2(1.0, 0.0);
        break;
    case 2:
        corner = float2(0.0, 1.0);
        break;
    case 3:
        corner = float2(1.0, 1.0);
        break;
    }

    VertOutput output;
    output.position = float4(pc.dst_pos + pc.dst_size * corner, 0.0, 1.0);
    output.uv = pc.src_pos + pc.src_size * corner;
    return output;
}

[shader("fragment")]
float4 frag(float2 uv: TextureCoord)
    : SV_Target
{
    float4 color = texture.Sample(uv);

    // Wayland specifies that textures have premultiplied alpha. If we just
    // import a dmabuf as as an _SRGB format, the colors are wrong, since vulkan
    // expects sRGB textures to have not-premultiplied alpha.
    //
    // Vulkan normally expects to do the sRGB -> linear conversion when sampling
    // in the shader. However, we're bypassing that operation here, by importing
    // the texture as UNORM (even though it's stored as sRGB) and then doing the
    // conversion manually.
    if (color.a == 0)
    {
        return float4(0);
    }
    else if (pc.color_space == InputTextureColorSpace::LINEAR_EXTENDED_SRGB)
    {
        // We're already in the right space for blending.
        return color;
    }

    color.rgb /= color.a;

    switch (pc.color_space)
    {
    case InputTextureColorSpace::SRGB:
        color.rgb = apply_eotf(color.rgb, TransferFunction::SRGB);
        break;
    case InputTextureColorSpace::HDR10:
        float3 linear = apply_eotf(color.rgb, TransferFunction::PQ);

        // The resulting values have the range 0-1, where 1.0 corresponds 10,000
        // nits. In order to effectively blend with SDR textures, we need to
        // scale based on our virtual display brightness, producing values where
        // 1.0 matches the maximum brightness that SDR content would produce. We
        // use the Rec. 2408 value of 203 nits for this. On this scale, a value
        // of 300 nits would result in a scaled value of about 1.47, and 1.0
        // would result in about 49.26. Either value would be clipped unless we
        // use a floating-point blend format (which we do).
        // TODO: allow HDR metadata to override the scaling factor. This is called
        // "nominal diffuse white level" or NDWL.
        linear = scale_pq_to_sdr(linear, SDR_REFERENCE_WHITE);

        // Transform from the BT.2020 primaries to the BT.709 primaries.
        color.rgb = transform(linear, Primaries::BT2020, Primaries::BT709);
        break;
    }

    color.rgb *= color.a;
    return color;
}

