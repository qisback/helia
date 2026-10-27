// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Vectorisable reductions: several independent f32 lanes widened to f64.

/// Sum of `x` using eight independent f32 lanes (vectorises), widened to f64.
pub(crate) fn sum_lanes(x: &[f32]) -> f64 {
    let mut lanes = [0.0f32; 8];
    let (chunks, rest) = x.as_chunks::<8>();
    for c in chunks {
        for (l, v) in lanes.iter_mut().zip(c) {
            *l += v;
        }
    }
    let tail: f32 = rest.iter().sum();
    lanes.iter().map(|&l| f64::from(l)).sum::<f64>() + f64::from(tail)
}

/// Sum of `x` and of `x * x`, as [`sum_lanes`].
pub(crate) fn sum_and_square_lanes(x: &[f32]) -> (f64, f64) {
    let mut lanes = [[0.0f32; 8]; 2];
    let (chunks, rest) = x.as_chunks::<8>();
    for c in chunks {
        let [la, lb] = &mut lanes;
        for ((a, b), v) in la.iter_mut().zip(lb.iter_mut()).zip(c) {
            *a += v;
            *b += v * v;
        }
    }
    let (mut ta, mut tb) = (0.0f32, 0.0f32);
    for &v in rest {
        ta += v;
        tb += v * v;
    }
    let add = |l: &[f32; 8], t: f32| l.iter().map(|&v| f64::from(v)).sum::<f64>() + f64::from(t);
    (add(&lanes[0], ta), add(&lanes[1], tb))
}
