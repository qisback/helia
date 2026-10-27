// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Mono image to RGB through a sampled colour table, mirroring
// helia_core::colour::ColourLut::lookup. The host replaces non-finite samples
// by 0 first, so the kernel only has to clamp.

struct Params {
    pixels: u32,
    lut_len: u32,
    stride: u32, // threads per row of the 2-D dispatch
    pad0: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> mono: array<f32>;
@group(0) @binding(2) var<storage, read> lut: array<f32>; // r, g, b per entry
@group(0) @binding(3) var<storage, read_write> rgb: array<f32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.y * p.stride + id.x;
    if (i >= p.pixels) {
        return;
    }
    let last = p.lut_len - 1u;
    let x = clamp(mono[i], 0.0, 1.0) * f32(last);
    let k = min(u32(x), last - 1u);
    let f = x - f32(k);
    for (var c = 0u; c < 3u; c = c + 1u) {
        let a = lut[3u * k + c];
        let b = lut[3u * (k + 1u) + c];
        rgb[3u * i + c] = a + (b - a) * f;
    }
}
