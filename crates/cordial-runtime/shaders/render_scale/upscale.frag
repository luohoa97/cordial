#version 450
//
// The upscale pass for CORDIAL_RENDER_SCALE.
//
// mode 1 is Snapdragon Game Super Resolution 1 (sgsr1_shader_mobile.frag,
// RGBA operation mode), ported from the GLSL ES 3.0 original to Vulkan GLSL:
// the viewport uniform became a push constant, the sampler a set 0 binding,
// and the operation mode, edge threshold and edge sharpness were fixed at the
// shipped defaults (mode 1, 8/255, 2.0). The filter itself is unchanged.
//
//   Copyright (c) 2025, Qualcomm Innovation Center, Inc. All rights reserved.
//   SPDX-License-Identifier: BSD-3-Clause
//   https://github.com/SnapdragonStudios/snapdragon-gsr
//
// The licence text (the repository's LICENSE, dated 2023) is reproduced in
// THIRD-PARTY-NOTICES.md. Rebuild the .spv beside this file with
// `glslangValidator -V upscale.frag -o upscale.frag.spv` (and likewise the .vert).
//
// mode 0 is plain bilinear, here so a screenshot can be compared against the
// cheapest possible upscale as well as against native rendering.

layout(push_constant) uniform Pc
{
    // 1/width, 1/height, width, height of the *input* (the engine's image).
    vec4 viewportInfo;
    uint mode;
} pc;

layout(set = 0, binding = 0) uniform sampler2D ps0;

layout(location = 0) in vec2 uv;
layout(location = 0) out vec4 outColor;

const float EdgeThreshold = 8.0 / 255.0;
const float EdgeSharpness = 2.0;

float fastLanczos2(float x)
{
    float wA = x - 4.0;
    float wB = x * wA - wA;
    wA *= wA;
    return wB * wA;
}

vec2 weightY(float dx, float dy, float c, float std_)
{
    float x = ((dx * dx) + (dy * dy)) * 0.55 + clamp(abs(c) * std_, 0.0, 1.0);
    float w = fastLanczos2(x);
    return vec2(w, w * c);
}

void main()
{
    vec4 color;
    color.xyz = textureLod(ps0, uv, 0.0).xyz;
    color.w = 1.0;

    if (pc.mode == 1u)
    {
        vec2 imgCoord = (uv * pc.viewportInfo.zw) + vec2(-0.5, 0.5);
        vec2 imgCoordPixel = floor(imgCoord);
        vec2 coord = imgCoordPixel * pc.viewportInfo.xy;
        vec2 pl = imgCoord - imgCoordPixel;
        vec4 left = textureGather(ps0, coord, 1);

        float edgeVote = abs(left.z - left.y) + abs(color.y - left.y) + abs(color.y - left.z);
        if (edgeVote > EdgeThreshold)
        {
            coord.x += pc.viewportInfo.x;

            vec4 right = textureGather(ps0, coord + vec2(pc.viewportInfo.x, 0.0), 1);
            vec4 upDown;
            upDown.xy = textureGather(ps0, coord + vec2(0.0, -pc.viewportInfo.y), 1).wz;
            upDown.zw = textureGather(ps0, coord + vec2(0.0, pc.viewportInfo.y), 1).yx;

            float mean = (left.y + left.z + right.x + right.w) * 0.25;
            left = left - vec4(mean);
            right = right - vec4(mean);
            upDown = upDown - vec4(mean);
            float centre = color.y - mean;

            float sum = (((((abs(left.x) + abs(left.y)) + abs(left.z)) + abs(left.w)) +
                          (((abs(right.x) + abs(right.y)) + abs(right.z)) + abs(right.w))) +
                         (((abs(upDown.x) + abs(upDown.y)) + abs(upDown.z)) + abs(upDown.w)));
            float std_ = 2.181818 / sum;

            vec2 aWY = weightY(pl.x, pl.y + 1.0, upDown.x, std_);
            aWY += weightY(pl.x - 1.0, pl.y + 1.0, upDown.y, std_);
            aWY += weightY(pl.x - 1.0, pl.y - 2.0, upDown.z, std_);
            aWY += weightY(pl.x, pl.y - 2.0, upDown.w, std_);
            aWY += weightY(pl.x + 1.0, pl.y - 1.0, left.x, std_);
            aWY += weightY(pl.x, pl.y - 1.0, left.y, std_);
            aWY += weightY(pl.x, pl.y, left.z, std_);
            aWY += weightY(pl.x + 1.0, pl.y, left.w, std_);
            aWY += weightY(pl.x - 1.0, pl.y - 1.0, right.x, std_);
            aWY += weightY(pl.x - 2.0, pl.y - 1.0, right.y, std_);
            aWY += weightY(pl.x - 2.0, pl.y, right.z, std_);
            aWY += weightY(pl.x - 1.0, pl.y, right.w, std_);

            float finalY = aWY.y / aWY.x;

            float maxY = max(max(left.y, left.z), max(right.x, right.w));
            float minY = min(min(left.y, left.z), min(right.x, right.w));
            finalY = clamp(EdgeSharpness * finalY, minY, maxY);

            float deltaY = finalY - centre;

            // Smooth high-contrast input.
            deltaY = clamp(deltaY, -23.0 / 255.0, 23.0 / 255.0);

            color.xyz = clamp(color.xyz + vec3(deltaY), 0.0, 1.0);
        }
    }

    outColor = vec4(color.xyz, 1.0);
}
