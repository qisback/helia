// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Vertical pass of the separable Gaussian blur (see gauss_h.wgsl). With
// mode 1 the result is subtracted from `minuend`: the background-removed
// frame used for alignment point correlation is `scored - blur(frame)`.
// The output rows may be padded (`out_stride` > width, `out_pad` columns on
// the left), which the correlation kernel relies on to read past the edges.

struct Params {
    width: u32,
    height: u32,
    frames: u32,
    radius: u32,
    mode: u32,
    out_stride: u32, // elements between output rows (width when unpadded)
    out_pad: u32,    // output columns left of the image in each row
    pad2: u32,
    taps: array<vec4<f32>, 32>,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> src: array<f32>;
@group(0) @binding(2) var<storage, read_write> dst: array<f32>;
@group(0) @binding(3) var<storage, read> minuend: array<f32>;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.width || gid.y >= p.height || gid.z >= p.frames) {
        return;
    }
    let plane = gid.z * p.width * p.height;
    let last = i32(p.height) - 1;
    var acc = 0.0;
    for (var i = 0u; i < 2u * p.radius + 1u; i = i + 1u) {
        let ys = clamp(i32(gid.y) + i32(i) - i32(p.radius), 0, last);
        acc = acc + p.taps[i >> 2u][i & 3u] * src[plane + u32(ys) * p.width + gid.x];
    }
    let idx = plane + gid.y * p.width + gid.x;
    let out_idx = gid.z * p.out_stride * p.height + gid.y * p.out_stride + p.out_pad + gid.x;
    if (p.mode == 1u) {
        dst[out_idx] = minuend[idx] - acc;
    } else {
        dst[out_idx] = acc;
    }
}
