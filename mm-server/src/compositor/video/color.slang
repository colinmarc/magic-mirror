module color;

// A set of color primaries, defined in terms of a transformation to/from XYZ
// space.
public struct PrimariesTransform
{
    public float3x3 to_xyz;
    public float3x3 from_xyz;
}

// Named sets of color primaries.
namespace Primaries
{
public static const PrimariesTransform BT709 = {
    float3x3(
        0.4124564f, 0.3575761f, 0.1804375f,
        0.2126729f, 0.7151522f, 0.0721750f,
        0.0193339f, 0.1191920f, 0.9503041f),
    float3x3(
        3.2404542f, -1.5371385f, -0.4985314f,
        -0.9692660f, 1.8760108f, 0.0415560f,
        0.0556434f, -0.2040259f, 1.0572252f)
};

public static const PrimariesTransform BT2020 = {
    float3x3(
        0.636958f, 0.1446169f, 0.1688810f,
        0.2627002f, 0.6779981f, 0.0593017f,
        0.0000000f, 0.0280727f, 1.0609851f),
    float3x3(
        1.7166512, -0.3556708, -0.2533663,
        -0.6666844, 1.6164812, 0.0157685,
        0.0176399, -0.0427706, 0.9421031),
};
}

// Applies the sRGB EOTF to a color, producing linear values.
public float3 srgb_eotf(float3 color)
{
    return float3(
        srgb_eotf(color.r),
        srgb_eotf(color.g),
        srgb_eotf(color.b));
}

// Applies the sRGB EOTF to one channel of a color, producing a linear value.
public float srgb_eotf(float channel)
{
    return channel > 0.04045 ? pow((channel + 0.055) / 1.055, 2.4) : channel / 12.92;
}

// Applies the inverse sRGB EOTF to a color, producing non-linear values. This
// is sometimes called gamma correction.
public float3 srgb_inverse_eotf(float3 color)
{
    return float3(
        srgb_inverse_eotf(color.r),
        srgb_inverse_eotf(color.g),
        srgb_inverse_eotf(color.b));
}

// Applies the inverse sRGB EOTF to one channel of a color, producing non-linear
// values. This is sometimes called gamma correction.
public float srgb_inverse_eotf(float channel)
{
    return channel > 0.0031308 ? 1.055 * pow(channel, 1.0 / 2.4) - 0.055 : 12.92 * channel;
}

// Applies the BT.709 EOTF to a color, producing linear values.
public float3 bt709_eotf(float3 color)
{
    return float3(
        bt709_eotf(color.r),
        bt709_eotf(color.g),
        bt709_eotf(color.b));
}

// Applies the BT.709 EOTF to one channel of a color, producing a linear value.
public float bt709_eotf(float channel)
{
    return channel > 0.081 ? pow((channel + 0.099) / 1.099, 1.0 / 0.45) : channel / 4.5;
}

// Applies the inverse BT.709 EOTF to a color, producing non-linear values. This
// is sometimes called gamma correction.
public float3 bt709_inverse_eotf(float3 color)
{
    return float3(
        bt709_inverse_eotf(color.r),
        bt709_inverse_eotf(color.g),
        bt709_inverse_eotf(color.b));
}

// Applies the inverse BT.709 EOTF to one channel of a color, producing non-linear
// values. This is sometimes called gamma correction.
public float bt709_inverse_eotf(float channel)
{
    return channel >= 0.018 ? 1.099 * pow(channel, 1.0 / 2.2) - 0.099 : 4.5 * channel;
}

static const float PQ_M1 = 0.1593017578125;
static const float PQ_M2 = 78.84375;
static const float PQ_C1 = 0.8359375;
static const float PQ_C2 = 18.8515625;
static const float PQ_C3 = 18.6875;

public static const float SDR_REFERENCE_WHITE = 203.0;
public static const float PQ_MAX_WHITE = 10000.0;

// Applies the Perceptual Quantizer EOTF to a color, producing linear values.
// The input should be in the range [0, 1], where 1 corresponds to the maximum
// 10,000 nits.
public float3 pq_eotf(float3 color)
{
    return float3(
        pq_eotf(color.r),
        pq_eotf(color.g),
        pq_eotf(color.b));
}

// Applies the Perceptual Quantizer EOTF to a color channel, producing linear
// values. The input should be in the range [0, 1], where 1 corresponds to the
// maximum 10,000 nits.
float pq_eotf(float channel)
{
    let c = pow(channel, 1.0 / PQ_M2);
    return pow(
        max(c - PQ_C1, 0.0) / (PQ_C2 - PQ_C3 * c),
        1.0 / PQ_M1);
}

// Applies the inverse Perceptual Quantizer EOTF to a color, producing non-linear
// values. The output will be in the range [0, 1], where 1 corresponds to the
// maximum 10,000 nits.
public float3 pq_inverse_eotf(float3 color)
{
    return float3(
        pq_inverse_eotf(color.r),
        pq_inverse_eotf(color.g),
        pq_inverse_eotf(color.b));
}

// Applies the inverse Perceptual Quantizer EOTF to a color channel, producing a
// non-linear value. The output will be in the range [0, 1], where 1 corresponds
// to the maximum 10,000 nits.
float pq_inverse_eotf(float channel)
{
    let c = pow(channel, PQ_M1);
    return pow(
        (PQ_C1 + PQ_C2 * c) / (1.0 + PQ_C3 * c),
        PQ_M2);
}

// Transform a color from one set of primaries to another. The colors must be
// linear, that is, they must have already been linearized using the relevant
// OETF.
public float4 transform(float4 color, PrimariesTransform pa, PrimariesTransform pb)
{
    return float4(
        transform(color.rgb, pa, pb),
        color.a);
}

// Transform a color from one set of primaries to another. The colors must be
// linear, that is, they must have already been linearized using the relevant
// inverse EOTF.
public float3 transform(float3 color, PrimariesTransform pa, PrimariesTransform pb)
{
    let mat = mul(pb.from_xyz, pa.to_xyz);
    return mul(mat, color);
}

// Available conversions to and from YCbCr color space.
public enum YCbCrModel
{
    BT709,
    BT2020,
}

static const float3x3 YCBCR_709_MATRIX = float3x3(
    0.2126, 0.7152, 0.0722,
    -0.114572, -0.385428, 0.5,
    0.5, -0.454153, -0.045847);

static const float3x3 YCBCR_2020_MATRIX = float3x3(
    0.2627, 0.6780, 0.0593,
    -0.139630, -0.360370, 0.5,
    0.5, -0.459786, -0.040214);

// Encode a color in the YCbCr color system. The color should already be in
// nonlinear space.
public float3 encode_ycbcr(float3 color, YCbCrModel model, bool full_range)
{
    float3 ycbcr;
    switch (model)
    {
    case YCbCrModel::BT709:
        ycbcr = mul(YCBCR_709_MATRIX, color);
        break;
    case YCbCrModel::BT2020:
        ycbcr = mul(YCBCR_2020_MATRIX, color);
        break;
    }

    // The matrix multiplication gives us Y in [0, 1] and Cb and Cr in [-0.5, 0.5].
    ycbcr.y += 0.5;
    ycbcr.z += 0.5;

    if (!full_range)
    {
        // This converts to "MPEG" or "Narrow" in the range [16, 235] and [16, 240].
        ycbcr = float3(
            (219.0 * ycbcr.x + 16.0) / 256.0,
            (224.0 * ycbcr.y + 16.0) / 256.0,
            (224.0 * ycbcr.z + 16.0) / 256.0);
    }

    return clamp(ycbcr, 0.0, 1.0);
}
