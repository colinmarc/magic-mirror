// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

const Sampler2D texture;

struct VertOutput
{
    float2 uv : TextureCoord;
    float4 position : SV_Position;
};

struct VertPushConstants
{
    float2 src_pos;
    float2 src_size;
    float2 dst_pos;
    float2 dst_size;
};

[[vk::push_constant]]
VertPushConstants vert_pc;

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
    output.position = float4(vert_pc.dst_pos + vert_pc.dst_size * corner, 0.0, 1.0);
    output.uv = vert_pc.src_pos + vert_pc.src_size * corner;
    return output;
}

float srgb_linearize(float x)
{
    if (x > 0.04045)
    {
        return pow((x + 0.055) / 1.055, 2.4);
    }
    else
    {
        return x / 12.92;
    }
}

[shader("fragment")]
float4 frag(float2 uv: TextureCoord)
    : SV_Target
{
    float4 color = texture.Sample(uv);

    // Wayland specifies that textures have premultiplied alpha. If we just
    // import a dmabuf as sRGB, the colors are wrong, since vulkan expects sRGB
    // textures to have not-premultiplied alpha.
    //
    // Vulkan normally expects to do the sRGB -> linear conversion when sampling
    // in the shader. However, we're bypassing that operation here, by importing
    // the texture as UNORM (even though it's stored as sRGB) and then doing the
    // conversion manually.
    //
    // TODO: For imported textures with no alpha channel (XR24), we should skip
    // this and use a sRGB view into the texture instead.
    if (color.a == 0)
    {
        return float4(0);
    }

    color.rgb /= color.a;

    color.rgb = float3(
        srgb_linearize(color.r),
        srgb_linearize(color.g),
        srgb_linearize(color.b));

    color.rgb *= color.a;
    color.a = 1;
    return color;
}

