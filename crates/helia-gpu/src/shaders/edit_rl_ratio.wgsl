// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Richardson-Lucy ratio observed / (blurred estimate + eps), mirroring
// helia_core::edit::deconv::richardson_lucy.

struct Params {
    n: u32,
    stride: u32, // threads per row of the 2-D dispatch
    eps: f32,
    pad0: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> obs: array<f32>;
@group(0) @binding(2) var<storage, read> blur: array<f32>;
@group(0) @binding(3) var<storage, read_write> ratio: array<f32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.y * p.stride + id.x;
    if (i >= p.n) {
        return;
    }
    ratio[i] = obs[i] / (blur[i] + p.eps);
}
