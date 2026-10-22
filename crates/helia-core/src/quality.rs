// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Per-frame sharpness scoring and frame selection.
//!
//! Both metrics run on a frame lightly blurred with a 3x3 binomial kernel
//! (`[1 2 1] x [1 2 1] / 16`, clamp-to-edge). The result is divided by the
//! squared mean of the blurred region after subtracting a per-frame noise
//! floor ([`noise_floor`]), so neither a brightness change nor a sky pedestal
//! reads as a sharpness change. The GPU kernels reproduce these definitions.
//!
//! # References
//!
//! - Fried, D. L. (1978), Probability of getting a lucky short-exposure image through turbulence, J. Opt.
//!   Soc. Am. 68, 1651-1658, doi:10.1364/JOSA.68.001651 (why selecting the sharpest frames helps).
//! - Law, N. M., Mackay, C. D. and Baldwin, J. E. (2006), Lucky imaging: high angular resolution imaging in
//!   the visible from the ground, Astron. Astrophys. 446, 739-745, doi:10.1051/0004-6361:20053695.

#![allow(
    clippy::many_single_char_names,
    reason = "x, y, w, h, dx, dy and r, g, b are the natural names in image and kernel maths"
)]

use std::fmt;
use std::str::FromStr;

use rayon::prelude::*;

use crate::error::{Error, Result};
use crate::frame::{Frame, Rect};
use crate::lanes::{sum_and_square_lanes, sum_lanes};

/// Sharpness metric.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Metric {
    /// Mean squared central-difference gradient magnitude.
    #[default]
    GradientEnergy,
    /// Variance of the 4-neighbour Laplacian.
    LaplacianVariance,
    /// Helmli and Scherer's mean of the pixel-to-neighbourhood ratio (CPU only).
    HelmliScherer,
    /// Median-filter gradient similarity, as `1 - similarity` (CPU only).
    Mfgs,
    /// Share of the 8x8 DCT energy in the high coefficients (CPU only).
    DctEnergyRatio,
    /// Band-limited Fourier power less a noise floor (CPU only).
    FourierBand,
    /// Mean local RMS contrast (CPU only).
    LocalContrast,
}

impl Metric {
    /// Every metric, in the order of the bake-off tables.
    pub const ALL: [Self; 7] = [
        Self::GradientEnergy,
        Self::LaplacianVariance,
        Self::HelmliScherer,
        Self::Mfgs,
        Self::DctEnergyRatio,
        Self::FourierBand,
        Self::LocalContrast,
    ];

    /// Short name, as `--metric` spells it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::GradientEnergy => "gradient",
            Self::LaplacianVariance => "laplacian",
            Self::HelmliScherer => "helmli",
            Self::Mfgs => "mfgs",
            Self::DctEnergyRatio => "dct",
            Self::FourierBand => "fourier",
            Self::LocalContrast => "contrast",
        }
    }

    /// Whether the GPU kernels implement this metric.
    #[must_use]
    pub fn on_gpu(self) -> bool {
        matches!(self, Self::GradientEnergy | Self::LaplacianVariance)
    }
}

/// 3x3 binomial blur of `data` over `r` (which must lie inside the frame).
/// Neighbours outside the frame are clamped to the edge.
fn blurred_region(data: &[f32], w: usize, h: usize, r: Rect) -> Vec<f32> {
    let rw = r.width();
    let y_lo = r.y0.saturating_sub(1);
    let y_hi = (r.y1 + 1).min(h);
    let mut hp = vec![0.0f32; (y_hi - y_lo) * rw];
    for yy in y_lo..y_hi {
        let row = &data[yy * w..(yy + 1) * w];
        let out = &mut hp[(yy - y_lo) * rw..(yy - y_lo + 1) * rw];
        // Columns whose left and right neighbours both lie inside the frame
        // and so need no clamping.
        let lo = r.x0.max(1).min(r.x1);
        let hi = r.x1.min(w - 1).max(lo);
        let clamped =
            |x: usize| 0.25 * (row[x.saturating_sub(1)] + 2.0 * row[x] + row[(x + 1).min(w - 1)]);
        for x in r.x0..lo {
            out[x - r.x0] = clamped(x);
        }
        if hi > lo {
            let o = &mut out[lo - r.x0..hi - r.x0];
            for (((o, &l), &c), &rr) in o
                .iter_mut()
                .zip(&row[lo - 1..hi - 1])
                .zip(&row[lo..hi])
                .zip(&row[lo + 1..=hi])
            {
                *o = 0.25 * (l + 2.0 * c + rr);
            }
        }
        for x in hi..r.x1 {
            out[x - r.x0] = clamped(x);
        }
    }
    let mut b = vec![0.0f32; r.area()];
    for y in r.y0..r.y1 {
        let up = &hp[(y.saturating_sub(1) - y_lo) * rw..][..rw];
        let mid = &hp[(y - y_lo) * rw..][..rw];
        let dn = &hp[((y + 1).min(h - 1) - y_lo) * rw..][..rw];
        let out = &mut b[(y - r.y0) * rw..(y - r.y0 + 1) * rw];
        for (((o, &u), &m), &d) in out.iter_mut().zip(up).zip(mid).zip(dn) {
            *o = 0.25 * (u + 2.0 * m + d);
        }
    }
    b
}

/// Per-pixel metric terms for columns `xs` of one blurred row.
///
/// `rows` are the blurred rows `y`, `y - 1` and `y + 1` (clamped at the frame
/// edge) for frame columns starting at `base`; they must cover every neighbour
/// column the output reads. `out[i]` receives, for column `xs.start + i`, the
/// squared central-difference gradient magnitude (gradient energy) or the
/// 4-neighbour Laplacian (Laplacian variance).
fn metric_row(
    metric: Metric,
    rows: [&[f32]; 3],
    base: usize,
    w: usize,
    xs: std::ops::Range<usize>,
    out: &mut [f32],
) {
    let [cur, up, dn] = rows;
    let at = |row: &[f32], x: usize| row[x - base];
    let edge = |x: usize| -> f32 {
        let (xl, xr) = (x.saturating_sub(1), (x + 1).min(w - 1));
        match metric {
            Metric::GradientEnergy => {
                let gx = 0.5 * (at(cur, xr) - at(cur, xl));
                let gy = 0.5 * (at(dn, x) - at(up, x));
                gx * gx + gy * gy
            }
            _ => at(cur, xl) + at(cur, xr) + at(up, x) + at(dn, x) - 4.0 * at(cur, x),
        }
    };
    // Interior columns read x - 1 and x + 1 straight from the row.
    let lo = xs.start.max(1).min(xs.end);
    let hi = xs.end.min(w - 1).max(lo);
    for x in xs.start..lo {
        out[x - xs.start] = edge(x);
    }
    if hi > lo {
        let n = hi - lo;
        let l = &cur[lo - 1 - base..][..n];
        let c = &cur[lo - base..][..n];
        let r = &cur[lo + 1 - base..][..n];
        let u = &up[lo - base..][..n];
        let d = &dn[lo - base..][..n];
        let o = &mut out[lo - xs.start..hi - xs.start];
        match metric {
            Metric::GradientEnergy => {
                for (((o, (&l, &r)), &u), &d) in o.iter_mut().zip(l.iter().zip(r)).zip(u).zip(d) {
                    let (gx, gy) = (0.5 * (r - l), 0.5 * (d - u));
                    *o = gx * gx + gy * gy;
                }
            }
            _ => {
                for ((((o, &l), (&c, &r)), &u), &d) in
                    o.iter_mut().zip(l).zip(c.iter().zip(r)).zip(u).zip(d)
                {
                    *o = l + r + u + d - 4.0 * c;
                }
            }
        }
    }
    for x in hi..xs.end {
        out[x - xs.start] = edge(x);
    }
}

/// Noise floor of a frame: its 1st percentile, estimated from every third
/// pixel in both directions. Cheap enough to compute per frame.
#[must_use]
pub fn noise_floor(frame: &Frame) -> f32 {
    let mut v: Vec<f32> = frame
        .data
        .chunks(frame.width)
        .step_by(3)
        .flat_map(|row| row.iter().step_by(3).copied())
        .filter(|x| x.is_finite())
        .collect();
    if v.is_empty() {
        return 0.0;
    }
    let k = v.len() / 100;
    *v.select_nth_unstable_by(k, f32::total_cmp).1
}

/// Soft frame weights in `[0.2, 1]` from quality scores (any order). The
/// worst score in the slice gets 0.2, the best 1, with a smoothstep between,
/// so marginal frames still contribute but sharp ones dominate. Equal or
/// non-finite scores get 1 and 0.2 respectively.
#[must_use]
pub fn soft_weights(scores: &[f32]) -> Vec<f32> {
    let finite = scores.iter().copied().filter(|v| v.is_finite());
    let lo = finite.clone().fold(f32::INFINITY, f32::min);
    let hi = finite.fold(f32::NEG_INFINITY, f32::max);
    scores
        .iter()
        .map(|&q| {
            if !q.is_finite() {
                0.2
            } else if hi - lo <= f32::EPSILON * hi.abs() {
                1.0
            } else {
                let t = ((q - lo) / (hi - lo)).clamp(0.0, 1.0);
                0.2 + 0.8 * t * t * (3.0 - 2.0 * t)
            }
        })
        .collect()
}

/// Sharpness of `frame` (or of `roi` within it). Higher is sharper.
/// Returns 0 for an empty region or one that is no brighter than the floor.
#[must_use]
pub fn score(frame: &Frame, metric: Metric, roi: Option<Rect>) -> f32 {
    score_with_floor(frame, metric, roi, noise_floor(frame))
}

/// [`score`] with a precomputed [`noise_floor`], for scoring many windows of one frame.
#[must_use]
pub fn score_with_floor(frame: &Frame, metric: Metric, roi: Option<Rect>, floor: f32) -> f32 {
    // The metrics beyond gradient and Laplacian carry their own noise handling.
    if let Some(s) = crate::quality_extra::score(frame, metric, roi) {
        return s;
    }
    let (w, h) = (frame.width, frame.height);
    let r = roi.unwrap_or_else(|| frame.rect()).clamp(w, h);
    if r.is_empty() {
        return 0.0;
    }
    // Gradient and Laplacian read one blurred pixel beyond the region.
    let e = Rect {
        x0: r.x0.saturating_sub(1),
        y0: r.y0.saturating_sub(1),
        x1: (r.x1 + 1).min(w),
        y1: (r.y1 + 1).min(h),
    };
    let b = blurred_region(&frame.data, w, h, e);
    let ew = e.width();
    let row = |y: usize| &b[(y - e.y0) * ew..(y - e.y0 + 1) * ew];
    let n = r.area() as f64;
    let (mut s_mean, mut s_a, mut s_b) = (0.0f64, 0.0f64, 0.0f64);
    let mut terms = vec![0.0f32; r.width()];
    for y in r.y0..r.y1 {
        let rows = [row(y), row(y.saturating_sub(1)), row((y + 1).min(h - 1))];
        s_mean += sum_lanes(&rows[0][r.x0 - e.x0..r.x1 - e.x0]);
        metric_row(metric, rows, e.x0, w, r.x0..r.x1, &mut terms);
        match metric {
            Metric::GradientEnergy => s_a += sum_lanes(&terms),
            Metric::LaplacianVariance => {
                let (a, sq) = sum_and_square_lanes(&terms);
                s_a += a;
                s_b += sq;
            }
            _ => {}
        }
    }
    combine_sums(metric, n, s_mean, s_a, s_b, floor)
}

/// Gradient-energy scores of many windows of one frame, each in time
/// proportional to its rows rather than its pixels.
///
/// Blurs the frame and tabulates the squared gradient of the blur once;
/// [`Self::score`] then equals
/// `score_with_floor(frame, Metric::GradientEnergy, Some(window), floor)` up to
/// summation order, without re-blurring the window and its border for every
/// call. Use it when a frame is scored through hundreds of windows (alignment
/// points). (Summed-area tables make a window O(1) but cost more to build than
/// a frame's worth of windows saves.)
pub struct WindowScorer {
    w: usize,
    h: usize,
    /// The 3x3-blurred frame.
    blurred: Vec<f32>,
    /// Squared gradient magnitude of `blurred`, edge-clamped as in [`score`].
    energy: Vec<f32>,
}

impl WindowScorer {
    /// Tabulate `frame`.
    #[must_use]
    pub fn new(frame: &Frame) -> Self {
        let (w, h) = (frame.width, frame.height);
        let blurred = blurred_region(&frame.data, w, h, frame.rect());
        let mut energy = vec![0.0f32; w * h];
        for (y, out) in energy.chunks_exact_mut(w).enumerate() {
            let rows = [
                &blurred[y * w..(y + 1) * w],
                &blurred[y.saturating_sub(1) * w..][..w],
                &blurred[(y + 1).min(h - 1) * w..][..w],
            ];
            metric_row(Metric::GradientEnergy, rows, 0, w, 0..w, out);
        }
        Self {
            w,
            h,
            blurred,
            energy,
        }
    }

    /// Gradient-energy sharpness of `window` (clipped to the frame).
    #[must_use]
    pub fn score(&self, window: Rect, floor: f32) -> f32 {
        let r = window.clamp(self.w, self.h);
        if r.is_empty() {
            return 0.0;
        }
        let (mut mean, mut energy) = (0.0f64, 0.0f64);
        for y in r.y0..r.y1 {
            let span = y * self.w + r.x0..y * self.w + r.x1;
            mean += sum_lanes(&self.blurred[span.clone()]);
            energy += sum_lanes(&self.energy[span]);
        }
        combine_sums(
            Metric::GradientEnergy,
            r.area() as f64,
            mean,
            energy,
            0.0,
            floor,
        )
    }
}

/// Turn region sums into a score. `s_mean` is the sum of blurred values,
/// `s_a` the sum of squared gradients (gradient energy) or of Laplacian values
/// (Laplacian variance), and `s_b` the sum of squared Laplacian values.
/// Shared with GPU backends so both sides finish identically.
#[must_use]
pub fn combine_sums(metric: Metric, n: f64, s_mean: f64, s_a: f64, s_b: f64, floor: f32) -> f32 {
    let mean = s_mean / n - f64::from(floor);
    if mean < 1e-6 {
        return 0.0;
    }
    let raw = match metric {
        Metric::GradientEnergy => s_a / n,
        Metric::LaplacianVariance => s_b / n - (s_a / n).powi(2),
        _ => return 0.0,
    };
    (raw / (mean * mean)) as f32
}

/// Score every frame of a slice in parallel.
#[must_use]
pub fn score_all(frames: &[Frame], metric: Metric, roi: Option<Rect>) -> Vec<f32> {
    frames.par_iter().map(|f| score(f, metric, roi)).collect()
}

/// Frames either side that a frame is compared with, in [`robust_scores`].
const NEIGHBOURS: usize = 5;
/// Spread multiples above the neighbours' level that make a score an outlier.
const OUTLIER_SIGMAS: f32 = 4.0;
/// Smallest rise over the neighbours, as a ratio, that is ever an outlier, so a
/// capture with very steady seeing does not flag its own noise.
const MIN_OUTLIER_RATIO: f32 = 1.08;

/// Scores with the ones an artefact has raised put back to what their
/// neighbours in time say, and the frames that were put back.
///
/// A satellite trail, a dark line or a burst of hot pixels adds sharp edges, so
/// the damaged frame scores *higher* and would be ranked first, then used to
/// build the reference (issue #172). Seeing changes slowly from frame
/// to frame compared with a trail, so each score is compared with the median
/// of the five frames either side of it (itself left out). The spread
/// of the log ratios over the whole capture (median absolute deviation, so the
/// damaged frames do not widen it) sets how far above the neighbours is still
/// seeing: a frame more than four spreads and 8% above gets its
/// neighbours' median instead of its own score. Only rises are treated;
/// a low score is a poor frame, which ranking already handles. Needs at least
/// five frames and positive finite scores; otherwise returns the input.
#[must_use]
pub fn robust_scores(scores: &[f32]) -> (Vec<f32>, Vec<usize>) {
    let n = scores.len();
    if n < 5 || scores.iter().any(|s| !s.is_finite() || *s <= 0.0) {
        return (scores.to_vec(), Vec::new());
    }
    let logs: Vec<f32> = scores.iter().map(|s| s.ln()).collect();
    let median = |v: &mut Vec<f32>| -> f32 {
        v.sort_by(f32::total_cmp);
        let m = v.len() / 2;
        if v.len() % 2 == 1 {
            v[m]
        } else {
            f32::midpoint(v[m - 1], v[m])
        }
    };
    let level: Vec<f32> = (0..n)
        .map(|i| {
            let mut near: Vec<f32> = (i.saturating_sub(NEIGHBOURS)..(i + NEIGHBOURS + 1).min(n))
                .filter(|&j| j != i)
                .map(|j| logs[j])
                .collect();
            median(&mut near)
        })
        .collect();
    let resid: Vec<f32> = logs.iter().zip(&level).map(|(l, m)| l - m).collect();
    let centre = median(&mut resid.clone());
    let mad = median(&mut resid.iter().map(|r| (r - centre).abs()).collect());
    let limit = (OUTLIER_SIGMAS * 1.4826 * mad).max(MIN_OUTLIER_RATIO.ln());
    let mut out = scores.to_vec();
    let mut flagged = Vec::new();
    for i in 0..n {
        if resid[i] - centre > limit {
            out[i] = level[i].exp();
            flagged.push(i);
        }
    }
    (out, flagged)
}

/// Indices ordered best first. NaN scores sort last; ties keep index order.
#[must_use]
pub fn rank(scores: &[f32]) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..scores.len()).collect();
    idx.sort_by(|&a, &b| {
        let (sa, sb) = (scores[a], scores[b]);
        match (sa.is_nan(), sb.is_nan()) {
            (true, true) => a.cmp(&b),
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            (false, false) => sb.total_cmp(&sa).then(a.cmp(&b)),
        }
    });
    idx
}

/// How many of the best frames to keep.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Keep {
    /// A fraction in `(0, 1]` of the frames.
    Fraction(f32),
    /// An absolute number of frames.
    Count(usize),
}

impl Default for Keep {
    fn default() -> Self {
        Self::Fraction(0.25)
    }
}

impl Keep {
    /// Frames kept out of `total`; at least one when `total > 0`.
    #[must_use]
    pub fn count(self, total: usize) -> usize {
        if total == 0 {
            return 0;
        }
        let n = match self {
            Self::Fraction(f) => (f64::from(f.clamp(0.0, 1.0)) * total as f64).round() as usize,
            Self::Count(c) => c,
        };
        n.clamp(1, total)
    }
}

impl FromStr for Keep {
    type Err = Error;

    /// `"25%"` is a fraction, a bare integer is a count.
    fn from_str(s: &str) -> Result<Self> {
        let s = s.trim();
        if let Some(p) = s.strip_suffix('%') {
            let v: f32 = p
                .trim()
                .parse()
                .map_err(|_| Error::Invalid(format!("bad percentage {s:?}")))?;
            if !(v > 0.0 && v <= 100.0) {
                return Err(Error::Invalid(format!("percentage {v} outside (0, 100]")));
            }
            Ok(Self::Fraction(v / 100.0))
        } else {
            let c: usize = s
                .parse()
                .map_err(|_| Error::Invalid(format!("bad frame count {s:?}")))?;
            if c == 0 {
                return Err(Error::Invalid("frame count must be at least 1".into()));
            }
            Ok(Self::Count(c))
        }
    }
}

impl fmt::Display for Keep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fraction(v) => write!(f, "{}%", v * 100.0),
            Self::Count(c) => write!(f, "{c} frames"),
        }
    }
}

/// Indices of the best frames, best first.
#[must_use]
pub fn select_best(scores: &[f32], keep: Keep) -> Vec<usize> {
    let mut r = rank(scores);
    r.truncate(keep.count(scores.len()));
    r
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_wrap,
    clippy::many_single_char_names,
    reason = "test oracle indexes with signed offsets"
)]
mod tests {
    use super::*;
    use crate::align::Shift;
    use crate::filter::gaussian_blur;
    use crate::testutil::{SyntheticConfig, SyntheticSun};

    /// The original per-pixel formulation, kept as the oracle for the row-based code.
    fn oracle_score(frame: &Frame, metric: Metric, roi: Option<Rect>, floor: f32) -> f32 {
        let (w, h) = (frame.width, frame.height);
        let r = roi.unwrap_or_else(|| frame.rect()).clamp(w, h);
        if r.is_empty() {
            return 0.0;
        }
        let blur = |x: usize, y: usize| -> f32 {
            let px = |xx: i64, yy: i64| {
                frame.data[(yy.clamp(0, h as i64 - 1) as usize) * w
                    + (xx.clamp(0, w as i64 - 1) as usize)]
            };
            let (x, y) = (x as i64, y as i64);
            let hp = |yy: i64| 0.25 * (px(x - 1, yy) + 2.0 * px(x, yy) + px(x + 1, yy));
            0.25 * (hp(y - 1) + 2.0 * hp(y) + hp(y + 1))
        };
        let n = r.area() as f64;
        let (mut s_mean, mut s_a, mut s_b) = (0.0f64, 0.0f64, 0.0f64);
        for y in r.y0..r.y1 {
            let (yu, yd) = (y.saturating_sub(1), (y + 1).min(h - 1));
            for x in r.x0..r.x1 {
                let (xl, xr) = (x.saturating_sub(1), (x + 1).min(w - 1));
                let c = blur(x, y);
                s_mean += f64::from(c);
                if metric == Metric::GradientEnergy {
                    let gx = 0.5 * (blur(xr, y) - blur(xl, y));
                    let gy = 0.5 * (blur(x, yd) - blur(x, yu));
                    s_a += f64::from(gx * gx + gy * gy);
                } else {
                    let l = blur(xl, y) + blur(xr, y) + blur(x, yu) + blur(x, yd) - 4.0 * c;
                    s_a += f64::from(l);
                    s_b += f64::from(l * l);
                }
            }
        }
        combine_sums(metric, n, s_mean, s_a, s_b, floor)
    }

    fn textured(w: usize, h: usize) -> Frame {
        let data = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f32, (i / w) as f32);
                0.3 + 0.2 * (x * 0.37).sin() * (y * 0.21).cos() + 0.001 * ((i * 7919) % 97) as f32
            })
            .collect();
        Frame {
            width: w,
            height: h,
            data,
            bayer: None,
        }
    }

    #[test]
    fn row_based_scores_match_the_per_pixel_oracle() {
        for (w, h) in [(1, 1), (2, 3), (9, 7), (40, 33), (67, 19)] {
            let f = textured(w, h);
            let floor = noise_floor(&f);
            let rois = [
                None,
                Some(Rect::new(0, 0, w.min(5), h.min(4))),
                Some(Rect::new(w / 2, h / 3, w, h)),
                Some(Rect::new(w.saturating_sub(3), 0, 3, h)),
            ];
            for metric in [Metric::GradientEnergy, Metric::LaplacianVariance] {
                for roi in rois {
                    let got = score_with_floor(&f, metric, roi, floor);
                    let want = oracle_score(&f, metric, roi, floor);
                    assert!(
                        (got - want).abs() <= 1e-4 * want.abs().max(1e-6),
                        "{w}x{h} {metric:?} {roi:?}: {got} vs {want}"
                    );
                }
            }
        }
    }

    #[test]
    fn window_scorer_equals_scoring_each_window() {
        let f = textured(83, 61);
        let floor = noise_floor(&f);
        let scorer = WindowScorer::new(&f);
        for win in [
            Rect::new(0, 0, 83, 61),
            Rect::new(10, 7, 32, 32),
            Rect::new(0, 0, 16, 16),
            Rect::new(70, 50, 13, 11),
            Rect::new(1, 1, 1, 1),
            Rect::new(60, 40, 64, 64),
        ] {
            let want = score_with_floor(&f, Metric::GradientEnergy, Some(win), floor);
            let got = scorer.score(win, floor);
            assert!(
                (got - want).abs() <= 1e-4 * want.abs().max(1e-6),
                "{win:?}: {got} vs {want}"
            );
        }
        assert_eq!(scorer.score(Rect::new(200, 200, 4, 4), floor), 0.0);
    }

    #[test]
    fn keep_parsing_and_counts() {
        assert_eq!("25%".parse::<Keep>().ok(), Some(Keep::Fraction(0.25)));
        assert_eq!("500".parse::<Keep>().ok(), Some(Keep::Count(500)));
        assert!("0%".parse::<Keep>().is_err());
        assert!("abc".parse::<Keep>().is_err());
        assert!("0".parse::<Keep>().is_err());
        assert_eq!(Keep::Fraction(0.25).count(100), 25);
        assert_eq!(Keep::Fraction(0.001).count(100), 1);
        assert_eq!(Keep::Count(500).count(100), 100);
        assert_eq!(Keep::Count(5).count(0), 0);
    }

    #[test]
    fn rank_is_descending_and_nan_last() {
        assert_eq!(rank(&[1.0, f32::NAN, 3.0, 2.0]), vec![2, 3, 0, 1]);
        assert_eq!(select_best(&[1.0, 5.0, 3.0], Keep::Count(2)), vec![1, 2]);
    }

    #[test]
    fn blurrier_scores_lower_for_both_metrics() {
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let clean = sun.render_clean(Shift::default(), None);
        let sigmas = [0.0, 0.8, 1.6, 2.4, 3.2];
        for metric in [Metric::GradientEnergy, Metric::LaplacianVariance] {
            let s: Vec<f32> = sigmas
                .iter()
                .map(|&sg| score(&gaussian_blur(&clean, sg), metric, None))
                .collect();
            assert!(
                s.windows(2).all(|p| p[0] > p[1]),
                "{metric:?} not monotone: {s:?}"
            );
        }
    }

    #[test]
    fn roi_scores_only_its_region() {
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let clean = sun.render_clean(Shift::default(), None);
        let blank = Rect::new(0, 0, 20, 20);
        assert!(score(&clean, Metric::GradientEnergy, Some(blank)) < 1e-6);
        assert!(score(&clean, Metric::GradientEnergy, None) > 1e-4);
    }

    #[test]
    fn soft_weights_span_range() {
        let w = soft_weights(&[1.0, 2.0, 3.0, f32::NAN]);
        assert!((w[0] - 0.2).abs() < 1e-6 && (w[2] - 1.0).abs() < 1e-6);
        assert!(w[1] > 0.2 && w[1] < 1.0 && (w[3] - 0.2).abs() < 1e-6);
        assert_eq!(soft_weights(&[2.0, 2.0]), vec![1.0, 1.0]);
    }

    #[test]
    fn floor_ignores_a_sky_pedestal() {
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let a = sun.render_clean(Shift::default(), None);
        let mut b = a.clone();
        b.data.iter_mut().for_each(|v| *v += 0.3);
        let (sa, sb) = (
            score(&a, Metric::GradientEnergy, None),
            score(&b, Metric::GradientEnergy, None),
        );
        assert!((sa - sb).abs() / sa < 0.02, "{sa} vs {sb}");
    }

    #[test]
    fn brightness_scale_invariant() {
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let a = sun.render_clean(Shift::default(), None);
        let mut b = a.clone();
        b.data.iter_mut().for_each(|v| *v *= 0.5);
        let (sa, sb) = (
            score(&a, Metric::GradientEnergy, None),
            score(&b, Metric::GradientEnergy, None),
        );
        assert!((sa - sb).abs() / sa < 1e-4);
    }

    /// Steady seeing: scores within a few percent of one level.
    fn steady(n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| 0.0017 * (1.0 + 0.02 * ((i * 7) % 5) as f32 / 4.0))
            .collect()
    }

    #[test]
    fn robust_scores_put_back_a_spike_and_name_it() {
        let mut s = steady(20);
        s[3] *= 1.25;
        s[12] *= 1.4;
        let (out, flagged) = robust_scores(&s);
        assert_eq!(flagged, vec![3, 12]);
        for i in [3, 12] {
            assert!((out[i] / 0.0017 - 1.0).abs() < 0.03, "{i}: {}", out[i]);
        }
        // Everything else is untouched.
        for i in (0..20).filter(|i| ![3, 12].contains(i)) {
            assert_eq!(out[i], s[i]);
        }
        assert_eq!(rank(&out).iter().filter(|&&i| i == 3).count(), 1);
    }

    #[test]
    fn robust_scores_leave_steady_noise_and_slow_drift_alone() {
        let (_, flagged) = robust_scores(&steady(40));
        assert!(flagged.is_empty(), "{flagged:?}");
        // The seeing improves by a factor of two over the capture.
        let drift: Vec<f32> = (0..60).map(|i| 0.001 * (1.0 + i as f32 / 60.0)).collect();
        let (out, flagged) = robust_scores(&drift);
        assert!(flagged.is_empty(), "{flagged:?}");
        assert_eq!(out, drift);
    }

    #[test]
    fn robust_scores_keep_a_run_of_damaged_frames_from_hiding_each_other() {
        let mut s = steady(30);
        for v in &mut s[10..13] {
            *v *= 1.6;
        }
        let (_, flagged) = robust_scores(&s);
        assert_eq!(flagged, vec![10, 11, 12]);
    }

    #[test]
    fn robust_scores_do_nothing_for_short_or_unusable_input() {
        let mut s = steady(4);
        s[1] *= 3.0;
        assert_eq!(robust_scores(&s), (s.clone(), Vec::new()));
        let mut s = steady(10);
        s[2] = 0.0;
        s[5] *= 3.0;
        assert_eq!(robust_scores(&s).1, Vec::<usize>::new());
        s[2] = f32::NAN;
        assert_eq!(robust_scores(&s).1, Vec::<usize>::new());
    }
}
