// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Gathers |a - b| on the regular grid the noise estimate samples, mirroring
// helia_core::edit::wavelet::mad_sigma_of_difference, so only a few thousand
// values go back to the host for the median.

struct Params {
    width: u32,
    nx: u32, // grid columns
    count: u32, // grid cells
    step: u32, // pixels between samples
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> a: array<f32>;
@group(0) @binding(2) var<storage, read> b: array<f32>;
@group(0) @binding(3) var<storage, read_write> out: array<f32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let k = id.x;
    if (k >= p.count) {
        return;
    }
    let x = (k % p.nx) * p.step;
    let y = (k / p.nx) * p.step;
    let i = y * p.width + x;
    out[k] = abs(a[i] - b[i]);
}
