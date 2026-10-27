// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// One column of the direct evaluation of the correlation surface around the
// current estimate, mirroring PhaseCorrelator::sample_cross: the real part of
// the inverse DFT of the cross-power spectrum at the estimate and at one pixel
// either side in x and in y, summed over the half plane with the column
// weights. One workgroup per (kx, frame); the five partial sums per column are
// added up by reg_refine_update.wgsl.
//
// Phasors use the angle reduced to one turn (fract of f * shift) so the
// argument stays small for the GPU's sine and cosine.

struct Params {
    nx: u32,
    ny: u32,
    nxh: u32,
    frames: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> cross: array<vec2<f32>>;
@group(0) @binding(2) var<storage, read> est: array<f32>;
@group(0) @binding(3) var<storage, read_write> partials: array<f32>;

const TAU: f32 = 6.283185307179586;

var<workgroup> red: array<vec2<f32>, 384>; // three sums of 128 threads

fn signed_freq(i: u32, n: u32) -> f32 {
    if (i <= n / 2u) {
        return f32(i);
    }
    return f32(i) - f32(n);
}

fn phasor(freq: f32, shift: f32) -> vec2<f32> {
    let a = TAU * fract(freq * shift);
    return vec2<f32>(cos(a), sin(a));
}

fn cmul(a: vec2<f32>, b: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(a.x * b.x - a.y * b.y, a.x * b.y + a.y * b.x);
}

@compute @workgroup_size(128)
fn main(
    @builtin(local_invocation_index) lid: u32,
    @builtin(workgroup_id) wid: vec3<u32>,
) {
    let kx = wid.x;
    let f = wid.y;
    let ex = est[f * 2u];
    let ey = est[f * 2u + 1u];
    let base = (f * p.nxh + kx) * p.ny;
    var s0 = vec2<f32>(0.0);
    var sm = vec2<f32>(0.0);
    var sp = vec2<f32>(0.0);
    for (var ky = lid; ky < p.ny; ky = ky + 128u) {
        let c = cross[base + ky];
        let fy = signed_freq(ky, p.ny) / f32(p.ny);
        s0 = s0 + cmul(c, phasor(fy, ey));
        sm = sm + cmul(c, phasor(fy, ey - 1.0));
        sp = sp + cmul(c, phasor(fy, ey + 1.0));
    }
    red[lid] = s0;
    red[128u + lid] = sm;
    red[256u + lid] = sp;
    workgroupBarrier();
    for (var s = 64u; s > 0u; s = s >> 1u) {
        if (lid < s) {
            red[lid] = red[lid] + red[lid + s];
            red[128u + lid] = red[128u + lid] + red[128u + lid + s];
            red[256u + lid] = red[256u + lid] + red[256u + lid + s];
        }
        workgroupBarrier();
    }
    if (lid == 0u) {
        let fx = signed_freq(kx, p.nx) / f32(p.nx);
        var w = 2.0;
        if (kx == 0u || (p.nx % 2u == 0u && kx == p.nx / 2u)) {
            w = 1.0;
        }
        let e0 = phasor(fx, ex);
        let out = (f * p.nxh + kx) * 5u;
        partials[out] = w * cmul(red[0], e0).x;
        partials[out + 1u] = w * cmul(red[0], phasor(fx, ex - 1.0)).x;
        partials[out + 2u] = w * cmul(red[0], phasor(fx, ex + 1.0)).x;
        partials[out + 3u] = w * cmul(red[128u], e0).x;
        partials[out + 4u] = w * cmul(red[256u], e0).x;
    }
}
