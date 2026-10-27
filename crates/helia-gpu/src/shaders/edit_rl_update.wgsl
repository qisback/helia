// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Richardson-Lucy update with the total-variation divisor, mirroring
// helia_core::edit::deconv::richardson_lucy: next = est * corr / denom,
// denom = max(1 - tv * div(unit gradient of est), floor), next >= 0.

struct Params {
    width: u32,
    height: u32,
    tv: f32,
    eps2: f32,
    floor: f32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> est: array<f32>;
@group(0) @binding(2) var<storage, read> corr: array<f32>;
@group(0) @binding(3) var<storage, read_write> next: array<f32>;

fn at(x: i32, y: i32) -> f32 {
    let xx = clamp(x, 0, i32(p.width) - 1);
    let yy = clamp(y, 0, i32(p.height) - 1);
    return est[u32(yy) * p.width + u32(xx)];
}

fn unit_gradient(x: i32, y: i32) -> vec2<f32> {
    let c = at(x, y);
    let gx = at(x + 1, y) - c;
    let gy = at(x, y + 1) - c;
    let n = sqrt(gx * gx + gy * gy + p.eps2);
    return vec2<f32>(gx / n, gy / n);
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.width || gid.y >= p.height) {
        return;
    }
    let x = i32(gid.x);
    let y = i32(gid.y);
    let i = gid.y * p.width + gid.x;
    var denom = 1.0;
    if (p.tv > 0.0) {
        let here = unit_gradient(x, y);
        let left = unit_gradient(x - 1, y);
        let above = unit_gradient(x, y - 1);
        let div = (here.x - left.x) + (here.y - above.y);
        denom = max(1.0 - p.tv * div, p.floor);
    }
    next[i] = max(est[i] * corr[i] / denom, 0.0);
}
