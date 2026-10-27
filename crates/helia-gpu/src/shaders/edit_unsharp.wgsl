// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Unsharp mask from the image and its blur, mirroring
// helia_core::edit::wavelet::unsharp.

struct Params {
    n: u32,
    stride: u32, // threads per row of the 2-D dispatch
    amount: f32,
    threshold: f32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> src: array<f32>;
@group(0) @binding(2) var<storage, read> blur: array<f32>;
@group(0) @binding(3) var<storage, read_write> dst: array<f32>;

fn soft(d: f32, t: f32) -> f32 {
    let m = max(abs(d) - t, 0.0);
    return select(m, -m, d < 0.0);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.y * p.stride + id.x;
    if (i >= p.n) {
        return;
    }
    dst[i] = src[i] + p.amount * soft(src[i] - blur[i], p.threshold);
}
