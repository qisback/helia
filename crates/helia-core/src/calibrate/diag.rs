// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! What a flat can and cannot fix, read off the flat itself.
//!
//! A flat divides out dust shadows and vignetting because they are fixed to the
//! sensor and the optics in front of it. Newton rings and the fringes of an
//! etalon are not fixed in that way: they move with focus, tilt and
//! temperature. A flat taken at another focus leaves them in place, or adds a
//! second set. The checks here find rings, strong vignetting (in H-alpha, the
//! shift of an etalon's pass-band across the field, its "sweet spot") and the
//! extent of dust, and say so.

#![allow(
    clippy::many_single_char_names,
    reason = "x, y, p, r, m, s are the natural names in image maths"
)]

use crate::frame::Frame;

/// Numbers measured on a normalised flat.
#[derive(Clone, Debug, PartialEq)]
pub struct FlatDiagnostics {
    /// Mean of the outer part of the field divided by the mean of the centre.
    pub edge_to_centre: f32,
    /// Half the peak-to-peak size of the ring-like part of the radial profile,
    /// as a fraction of the flat (0.01 is one per cent).
    pub ring_amplitude: f32,
    /// Number of times the ring-like part crosses its trend, beyond noise.
    pub ring_crossings: usize,
    /// Fraction of pixels more than 5 per cent below their surroundings.
    pub dust_fraction: f32,
    /// What the numbers add up to, for the user.
    pub warnings: Vec<String>,
}

/// Radial bins are this many binned pixels wide.
const BIN: f32 = 1.5;

/// Edge/centre ratio below which the vignetting is called strong.
pub const STRONG_VIGNETTING: f32 = 0.75;
/// Ring amplitude (fraction) above which rings are reported.
pub const RING_AMPLITUDE: f32 = 0.004;
/// Crossings needed to call a pattern rings rather than a bump.
pub const RING_CROSSINGS: usize = 3;
/// Fraction of dust-shadowed pixels worth mentioning.
pub const DUST_FRACTION: f32 = 0.005;

/// Bin a flat 2 x 2 (so a mosaic's colours average out) into a smaller plane.
fn binned(f: &Frame) -> (Vec<f32>, usize, usize) {
    let (bw, bh) = (f.width / 2, f.height / 2);
    let mut out = vec![0.0f32; bw * bh];
    for y in 0..bh {
        for x in 0..bw {
            let i = 2 * y * f.width + 2 * x;
            out[y * bw + x] =
                (f.data[i] + f.data[i + 1] + f.data[i + f.width] + f.data[i + f.width + 1]) / 4.0;
        }
    }
    (out, bw, bh)
}

/// Least-squares polynomial of degree `deg` through `(x, y, weight)`.
fn polyfit(pts: &[(f64, f64, f64)], deg: usize) -> Option<Vec<f64>> {
    let n = deg + 1;
    let mut a = vec![vec![0.0f64; n + 1]; n];
    for &(x, y, w) in pts {
        let mut pw = vec![1.0f64; 2 * n];
        for k in 1..2 * n {
            pw[k] = pw[k - 1] * x;
        }
        for i in 0..n {
            for j in 0..n {
                a[i][j] += w * pw[i + j];
            }
            a[i][n] += w * y * pw[i];
        }
    }
    for c in 0..n {
        let p = (c..n).max_by(|&i, &j| a[i][c].abs().total_cmp(&a[j][c].abs()))?;
        if a[p][c].abs() < 1e-12 {
            return None;
        }
        a.swap(c, p);
        for r in 0..n {
            if r != c {
                let f = a[r][c] / a[c][c];
                let pivot = a[c].clone();
                for (v, p) in a[r][c..].iter_mut().zip(&pivot[c..]) {
                    *v -= f * p;
                }
            }
        }
    }
    Some((0..n).map(|i| a[i][n] / a[i][i]).collect())
}

/// Measure a normalised flat (mean 1 within each colour).
#[must_use]
#[allow(clippy::too_many_lines, reason = "one pass per measurement, in order")]
pub fn analyse(flat: &Frame) -> FlatDiagnostics {
    let (p, bw, bh) = binned(flat);
    let mut d = FlatDiagnostics {
        edge_to_centre: 1.0,
        ring_amplitude: 0.0,
        ring_crossings: 0,
        dust_fraction: 0.0,
        warnings: Vec::new(),
    };
    if bw < 16 || bh < 16 {
        return d;
    }
    let (cx, cy) = ((bw as f32 - 1.0) / 2.0, (bh as f32 - 1.0) / 2.0);
    let rmax = cx.min(cy);
    let nbin = (rmax / BIN) as usize;
    let mut sum = vec![0.0f64; nbin];
    let mut sq = vec![0.0f64; nbin];
    let mut cnt = vec![0.0f64; nbin];
    for y in 0..bh {
        for x in 0..bw {
            let r = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
            let b = (r / BIN) as usize;
            if b < nbin {
                let v = f64::from(p[y * bw + x]);
                sum[b] += v;
                sq[b] += v * v;
                cnt[b] += 1.0;
            }
        }
    }
    let prof: Vec<(f64, f64, f64)> = (0..nbin)
        .filter(|&b| cnt[b] >= 8.0)
        .map(|b| {
            let m = sum[b] / cnt[b];
            let var = (sq[b] / cnt[b] - m * m).max(0.0);
            // Position in 0..1; the error of the mean of the ring.
            (
                (b as f64 + 0.5) * f64::from(BIN) / f64::from(rmax),
                m,
                var / cnt[b],
            )
        })
        .collect();
    if prof.len() < 12 {
        return d;
    }
    let mean_of = |lo: f64, hi: f64| -> f64 {
        let v: Vec<f64> = prof
            .iter()
            .filter(|&&(x, _, _)| x >= lo && x < hi)
            .map(|&(_, m, _)| m)
            .collect();
        if v.is_empty() {
            1.0
        } else {
            v.iter().sum::<f64>() / v.len() as f64
        }
    };
    let centre = mean_of(0.0, 0.1);
    let edge = mean_of(0.8, 1.01);
    if centre > 0.0 {
        d.edge_to_centre = (edge / centre) as f32;
    }

    // Rings: what is left of the radial profile after a smooth trend.
    let pts: Vec<(f64, f64, f64)> = prof.iter().map(|&(x, m, _)| (x, m, 1.0)).collect();
    if let Some(c) = polyfit(&pts, 4) {
        let resid: Vec<f64> = prof
            .iter()
            .map(|&(x, m, _)| {
                let t = c.iter().rev().fold(0.0, |acc, &k| acc * x + k);
                m - t
            })
            .collect();
        let noise = (prof.iter().map(|&(_, _, v)| v).sum::<f64>() / prof.len() as f64).sqrt();
        let thr = (3.0 * noise).max(f64::from(RING_AMPLITUDE) * 0.5);
        let mut state = 0i8;
        let mut crossings = 0usize;
        let (mut hi, mut lo) = (0.0f64, 0.0f64);
        for &r in &resid {
            hi = hi.max(r);
            lo = lo.min(r);
            let s = if r > thr {
                1
            } else if r < -thr {
                -1
            } else {
                0
            };
            if s != 0 {
                if state != 0 && s != state {
                    crossings += 1;
                }
                state = s;
            }
        }
        d.ring_amplitude = ((hi - lo) / 2.0) as f32;
        d.ring_crossings = crossings;
    }

    // Dust: pixels well under a wide local mean.
    let r = 12usize;
    let mut integ = vec![0.0f64; (bw + 1) * (bh + 1)];
    for y in 0..bh {
        let mut row = 0.0;
        for x in 0..bw {
            row += f64::from(p[y * bw + x]);
            integ[(y + 1) * (bw + 1) + x + 1] = integ[y * (bw + 1) + x + 1] + row;
        }
    }
    let mut low = 0usize;
    let mut total = 0usize;
    for y in r..bh - r {
        for x in r..bw - r {
            let (x0, x1, y0, y1) = (x - r, x + r + 1, y - r, y + r + 1);
            let s =
                integ[y1 * (bw + 1) + x1] - integ[y0 * (bw + 1) + x1] - integ[y1 * (bw + 1) + x0]
                    + integ[y0 * (bw + 1) + x0];
            let m = s / ((2 * r + 1) * (2 * r + 1)) as f64;
            total += 1;
            if f64::from(p[y * bw + x]) < 0.95 * m {
                low += 1;
            }
        }
    }
    if total > 0 {
        d.dust_fraction = low as f32 / total as f32;
    }

    if d.edge_to_centre < STRONG_VIGNETTING {
        d.warnings.push(format!(
            "strong vignetting: the edge of the field is {:.0}% of the centre. The flat corrects the brightness, but not what caused it. With an H-alpha etalon the same fall-off is the pass-band moving off the line (the sweet spot), which a flat cannot put back, so keep what matters near the centre",
            d.edge_to_centre * 100.0
        ));
    }
    if d.ring_amplitude >= RING_AMPLITUDE && d.ring_crossings >= RING_CROSSINGS {
        d.warnings.push(format!(
            "the flat shows rings ({:.1}% in size, {} crossings): Newton rings or etalon fringes. They move with focus, tilt and temperature, so this flat only removes them from lights taken at exactly the same focus and set-up; otherwise it can add a second set",
            d.ring_amplitude * 100.0,
            d.ring_crossings
        ));
    }
    if d.dust_fraction >= DUST_FRACTION {
        d.warnings.push(format!(
            "dust: {:.1}% of the flat is more than 5% below its surroundings. Dust on a window or filter can shift between sessions, so take a new flat after cleaning or moving anything",
            d.dust_fraction * 100.0
        ));
    }
    d
}

/// Advice that goes with a filter, whatever the numbers say.
#[must_use]
pub fn filter_advice(filter: Option<crate::colour::Preset>) -> Vec<String> {
    if filter == Some(crate::colour::Preset::Ha) {
        vec![
            "H-alpha: take the flats at the science focus and tilt, through the etalon, because its fringes and Newton rings move with focus. Check the field for the etalon's sweet spot: outside it the pass-band is off the line and no flat restores it"
                .to_owned(),
        ]
    } else {
        Vec::new()
    }
}
