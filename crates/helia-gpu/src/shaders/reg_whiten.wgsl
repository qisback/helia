// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Whitening and band-pass of the cross-power spectrum in place, as
// PhaseCorrelator::whiten: scale = low_y * low_x * (1 - stop_x * stop_y) /
// max(|c| + floor, tiny).

struct Params {
    nx: u32,
    ny: u32,
    nxh: u32,
    frames: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read_write> spec: array<vec2<f32>>;
@group(0) @binding(2) var<storage, read> floors: array<f32>;
@group(0) @binding(3) var<storage, read> low_x: array<f32>;
@group(0) @binding(4) var<storage, read> low_y: array<f32>;
@group(0) @binding(5) var<storage, read> stop_x: array<f32>;
@group(0) @binding(6) var<storage, read> stop_y: array<f32>;

@compute @workgroup_size(256)
fn main(
    @builtin(global_invocation_id) gid: vec3<u32>,
) {
    let total = p.nxh * p.ny;
    if (gid.x >= total || gid.y >= p.frames) {
        return;
    }
    let idx = gid.y * total + gid.x;
    let kx = gid.x / p.ny;
    let ky = gid.x % p.ny;
    let c = spec[idx];
    let m = sqrt(c.x * c.x + c.y * c.y);
    let high = 1.0 - stop_x[kx] * stop_y[ky];
    let scale = low_y[ky] * low_x[kx] * high / max(m + floors[gid.y], 1.17549435e-38);
    spec[idx] = c * scale;
}
