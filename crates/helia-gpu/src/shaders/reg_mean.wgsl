// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Partial sums of every frame of a batch, for the mean subtracted before the
// phase correlation. One workgroup per (group, frame).

struct Params {
    width: u32,
    height: u32,
    frames: u32,
    groups: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> mono: array<f32>;
@group(0) @binding(2) var<storage, read_write> partials: array<f32>;

var<workgroup> red: array<f32, 256>;

@compute @workgroup_size(256)
fn main(
    @builtin(local_invocation_index) lid: u32,
    @builtin(workgroup_id) wid: vec3<u32>,
) {
    let pixels = p.width * p.height;
    let base = wid.y * pixels;
    var sum = 0.0;
    for (var i = wid.x * 256u + lid; i < pixels; i = i + p.groups * 256u) {
        sum = sum + mono[base + i];
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
        partials[wid.y * p.groups + wid.x] = red[0];
    }
}
