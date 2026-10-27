// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Sub-pixel resampling.
//!
//! Convention: resampling by shift `s` produces `out(x, y) = src(x + s.dx, y + s.dy)`.
//! Pixels whose source position falls outside `[0, w-1] x [0, h-1]` are invalid
//! and reported as NaN by [`resample_region`].
//!
//! # References
//!
//! - Duchon, C. E. (1979), Lanczos filtering in one and two dimensions, J. Appl. Meteorol. 18, 1016-1022,
//!   doi:10.1175/1520-0450(1979)018<1016:LFIOAT>2.0.CO;2.

#![allow(
    clippy::cast_possible_wrap,
    reason = "pixel coordinates are bounded by the 65535 dimension limit and signed offsets are needed for clamping"
)]

use crate::align::Shift;
use crate::frame::{Frame, Rect};

/// Interpolation kernel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kernel {
    /// 2x2 bilinear.
    Bilinear,
    /// 6x6 Lanczos with a = 3.
    #[default]
    Lanczos3,
}

/// Lanczos-3 window function.
#[must_use]
pub fn lanczos3(x: f32) -> f32 {
    let ax = x.abs();
    if ax < 1e-6 {
        1.0
    } else if ax >= 3.0 {
        0.0
    } else {
        let px = std::f32::consts::PI * x;
        3.0 * px.sin() * (px / 3.0).sin() / (px * px)
    }
}

/// One-axis interpolation weights for a fractional offset.
#[derive(Clone, Copy, Debug)]
pub struct Taps {
    /// Offset of the first tap from `floor(position)`.
    pub lo: i64,
    /// Number of taps (2 or 6).
    pub n: usize,
    /// Tap weights, summing to one; entries past `n` are zero.
    pub w: [f32; 6],
}

impl Taps {
    /// Weights for sampling `frac` (in `[0, 1)`) past an integer position.
    #[must_use]
    pub fn new(kernel: Kernel, frac: f32) -> Self {
        match kernel {
            Kernel::Bilinear => Self {
                lo: 0,
                n: 2,
                w: [1.0 - frac, frac, 0.0, 0.0, 0.0, 0.0],
            },
            Kernel::Lanczos3 => {
                let mut w = [0.0f32; 6];
                let mut sum = 0.0;
                for (i, wv) in w.iter_mut().enumerate() {
                    *wv = lanczos3(frac - (i as f32 - 2.0));
                    sum += *wv;
                }
                for v in &mut w {
                    *v /= sum;
                }
                Self { lo: -2, n: 6, w }
            }
        }
    }
}

/// Split into integer floor and fractional part.
#[must_use]
pub fn split(v: f32) -> (i64, f32) {
    let f = v.floor();
    (f as i64, v - f)
}

/// Interpolate at an arbitrary point, clamping coordinates to the frame.
#[must_use]
pub fn sample(src: &Frame, x: f32, y: f32, kernel: Kernel) -> f32 {
    let (ix, fx) = split(x);
    let (iy, fy) = split(y);
    let tx = Taps::new(kernel, fx);
    let ty = Taps::new(kernel, fy);
    let (w, h) = (src.width as i64, src.height as i64);
    let mut acc = 0.0;
    for j in 0..ty.n {
        let yy = (iy + ty.lo + j as i64).clamp(0, h - 1) as usize;
        let row = src.row(yy);
        let mut racc = 0.0;
        for i in 0..tx.n {
            let xx = (ix + tx.lo + i as i64).clamp(0, w - 1) as usize;
            racc += tx.w[i] * row[xx];
        }
        acc += ty.w[j] * racc;
    }
    acc
}

/// Resample the part of the output grid covered by `rect` into `out`
/// (row-major, `rect.area()` long). Invalid pixels are NaN.
///
/// Separable: every source row the region touches is filtered horizontally
/// once, then each output row combines `n` of those rows. Per sample the
/// operations and their order are those of the direct 2-D evaluation.
///
/// # Panics
/// If `out.len() != rect.area()` or `rect` lies outside the frame (caller bug).
pub fn resample_region(src: &Frame, shift: Shift, rect: Rect, kernel: Kernel, out: &mut [f32]) {
    assert_eq!(out.len(), rect.area());
    let (w, h) = (src.width as i64, src.height as i64);
    let (ox, fx) = split(shift.dx);
    let (oy, fy) = split(shift.dy);
    let tx = Taps::new(kernel, fx);
    let ty = Taps::new(kernel, fy);
    let (rw, rh) = (rect.width(), rect.height());
    if rw == 0 || rh == 0 {
        return;
    }
    // Horizontally filtered source rows: row `r` is source row
    // `rect.y0 + oy + ty.lo + r`, clamped to the frame.
    let n_rows = rh + ty.n - 1;
    let mut hrows = vec![0.0f32; n_rows * rw];
    // Columns whose taps all lie inside the frame take the contiguous path.
    let first = rect.x0 as i64 + ox + tx.lo;
    let last = first + tx.n as i64 - 1;
    let fast_lo = ((-first).max(0) as usize).min(rw);
    let fast_hi = ((w - 1 - last + 1).clamp(0, rw as i64) as usize).max(fast_lo);
    for (r, hrow) in hrows.chunks_exact_mut(rw).enumerate() {
        let yy = (rect.y0 as i64 + oy + ty.lo + r as i64).clamp(0, h - 1) as usize;
        let row = src.row(yy);
        let clamped = |i: usize| {
            let mut acc = 0.0f32;
            for k in 0..tx.n {
                let xs = (rect.x0 as i64 + i as i64 + ox + tx.lo + k as i64).clamp(0, w - 1);
                acc += tx.w[k] * row[xs as usize];
            }
            acc
        };
        for i in (0..fast_lo).chain(fast_hi..rw) {
            hrow[i] = clamped(i);
        }
        if fast_hi > fast_lo {
            let start = (rect.x0 as i64 + fast_lo as i64 + ox + tx.lo) as usize;
            let n = fast_hi - fast_lo;
            let o = &mut hrow[fast_lo..fast_hi];
            o.fill(0.0);
            for k in 0..tx.n {
                let wk = tx.w[k];
                for (o, &v) in o.iter_mut().zip(&row[start + k..start + k + n]) {
                    *o += wk * v;
                }
            }
        }
    }
    let xvalid: Vec<bool> = (rect.x0..rect.x1)
        .map(|x| {
            let sx = x as f32 + shift.dx;
            sx >= 0.0 && sx <= (w - 1) as f32
        })
        .collect();
    for (row_out, y) in (rect.y0..rect.y1).enumerate() {
        let sy = y as f32 + shift.dy;
        let out_row = &mut out[row_out * rw..(row_out + 1) * rw];
        if !(sy >= 0.0 && sy <= (h - 1) as f32) {
            out_row.fill(f32::NAN);
            continue;
        }
        out_row.fill(0.0);
        for j in 0..ty.n {
            let wj = ty.w[j];
            let hrow = &hrows[(row_out + j) * rw..(row_out + j + 1) * rw];
            for (o, &v) in out_row.iter_mut().zip(hrow) {
                *o += wj * v;
            }
        }
        for (o, &ok) in out_row.iter_mut().zip(&xvalid) {
            if !ok {
                *o = f32::NAN;
            }
        }
    }
}

/// Whole-frame shift. Pixels sourced from outside the frame take the
/// clamped edge value rather than NaN.
#[must_use]
pub fn shifted(frame: &Frame, shift: Shift, kernel: Kernel) -> Frame {
    let mut out = vec![0.0; frame.width * frame.height];
    resample_region(frame, shift, frame.rect(), kernel, &mut out);
    for (i, v) in out.iter_mut().enumerate() {
        if v.is_nan() {
            let (x, y) = (i % frame.width, i / frame.width);
            *v = sample(frame, x as f32 + shift.dx, y as f32 + shift.dy, kernel);
        }
    }
    Frame {
        width: frame.width,
        height: frame.height,
        data: out,
        bayer: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(w: usize, h: usize) -> Frame {
        let data = (0..w * h)
            .map(|i| ((i % w) as f32) * 0.5 + ((i / w) as f32) * 0.25)
            .collect();
        Frame {
            width: w,
            height: h,
            data,
            bayer: None,
        }
    }

    /// Direct 2-D evaluation with explicit clamping: the definition of the region.
    fn direct_region(src: &Frame, shift: Shift, rect: Rect, kernel: Kernel) -> Vec<f32> {
        let (w, h) = (src.width as i64, src.height as i64);
        let (ox, fx) = split(shift.dx);
        let (oy, fy) = split(shift.dy);
        let (tx, ty) = (Taps::new(kernel, fx), Taps::new(kernel, fy));
        let mut out = Vec::new();
        for y in rect.y0..rect.y1 {
            for x in rect.x0..rect.x1 {
                let (sx, sy) = (x as f32 + shift.dx, y as f32 + shift.dy);
                if !(sx >= 0.0 && sx <= (w - 1) as f32 && sy >= 0.0 && sy <= (h - 1) as f32) {
                    out.push(f32::NAN);
                    continue;
                }
                let mut acc = 0.0f32;
                for j in 0..ty.n {
                    let yy = (y as i64 + oy + ty.lo + j as i64).clamp(0, h - 1) as usize;
                    let mut racc = 0.0f32;
                    for k in 0..tx.n {
                        let xx = (x as i64 + ox + tx.lo + k as i64).clamp(0, w - 1) as usize;
                        racc += tx.w[k] * src.row(yy)[xx];
                    }
                    acc += ty.w[j] * racc;
                }
                out.push(acc);
            }
        }
        out
    }

    #[test]
    fn separable_region_is_bit_identical_to_direct_evaluation() {
        let f = Frame {
            width: 37,
            height: 29,
            data: (0..37 * 29)
                .map(|i| ((i * 31 % 97) as f32) / 97.0)
                .collect(),
            bayer: None,
        };
        let shifts = [
            Shift::new(0.0, 0.0),
            Shift::new(0.37, -0.61),
            Shift::new(-3.2, 4.9),
            Shift::new(2.5, 0.001),
            Shift::new(40.0, -2.0),
            Shift::new(-0.5, 30.5),
        ];
        let rects = [
            f.rect(),
            Rect::new(0, 0, 8, 5),
            Rect::new(30, 20, 7, 9),
            Rect::new(10, 3, 1, 1),
            Rect::new(5, 0, 20, 29),
        ];
        for kernel in [Kernel::Bilinear, Kernel::Lanczos3] {
            for shift in shifts {
                for rect in rects {
                    let mut got = vec![0.0; rect.area()];
                    resample_region(&f, shift, rect, kernel, &mut got);
                    let want = direct_region(&f, shift, rect, kernel);
                    for (g, w) in got.iter().zip(&want) {
                        assert!(
                            g.to_bits() == w.to_bits(),
                            "{kernel:?} {shift:?} {rect:?}: {g} vs {w}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn kernels_reproduce_linear_ramp() {
        let f = ramp(20, 20);
        for k in [Kernel::Bilinear, Kernel::Lanczos3] {
            let v = sample(&f, 7.3, 9.6, k);
            let expect = 7.3 * 0.5 + 9.6 * 0.25;
            assert!((v - expect).abs() < 0.02, "{k:?} {v} vs {expect}");
        }
    }

    #[test]
    fn region_matches_point_sampling_and_flags_invalid() {
        let f = ramp(16, 16);
        let s = Shift { dx: 2.4, dy: -1.3 };
        let mut out = vec![0.0; 256];
        resample_region(&f, s, f.rect(), Kernel::Lanczos3, &mut out);
        let p = sample(&f, 5.0 + 2.4, 6.0 - 1.3, Kernel::Lanczos3);
        assert!((out[6 * 16 + 5] - p).abs() < 1e-4);
        assert!(out[0].is_nan(), "row 0 samples y=-1.3, outside");
        assert!(out[15].is_nan(), "x=17.4 is outside");
    }
}
