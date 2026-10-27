// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Weighted-mean accumulation of alignment point tiles from a batch of decoded
// frames, mirroring helia_core::pipeline tile stacking (resample_region then
// TileStack::add): sums are kept relative to the reference over the tile.
//
// One thread per tile pixel (x) and tile (y); each loops over the batch.

struct Params {
    width: u32,
    height: u32,
    frames: u32,
    tiles: u32,
}

struct Deposit {
    dx: f32,
    dy: f32,
    weight: f32,
    valid: u32,
    ox: i32,
    oy: i32,
    n_taps: u32,
    lo: i32,
    wx: array<vec4<f32>, 2>,
    wy: array<vec4<f32>, 2>,
}

struct Tile {
    x0: u32,
    y0: u32,
    w: u32,
    h: u32,
    off: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> mono: array<f32>;
@group(0) @binding(2) var<storage, read> deposits: array<Deposit>;
@group(0) @binding(3) var<storage, read> tiles: array<Tile>;
@group(0) @binding(4) var<storage, read> centre: array<f32>;
@group(0) @binding(5) var<storage, read_write> tile_sum: array<f32>;
@group(0) @binding(6) var<storage, read_write> tile_weight: array<f32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.y >= p.tiles) {
        return;
    }
    let t = tiles[gid.y];
    if (gid.x >= t.w * t.h) {
        return;
    }
    let px = t.x0 + gid.x % t.w;
    let py = t.y0 + gid.x / t.w;
    let max_x = i32(p.width) - 1;
    let max_y = i32(p.height) - 1;
    let c = centre[t.off + gid.x];
    var sum = 0.0;
    var wsum = 0.0;
    for (var f = 0u; f < p.frames; f = f + 1u) {
        let d = deposits[f * p.tiles + gid.y];
        if (d.valid == 0u) {
            continue;
        }
        let sx = f32(px) + d.dx;
        let sy = f32(py) + d.dy;
        if (!(sx >= 0.0 && sx <= f32(max_x)) || !(sy >= 0.0 && sy <= f32(max_y))) {
            continue;
        }
        let base = f * p.width * p.height;
        var acc = 0.0;
        for (var j = 0u; j < d.n_taps; j = j + 1u) {
            let yy = clamp(i32(py) + d.oy + d.lo + i32(j), 0, max_y);
            var racc = 0.0;
            for (var i = 0u; i < d.n_taps; i = i + 1u) {
                let xx = clamp(i32(px) + d.ox + d.lo + i32(i), 0, max_x);
                racc = racc + d.wx[i >> 2u][i & 3u] * mono[base + u32(yy) * p.width + u32(xx)];
            }
            acc = acc + d.wy[j >> 2u][j & 3u] * racc;
        }
        sum = sum + d.weight * (acc - c);
        wsum = wsum + d.weight;
    }
    let idx = t.off + gid.x;
    tile_sum[idx] = tile_sum[idx] + sum;
    tile_weight[idx] = tile_weight[idx] + wsum;
}
