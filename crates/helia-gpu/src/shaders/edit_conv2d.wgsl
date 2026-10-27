// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Direct 2-D convolution with a square kernel (the Airy PSF), mirroring
// helia_core::edit::conv::direct: clamp-to-edge borders, rows then columns.

struct Params {
    width: u32,
    height: u32,
    radius: u32,
    pad0: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> kern: array<f32>;
@group(0) @binding(2) var<storage, read> src: array<f32>;
@group(0) @binding(3) var<storage, read_write> dst: array<f32>;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.width || gid.y >= p.height) {
        return;
    }
    let side = 2u * p.radius + 1u;
    let lx = i32(p.width) - 1;
    let ly = i32(p.height) - 1;
    var acc = 0.0;
    for (var j = 0u; j < side; j = j + 1u) {
        let ys = u32(clamp(i32(gid.y) + i32(j) - i32(p.radius), 0, ly));
        for (var i = 0u; i < side; i = i + 1u) {
            let xs = u32(clamp(i32(gid.x) + i32(i) - i32(p.radius), 0, lx));
            acc = acc + kern[j * side + i] * src[ys * p.width + xs];
        }
    }
    dst[gid.y * p.width + gid.x] = acc;
}
