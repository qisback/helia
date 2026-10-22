// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Two-pass robust shift-and-add of one frame. The resample is the one in
// accumulate.wgsl. Mode 0 gathers per-pixel statistics (weighted sum and weight
// for the result, unweighted sum and sum of squares and the count for the
// rejection, all relative to the centre image); mode 1 adds the frame under the
// rule in `kind` (0 hard clip, 1 Huber, 2 winsorised) using the mean and limit
// k * sigma of the other samples at the pixel. Mirrors
// helia_core::stack::{RobustKind::apply, robust_reference}.

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
    mode: u32,
    kind: u32,
    k: f32,
    wx0: vec4<f32>,
    wx1: vec4<f32>,
    wy0: vec4<f32>,
    wy1: vec4<f32>,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> frame: array<f32>;
@group(0) @binding(2) var<storage, read> centre: array<f32>;
// Two vec4 per pixel. First: x weighted sum, z weight, w count. Second: x
// unweighted sum, y unweighted sum of squares.
@group(0) @binding(3) var<storage, read_write> stats: array<vec4<f32>>;
// x: robust sum, y: robust weight.
@group(0) @binding(4) var<storage, read_write> robust: array<vec2<f32>>;

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
    let d = acc - centre[idx];
    var st = stats[2u * idx];
    var su = stats[2u * idx + 1u];
    if (p.mode == 0u) {
        st = vec4<f32>(st.x + p.weight * d, 0.0, st.z + p.weight, st.w + 1.0);
        su = vec4<f32>(su.x + d, su.y + d * d, 0.0, 0.0);
        stats[2u * idx] = st;
        stats[2u * idx + 1u] = su;
        return;
    }
    // Pass two. Fewer than three samples: no spread, nothing is rejected.
    // From six up the sample is left out of its own statistics.
    let n = st.w;
    var mean = 0.0;
    var lim = 3.4e38;
    if (n >= 1.0) {
        mean = su.x / n;
    }
    if (n >= 6.0) {
        let n1 = n - 1.0;
        let s1 = su.x - d;
        let q1 = su.y - d * d;
        mean = s1 / n1;
        lim = p.k * sqrt(max((q1 - s1 * mean) / (n1 - 1.0), 0.0));
    } else if (n >= 3.0) {
        lim = p.k * sqrt(max(su.y / n - mean * mean, 0.0));
    }
    let a = abs(d - mean);
    var f = 1.0;
    var val = d;
    if (p.kind == 0u) {
        if (a > lim) {
            f = 0.0;
        }
    } else if (p.kind == 1u) {
        if (a > lim) {
            f = lim / a;
        }
    } else {
        val = clamp(d, mean - lim, mean + lim);
    }
    if (f > 0.0) {
        var rb = robust[idx];
        rb = vec2<f32>(rb.x + p.weight * f * val, rb.y + p.weight * f);
        robust[idx] = rb;
    }
}
