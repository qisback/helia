// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Forward transform along rows for the phase correlation: the frame is
// mean-subtracted, Hann-windowed and zero-padded to the transform length, as
// helia_core::align::global does, and only the non-negative frequencies are
// kept, stored transposed (`spec[frame][kx][y]`).
//
// One workgroup per (row, frame); every thread reaches every barrier, rows
// below the frame (padding) simply transform zeros.

struct Params {
    width: u32,
    height: u32,
    nx: u32,
    ny: u32,
    nxh: u32,
    frames: u32,
    pad0: u32,
    pad1: u32,
    plan: u32, // word offset of the plan in fft_plans
    pad2: u32,
    pad3: u32,
    pad4: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> mono: array<f32>;
@group(0) @binding(2) var<storage, read> mean: array<f32>;
@group(0) @binding(3) var<storage, read> win_x: array<f32>;
@group(0) @binding(4) var<storage, read> win_y: array<f32>;
@group(0) @binding(5) var<storage, read_write> spec: array<vec2<f32>>;
@group(0) @binding(6) var<storage, read> fft_plans: array<u32>;

@compute @workgroup_size(128)
fn main(
    @builtin(local_invocation_index) lid: u32,
    @builtin(workgroup_id) wid: vec3<u32>,
) {
    let y = wid.x;
    let f = wid.y;
    let live = y < p.height;
    let row = f * p.width * p.height + select(0u, y, live) * p.width;
    let wy = win_y[select(0u, y, live)];
    for (var n = lid; n < p.nx; n = n + 128u) {
        var v = 0.0;
        if (live && n < p.width) {
            v = (mono[row + n] - mean[f]) * wy * win_x[n];
        }
        fft_buf[fft_slot(p.plan, n)] = vec2<f32>(v, 0.0);
    }
    workgroupBarrier();
    fft_run(p.plan, -1.0, lid, 128u);
    for (var kx = lid; kx < p.nxh; kx = kx + 128u) {
        spec[(f * p.nxh + kx) * p.ny + y] = fft_buf[kx];
    }
}
