// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// 3x3 binomial blur (clamp-to-edge) of every frame of a batch, mirroring
// helia_core::quality (blurred_region over the whole frame).

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
@group(0) @binding(2) var<storage, read_write> blurred: array<f32>;

fn px(base: u32, x: i32, y: i32) -> f32 {
    let cx = clamp(x, 0, i32(p.width) - 1);
    let cy = clamp(y, 0, i32(p.height) - 1);
    return mono[base + u32(cy) * p.width + u32(cx)];
}

fn hblur(base: u32, x: i32, y: i32) -> f32 {
    return 0.25 * (px(base, x - 1, y) + 2.0 * px(base, x, y) + px(base, x + 1, y));
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.width || gid.y >= p.height || gid.z >= p.frames) {
        return;
    }
    let base = gid.z * p.width * p.height;
    let x = i32(gid.x);
    let y = i32(gid.y);
    let up = hblur(base, x, max(y - 1, 0));
    let mid = hblur(base, x, y);
    let dn = hblur(base, x, min(y + 1, i32(p.height) - 1));
    blurred[base + gid.y * p.width + gid.x] = 0.25 * (up + 2.0 * mid + dn);
}
