// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Sum the per-column partials of reg_refine.wgsl and take one parabolic
// refinement step: est += (parabola(left, centre, right), parabola(up, centre, down)).
// One workgroup per frame.

struct Params {
    nxh: u32,
    frames: u32,
    pad0: u32,
    pad1: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> partials: array<f32>;
@group(0) @binding(2) var<storage, read_write> est: array<f32>;

var<workgroup> red: array<f32, 1280>; // five sums of 256 threads

fn parabola(l: f32, c: f32, r: f32) -> f32 {
    let denom = l - 2.0 * c + r;
    if (denom >= -1.0e-12) {
        return 0.0;
    }
    return clamp(0.5 * (l - r) / denom, -0.5, 0.5);
}

@compute @workgroup_size(256)
fn main(
    @builtin(local_invocation_index) lid: u32,
    @builtin(workgroup_id) wid: vec3<u32>,
) {
    var acc = array<f32, 5>(0.0, 0.0, 0.0, 0.0, 0.0);
    for (var kx = lid; kx < p.nxh; kx = kx + 256u) {
        let base = (wid.x * p.nxh + kx) * 5u;
        for (var i = 0u; i < 5u; i = i + 1u) {
            acc[i] = acc[i] + partials[base + i];
        }
    }
    for (var i = 0u; i < 5u; i = i + 1u) {
        red[i * 256u + lid] = acc[i];
    }
    workgroupBarrier();
    for (var s = 128u; s > 0u; s = s >> 1u) {
        if (lid < s) {
            for (var i = 0u; i < 5u; i = i + 1u) {
                red[i * 256u + lid] = red[i * 256u + lid] + red[i * 256u + lid + s];
            }
        }
        workgroupBarrier();
    }
    if (lid == 0u) {
        let c = red[0];
        let left = red[256];
        let right = red[512];
        let up = red[768];
        let down = red[1024];
        est[wid.x * 2u] = est[wid.x * 2u] + parabola(left, c, right);
        est[wid.x * 2u + 1u] = est[wid.x * 2u + 1u] + parabola(up, c, down);
    }
}
