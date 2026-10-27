// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// In-place mixed-radix (2, 3, 4, 5) FFT of one line held in workgroup memory.
//
// Decimation in time. The input is placed in mixed-radix digit-reversed
// order, then each stage combines `radix` neighbouring sub-transforms, all in
// the same array: a butterfly reads and writes the same `radix` positions.
// With radices r_1 .. r_m and spans L_0 = 1, L_s = L_(s-1) * r_s, stage s
// forms, for every group G and k1 < L_(s-1), the butterfly over the elements
// at G * L_s + k1 + a * L_(s-1) (a < r_s), after multiplying element a by
// exp(sign * 2 pi i * a * k1 / L_s).
//
// The mixed-radix decimation-in-time scheme is the Cooley-Tukey algorithm: Cooley, J. W. and Tukey,
// J. W. (1965), Math. Comput. 19, 297-301, doi:10.2307/2003354. Written from that description.
//
// This text is prepended to the registration kernels.

// A plan is 32 words in the kernel's `fft_plans` storage buffer (which the
// kernel declares): [n, stages, pad, pad, radix x 12, span x 16], read from
// the word offset `plan`.
const TAU: f32 = 6.283185307179586;
const SQRT3_2: f32 = 0.8660254037844386;
const C1: f32 = 0.30901699437494745; // cos(2 pi / 5)
const C2: f32 = -0.8090169943749473; // cos(4 pi / 5)
const S1: f32 = 0.9510565162951535; // sin(2 pi / 5)
const S2: f32 = 0.5877852522924731; // sin(4 pi / 5)

var<workgroup> fft_buf: array<vec2<f32>, 2048>;

fn cmul(a: vec2<f32>, b: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(a.x * b.x - a.y * b.y, a.x * b.y + a.y * b.x);
}

// i * v
fn times_i(v: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(-v.y, v.x);
}

fn radix_of(plan: u32, s: u32) -> u32 {
    return fft_plans[plan + 4u + s];
}

fn span_of(plan: u32, s: u32) -> u32 {
    return fft_plans[plan + 16u + s];
}

// Position of input sample n after the digit reversal.
fn fft_slot(plan: u32, n: u32) -> u32 {
    var rem = n;
    var pos = 0u;
    for (var s = fft_plans[plan + 1u]; s > 0u; s = s - 1u) {
        let r = radix_of(plan, s - 1u);
        pos = pos + (rem % r) * span_of(plan, s - 1u);
        rem = rem / r;
    }
    return pos;
}

// exp(sign * i * TAU * num / den) for num < den, with the angle reduced to one turn.
fn twiddle(num: u32, den: u32, sign: f32) -> vec2<f32> {
    let a = TAU * f32(num) / f32(den);
    return vec2<f32>(cos(a), sign * sin(a));
}

// Run all stages over fft_buf. Every thread of the workgroup must call this
// (it contains barriers). `sign` is -1 for the forward transform, +1 inverse.
fn fft_run(plan: u32, sign: f32, lid: u32, threads: u32) {
    let n = fft_plans[plan];
    for (var s = 0u; s < fft_plans[plan + 1u]; s = s + 1u) {
        let r = radix_of(plan, s);
        let lprev = span_of(plan, s);
        let lcur = span_of(plan, s + 1u);
        let butterflies = n / r;
        for (var j = lid; j < butterflies; j = j + threads) {
            let g = j / lprev;
            let k1 = j % lprev;
            let base = g * lcur + k1;
            var v0 = fft_buf[base];
            var v1 = vec2<f32>(0.0);
            var v2 = vec2<f32>(0.0);
            var v3 = vec2<f32>(0.0);
            var v4 = vec2<f32>(0.0);
            if (k1 == 0u) {
                v1 = fft_buf[base + lprev];
                if (r > 2u) { v2 = fft_buf[base + 2u * lprev]; }
                if (r > 3u) { v3 = fft_buf[base + 3u * lprev]; }
                if (r > 4u) { v4 = fft_buf[base + 4u * lprev]; }
            } else {
                v1 = cmul(fft_buf[base + lprev], twiddle(k1, lcur, sign));
                if (r > 2u) { v2 = cmul(fft_buf[base + 2u * lprev], twiddle(2u * k1, lcur, sign)); }
                if (r > 3u) { v3 = cmul(fft_buf[base + 3u * lprev], twiddle(3u * k1, lcur, sign)); }
                if (r > 4u) { v4 = cmul(fft_buf[base + 4u * lprev], twiddle(4u * k1, lcur, sign)); }
            }
            if (r == 2u) {
                fft_buf[base] = v0 + v1;
                fft_buf[base + lprev] = v0 - v1;
            } else if (r == 3u) {
                let t1 = v1 + v2;
                let t2 = v0 - 0.5 * t1;
                let d = times_i(v1 - v2) * (sign * SQRT3_2);
                fft_buf[base] = v0 + t1;
                fft_buf[base + lprev] = t2 + d;
                fft_buf[base + 2u * lprev] = t2 - d;
            } else if (r == 4u) {
                let a = v0 + v2;
                let b = v0 - v2;
                let c = v1 + v3;
                let d = times_i(v1 - v3) * sign;
                fft_buf[base] = a + c;
                fft_buf[base + lprev] = b + d;
                fft_buf[base + 2u * lprev] = a - c;
                fft_buf[base + 3u * lprev] = b - d;
            } else {
                let t1 = v1 + v4;
                let t2 = v2 + v3;
                let t3 = v1 - v4;
                let t4 = v2 - v3;
                let a1 = v0 + C1 * t1 + C2 * t2;
                let a2 = v0 + C2 * t1 + C1 * t2;
                let b1 = times_i(S1 * t3 + S2 * t4) * sign;
                let b2 = times_i(S2 * t3 - S1 * t4) * sign;
                fft_buf[base] = v0 + t1 + t2;
                fft_buf[base + lprev] = a1 + b1;
                fft_buf[base + 4u * lprev] = a1 - b1;
                fft_buf[base + 2u * lprev] = a2 + b2;
                fft_buf[base + 3u * lprev] = a2 - b2;
            }
        }
        workgroupBarrier();
    }
}
