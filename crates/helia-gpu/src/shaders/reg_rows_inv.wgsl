// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Inverse transform along rows of the (column-inverted) half spectrum to the
// real correlation surface `surface[frame][y][x]`. The missing negative
// frequencies are the conjugates of the positive ones; the bins that must be
// real (DC, and Nyquist for even lengths) have their imaginary part dropped,
// as realfft does.

struct Params {
    nx: u32,
    ny: u32,
    nxh: u32,
    frames: u32,
    plan: u32, // word offset of the plan in fft_plans
    pad2: u32,
    pad3: u32,
    pad4: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> spec: array<vec2<f32>>;
@group(0) @binding(2) var<storage, read_write> surface: array<f32>;
@group(0) @binding(3) var<storage, read> fft_plans: array<u32>;

@compute @workgroup_size(128)
fn main(
    @builtin(local_invocation_index) lid: u32,
    @builtin(workgroup_id) wid: vec3<u32>,
) {
    let y = wid.x;
    let f = wid.y;
    let col = f * p.nxh * p.ny + y;
    for (var n = lid; n < p.nx; n = n + 128u) {
        var v: vec2<f32>;
        if (n < p.nxh) {
            v = spec[col + n * p.ny];
        } else {
            let t = spec[col + (p.nx - n) * p.ny];
            v = vec2<f32>(t.x, -t.y);
        }
        if (n == 0u || (p.nx % 2u == 0u && n == p.nx / 2u)) {
            v.y = 0.0;
        }
        fft_buf[fft_slot(p.plan, n)] = v;
    }
    workgroupBarrier();
    fft_run(p.plan, 1.0, lid, 128u);
    let out = (f * p.ny + y) * p.nx;
    for (var n = lid; n < p.nx; n = n + 128u) {
        surface[out + n] = fft_buf[n].x;
    }
}
