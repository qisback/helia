// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Squared gradient magnitude of the 3x3-blurred frames with clamp-to-edge
// neighbours: the per-pixel term of the gradient-energy sharpness, see
// helia_core::quality::WindowScorer.

struct Params {
    width: u32,
    height: u32,
    frames: u32,
    pad: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> blurred: array<f32>;
@group(0) @binding(2) var<storage, read_write> energy: array<f32>;

fn bl(base: u32, x: i32, y: i32) -> f32 {
    let cx = clamp(x, 0, i32(p.width) - 1);
    let cy = clamp(y, 0, i32(p.height) - 1);
    return blurred[base + u32(cy) * p.width + u32(cx)];
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.width || gid.y >= p.height || gid.z >= p.frames) {
        return;
    }
    let base = gid.z * p.width * p.height;
    let x = i32(gid.x);
    let y = i32(gid.y);
    let gx = 0.5 * (bl(base, x + 1, y) - bl(base, x - 1, y));
    let gy = 0.5 * (bl(base, x, y + 1) - bl(base, x, y - 1));
    energy[base + gid.y * p.width + gid.x] = gx * gx + gy * gy;
}
