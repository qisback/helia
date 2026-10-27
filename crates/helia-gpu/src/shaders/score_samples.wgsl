// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Every third pixel in both directions of each frame: the sample the CPU
// takes its noise floor (a low percentile) from, see helia_core::quality::noise_floor.

struct Params {
    width: u32,
    height: u32,
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
    metric: u32,
    frames: u32,
    groups_x: u32,
    groups_y: u32,
    samples: u32,
    sample_cols: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> mono: array<f32>;
@group(0) @binding(2) var<storage, read_write> out_samples: array<f32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= p.samples || gid.z >= p.frames) {
        return;
    }
    let sx = (i % p.sample_cols) * 3u;
    let sy = (i / p.sample_cols) * 3u;
    out_samples[gid.z * p.samples + i] = mono[gid.z * p.width * p.height + sy * p.width + sx];
}
