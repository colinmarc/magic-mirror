// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

import color;

const Sampler2D blend_image;

const RWTexture2D<float> luminance;

#ifdef SEMIPLANAR
const RWTexture2D<float2> chroma_uv;
#else
const RWTexture2D<float> chroma_u;
const RWTexture2D<float> chroma_v;
#endif

// This must match the enum in convert.rs.
enum InputTextureColorSpace
{
    SRGB = 0,
    LINEAR_EXTENDED_SRGB = 1,
    HDR10 = 2,
}

/// This must match the enum in convert.rs.
enum OutputProfile
{
    HD = 0,
    HDR10 = 1,
}

struct PushConstants
{
    InputTextureColorSpace input_color_space;
    OutputProfile output_profile;
}

[[vk::push_constant]]
PushConstants pc;

float3 to_bt709(float3 rgb, InputTextureColorSpace color_space)
{
    float3 linear;
    switch (color_space)
    {
    case InputTextureColorSpace::SRGB:
        linear = srgb_eotf(rgb);
        break;
    case InputTextureColorSpace::HDR10:
    {
        // Treat 203 nits as 1.0, and clip everything above that.
        linear = pq_eotf(rgb);
        linear = clamp(linear * (PQ_MAX_WHITE / SDR_REFERENCE_WHITE), 0.0, 1.0);
        break;
    }
    case InputTextureColorSpace::LINEAR_EXTENDED_SRGB:
        linear = clamp(rgb, 0.0, 1.0);
        break;
    }

    return bt709_inverse_eotf(linear);
}

float3 to_bt2020_pq(float3 rgb, InputTextureColorSpace color_space)
{
    float3 bt2020_linear;
    switch (color_space)
    {
    case InputTextureColorSpace::SRGB:
        bt2020_linear = transform(srgb_eotf(rgb), Primaries::BT709, Primaries::BT2020);
        break;
    case InputTextureColorSpace::LINEAR_EXTENDED_SRGB:
        bt2020_linear = transform(rgb, Primaries::BT709, Primaries::BT2020);
        break;
    case InputTextureColorSpace::HDR10:
        // Happy identity path.
        return rgb;
    }

    // Tone-map 1.0 to 203 nits, then delinearize.
    return clamp(pq_inverse_eotf(bt2020_linear * (SDR_REFERENCE_WHITE / PQ_MAX_WHITE)), 0.0, 1.0);
}

[shader("compute")]
[numthreads(16, 16)]
void main(uint2 self_id: SV_DispatchThreadID)
{
    let coords = uint2(self_id.x * 2, self_id.y * 2);
    let chroma_coords = coords / 2;

    int j, k;
    float us[4];
    float vs[4];
    for (k = 0; k < 2; k += 1)
    {
        for (j = 0; j < 2; j += 1)
        {
            let texel_coords = coords + uint2(j, k);
            float4 texel = blend_image.Load(uint3(texel_coords, 0));

            float3 yuv;
            switch (pc.output_profile)
            {
            case OutputProfile::HD:
                yuv = encode_ycbcr(to_bt709(texel.rgb, pc.input_color_space),
                                   YCbCrModel::BT709, false);
                break;
            case OutputProfile::HDR10:
                yuv = encode_ycbcr(to_bt2020_pq(texel.rgb, pc.input_color_space),
                                   YCbCrModel::BT2020, false);
                break;
            }

            luminance[texel_coords] = yuv.x;

            int i = k * 2 + j;
            us[i] = yuv.y;
            vs[i] = yuv.z;
        }
    }

    let u = lerp(lerp(us[0], us[1], 0.5), lerp(us[2], us[3], 0.5), 0.5);
    let v = lerp(lerp(vs[0], vs[1], 0.5), lerp(vs[2], vs[3], 0.5), 0.5);

#ifdef SEMIPLANAR
    chroma_uv[chroma_coords] = float2(u, v);
#else
    chroma_u[chroma_coords] = u;
    chroma_v[chroma_coords] = v;
#endif
}
