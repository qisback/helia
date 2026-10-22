// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Convolutions shared by the edit operations. All use clamp-to-edge borders
//! and add the taps in order from zero, which the GPU kernels repeat so the
//! two agree to float contraction.

use rayon::prelude::*;

/// Separable convolution with `taps` (odd length) spaced `step` pixels apart:
/// `step = 1` is an ordinary convolution, `step = 2^j` the a-trous ("with
/// holes") form.
#[must_use]
pub fn separable(src: &[f32], w: usize, h: usize, taps: &[f32], step: usize) -> Vec<f32> {
    debug_assert_eq!(src.len(), w * h);
    let reach = (taps.len() / 2) * step;
    let mut tmp = vec![0.0f32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, out)| {
        let row = &src[y * w..(y + 1) * w];
        let mut pad = vec![0.0f32; w + 2 * reach];
        pad[..reach].fill(row[0]);
        pad[reach..reach + w].copy_from_slice(row);
        pad[reach + w..].fill(row[w - 1]);
        for (i, kv) in taps.iter().enumerate() {
            let o = i * step;
            for (d, p) in out.iter_mut().zip(&pad[o..o + w]) {
                *d += kv * p;
            }
        }
    });
    let mut out = vec![0.0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, dst)| {
        for (i, kv) in taps.iter().enumerate() {
            let yi = (y + i * step).saturating_sub(reach).min(h - 1);
            for (d, s) in dst.iter_mut().zip(&tmp[yi * w..(yi + 1) * w]) {
                *d += kv * s;
            }
        }
    });
    out
}

/// Direct 2-D convolution with a square kernel of side `2 * radius + 1`.
#[must_use]
pub fn direct(src: &[f32], w: usize, h: usize, kernel: &[f32], radius: usize) -> Vec<f32> {
    let side = 2 * radius + 1;
    debug_assert_eq!(kernel.len(), side * side);
    let mut out = vec![0.0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, dst)| {
        for (x, d) in dst.iter_mut().enumerate() {
            let mut acc = 0.0f32;
            for j in 0..side {
                let yy = (y + j).saturating_sub(radius).min(h - 1);
                let row = &src[yy * w..(yy + 1) * w];
                for i in 0..side {
                    let xx = (x + i).saturating_sub(radius).min(w - 1);
                    acc += kernel[j * side + i] * row[xx];
                }
            }
            *d = acc;
        }
    });
    out
}

/// Running minimum or maximum over a square window of half-width `radius`
/// (clamped at the borders), as two separable passes.
#[must_use]
pub fn window_extreme(src: &[f32], w: usize, h: usize, radius: usize, max: bool) -> Vec<f32> {
    let pick = |a: f32, b: f32| if max { a.max(b) } else { a.min(b) };
    let mut tmp = vec![0.0f32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, out)| {
        let row = &src[y * w..(y + 1) * w];
        for (x, o) in out.iter_mut().enumerate() {
            let (a, b) = (x.saturating_sub(radius), (x + radius).min(w - 1));
            *o = row[a..=b].iter().copied().fold(row[x], pick);
        }
    });
    let mut out = vec![0.0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, dst)| {
        let (a, b) = (y.saturating_sub(radius), (y + radius).min(h - 1));
        dst.copy_from_slice(&tmp[y * w..(y + 1) * w]);
        for yy in a..=b {
            for (d, s) in dst.iter_mut().zip(&tmp[yy * w..(yy + 1) * w]) {
                *d = pick(*d, *s);
            }
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_tap_is_a_copy() {
        let src: Vec<f32> = (0..35).map(|i| i as f32).collect();
        assert_eq!(separable(&src, 7, 5, &[0.0, 1.0, 0.0], 3), src);
    }

    #[test]
    fn direct_matches_separable_for_an_outer_product() {
        let (w, h) = (11, 9);
        let src: Vec<f32> = (0..w * h).map(|i| ((i * 31) % 17) as f32 / 17.0).collect();
        let t = [0.25f32, 0.5, 0.25];
        let mut k2 = vec![0.0f32; 9];
        for j in 0..3 {
            for i in 0..3 {
                k2[j * 3 + i] = t[j] * t[i];
            }
        }
        let a = separable(&src, w, h, &t, 1);
        let b = direct(&src, w, h, &k2, 1);
        for (x, y) in a.iter().zip(&b) {
            assert!((x - y).abs() < 1e-6);
        }
    }

    #[test]
    fn extremes_find_the_window_max_and_min() {
        let mut src = vec![0.5f32; 25];
        src[12] = 1.0;
        src[0] = 0.0;
        let hi = window_extreme(&src, 5, 5, 1, true);
        let lo = window_extreme(&src, 5, 5, 1, false);
        assert_eq!(hi[6], 1.0);
        assert_eq!(hi[24], 0.5);
        assert_eq!(lo[6], 0.0);
        assert_eq!(lo[24], 0.5);
    }
}
