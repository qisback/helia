// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Monotone cubic tone curve in place, mirroring
// helia_core::edit::tone::Curve::apply_knots. `knots` holds x, y and slope m
// of each knot as three consecutive runs of `count` values.

struct Params {
    n: u32,
    stride: u32, // threads per row of the 2-D dispatch
    count: u32,
    pad0: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> knots: array<f32>;
@group(0) @binding(2) var<storage, read_write> buf: array<f32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.y * p.stride + id.x;
    if (i >= p.n) {
        return;
    }
    let v = clamp(buf[i], 0.0, 1.0);
    let last = p.count - 1u;
    // The segment holding v: the last interior knot at or below v. A counted
    // loop with no short-circuit condition, so optimising shader compilers (the
    // DX12 path without debug info) cannot read past the table.
    var k = 0u;
    for (var j = 1u; j < last; j = j + 1u) {
        if (v >= knots[j]) {
            k = j;
        }
    }
    let x0 = knots[k];
    let x1 = knots[k + 1u];
    let y0 = knots[p.count + k];
    let y1 = knots[p.count + k + 1u];
    let m0 = knots[2u * p.count + k];
    let m1 = knots[2u * p.count + k + 1u];
    let h = x1 - x0;
    let t = clamp((v - x0) / h, 0.0, 1.0);
    let t2 = t * t;
    let t3 = t * t * t;
    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
    let h10 = t3 - 2.0 * t2 + t;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;
    buf[i] = h00 * y0 + h10 * h * m0 + h01 * y1 + h11 * h * m1;
}
