// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

static const float2 positions[3] = float2[](
    float2(0.0, -0.5),
    float2(0.5, 0.5),
    float2(-0.5, 0.5));

static const float3 colors[3] = float3[](
    float3(1.0, 0.0, 0.0),
    float3(0.0, 1.0, 0.0),
    float3(0.0, 0.0, 1.0));

struct PushConstants
{
    float4 colors_enabled;
};

[[vk::push_constant]]
PushConstants pc;

struct VertOutput
{
    float3 color : Color;
    float4 sv_position : SV_Position;
};

[shader("vertex")]
VertOutput vert(uint vid: SV_VertexID)
{
    VertOutput output;

    output.color = colors[vid];
    output.sv_position = float4(positions[vid], 0.0, 1.0);

    return output;
}

[shader("fragment")]
float4 frag(
    float3 color: Color)
    : SV_Target
{
    return float4(color * pc.colors_enabled.xyz, 1.0);
}

static const float PQ_M1 = 0.1593017578125;
static const float PQ_M2 = 78.84375;
static const float PQ_C1 = 0.8359375;
static const float PQ_C2 = 18.8515625;
static const float PQ_C3 = 18.6875;

float linearize_pq(float channel)
{
    let c = pow(channel, 1.0 / PQ_M2);
    return pow(
        max(c - PQ_C1, 0.0) / (PQ_C2 - PQ_C3 * c),
        1.0 / PQ_M1);
}

float delinearize_pq(float channel)
{
    let c = pow(channel, PQ_M1);
    return pow(
        (PQ_C1 + PQ_C2 * c) / (1.0 + PQ_C3 * c),
        PQ_M2);
}
