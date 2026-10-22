// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Limb-darkening flatten in place, mirroring helia_core::limbdark::FlattenPlan::apply:
// inside the disc, v = background + (v - background) * gain(r / R), with the
// gain read linearly from a table of BINS + 1 nodes.

struct Params {
    n: u32,
    stride: u32, // threads per row of the 2-D dispatch
    width: u32,
    bins: u32,
    cx: f32,
    cy: f32,
    inv_r: f32,
    background: f32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> gain: array<f32>;
@group(0) @binding(2) var<storage, read_write> buf: array<f32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.y * p.stride + id.x;
    if (i >= p.n) {
        return;
    }
    let x = f32(i % p.width);
    let y = f32(i / p.width);
    let dx = x - p.cx;
    let dy = y - p.cy;
    let u = sqrt(dx * dx + dy * dy) * p.inv_r;
    if (u < 1.0) {
        let bins = f32(p.bins);
        let t = clamp(u * bins, 0.0, bins);
        let k = min(u32(t), p.bins - 1u);
        let f = t - f32(k);
        let g = gain[k] * (1.0 - f) + gain[k + 1u] * f;
        buf[i] = p.background + (buf[i] - p.background) * g;
    }
}
