// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

#version 450

layout(binding = 0) uniform sampler2D tex;

layout(location = 0) in vec2 uv;

layout(location = 0) out vec4 color;

float srgb_channel_to_linear(float x) {
    return mix(x / 12.92,
        pow((x + 0.055) / 1.055, 2.4),
        x > 0.04045);
}

vec4 srgb_to_linear(vec4 color) {
	if (color.a == 0) {
		return vec4(0);
	}

	color.rgb /= color.a;
	color.rgb = vec3(
		srgb_channel_to_linear(color.r),
		srgb_channel_to_linear(color.g),
		srgb_channel_to_linear(color.b)
	);

	color.rgb *= color.a;
	return color;
}

void main() {
    vec4 tex_color = texture(tex, uv);

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
    color = srgb_to_linear(tex_color);
}