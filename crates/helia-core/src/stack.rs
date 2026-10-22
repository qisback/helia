// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Combining registered samples.
//!
//! Every deposit of a frame onto the output goes through one of two paths,
//! both taking `(frame, shift, weight)`:
//!
//! * [`TileStack`] for a rectangle of the output, fed by
//!   [`crate::resample::resample_region`];
//! * the [`crate::backend::Accumulator`] splat interface, created from a
//!   [`Deposit`] so that a drizzle implementation can later be selected without
//!   touching the callers.
//!
//! The default combination is a weighted mean (weights come from
//! [`crate::quality::soft_weights`]). The robust combinations are optional:
//! they need the per-pixel mean and spread before they can act, so they run in
//! two passes over the frames. [`TileStack::add`] collects statistics,
//! [`TileStack::begin_clip`] (or [`TileStack::begin_robust`]) fixes the rule,
//! and [`TileStack::add_clipped`] judges each sample against the centre and
//! limit `k * sigma` of the others at its pixel ([`robust_reference`]) and sums
//! the second pass under one [`RobustKind`]: a hard clip drops samples beyond
//! the limit, a Huber step down-weights them in proportion to the limit over
//! their distance, a winsorised mean clamps them to the limit. Memory stays
//! O(pixels) whatever the frame count. Sums are kept relative to a centre image
//! (the reference) so single precision stays accurate.
//!
//! # References
//!
//! - Huber, P. J. (1964), Robust estimation of a location parameter, Ann. Math. Statist. 35, 73-101,
//!   doi:10.1214/aoms/1177703732; Huber, P. J. and Ronchetti, E. M. (2009), Robust Statistics, 2nd ed.,
//!   Wiley, doi:10.1002/9780470434697 (the Huber step and winsorising).
//! - Fruchter, A. S. and Hook, R. N. (2002), Drizzle, Publ. Astron. Soc. Pac. 114, 144-152,
//!   doi:10.1086/338393 (the drizzle deposit, when it is implemented).

use crate::error::{Error, Result};
use crate::frame::Rect;
use crate::resample::Kernel;

/// How samples are combined.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StackMode {
    /// Weighted arithmetic mean (the default).
    Mean,
    /// Weighted mean after rejecting samples further than `kappa` sigma from
    /// the mean of the other samples (unweighted statistics).
    SigmaClip {
        /// Rejection threshold in standard deviations.
        kappa: f32,
    },
    /// One Huber M-estimator step: a sample further than `k` sigma from the
    /// mean of the other samples is down-weighted by `k * sigma / distance`.
    Huber {
        /// Threshold in standard deviations.
        k: f32,
    },
    /// Weighted mean after clamping samples to within `k` sigma of the mean of
    /// the other samples.
    Winsor {
        /// Threshold in standard deviations.
        k: f32,
    },
}

/// What the second streaming pass does with a sample beyond the limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RobustKind {
    /// Discard it (sigma clipping).
    Sigma,
    /// Down-weight it (Huber).
    Huber,
    /// Clamp it to the limit (winsorised mean).
    Winsor,
}

impl RobustKind {
    /// Threshold used when none is given, in standard deviations.
    #[must_use]
    pub fn default_k(self) -> f32 {
        match self {
            Self::Sigma => 2.5,
            Self::Huber => 1.345,
            Self::Winsor => 2.0,
        }
    }

    /// The pixel rule for a sample `d` when the first-pass mean is `mean` and
    /// the limit is `lim`: the factor its weight is multiplied by, and the
    /// value that is summed. This is the CPU specification the GPU kernels
    /// follow.
    #[inline]
    #[must_use]
    pub fn apply(self, d: f32, mean: f32, lim: f32) -> (f32, f32) {
        let r = d - mean;
        match self {
            Self::Sigma => (if r.abs() <= lim { 1.0 } else { 0.0 }, d),
            Self::Huber => {
                let a = r.abs();
                (if a <= lim { 1.0 } else { lim / a }, d)
            }
            Self::Winsor => (1.0, d.clamp(mean - lim, mean + lim)),
        }
    }
}

impl StackMode {
    /// The second pass this mode needs, as `(kind, k)`; `None` for the mean.
    #[must_use]
    pub fn second_pass(self) -> Option<(RobustKind, f32)> {
        match self {
            Self::Mean => None,
            Self::SigmaClip { kappa } => Some((RobustKind::Sigma, kappa)),
            Self::Huber { k } => Some((RobustKind::Huber, k)),
            Self::Winsor { k } => Some((RobustKind::Winsor, k)),
        }
    }

    /// Mode for `kind` at threshold `k`.
    #[must_use]
    pub fn from_kind(kind: RobustKind, k: f32) -> Self {
        match kind {
            RobustKind::Sigma => Self::SigmaClip { kappa: k },
            RobustKind::Huber => Self::Huber { k },
            RobustKind::Winsor => Self::Winsor { k },
        }
    }
}

/// What the robust modes do and do not remove, for the tools to print.
pub const ROBUST_SCOPE_NOTE: &str = "robust stacking removes what differs between frames (a satellite, a cosmic ray, a hot pixel the alignment moves); a defect fixed on the sensor sits on the same pixel in every frame and is not an outlier, use --dark or --bad-pixels for it";

/// Fewest samples at a pixel for which the spread is estimated without the
/// sample being judged (leave-one-out); below this the plain spread is used.
pub const LOO_MIN: f32 = 6.0;

/// Centre and limit `k * sigma` that judge one sample `d` (relative to the
/// reference image), from the first-pass sums of the other samples at the
/// pixel: `sum` and `sumsq` of the relative values, `count` samples, `d`
/// included. Infinite limit where a spread is not trustworthy (fewer than
/// three samples).
///
/// The statistics are deliberately unweighted: the frame weights say how much a
/// sample counts in the result, not how much it is believed. With weights, a
/// few frames with a high quality score pull the mean towards themselves and
/// widen the spread, so an artefact in them is never far enough away (issue
/// #150). The sample itself is left out (leave-one-out) from [`LOO_MIN`]
/// samples up, so a spike cannot inflate the spread it is measured against.
#[inline]
#[must_use]
pub fn robust_reference(k: f32, sum: f32, sumsq: f32, count: f32, d: f32) -> (f32, f32) {
    if count < 3.0 {
        return (if count > 0.0 { sum / count } else { 0.0 }, f32::INFINITY);
    }
    if count < LOO_MIN {
        let m = sum / count;
        return (m, k * (sumsq / count - m * m).max(0.0).sqrt());
    }
    let n1 = count - 1.0;
    let (s1, q1) = (sum - d, sumsq - d * d);
    let m = s1 / n1;
    (m, k * ((q1 - s1 * m) / (n1 - 1.0)).max(0.0).sqrt())
}

/// In-memory reference for one pixel: the same estimate the streaming passes
/// produce, computed from all the samples at once in double precision. The
/// centre and spread ignore the weights (see [`robust_reference`]); the weights
/// only decide how much each surviving sample counts. `NaN` samples are
/// skipped; `NaN` is returned when none remain.
#[must_use]
pub fn robust_location(samples: &[f32], weights: &[f32], kind: RobustKind, k: f32) -> f32 {
    let kept: Vec<(f64, f64)> = samples
        .iter()
        .zip(weights)
        .filter(|(v, _)| !v.is_nan())
        .map(|(&v, &w)| (f64::from(v), f64::from(w)))
        .collect();
    let total: f64 = kept.iter().map(|&(_, w)| w).sum();
    if kept.is_empty() || total <= 0.0 {
        return f32::NAN;
    }
    let n = kept.len() as f64;
    let (sum, sumsq) = kept
        .iter()
        .fold((0.0, 0.0), |(s, q), &(v, _)| (s + v, q + v * v));
    let (mut num, mut den) = (0.0f64, 0.0f64);
    for &(value, weight) in &kept {
        let (mean, limit) = if n < 3.0 {
            (sum / n, f64::INFINITY)
        } else if n < f64::from(LOO_MIN) {
            let m = sum / n;
            (m, f64::from(k) * (sumsq / n - m * m).max(0.0).sqrt())
        } else {
            let (s1, q1) = (sum - value, sumsq - value * value);
            let m = s1 / (n - 1.0);
            (
                m,
                f64::from(k) * ((q1 - s1 * m) / (n - 2.0)).max(0.0).sqrt(),
            )
        };
        let dist = (value - mean).abs();
        let (factor, summed) = match kind {
            RobustKind::Sigma => (if dist <= limit { 1.0 } else { 0.0 }, value),
            RobustKind::Huber => (if dist <= limit { 1.0 } else { limit / dist }, value),
            RobustKind::Winsor => (1.0, value.clamp(mean - limit, mean + limit)),
        };
        num += weight * factor * summed;
        den += weight * factor;
    }
    if den > 0.0 {
        (num / den) as f32
    } else {
        (kept.iter().map(|&(v, w)| v * w).sum::<f64>() / total) as f32
    }
}

/// How a registered frame is deposited onto the output grid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Deposit {
    /// Interpolate the frame onto the output grid (1:1 scale).
    Resample(Kernel),
    /// Drizzle: shrink input pixels by `pixfrac` and splat them onto a grid
    /// `scale` times finer. Not implemented yet.
    Drizzle {
        /// Output pixels per input pixel (1.5 or 2).
        scale: f32,
        /// Input pixel shrink factor in `(0, 1]`.
        pixfrac: f32,
    },
}

impl Deposit {
    /// Check the parameters and that this crate can perform the deposit.
    ///
    /// # Errors
    /// [`Error::Invalid`] for out-of-range drizzle parameters, otherwise
    /// [`Error::Unsupported`] for any drizzle deposit.
    pub fn validate(&self) -> Result<()> {
        match *self {
            Self::Resample(_) => Ok(()),
            Self::Drizzle { scale, pixfrac } => {
                if !((scale - 1.5).abs() < 1e-6 || (scale - 2.0).abs() < 1e-6) {
                    return Err(Error::Invalid(format!(
                        "drizzle scale {scale} must be 1.5 or 2"
                    )));
                }
                if !(pixfrac > 0.0 && pixfrac <= 1.0) {
                    return Err(Error::Invalid(format!(
                        "drizzle pixfrac {pixfrac} must be in (0, 1]"
                    )));
                }
                Err(Error::Unsupported(format!(
                    "{scale}x drizzle is not implemented yet"
                )))
            }
        }
    }
}

/// Accumulator for one rectangle of the output.
pub struct TileStack {
    rect: Rect,
    centre: Vec<f32>,
    /// Weighted sum of the relative samples (the result when nothing is clipped).
    sum: Vec<f32>,
    /// Unweighted sum and sum of squares, for the rejection statistics.
    usum: Vec<f32>,
    sumsq: Vec<f32>,
    wt: Vec<f32>,
    count: Vec<f32>,
    clip: Option<ClipState>,
}

struct ClipState {
    kind: RobustKind,
    k: f32,
    sum: Vec<f32>,
    wt: Vec<f32>,
}

impl TileStack {
    /// Start an accumulator. `centre` is the reference over `rect`.
    ///
    /// # Panics
    /// If `centre.len() != rect.area()` (caller bug).
    #[must_use]
    pub fn new(rect: Rect, centre: Vec<f32>, track_variance: bool) -> Self {
        assert_eq!(centre.len(), rect.area());
        let a = rect.area();
        Self {
            rect,
            centre,
            sum: vec![0.0; a],
            usum: if track_variance {
                vec![0.0; a]
            } else {
                Vec::new()
            },
            sumsq: if track_variance {
                vec![0.0; a]
            } else {
                Vec::new()
            },
            wt: vec![0.0; a],
            count: vec![0.0; a],
            clip: None,
        }
    }

    /// Rectangle covered.
    #[must_use]
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Add one frame's samples with `weight` (`NaN` samples are skipped).
    pub fn add(&mut self, samples: &[f32], weight: f32) {
        let track = !self.sumsq.is_empty();
        for (i, &v) in samples.iter().enumerate() {
            if v.is_nan() {
                continue;
            }
            let d = v - self.centre[i];
            self.sum[i] += weight * d;
            self.wt[i] += weight;
            self.count[i] += 1.0;
            if track {
                self.usum[i] += d;
                self.sumsq[i] += d * d;
            }
        }
    }

    /// Freeze mean and spread for a hard clip; subsequent samples go through
    /// [`Self::add_clipped`]. Pixels with fewer than three samples are never
    /// clipped.
    pub fn begin_clip(&mut self, kappa: f32) {
        self.begin_robust(RobustKind::Sigma, kappa);
    }

    /// Fix the statistics for the second pass under `kind` at `k` sigma. The
    /// per-sample centre and limit come from the unweighted first-pass sums, see
    /// [`robust_reference`]. Needs the variance tracking of [`Self::new`].
    pub fn begin_robust(&mut self, kind: RobustKind, k: f32) {
        let a = self.rect.area();
        self.clip = Some(ClipState {
            kind,
            k,
            sum: vec![0.0; a],
            wt: vec![0.0; a],
        });
    }

    /// Second-pass add under the rule fixed by [`Self::begin_robust`].
    pub fn add_clipped(&mut self, samples: &[f32], weight: f32) {
        let Some(c) = self.clip.as_mut() else {
            return;
        };
        let tracked = !self.sumsq.is_empty();
        for (i, &v) in samples.iter().enumerate() {
            if v.is_nan() {
                continue;
            }
            let d = v - self.centre[i];
            let (mean, lim) = if tracked {
                robust_reference(c.k, self.usum[i], self.sumsq[i], self.count[i], d)
            } else {
                (0.0, f32::INFINITY)
            };
            let (f, val) = c.kind.apply(d, mean, lim);
            if f > 0.0 {
                c.sum[i] += weight * f * val;
                c.wt[i] += weight * f;
            }
        }
    }

    /// Result tile; `NaN` where no sample landed. If clipping rejected every
    /// sample at a pixel, the unclipped mean is used there.
    #[must_use]
    pub fn finish(&self) -> Vec<f32> {
        (0..self.rect.area())
            .map(|i| {
                let (s, w) = match &self.clip {
                    Some(c) if c.wt[i] > 0.0 => (c.sum[i], c.wt[i]),
                    _ => (self.sum[i], self.wt[i]),
                };
                if w > 0.0 {
                    self.centre[i] + s / w
                } else {
                    f32::NAN
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tile(vals: &[(&[f32], f32)], clip: Option<f32>) -> Vec<f32> {
        let n = vals[0].0.len();
        let mut t = TileStack::new(Rect::new(0, 0, n, 1), vec![0.5; n], clip.is_some());
        for (v, w) in vals {
            t.add(v, *w);
        }
        if let Some(k) = clip {
            t.begin_clip(k);
            for (v, w) in vals {
                t.add_clipped(v, *w);
            }
        }
        t.finish()
    }

    #[test]
    fn weighted_mean_ignores_nan() {
        let out = tile(&[(&[1.0, f32::NAN], 1.0), (&[4.0, f32::NAN], 2.0)], None);
        assert!((out[0] - 3.0).abs() < 1e-6, "{}", out[0]);
        assert!(out[1].is_nan());
    }

    #[test]
    fn sigma_clip_rejects_outlier() {
        let mut rows: Vec<Vec<f32>> = (0..20).map(|i| vec![1.0 + 0.01 * (i % 3) as f32]).collect();
        rows.push(vec![50.0]);
        let refs: Vec<(&[f32], f32)> = rows.iter().map(|r| (r.as_slice(), 1.0)).collect();
        let plain = tile(&refs, None)[0];
        let clipped = tile(&refs, Some(2.0))[0];
        assert!(plain > 3.0, "{plain}");
        assert!((clipped - 1.01).abs() < 0.02, "{clipped}");
    }

    #[test]
    fn a_heavily_weighted_outlier_is_still_rejected() {
        // The frames with the artefact score best and so carry the most
        // weight; the spread must not be measured with those weights or the
        // outlier hides inside it (issue #150).
        let mut rows: Vec<(Vec<f32>, f32)> = (0..19)
            .map(|i| (vec![1.0 + 0.01 * (i % 3) as f32], 0.2))
            .collect();
        rows.push((vec![5.0], 1.0));
        let refs: Vec<(&[f32], f32)> = rows.iter().map(|(r, w)| (r.as_slice(), *w)).collect();
        let clipped = tile(&refs, Some(2.5))[0];
        assert!((clipped - 1.01).abs() < 0.02, "{clipped}");
    }

    #[test]
    fn the_sample_is_left_out_of_its_own_spread() {
        // Three of twenty samples share a value far from the rest: with the
        // plain spread they inflate it until they sit inside a 2.5 sigma clip.
        let mut rows: Vec<Vec<f32>> = (0..17).map(|i| vec![1.0 + 0.01 * (i % 3) as f32]).collect();
        rows.extend((0..3).map(|_| vec![3.0]));
        let refs: Vec<(&[f32], f32)> = rows.iter().map(|r| (r.as_slice(), 1.0)).collect();
        let clipped = tile(&refs, Some(2.5))[0];
        assert!((clipped - 1.01).abs() < 0.02, "{clipped}");
    }

    #[test]
    fn few_samples_use_the_plain_spread_and_two_are_never_clipped() {
        let (m, lim) = robust_reference(2.5, 2.0, 2.0, 2.0, 0.5);
        assert!((m - 1.0).abs() < 1e-6 && lim.is_infinite());
        // Three samples (0, 0, 3): plain mean 1, population sigma sqrt(2).
        let (m, lim) = robust_reference(1.0, 3.0, 9.0, 3.0, 3.0);
        assert!(
            (m - 1.0).abs() < 1e-6 && (lim - 2.0f32.sqrt()).abs() < 1e-5,
            "{m} {lim}"
        );
        // Six samples: the judged one is left out.
        let (m, lim) = robust_reference(1.0, 5.0, 25.0, 6.0, 5.0);
        assert!(m.abs() < 1e-6 && lim.abs() < 1e-6, "{m} {lim}");
    }

    #[test]
    fn huber_and_winsor_limit_an_outlier_and_keep_clean_data() {
        let mut rows: Vec<Vec<f32>> = (0..30).map(|i| vec![1.0 + 0.01 * (i % 3) as f32]).collect();
        let clean: Vec<(&[f32], f32)> = rows.iter().map(|r| (r.as_slice(), 1.0)).collect();
        let run = |rows: &[(&[f32], f32)], kind: RobustKind| {
            let mut t = TileStack::new(Rect::new(0, 0, 1, 1), vec![0.5], true);
            for (v, w) in rows {
                t.add(v, *w);
            }
            t.begin_robust(kind, kind.default_k());
            for (v, w) in rows {
                t.add_clipped(v, *w);
            }
            t.finish()[0]
        };
        let plain = tile(&clean, None)[0];
        for kind in [RobustKind::Huber, RobustKind::Winsor] {
            assert!((run(&clean, kind) - plain).abs() < 1e-3, "{kind:?}");
        }
        rows.push(vec![20.0]);
        let dirty: Vec<(&[f32], f32)> = rows.iter().map(|r| (r.as_slice(), 1.0)).collect();
        let mean = tile(&dirty, None)[0];
        for kind in [RobustKind::Huber, RobustKind::Winsor] {
            let v = run(&dirty, kind);
            assert!(
                v < mean && (v - plain).abs() < 0.6 * (mean - plain),
                "{kind:?} {v} {mean}"
            );
        }
    }

    #[test]
    fn robust_modes_name_their_second_pass() {
        assert_eq!(StackMode::Mean.second_pass(), None);
        for kind in [RobustKind::Sigma, RobustKind::Huber, RobustKind::Winsor] {
            let m = StackMode::from_kind(kind, 3.0);
            assert_eq!(m.second_pass(), Some((kind, 3.0)));
        }
    }

    #[test]
    fn drizzle_deposit_is_a_typed_stub() {
        assert!(Deposit::Resample(Kernel::Lanczos3).validate().is_ok());
        assert!(matches!(
            Deposit::Drizzle {
                scale: 2.0,
                pixfrac: 0.7
            }
            .validate(),
            Err(Error::Unsupported(_))
        ));
        assert!(matches!(
            Deposit::Drizzle {
                scale: 3.0,
                pixfrac: 0.7
            }
            .validate(),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            Deposit::Drizzle {
                scale: 2.0,
                pixfrac: 0.0
            }
            .validate(),
            Err(Error::Invalid(_))
        ));
    }
}
