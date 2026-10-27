// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Horizontal pass of the separable Gaussian blur, mirroring
// helia_core::filter::gaussian_blur: clamp-to-edge borders and the taps added
// in order from zero, as the CPU does, so the two agree to float contraction.

struct Params {
    width: u32,
    height: u32,
    frames: u32,
    radius: u32,
    mode: u32, // unused here
    pad0: u32,
    pad1: u32,
    pad2: u32,
    taps: array<vec4<f32>, 32>,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> src: array<f32>;
@group(0) @binding(2) var<storage, read_write> dst: array<f32>;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.width || gid.y >= p.height || gid.z >= p.frames) {
        return;
    }
    let base = gid.z * p.width * p.height + gid.y * p.width;
    let last = i32(p.width) - 1;
    var acc = 0.0;
    for (var i = 0u; i < 2u * p.radius + 1u; i = i + 1u) {
        let xs = clamp(i32(gid.x) + i32(i) - i32(p.radius), 0, last);
        acc = acc + p.taps[i >> 2u][i & 3u] * src[base + u32(xs)];
    }
    dst[base + gid.x] = acc;
}
