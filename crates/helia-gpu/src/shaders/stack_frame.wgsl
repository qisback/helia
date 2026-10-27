// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Whole-frame shift-and-add of a batch of decoded frames into the running
// sums, mirroring helia_core::backend::CpuAccumulator: tap weights come from
// the host (so no transcendental functions run here), samples whose source
// lies outside the frame contribute nothing, frames are added in order.

struct Params {
    width: u32,
    height: u32,
    frames: u32,
    pad: u32,
}

struct Deposit {
    dx: f32,
    dy: f32,
    weight: f32,
    valid: u32,
    ox: i32,
    oy: i32,
    n_taps: u32,
    lo: i32,
    wx: array<vec4<f32>, 2>,
    wy: array<vec4<f32>, 2>,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> mono: array<f32>;
@group(0) @binding(2) var<storage, read> deposits: array<Deposit>;
@group(0) @binding(3) var<storage, read_write> acc_sum: array<f32>;
@group(0) @binding(4) var<storage, read_write> acc_weight: array<f32>;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.width || gid.y >= p.height) {
        return;
    }
    let max_x = i32(p.width) - 1;
    let max_y = i32(p.height) - 1;
    let idx = gid.y * p.width + gid.x;
    var sum = acc_sum[idx];
    var wsum = acc_weight[idx];
    for (var f = 0u; f < p.frames; f = f + 1u) {
        let d = deposits[f];
        if (d.valid == 0u) {
            continue;
        }
        let sx = f32(gid.x) + d.dx;
        let sy = f32(gid.y) + d.dy;
        if (!(sx >= 0.0 && sx <= f32(max_x)) || !(sy >= 0.0 && sy <= f32(max_y))) {
            continue;
        }
        let base = f * p.width * p.height;
        var acc = 0.0;
        for (var j = 0u; j < d.n_taps; j = j + 1u) {
            let yy = clamp(i32(gid.y) + d.oy + d.lo + i32(j), 0, max_y);
            var racc = 0.0;
            for (var i = 0u; i < d.n_taps; i = i + 1u) {
                let xx = clamp(i32(gid.x) + d.ox + d.lo + i32(i), 0, max_x);
                racc = racc + d.wx[i >> 2u][i & 3u] * mono[base + u32(yy) * p.width + u32(xx)];
            }
            acc = acc + d.wy[j >> 2u][j & 3u] * racc;
        }
        sum = sum + d.weight * acc;
        wsum = wsum + d.weight;
    }
    acc_sum[idx] = sum;
    acc_weight[idx] = wsum;
}
