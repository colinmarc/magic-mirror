// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

#version 450

layout(push_constant) uniform UBO {
    vec2 src_pos;
    vec2 src_size;
    vec2 dst_pos;
    vec2 dst_size;
} surface;

layout(location = 0) out vec2 uv;

void main() {
    vec2 corner;
    switch (gl_VertexIndex % 4) {
        case 0: 
            corner = vec2(0.0, 0.0);
            break;
        case 1:
            corner =  vec2(1.0, 0.0);
            break;
        case 2:
            corner =  vec2(0.0, 1.0);
            break;
        case 3:
            corner =  vec2(1.0, 1.0);
            break;
    }

    gl_Position = vec4(surface.dst_pos + surface.dst_size * corner, 0.0, 1.0);
    uv = surface.src_pos + surface.src_size * corner;
}