// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Levels in place, mirroring helia_core::edit::tone::Levels::apply.

struct Params {
    n: u32,
    stride: u32, // threads per row of the 2-D dispatch
    black: f32,
    range: f32, // white - black
    inv_gamma: f32,
    out_black: f32,
    span: f32, // out_white - out_black
    use_pow: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read_write> buf: array<f32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.y * p.stride + id.x;
    if (i >= p.n) {
        return;
    }
    let v = clamp(buf[i], 0.0, 1.0);
    var t = clamp((v - p.black) / p.range, 0.0, 1.0);
    if (p.use_pow != 0u && t > 0.0) {
        t = pow(t, p.inv_gamma);
    }
    buf[i] = p.out_black + p.span * t;
}
