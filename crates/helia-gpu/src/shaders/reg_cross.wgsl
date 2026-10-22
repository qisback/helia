// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Cross-power spectrum F * conj(R) in place, and the partial sums of its
// magnitude over the whole (not just the half) plane: columns that are their
// own mirror image count once, the others twice.

struct Params {
    nx: u32,
    ny: u32,
    nxh: u32,
    frames: u32,
    groups: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read_write> spec: array<vec2<f32>>;
@group(0) @binding(2) var<storage, read> reference: array<vec2<f32>>;
@group(0) @binding(3) var<storage, read_write> partials: array<f32>;

var<workgroup> red: array<f32, 256>;

@compute @workgroup_size(256)
fn main(
    @builtin(local_invocation_index) lid: u32,
    @builtin(workgroup_id) wid: vec3<u32>,
) {
    let total = p.nxh * p.ny;
    let i = wid.x * 256u + lid;
    var mag = 0.0;
    if (i < total) {
        let idx = wid.y * total + i;
        let c = spec[idx];
        let r = reference[i];
        let prod = vec2<f32>(c.x * r.x + c.y * r.y, c.y * r.x - c.x * r.y);
        spec[idx] = prod;
        let kx = i / p.ny;
        var weight = 2.0;
        if (kx == 0u || (p.nx % 2u == 0u && kx == p.nx / 2u)) {
            weight = 1.0;
        }
        mag = weight * sqrt(prod.x * prod.x + prod.y * prod.y);
    }
    red[lid] = mag;
    workgroupBarrier();
    for (var s = 128u; s > 0u; s = s >> 1u) {
        if (lid < s) {
            red[lid] = red[lid] + red[lid + s];
        }
        workgroupBarrier();
    }
    if (lid == 0u) {
        partials[wid.y * p.groups + wid.x] = red[0];
    }
}
