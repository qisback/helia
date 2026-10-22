// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Integer peak of each correlation surface (the first maximum in row-major
// order, as the CPU scans it) and the first parabolic refinement step from
// the surface's own neighbours, writing the estimate `est[frame] = (dx, dy)`.

struct Params {
    nx: u32,
    ny: u32,
    frames: u32,
    pad: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> surface: array<f32>;
@group(0) @binding(2) var<storage, read_write> est: array<f32>;

var<workgroup> best_val: array<f32, 256>;
var<workgroup> best_idx: array<u32, 256>;

fn signed_freq(i: u32, n: u32) -> f32 {
    if (i <= n / 2u) {
        return f32(i);
    }
    return f32(i) - f32(n);
}

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
    let total = p.nx * p.ny;
    let base = wid.x * total;
    var bv = -3.0e38;
    var bi = 0u;
    for (var i = lid; i < total; i = i + 256u) {
        let v = surface[base + i];
        if (v > bv) {
            bv = v;
            bi = i;
        }
    }
    best_val[lid] = bv;
    best_idx[lid] = bi;
    workgroupBarrier();
    for (var s = 128u; s > 0u; s = s >> 1u) {
        if (lid < s) {
            let ov = best_val[lid + s];
            let oi = best_idx[lid + s];
            if (ov > best_val[lid] || (ov == best_val[lid] && oi < best_idx[lid])) {
                best_val[lid] = ov;
                best_idx[lid] = oi;
            }
        }
        workgroupBarrier();
    }
    if (lid == 0u) {
        let idx = best_idx[0];
        let bx = idx % p.nx;
        let by = idx / p.nx;
        let c = surface[base + idx];
        let left = surface[base + by * p.nx + (bx + p.nx - 1u) % p.nx];
        let right = surface[base + by * p.nx + (bx + 1u) % p.nx];
        let up = surface[base + ((by + p.ny - 1u) % p.ny) * p.nx + bx];
        let down = surface[base + ((by + 1u) % p.ny) * p.nx + bx];
        est[wid.x * 2u] = signed_freq(bx, p.nx) + parabola(left, c, right);
        est[wid.x * 2u + 1u] = signed_freq(by, p.ny) + parabola(up, c, down);
    }
}
