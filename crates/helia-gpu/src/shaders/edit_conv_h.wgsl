// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Horizontal pass of a separable convolution with taps `step` pixels apart
// (step 1 is a plain convolution, 2^j the a-trous form), mirroring
// helia_core::edit::conv::separable: clamp-to-edge borders, taps added in
// order from zero.

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
    let base = gid.y * p.width;
    let last = i32(p.width) - 1;
    let centre = i32(p.ntaps / 2u);
    var acc = 0.0;
    for (var i = 0u; i < p.ntaps; i = i + 1u) {
        let xs = clamp(i32(gid.x) + (i32(i) - centre) * i32(p.step), 0, last);
        acc = acc + taps[i] * src[base + u32(xs)];
    }
    dst[base + gid.x] = acc;
}
