
const Sampler2D blend_image;

[[vk::image_format("r8")]]
const RWTexture2D<float> luminance;

#ifdef SEMIPLANAR
[[vk::image_format("rg8")]]
const RWTexture2D<float2> chroma_uv;
#else
[[vk::image_format("r8")]]
const RWTexture2D<float> chroma_u;
[[vk::image_format("r8")]]
const RWTexture2D<float> chroma_v;
#endif

struct PushConstants
{
    float3x3 color_transform;
}

[[vk::push_constant]]
PushConstants pc;

float3 rgb_to_ycbcr(float3 color)
{
    let yuv = mul(color, transpose(pc.color_transform));

    // The matrix multiplication gives us Y in [0, 1] and Cb and Cr in [-0.5, 0.5].
    // This converts to "MPEG" or "Narrow" in the range [16, 235] and [16, 240].
    return float3(
        (219.0 * yuv.x + 16.0) / 256.0,
        (224.0 * yuv.y + 128.0) / 256.0,
        (224.0 * yuv.z + 128.0) / 256.0);
}

float rgb709_unlinear(float s)
{
    if (s >= 0.018)
    {
        return 1.099 * pow(s, 1.0 / 2.2) - 0.099;
    }
    else
    {
        return 4.5 * s;
    }
}

float3 unlinearize(float3 color)
{
    return float3(
        rgb709_unlinear(color.r),
        rgb709_unlinear(color.g),
        rgb709_unlinear(color.b));
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
            let texel = blend_image.Load(uint3(texel_coords, 0));
            let yuv = rgb_to_ycbcr(unlinearize(texel.rgb));

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
