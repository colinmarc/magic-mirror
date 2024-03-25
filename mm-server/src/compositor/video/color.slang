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

// Named transfer functions.
public enum TransferFunction
{
    LINEAR,
    SRGB,
    BT709,
    PQ,
}

// Applies the EOTF of a transfer function to a color, linearizing the value.
// This is sometimes called removing gamma correction.
public float4 apply_eotf(float4 color, TransferFunction transferFunction)
{
    return float4(
        apply_eotf(color.rgb, transferFunction),
        color.a);
}

// Applies the EOTF of a transfer function to a color, linearizing the value.
// This is sometimes called removing gamma correction.
public float3 apply_eotf(float3 color, TransferFunction transferFunction)
{
    return float3(
        apply_eotf(color.r, transferFunction),
        apply_eotf(color.g, transferFunction),
        apply_eotf(color.b, transferFunction));
}

// Applies the EOTF of a transfer function to one channel of a color. This is
// sometimes called removing gamma correction.
public float apply_eotf(float channel, TransferFunction transferFunction)
{
    switch (transferFunction)
    {
    case TransferFunction::LINEAR:
        return channel;
    case TransferFunction::SRGB:
        return channel > 0.04045 ? pow((channel + 0.055) / 1.055, 2.4) : channel / 12.92;
    case TransferFunction::BT709:
        return channel > 0.081 ? pow((channel + 0.099) / 1.099, 1.0 / 0.45) : channel / 4.5;
    case TransferFunction::PQ:
        return pq_eotf(channel);
    }
}

// Applies the inverse EOTF of a transfer function to a color, resulting in
// non-linear values. This is sometimes called gamma correction.
public float4 apply_inverse_eotf(float4 color, TransferFunction transferFunction)
{
    return float4(
        apply_inverse_eotf(color.rgb, transferFunction),
        color.a);
}

// Applies the inverse EOTF of a transfer function to a color, resulting in
// non-linear values. This is sometimes called gamma correction.
public float3 apply_inverse_eotf(float3 color, TransferFunction transferFunction)
{
    return float3(
        apply_inverse_eotf(color.r, transferFunction),
        apply_inverse_eotf(color.g, transferFunction),
        apply_inverse_eotf(color.b, transferFunction));
}

// Applies the inverse EOTF of a transfer function to one channel of a color,
// resulting in non-linear values. This is sometimes called gamma correction.
public float apply_inverse_eotf(float channel, TransferFunction transferFunction)
{
    switch (transferFunction)
    {
    case TransferFunction::LINEAR:
        return channel;
    case TransferFunction::SRGB:
        return channel > 0.0031308 ? 1.055 * pow(channel, 1.0 / 2.4) - 0.055 : 12.92 * channel;
    case TransferFunction::BT709:
        return channel >= 0.018 ? 1.099 * pow(channel, 1.0 / 2.2) - 0.099 : 4.5 * channel;
    case TransferFunction::PQ:
        return pq_inverse_eotf(channel);
    }
}

static const float PQ_M1 = 0.1593017578125;
static const float PQ_M2 = 78.84375;
static const float PQ_C1 = 0.8359375;
static const float PQ_C2 = 18.8515625;
static const float PQ_C3 = 18.6875;

public static const float SDR_REFERENCE_WHITE = 203.0;

// Applies the Perceptual Quantizer EOTF to a color channel. The input should be
// in the range [0, 1], where 1 corresponds to the maximum 10,000 nits.
float pq_eotf(float channel)
{
    let c = pow(channel, 1.0 / PQ_M2);
    return pow(
        max(c - PQ_C1, 0.0) / (PQ_C2 - PQ_C3 * c),
        1.0 / PQ_M1);
}

// Applies the inverse Perceptual Quantizer EOTF to a color channel. The output
// will be in the range [0, 1], where 1 corresponds to the maximum 10,000 nits.
float pq_inverse_eotf(float channel)
{
    let c = pow(channel, PQ_M1);
    return pow(
        (PQ_C1 + PQ_C2 * c) / (1.0 + PQ_C3 * c),
        PQ_M2);
}

// Applies a scaling function on the result of the PQ EOTF to map it to SDR
// space, based on a hypothetical maximum nits value for SDR.
public float4 scale_pq_to_sdr(float4 color, float max_nits)
{
    return float4(
        scale_pq_to_sdr(color.rgb, max_nits),
        color.a);
}

// Applies a scaling function on the result of the PQ EOTF to map it to SDR
// space, based on a hypothetical maximum nits value for SDR.
public float3 scale_pq_to_sdr(float3 color, float max_nits)
{
    return color * (10000.0 / max_nits);
}

// Applies a scaling function on the result of the PQ EOTF to map it to SDR
// space, based on a hypothetical maximum nits value for SDR.
public float scale_pq_to_sdr(float channel, float max_nits)
{
    return channel * (10000.0 / max_nits);
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
    // XXX if it doesn't work, I got the row-major/column-major thing wrong here.
    // let mat = mul(pa.to_xyz, pb.from_xyz);
    // let xyz = mul(pa.to_xyz, color);
    // return mul(pb.from_xyz, xyz);
    let color = clamp(color, 0.0, 1.0);
    let xyz = mul(pa.to_xyz, color);
    return mul(pb.from_xyz, xyz);
    // return mul(color, mat);
    // TODO premul
}
