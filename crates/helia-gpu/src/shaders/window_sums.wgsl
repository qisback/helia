// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Sums of the 3x3-blurred samples and of their squared gradient over a list of
// windows (frame, x0, y0, w, h): the inputs of the gradient-energy sharpness
// score. One workgroup per window.

struct Params {
    width: u32,
    height: u32,
    count: u32,
    pad: u32,
}

struct Window {
    frame: u32,
    x0: u32,
    y0: u32,
    w: u32,
    h: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> blurred: array<f32>;
@group(0) @binding(2) var<storage, read> energy: array<f32>;
@group(0) @binding(3) var<storage, read> windows: array<Window>;
@group(0) @binding(4) var<storage, read_write> sums: array<f32>;

var<workgroup> red_a: array<f32, 64>;
var<workgroup> red_b: array<f32, 64>;

@compute @workgroup_size(64)
fn main(
    @builtin(local_invocation_index) lid: u32,
    @builtin(workgroup_id) wid: vec3<u32>,
) {
    // Every thread reaches the barriers; surplus workgroups sum nothing. (An
    // early return before a barrier crashes the WARP DX12 adapter.)
    let live = wid.x < p.count;
    let idx = select(0u, wid.x, live);
    let win = windows[idx];
    let plane = win.frame * p.width * p.height;
    var sum_a = 0.0;
    var sum_b = 0.0;
    // Each thread sums down a column, so neighbouring threads read neighbouring
    // samples of a row.
    for (var col = lid; col < win.w; col = col + 64u) {
        if (live) {
            for (var row = 0u; row < win.h; row = row + 1u) {
                let at = plane + (win.y0 + row) * p.width + win.x0 + col;
                sum_a = sum_a + blurred[at];
                sum_b = sum_b + energy[at];
            }
        }
    }
    red_a[lid] = sum_a;
    red_b[lid] = sum_b;
    workgroupBarrier();
    for (var s = 32u; s > 0u; s = s >> 1u) {
        if (lid < s) {
            red_a[lid] = red_a[lid] + red_a[lid + s];
            red_b[lid] = red_b[lid] + red_b[lid + s];
        }
        workgroupBarrier();
    }
    if (lid == 0u && live) {
        sums[wid.x * 2u] = red_a[0];
        sums[wid.x * 2u + 1u] = red_b[0];
    }
}
