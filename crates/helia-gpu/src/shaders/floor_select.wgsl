// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// The noise floor of each frame: the value of rank n / 100 (counting from the
// smallest, from zero) among the n finite samples taken at every third pixel in
// both directions, as `floor_of` in score.rs does on the host. A selection by
// radix (four passes of eight bits over an order-preserving key), so that the
// answer is a sample of the frame, not an approximation, and does not depend
// on the order the threads add in.
//
// Eight dispatches per batch, `hist` then `pick`, four times. `hist` runs
// SLICES workgroups per frame and adds a histogram of the next byte of the
// samples that match the bytes chosen so far into the frame's record in `sel`;
// `pick` (one thread per frame) takes the byte holding the wanted rank, clears
// the histogram and, after the fourth byte, writes the floor and clears the
// record, so the buffers are zero again for the next batch. The record of a
// frame is 256 histogram words followed by [prefix, rank, count, step];
// after the records come the floors, as bit patterns, from word `floor_base`.

struct Params {
    width: u32,
    height: u32,
    frames: u32,
    samples: u32,
    sample_cols: u32,
    slices: u32,
    floor_base: u32,
    pad2: u32,
}

const RECORD: u32 = 260u;

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read_write> sel: array<atomic<u32>>;
@group(0) @binding(2) var<storage, read> mono: array<f32>;

var<workgroup> local: array<atomic<u32>, 256>;
var<workgroup> n_local: atomic<u32>;

fn key_of(bits: u32) -> u32 {
    // Total order of f32 bit patterns, as f32::total_cmp.
    return select(bits | 0x80000000u, ~bits, (bits & 0x80000000u) != 0u);
}

// The bytes above the one being counted, with no variable shift count.
fn high_mask(step: u32) -> u32 {
    var m = 0u;
    if (step == 1u) { m = 0xff000000u; }
    if (step == 2u) { m = 0xffff0000u; }
    if (step == 3u) { m = 0xffffff00u; }
    return m;
}

@compute @workgroup_size(256)
fn hist(@builtin(local_invocation_index) lid: u32, @builtin(workgroup_id) wid: vec3<u32>) {
    let rec = wid.y * RECORD;
    let base = wid.y * p.width * p.height;
    let step = atomicLoad(&sel[rec + 259u]);
    let shift = 24u - 8u * step;
    // Samples whose higher bytes equal the bytes chosen so far.
    let high = high_mask(step);
    let want = atomicLoad(&sel[rec + 256u]);
    atomicStore(&local[lid], 0u);
    if (lid == 0u) {
        atomicStore(&n_local, 0u);
    }
    workgroupBarrier();
    var mine = 0u;
    for (var i = wid.x * 256u + lid; i < p.samples; i = i + p.slices * 256u) {
        let sx = (i % p.sample_cols) * 3u;
        let sy = (i / p.sample_cols) * 3u;
        let bits = bitcast<u32>(mono[base + sy * p.width + sx]);
        if ((bits & 0x7f800000u) != 0x7f800000u) {
            mine = mine + 1u;
            let key = key_of(bits);
            if ((key & high) == want) {
                atomicAdd(&local[(key >> shift) & 255u], 1u);
            }
        }
    }
    if (step == 0u) {
        atomicAdd(&n_local, mine);
    }
    workgroupBarrier();
    let c = atomicLoad(&local[lid]);
    if (c != 0u) {
        atomicAdd(&sel[rec + lid], c);
    }
    if (step == 0u && lid == 0u) {
        atomicAdd(&sel[rec + 258u], atomicLoad(&n_local));
    }
}

@compute @workgroup_size(1)
fn pick(@builtin(workgroup_id) wid: vec3<u32>) {
    let rec = wid.x * RECORD;
    let step = atomicLoad(&sel[rec + 259u]);
    if (step == 0u) {
        atomicStore(&sel[rec + 257u], atomicLoad(&sel[rec + 258u]) / 100u);
    }
    var rank = atomicLoad(&sel[rec + 257u]);
    // The bin holding `rank`, scanned over all 256 bins with no early exit and
    // no short-circuit: a loop with a data-dependent break and a value used
    // after it was miscompiled by the DX12 shader compiler on WARP.
    var cum = 0u;
    var bin = 255u;
    var done = 0u;
    for (var i = 0u; i < 256u; i = i + 1u) {
        let c = atomicLoad(&sel[rec + i]);
        let hit = (1u - done) & u32(cum + c > rank);
        bin = select(bin, i, hit == 1u);
        done = done | hit;
        cum = cum + c * (1u - done);
    }
    for (var i = 0u; i < 256u; i = i + 1u) {
        atomicStore(&sel[rec + i], 0u);
    }
    let prefix = atomicLoad(&sel[rec + 256u]) | (bin << (24u - 8u * step));
    if (step < 3u) {
        atomicStore(&sel[rec + 256u], prefix);
        atomicStore(&sel[rec + 257u], rank - cum);
        atomicStore(&sel[rec + 259u], step + 1u);
    } else {
        // No finite sample: the floor is zero.
        let n = atomicLoad(&sel[rec + 258u]);
        let bits = select(~prefix, prefix & 0x7fffffffu, (prefix & 0x80000000u) != 0u);
        atomicStore(&sel[p.floor_base + wid.x], select(bits, 0u, n == 0u));
        for (var i = 256u; i < RECORD; i = i + 1u) {
            atomicStore(&sel[rec + i], 0u);
        }
    }
}
