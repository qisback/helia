// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Per-workgroup sums of the sharpness terms of the blurred frames, mirroring
// helia_core::quality::score_with_floor. The host adds the partial sums in f64.

struct Params {
    width: u32,
    height: u32,
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
    metric: u32, // 0 = gradient energy, 1 = Laplacian variance
    frames: u32,
    groups_x: u32,
    groups_y: u32,
    samples: u32,
    sample_cols: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> blurred: array<f32>;
@group(0) @binding(2) var<storage, read_write> partials: array<f32>;

fn bl(base: u32, x: i32, y: i32) -> f32 {
    let cx = clamp(x, 0, i32(p.width) - 1);
    let cy = clamp(y, 0, i32(p.height) - 1);
    return blurred[base + u32(cy) * p.width + u32(cx)];
}

var<workgroup> s_mean: array<f32, 256>;
var<workgroup> s_a: array<f32, 256>;
var<workgroup> s_b: array<f32, 256>;

@compute @workgroup_size(16, 16)
fn main(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(local_invocation_index) lid: u32,
    @builtin(workgroup_id) wid: vec3<u32>,
) {
    let base = wid.z * p.width * p.height;
    let x = p.x0 + gid.x;
    let y = p.y0 + gid.y;
    var m = 0.0;
    var a = 0.0;
    var b = 0.0;
    if (x < p.x1 && y < p.y1 && wid.z < p.frames) {
        let xi = i32(x);
        let yi = i32(y);
        let c = bl(base, xi, yi);
        m = c;
        if (p.metric == 0u) {
            let gx = 0.5 * (bl(base, xi + 1, yi) - bl(base, xi - 1, yi));
            let gy = 0.5 * (bl(base, xi, yi + 1) - bl(base, xi, yi - 1));
            a = gx * gx + gy * gy;
        } else {
            let l = bl(base, xi - 1, yi) + bl(base, xi + 1, yi) + bl(base, xi, yi - 1) + bl(base, xi, yi + 1) - 4.0 * c;
            a = l;
            b = l * l;
        }
    }
    s_mean[lid] = m;
    s_a[lid] = a;
    s_b[lid] = b;
    workgroupBarrier();
    for (var stride = 128u; stride > 0u; stride = stride >> 1u) {
        if (lid < stride) {
            s_mean[lid] = s_mean[lid] + s_mean[lid + stride];
            s_a[lid] = s_a[lid] + s_a[lid + stride];
            s_b[lid] = s_b[lid] + s_b[lid + stride];
        }
        workgroupBarrier();
    }
    if (lid == 0u && wid.z < p.frames) {
        let idx = ((wid.z * p.groups_y + wid.y) * p.groups_x + wid.x) * 3u;
        partials[idx] = s_mean[0];
        partials[idx + 1u] = s_a[0];
        partials[idx + 2u] = s_b[0];
    }
}
