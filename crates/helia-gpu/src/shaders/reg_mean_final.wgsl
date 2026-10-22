// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Frame means from the partial sums of reg_mean.wgsl (also reused to turn the
// partial cross-power magnitudes into the whitening floor: `scale` is then
// whiten_eps / (nx * ny) instead of 1 / (width * height)).

struct Params {
    groups: u32,
    frames: u32,
    scale: f32,
    pad: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> partials: array<f32>;
@group(0) @binding(2) var<storage, read_write> result: array<f32>;

var<workgroup> red: array<f32, 256>;

@compute @workgroup_size(256)
fn main(
    @builtin(local_invocation_index) lid: u32,
    @builtin(workgroup_id) wid: vec3<u32>,
) {
    var sum = 0.0;
    for (var i = lid; i < p.groups; i = i + 256u) {
        sum = sum + partials[wid.x * p.groups + i];
    }
    red[lid] = sum;
    workgroupBarrier();
    for (var s = 128u; s > 0u; s = s >> 1u) {
        if (lid < s) {
            red[lid] = red[lid] + red[lid + s];
        }
        workgroupBarrier();
    }
    if (lid == 0u) {
        result[wid.x] = red[0] * p.scale;
    }
}
