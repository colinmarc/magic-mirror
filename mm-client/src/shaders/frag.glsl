// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

#version 450

layout(binding = 0) uniform sampler2D tex;

layout(location = 0) in vec2 uv;

layout(location = 0) out vec4 color;

float srgb_unlinear(float s) {
    return mix(12.92*s, 1.055*pow(s, 1.0/2.4) - 0.055, s >= 0.0031);
}

// Rec. 2020 uses the same transfer function as Rec. 709, so we don't need to
// distinguish between them. We currently only support those two color spaces.
float linearize(float s) {
    return mix(
        s / 4.5,
        pow((s + 0.099) / 1.099, 1.0 / 0.45),
        s > 0.081);
}

void main() {
    // When sampling the video texture, vulkan does the matrix multiplication
    // for us, but doesn't apply any transfer function.
    vec4 rgb709 = texture(tex, uv);
    vec4 linear = vec4(
        linearize(rgb709.r),
        linearize(rgb709.g),
        linearize(rgb709.b),
        rgb709.a
    );

    // Unlinearize back into sRGB for rendering.
    color = vec4(
        srgb_unlinear(linear.r),
        srgb_unlinear(linear.g),
        srgb_unlinear(linear.b),
        linear.a
    );
}
