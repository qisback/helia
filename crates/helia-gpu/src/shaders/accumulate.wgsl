// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Shift + resample + weighted add of one frame into the running sum.
// Mirrors helia_core::resample::resample_region: tap weights are computed on
// the CPU (they are constant for a whole frame) so no transcendental
// functions run here and the two sides agree to float rounding.

struct Params {
    width: u32,
    height: u32,
    ox: i32,
    oy: i32,
    dx: f32,
    dy: f32,
    weight: f32,
    n_taps: u32,
    lo: i32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
    wx0: vec4<f32>,
    wx1: vec4<f32>,
    wy0: vec4<f32>,
    wy1: vec4<f32>,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> frame: array<f32>;
@group(0) @binding(2) var<storage, read_write> acc_sum: array<f32>;
@group(0) @binding(3) var<storage, read_write> acc_weight: array<f32>;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.width || gid.y >= p.height) {
        return;
    }
    let sx = f32(gid.x) + p.dx;
    let sy = f32(gid.y) + p.dy;
    if (!(sx >= 0.0 && sx <= f32(p.width - 1u)) || !(sy >= 0.0 && sy <= f32(p.height - 1u))) {
        return;
    }
    var wx = array<f32, 8>(p.wx0.x, p.wx0.y, p.wx0.z, p.wx0.w, p.wx1.x, p.wx1.y, p.wx1.z, p.wx1.w);
    var wy = array<f32, 8>(p.wy0.x, p.wy0.y, p.wy0.z, p.wy0.w, p.wy1.x, p.wy1.y, p.wy1.z, p.wy1.w);
    let max_x = i32(p.width) - 1;
    let max_y = i32(p.height) - 1;
    var acc = 0.0;
    for (var j = 0u; j < p.n_taps; j = j + 1u) {
        let yy = clamp(i32(gid.y) + p.oy + p.lo + i32(j), 0, max_y);
        var racc = 0.0;
        for (var i = 0u; i < p.n_taps; i = i + 1u) {
            let xx = clamp(i32(gid.x) + p.ox + p.lo + i32(i), 0, max_x);
            racc = racc + wx[i] * frame[u32(yy) * p.width + u32(xx)];
        }
        acc = acc + wy[j] * racc;
    }
    let idx = gid.y * p.width + gid.x;
    acc_sum[idx] = acc_sum[idx] + p.weight * acc;
    acc_weight[idx] = acc_weight[idx] + p.weight;
}
