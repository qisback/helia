// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Transform along columns of the half spectrum, `[frame][kx][ky]` with ky
// contiguous. One workgroup per (kx, frame). `inverse` selects the sign.

struct Params {
    ny: u32,
    nxh: u32,
    frames: u32,
    inverse: u32,
    plan: u32, // word offset of the plan in fft_plans
    pad2: u32,
    pad3: u32,
    pad4: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> src: array<vec2<f32>>;
@group(0) @binding(2) var<storage, read_write> dst: array<vec2<f32>>;
@group(0) @binding(3) var<storage, read> fft_plans: array<u32>;

@compute @workgroup_size(128)
fn main(
    @builtin(local_invocation_index) lid: u32,
    @builtin(workgroup_id) wid: vec3<u32>,
) {
    let base = (wid.y * p.nxh + wid.x) * p.ny;
    for (var n = lid; n < p.ny; n = n + 128u) {
        fft_buf[fft_slot(p.plan, n)] = src[base + n];
    }
    workgroupBarrier();
    var sign = -1.0;
    if (p.inverse != 0u) {
        sign = 1.0;
    }
    fft_run(p.plan, sign, lid, 128u);
    for (var n = lid; n < p.ny; n = n + 128u) {
        dst[base + n] = fft_buf[n];
    }
}
