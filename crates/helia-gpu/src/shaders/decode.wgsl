// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Stored frame bytes to mono f32, mirroring helia_core::io::raw::RowLayout::row
// followed by helia_core::debayer::Debayer::apply_rows.
//
// The demosaic is one dense 5x5 stencil per Bayer phase (y % 2 * 2 + x % 2),
// summed in the same row-major order as the CPU; borders mirror by an even
// distance. Without a Bayer tag the stored value is returned as is.

struct Params {
    width: u32,
    height: u32,
    pitch_words: u32, // spacing of frames in `raw`, in u32 words
    stride: u32,      // bytes per stored row
    sample: u32,      // 0 u8, 1 u16 little-endian, 2 u16 big-endian, 3 f32
    planes: u32,      // 0 one plane, 1 mean of normalised planes, 2 integer mean of 8-bit planes
    bottom_up: u32,
    bayer: u32,
    frames: u32,
    out_base: u32,    // first frame slot written in `mono`
    pad0: u32,
    pad1: u32,
    stencil: array<vec4<f32>, 25>,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> raw: array<u32>;
@group(0) @binding(2) var<storage, read_write> mono: array<f32>;

fn byte_at(i: u32) -> u32 {
    return (raw[i >> 2u] >> ((i & 3u) * 8u)) & 255u;
}

fn u16_at(o: u32) -> f32 {
    var v: u32;
    if (p.sample == 1u) {
        v = byte_at(o) | (byte_at(o + 1u) << 8u);
    } else {
        v = (byte_at(o) << 8u) | byte_at(o + 1u);
    }
    return f32(v) / 65535.0;
}

// Normalised sample at image column x, row y (0 = top) of the frame at byte `base`.
fn value(base: u32, x: u32, y: u32) -> f32 {
    var sy = y;
    if (p.bottom_up != 0u) {
        sy = p.height - 1u - y;
    }
    let row = base + sy * p.stride;
    if (p.planes == 0u) {
        if (p.sample == 0u) {
            return f32(byte_at(row + x)) / 255.0;
        }
        if (p.sample == 3u) {
            return bitcast<f32>(raw[(row >> 2u) + x]);
        }
        return u16_at(row + 2u * x);
    }
    if (p.planes == 1u) {
        if (p.sample == 0u) {
            let o = row + 3u * x;
            return (f32(byte_at(o)) / 255.0 + f32(byte_at(o + 1u)) / 255.0 + f32(byte_at(o + 2u)) / 255.0) / 3.0;
        }
        let o = row + 6u * x;
        return (u16_at(o) + u16_at(o + 2u) + u16_at(o + 4u)) / 3.0;
    }
    let o = row + 3u * x;
    return (f32(byte_at(o)) + f32(byte_at(o + 1u)) + f32(byte_at(o + 2u))) / (3.0 * 255.0);
}

fn reflect_index(i: i32, n: i32) -> i32 {
    if (i < 0) {
        return -i;
    }
    if (i >= n) {
        return 2 * (n - 1) - i;
    }
    return i;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.width || gid.y >= p.height || gid.z >= p.frames) {
        return;
    }
    let base = gid.z * p.pitch_words * 4u;
    let out_idx = (p.out_base + gid.z) * p.width * p.height + gid.y * p.width + gid.x;
    if (p.bayer == 0u) {
        mono[out_idx] = value(base, gid.x, gid.y);
        return;
    }
    let phase = (gid.y & 1u) * 2u + (gid.x & 1u);
    var acc = 0.0;
    for (var r = 0; r < 5; r = r + 1) {
        let yy = reflect_index(i32(gid.y) + r - 2, i32(p.height));
        for (var c = 0; c < 5; c = c + 1) {
            let k = phase * 25u + u32(r * 5 + c);
            let coeff = p.stencil[k >> 2u][k & 3u];
            if (coeff != 0.0) {
                let xx = reflect_index(i32(gid.x) + c - 2, i32(p.width));
                acc = acc + coeff * value(base, u32(xx), u32(yy));
            }
        }
    }
    mono[out_idx] = acc;
}
