// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// The limb and sunspot ringing guard, mirroring
// helia_core::edit::deconv::Protection::apply: hold the result to the observed
// range where the guard weight says so.

struct Params {
    n: u32,
    stride: u32, // threads per row of the 2-D dispatch
    pad0: u32,
    pad1: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> weight: array<f32>;
@group(0) @binding(2) var<storage, read> lo: array<f32>;
@group(0) @binding(3) var<storage, read> hi: array<f32>;
@group(0) @binding(4) var<storage, read_write> est: array<f32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.y * p.stride + id.x;
    if (i >= p.n) {
        return;
    }
    let e = est[i];
    let c = clamp(e, lo[i], hi[i]);
    est[i] = e + weight[i] * (c - e);
}
