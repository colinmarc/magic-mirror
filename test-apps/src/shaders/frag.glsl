// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

#version 450

layout(location = 0) in vec3 inColor;

layout(location = 0) out vec4 color;

void main() {
    color = vec4(inColor, 1.0);
}
