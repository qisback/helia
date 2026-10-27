// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Vertical pass of the separable convolution; see edit_conv_h.wgsl.

struct Params {
    width: u32,
    height: u32,
    ntaps: u32,
    step: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> taps: array<f32>;
@group(0) @binding(2) var<storage, read> src: array<f32>;
@group(0) @binding(3) var<storage, read_write> dst: array<f32>;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.width || gid.y >= p.height) {
        return;
    }
    let last = i32(p.height) - 1;
    let centre = i32(p.ntaps / 2u);
    var acc = 0.0;
    for (var i = 0u; i < p.ntaps; i = i + 1u) {
        let ys = clamp(i32(gid.y) + (i32(i) - centre) * i32(p.step), 0, last);
        acc = acc + taps[i] * src[u32(ys) * p.width + gid.x];
    }
    dst[gid.y * p.width + gid.x] = acc;
}
