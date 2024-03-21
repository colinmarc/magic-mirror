// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

#version 450

layout (location = 0) out vec2 uv;

layout(push_constant) uniform UBO {
    vec2 aspect;
} pc;

void main()
{
    uv = vec2((gl_VertexIndex << 1) & 2, gl_VertexIndex & 2) / 2.0;
    gl_Position = vec4((uv * 2.0f - 1.0f) / pc.aspect, 0.0f, 1.0f);
}
