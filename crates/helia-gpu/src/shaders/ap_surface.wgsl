// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// Zero-normalised cross-correlation surface of every alignment point in every
// frame of a batch, mirroring helia_core::align::multipoint::correlation_surface,
// plus the window sums of the best offset for the sharpness score.
//
// One workgroup per (point, frame). A thread owns three neighbouring offsets
// in x of one row of the search area and one band of template rows, so each
// patch and template sample it loads feeds three offsets (a third of the
// loads per multiply-add); the partial sums are combined through workgroup
// memory. The surface itself stays on the device: the best offset is found
// here, and the record per (frame, point), `stride` (14) floats, holds
//   0       index of the best offset in the surface, -1 if none
//   1       1 if the peak can be fitted (correlation at least `min_corr`,
//           not on the border of the search area, 3x3 neighbourhood defined)
//   2       the peak correlation
//   3, 4    sum of the 3x3-blurred samples in the window at the best offset,
//           sum of the squared gradient of the blurred frame there
//   5..14   the 3x3 neighbourhood of the peak, row-major (when 1 is set)
// The sub-pixel fit stays on the host: a fit on the device differs from it in
// the last digits (see the Performance guide).

struct Params {
    width: u32,
    height: u32,
    frames: u32,
    radius: u32,
    side: u32,
    points: u32,
    stride: u32,
    parts: u32,        // row bands per offset triple
    rows_per_part: u32,
    corr_stride: u32,  // elements between rows of `corr` (width + 2 * corr_pad)
    min_corr: f32,
    pad0: u32,
}

struct Point {
    x0: i32,
    y0: i32,
    rw: u32,
    rh: u32,
    toff: u32,
    norm: f32,
    pad0: u32,
    pad1: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> corr: array<f32>;
@group(0) @binding(2) var<storage, read> blurred: array<f32>;
@group(0) @binding(3) var<storage, read> energy: array<f32>;
@group(0) @binding(4) var<storage, read> tmpl: array<f32>;
@group(0) @binding(5) var<storage, read> points: array<Point>;
@group(0) @binding(6) var<storage, read> guess: array<vec2<i32>>;
@group(0) @binding(7) var<storage, read_write> out: array<f32>;

const NONE: f32 = -3.0e38;

// Partial sums per item: index (item * 3 + k) for offset k of the triple.
var<workgroup> part_sp: array<f32, 384>;
var<workgroup> part_spp: array<f32, 384>;
var<workgroup> part_stp: array<f32, 384>;
var<workgroup> surf: array<f32, 256>;
var<workgroup> red_a: array<f32, 128>;
var<workgroup> red_b: array<f32, 128>;
var<workgroup> best_index: i32;

@compute @workgroup_size(128)
fn main(
    @builtin(local_invocation_index) lid: u32,
    @builtin(workgroup_id) wid: vec3<u32>,
) {
    let pt = points[wid.x];
    let f = wid.y;
    let g = guess[f];
    let r = i32(p.radius);
    let side = i32(p.side);
    let n_off = side * side;
    let w = i32(p.width);
    let h = i32(p.height);
    let plane = f * p.width * p.height;
    let rw = i32(pt.rw);
    let rh = i32(pt.rh);
    let area = f32(pt.rw * pt.rh);
    let triples = (p.side + 2u) / 3u;
    let items = p.side * triples * p.parts;

    // Phase 1: partial sums of one item. `corr` has two zero columns either
    // side of every row, so the triple's reads never leave the buffer even when
    // some of its offsets fall off the frame (those are discarded below); an
    // item whose rows leave the frame, or whose three offsets all do, is skipped.
    let corr_plane = f * p.corr_stride * p.height;
    if (lid < items) {
        let part = lid % p.parts;
        let triple = (lid / p.parts) % triples;
        let oy = i32(lid / (p.parts * triples)) - r;
        let ox0 = i32(triple) * 3 - r;
        let x0 = pt.x0 + g.x + ox0;
        let y0 = pt.y0 + g.y + oy;
        var sp0 = 0.0; var spp0 = 0.0; var stp0 = 0.0;
        var sp1 = 0.0; var spp1 = 0.0; var stp1 = 0.0;
        var sp2 = 0.0; var spp2 = 0.0; var stp2 = 0.0;
        let rows_ok = y0 >= 0 && y0 + rh <= h;
        let cols_ok = x0 + 2 >= 0 && x0 + rw <= w;
        if (rows_ok && cols_ok) {
            let row_lo = i32(part * p.rows_per_part);
            let row_hi = min(row_lo + i32(p.rows_per_part), rh);
            for (var row = row_lo; row < row_hi; row = row + 1) {
                // Index of the first sample of offset 0 in this patch row.
                let ib = corr_plane + u32((y0 + row) * i32(p.corr_stride) + x0 + 2);
                let tb = pt.toff + u32(row * rw);
                var p0 = corr[ib];
                var p1 = corr[ib + 1u];
                var col = 0u;
                // Two columns at a time.
                for (; col + 1u < pt.rw; col = col + 2u) {
                    let q2 = corr[ib + col + 2u];
                    let q3 = corr[ib + col + 3u];
                    let t0 = tmpl[tb + col];
                    let t1 = tmpl[tb + col + 1u];
                    sp0 = sp0 + p0 + p1; spp0 = spp0 + p0 * p0 + p1 * p1; stp0 = stp0 + p0 * t0 + p1 * t1;
                    sp1 = sp1 + p1 + q2; spp1 = spp1 + p1 * p1 + q2 * q2; stp1 = stp1 + p1 * t0 + q2 * t1;
                    sp2 = sp2 + q2 + q3; spp2 = spp2 + q2 * q2 + q3 * q3; stp2 = stp2 + q2 * t0 + q3 * t1;
                    p0 = q2;
                    p1 = q3;
                }
                if (col < pt.rw) {
                    let a0 = p0;
                    let a1 = p1;
                    let a2 = corr[ib + col + 2u];
                    let t = tmpl[tb + col];
                    sp0 = sp0 + a0; spp0 = spp0 + a0 * a0; stp0 = stp0 + a0 * t;
                    sp1 = sp1 + a1; spp1 = spp1 + a1 * a1; stp1 = stp1 + a1 * t;
                    sp2 = sp2 + a2; spp2 = spp2 + a2 * a2; stp2 = stp2 + a2 * t;
                }
            }
        }
        part_sp[lid * 3u] = sp0; part_spp[lid * 3u] = spp0; part_stp[lid * 3u] = stp0;
        part_sp[lid * 3u + 1u] = sp1; part_spp[lid * 3u + 1u] = spp1; part_stp[lid * 3u + 1u] = stp1;
        part_sp[lid * 3u + 2u] = sp2; part_spp[lid * 3u + 2u] = spp2; part_stp[lid * 3u + 2u] = stp2;
    }
    workgroupBarrier();

    // Phase 2: one thread per offset combines the bands.
    for (var o = i32(lid); o < n_off; o = o + 128) {
        let oy = o / side - r;
        let ox = o % side - r;
        let x0 = pt.x0 + g.x + ox;
        let y0 = pt.y0 + g.y + oy;
        var val = NONE;
        if (x0 >= 0 && y0 >= 0 && x0 + rw <= w && y0 + rh <= h) {
            let triple = u32((ox + r) / 3);
            let k = u32((ox + r) % 3);
            var sp = 0.0;
            var spp = 0.0;
            var stp = 0.0;
            for (var part = 0u; part < p.parts; part = part + 1u) {
                let item = (u32(oy + r) * triples + triple) * p.parts + part;
                sp = sp + part_sp[item * 3u + k];
                spp = spp + part_spp[item * 3u + k];
                stp = stp + part_stp[item * 3u + k];
            }
            let v = spp - sp * sp / area;
            if (v > 1.0e-12) {
                val = stp / (pt.norm * sqrt(v));
            }
        }
        surf[o] = val;
    }
    workgroupBarrier();

    if (lid == 0u) {
        // First maximum in row-major order, as the CPU scans it.
        var best = NONE;
        var bi = -1;
        for (var i = 0; i < n_off; i = i + 1) {
            if (surf[i] > best) {
                best = surf[i];
                bi = i;
            }
        }
        best_index = bi;
    }
    workgroupBarrier();

    let rec = (f * p.points + wid.x) * p.stride;

    // Window sums at the best offset.
    var sum_a = 0.0;
    var sum_b = 0.0;
    let bi = best_index;
    if (bi >= 0) {
        let wx = pt.x0 + g.x + (bi % side - r);
        let wy = pt.y0 + g.y + (bi / side - r);
        // Each thread sums down a column, so neighbouring threads read
        // neighbouring samples of a row.
        for (var col = i32(lid); col < rw; col = col + 128) {
            for (var row = 0; row < rh; row = row + 1) {
                let at = plane + u32((wy + row) * w + wx + col);
                sum_a = sum_a + blurred[at];
                sum_b = sum_b + energy[at];
            }
        }
    }
    red_a[lid] = sum_a;
    red_b[lid] = sum_b;
    workgroupBarrier();
    for (var s = 64u; s > 0u; s = s >> 1u) {
        if (lid < s) {
            red_a[lid] = red_a[lid] + red_a[lid + s];
            red_b[lid] = red_b[lid] + red_b[lid + s];
        }
        workgroupBarrier();
    }
    if (lid == 0u) {
        var rec_out: array<f32, 14>;
        for (var i = 0u; i < 14u; i = i + 1u) {
            rec_out[i] = 0.0;
        }
        rec_out[0] = f32(bi);
        rec_out[3] = red_a[0];
        rec_out[4] = red_b[0];
        if (bi >= 0) {
            let best = surf[bi];
            rec_out[2] = best;
            let bx = bi % side;
            let by = bi / side;
            if (best >= p.min_corr && bx > 0 && by > 0 && bx < side - 1 && by < side - 1) {
                var nb: array<f32, 9>;
                var ok = true;
                for (var k = 0; k < 9; k = k + 1) {
                    let v = surf[(by + k / 3 - 1) * side + bx + k % 3 - 1];
                    nb[k] = v;
                    // Undefined (sentinel), infinite or NaN neighbours refuse the fit.
                    ok = ok && v > -1.0e30 && v < 3.0e38;
                }
                if (ok) {
                    rec_out[1] = 1.0;
                    for (var k = 0; k < 9; k = k + 1) {
                        rec_out[5 + k] = nb[k];
                    }
                }
            }
        }
        for (var i = 0u; i < 14u; i = i + 1u) {
            out[rec + i] = rec_out[i];
        }
    }
}
