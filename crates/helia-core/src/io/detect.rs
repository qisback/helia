// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Finding a Bayer mosaic in frames whose header says they are mono.
//!
//! Some recorders (`SharpCap` writing a ZWO colour camera as SER is the known
//! case) label raw Bayer data as mono. Trusting the header then leaves the
//! mosaic in the stack. This module looks at the data instead.
//!
//! A colour-filter array puts a different colour filter over each pixel of a
//! repeating 2x2 cell, so over an area of smooth scene the four pixel phases
//! have different means, while on a true mono sensor they agree to well under
//! a percent. The test measures those four means on the solar disc (found by
//! brightness, away from the limb), averaged over a few frames spread through
//! the capture.
//!
//! The three non-DC bins of the 2x2 discrete Fourier transform (the Nyquist
//! peaks along x, along y and on the diagonal) are exactly the three signed
//! combinations of the phase means, so this is the Nyquist-peak test computed
//! without a transform. Using the means directly also names the pattern: the
//! two green phases agree with each other, so the diagonal on which the phases
//! agree is green, and the other diagonal holds red and blue.
//!
//! Red against blue cannot be read from the geometry. In order of trust the
//! choice is made from the sensor layout a `SharpCap` sidecar states, the
//! colour the filter in use should pass (H-alpha red, calcium K blue), the
//! default layout of the camera model, and finally brightness (the dimmer of
//! the two is taken as blue, which is right for H-alpha, where blue is almost
//! black). [`RbBasis`] says which was used, so the user can be told.

use crate::debayer::Channel;
use crate::frame::{BayerPattern, Frame, Rect};

/// Frames examined at most.
pub const MAX_FRAMES: usize = 5;
/// Relative spread of the four phase means at and above which a mosaic is
/// reported.
pub const MOSAIC_SPREAD: f64 = 0.05;
/// Relative spread below which the data is called mono. Between this and
/// [`MOSAIC_SPREAD`] the result is inconclusive.
pub const MONO_SPREAD: f64 = 0.02;
/// The green diagonal must differ internally by at most this share of the
/// other diagonal's internal difference.
const GREEN_AGREES: f64 = 0.35;
/// Smaller of red and blue below this share of the larger counts as clearly
/// different.
const RB_DIFFERENT: f64 = 0.8;
/// Mean level (0..1) under which there is nothing to measure.
const TOO_DARK: f64 = 0.002;
/// Disc radius, in 2x2 cells, under which the area is too small to measure.
const MIN_RADIUS_CELLS: f64 = 16.0;
/// Share of the brightness range, above the dark level, that counts as disc.
const DISC_LEVEL: f32 = 0.3;

/// What else is known about the capture, to settle red against blue.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hints {
    /// A sensor layout the user or a sidecar stated outright.
    pub stated: Option<BayerPattern>,
    /// The layout colour cameras of this model normally have.
    pub model_default: Option<BayerPattern>,
    /// The colour the filter in use passes: [`Channel::Red`] (H-alpha) or
    /// [`Channel::Blue`] (calcium K); anything else says nothing.
    pub bright: Option<Channel>,
}

/// What settled which of the two non-green phases is red.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RbBasis {
    /// A layout stated by a sidecar or the user.
    Stated,
    /// The colour the filter should pass.
    Filter,
    /// The usual layout of the camera model.
    Model,
    /// The dimmer phase was taken to be blue.
    Brightness,
}

impl RbBasis {
    /// Short phrase for a message.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::Stated => "the sensor layout stated in the sidecar",
            Self::Filter => "the colour the filter passes",
            Self::Model => "the usual layout of this camera model",
            Self::Brightness => "assuming the dimmer of the two is blue",
        }
    }
}

/// The verdict of [`detect`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The four phases agree: no mosaic.
    NoMosaic,
    /// A Bayer mosaic with this pattern.
    Mosaic {
        /// The pattern, named by its top-left 2x2 cell.
        pattern: BayerPattern,
        /// How red was told from blue.
        basis: RbBasis,
    },
    /// The data cannot settle it, for the reason given.
    Inconclusive(&'static str),
}

/// Measurements and verdict.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Detection {
    /// Frames that contributed.
    pub frames_used: usize,
    /// Area measured in the first frame that contributed.
    pub region: Rect,
    /// Mean level (0..1) of pixels at (even x, even y), (odd x, even y),
    /// (even x, odd y), (odd x, odd y), in that order.
    pub means: [f64; 4],
    /// (largest mean - smallest mean) / average mean.
    pub spread: f64,
    /// The verdict.
    pub outcome: Outcome,
}

fn empty(reason: &'static str) -> Detection {
    Detection {
        frames_used: 0,
        region: Rect::new(0, 0, 0, 0),
        means: [0.0; 4],
        spread: 0.0,
        outcome: Outcome::Inconclusive(reason),
    }
}

/// Indices of up to [`MAX_FRAMES`] frames spread evenly over `count`.
#[must_use]
pub fn sample_indices(count: usize) -> Vec<usize> {
    let k = count.min(MAX_FRAMES);
    match k {
        0 => Vec::new(),
        1 => vec![0],
        _ => (0..k).map(|i| i * (count - 1) / (k - 1)).collect(),
    }
}

/// Brightness level separating disc from sky: a share of the way from the
/// dark end to the bright end of the 2x2 cell means.
fn disc_threshold(cells: &[f32]) -> Option<f32> {
    let step = (cells.len() / 50_000).max(1);
    let mut sample: Vec<f32> = cells.iter().step_by(step).copied().collect();
    if sample.is_empty() {
        return None;
    }
    sample.sort_by(f32::total_cmp);
    let at = |q: f32| sample[((sample.len() - 1) as f32 * q) as usize];
    let (lo, hi) = (at(0.01), at(0.99));
    ((hi - lo) > 1e-3).then_some(lo + DISC_LEVEL * (hi - lo))
}

/// The area to measure in `f`: a square inside the bright disc, well away from
/// the limb, with even origin and size. `None` when there is no usable disc.
fn disc_box(f: &Frame) -> Option<Rect> {
    let (bw, bh) = (f.width / 2, f.height / 2);
    if bw < 2 * MIN_RADIUS_CELLS as usize || bh < 2 * MIN_RADIUS_CELLS as usize {
        return None;
    }
    let mut cells = vec![0.0f32; bw * bh];
    for (y, row) in cells.chunks_exact_mut(bw).enumerate() {
        let (a, b) = (&f.data[2 * y * f.width..], &f.data[(2 * y + 1) * f.width..]);
        for (x, c) in row.iter_mut().enumerate() {
            *c = 0.25 * (a[2 * x] + a[2 * x + 1] + b[2 * x] + b[2 * x + 1]);
        }
    }
    let t = disc_threshold(&cells)?;
    let (mut n, mut sx, mut sy) = (0.0f64, 0.0f64, 0.0f64);
    for (y, row) in cells.chunks_exact(bw).enumerate() {
        for (x, &c) in row.iter().enumerate() {
            if c > t {
                n += 1.0;
                sx += x as f64;
                sy += y as f64;
            }
        }
    }
    let r = (n / std::f64::consts::PI).sqrt();
    if r < MIN_RADIUS_CELLS {
        return None;
    }
    let half = (0.5 * r) as usize;
    let (cx, cy) = ((sx / n) as usize, (sy / n) as usize);
    let (x0, x1) = (cx.saturating_sub(half), (cx + half).min(bw));
    let (y0, y1) = (cy.saturating_sub(half), (cy + half).min(bh));
    (x1 > x0 && y1 > y0).then(|| Rect {
        x0: 2 * x0,
        y0: 2 * y0,
        x1: 2 * x1,
        y1: 2 * y1,
    })
}

/// Mean of each pixel phase inside `r` (even origin).
fn phase_means(f: &Frame, r: Rect) -> [f64; 4] {
    let mut sum = [0.0f64; 4];
    for y in r.y0..r.y1 {
        let row = &f.data[y * f.width + r.x0..y * f.width + r.x1];
        let (mut even, mut odd) = (0.0f64, 0.0f64);
        for pair in row.as_chunks::<2>().0 {
            even += f64::from(pair[0]);
            odd += f64::from(pair[1]);
        }
        let k = (y & 1) * 2;
        sum[k] += even;
        sum[k + 1] += odd;
    }
    let cells = (r.width() / 2 * (r.height() / 2)) as f64;
    sum.map(|s| s / cells)
}

/// Decide from the four averaged phase means.
#[must_use]
pub fn classify(means: [f64; 4], hints: Hints) -> (f64, Outcome) {
    let mean = means.iter().sum::<f64>() / 4.0;
    if mean < TOO_DARK {
        return (
            0.0,
            Outcome::Inconclusive("the image is too dark to measure"),
        );
    }
    let hi = means.iter().copied().fold(f64::MIN, f64::max);
    let lo = means.iter().copied().fold(f64::MAX, f64::min);
    let spread = (hi - lo) / mean;
    if spread < MONO_SPREAD {
        return (spread, Outcome::NoMosaic);
    }
    if spread < MOSAIC_SPREAD {
        return (
            spread,
            Outcome::Inconclusive("the 2x2 differences are too small to tell"),
        );
    }
    let [m00, m10, m01, m11] = means;
    let (d_main, d_anti) = ((m00 - m11).abs(), (m10 - m01).abs());
    let (d_lo, d_hi) = (d_main.min(d_anti), d_main.max(d_anti));
    let gap = ((m00 + m11) - (m10 + m01)).abs() / 2.0;
    let flat = MOSAIC_SPREAD * mean;
    // Green sits on the diagonal whose two phases agree.
    let greens_main = if d_hi < flat && gap >= flat {
        // Both diagonals agree inside themselves (red close to blue): green is
        // the brighter one, as it is on every colour sensor.
        m00 + m11 > m10 + m01
    } else if d_lo <= GREEN_AGREES * d_hi {
        d_main < d_anti
    } else {
        return (
            spread,
            Outcome::Inconclusive("the 2x2 pattern is not like a colour-filter array"),
        );
    };
    // The other diagonal holds red and blue. `first` is the phase that is red
    // in the RGGB (or GRBG) member of the family, `second` in the other.
    let (first, second, red_first, red_second) = if greens_main {
        (m10, m01, BayerPattern::Grbg, BayerPattern::Gbrg)
    } else {
        (m00, m11, BayerPattern::Rggb, BayerPattern::Bggr)
    };
    let in_family =
        |p: BayerPattern| matches!(p, BayerPattern::Grbg | BayerPattern::Gbrg) == greens_main;
    let different = first.min(second) < RB_DIFFERENT * first.max(second);
    let (pattern, basis) = if let Some(p) = hints.stated.filter(|p| in_family(*p)) {
        (p, RbBasis::Stated)
    } else if let (true, Some(c @ (Channel::Red | Channel::Blue))) = (different, hints.bright) {
        let first_is_brighter = first > second;
        let red_is_brighter = c == Channel::Red;
        let p = if first_is_brighter == red_is_brighter {
            red_first
        } else {
            red_second
        };
        (p, RbBasis::Filter)
    } else if let Some(p) = hints.model_default.filter(|p| in_family(*p)) {
        (p, RbBasis::Model)
    } else {
        // The dimmer phase is blue.
        let p = if first >= second {
            red_first
        } else {
            red_second
        };
        (p, RbBasis::Brightness)
    };
    (spread, Outcome::Mosaic { pattern, basis })
}

/// Look for a Bayer mosaic in `frames`, which are read as stored (the tag in
/// [`Frame::bayer`] is ignored). Pass a few frames spread through the capture;
/// see [`sample_indices`].
#[must_use]
pub fn detect(frames: &[Frame], hints: Hints) -> Detection {
    let mut total = [0.0f64; 4];
    let mut used = 0usize;
    let mut region = None;
    for f in frames.iter().filter(|f| f.data.len() == f.width * f.height) {
        let Some(r) = disc_box(f) else { continue };
        let m = phase_means(f, r);
        for (t, v) in total.iter_mut().zip(m) {
            *t += v;
        }
        used += 1;
        region.get_or_insert(r);
    }
    let Some(region) = region else {
        return empty("no bright disc large enough to measure was found");
    };
    let means = total.map(|t| t / used as f64);
    let (spread, outcome) = classify(means, hints);
    Detection {
        frames_used: used,
        region,
        means,
        spread,
        outcome,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::mosaic_disc;

    fn frames(pattern: Option<BayerPattern>, gains: [f32; 3], n: usize) -> Vec<Frame> {
        (0..n)
            .map(|i| mosaic_disc(240, 200, pattern, gains, 7 + i as u64))
            .collect()
    }

    const PATTERNS: [BayerPattern; 4] = [
        BayerPattern::Rggb,
        BayerPattern::Bggr,
        BayerPattern::Grbg,
        BayerPattern::Gbrg,
    ];

    #[test]
    #[cfg_attr(miri, ignore = "whole-frame pixel maths: far too slow under Miri")]
    fn every_pattern_is_named_correctly() {
        // Typical white-light gains: red and blue both below green, red above blue.
        for p in PATTERNS {
            let d = detect(&frames(Some(p), [0.75, 1.0, 0.55], 3), Hints::default());
            assert_eq!(d.frames_used, 3);
            assert!(d.spread > 0.2, "{}", d.spread);
            assert_eq!(
                d.outcome,
                Outcome::Mosaic {
                    pattern: p,
                    basis: RbBasis::Brightness
                },
                "{p:?}"
            );
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "whole-frame pixel maths: far too slow under Miri")]
    fn true_mono_is_not_flagged() {
        let d = detect(&frames(None, [1.0; 3], 4), Hints::default());
        assert_eq!(d.outcome, Outcome::NoMosaic, "{d:?}");
        assert!(d.spread < MONO_SPREAD, "{}", d.spread);
    }

    #[test]
    #[cfg_attr(miri, ignore = "whole-frame pixel maths: far too slow under Miri")]
    fn a_mono_sensor_with_column_gain_stripes_is_not_a_mosaic() {
        let mut f = frames(None, [1.0; 3], 3);
        for fr in &mut f {
            let w = fr.width;
            for (i, v) in fr.data.iter_mut().enumerate() {
                if (i % w) % 2 == 1 {
                    *v *= 0.8;
                }
            }
        }
        let d = detect(&f, Hints::default());
        assert!(
            matches!(d.outcome, Outcome::Inconclusive(_)),
            "stripes are not a colour-filter array: {d:?}"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "whole-frame pixel maths: far too slow under Miri")]
    fn h_alpha_with_a_black_blue_channel_is_found() {
        // The Sun_Test.ser case: red and green strong, blue nearly black.
        for p in PATTERNS {
            let d = detect(&frames(Some(p), [0.94, 1.0, 0.005], 3), Hints::default());
            assert_eq!(
                d.outcome,
                Outcome::Mosaic {
                    pattern: p,
                    basis: RbBasis::Brightness
                },
                "{p:?}: {d:?}"
            );
            assert!(d.spread > 1.0);
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "whole-frame pixel maths: far too slow under Miri")]
    fn the_filter_settles_red_against_blue() {
        // Calcium K: blue strong, red and green almost black. The dimmer-is-blue
        // guess is wrong here; the filter hint corrects it.
        let gains = [0.02, 0.03, 1.0];
        for p in PATTERNS {
            let f = frames(Some(p), gains, 3);
            let guess = detect(&f, Hints::default()).outcome;
            let hinted = detect(
                &f,
                Hints {
                    bright: Some(Channel::Blue),
                    ..Hints::default()
                },
            );
            assert_eq!(
                hinted.outcome,
                Outcome::Mosaic {
                    pattern: p,
                    basis: RbBasis::Filter
                },
                "{p:?}"
            );
            assert_ne!(guess, hinted.outcome, "{p:?}: brightness alone is fooled");
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "whole-frame pixel maths: far too slow under Miri")]
    fn a_stated_layout_beats_the_other_clues() {
        let f = frames(Some(BayerPattern::Bggr), [0.75, 1.0, 0.55], 2);
        let d = detect(
            &f,
            Hints {
                stated: Some(BayerPattern::Bggr),
                model_default: Some(BayerPattern::Rggb),
                bright: Some(Channel::Red),
            },
        );
        assert_eq!(
            d.outcome,
            Outcome::Mosaic {
                pattern: BayerPattern::Bggr,
                basis: RbBasis::Stated
            }
        );
        // A stated layout in the wrong family is ignored: the data decides.
        let d = detect(
            &f,
            Hints {
                stated: Some(BayerPattern::Grbg),
                ..Hints::default()
            },
        );
        assert!(matches!(
            d.outcome,
            Outcome::Mosaic {
                pattern: BayerPattern::Bggr,
                basis: RbBasis::Brightness
            }
        ));
    }

    #[test]
    #[cfg_attr(miri, ignore = "whole-frame pixel maths: far too slow under Miri")]
    fn near_equal_red_and_blue_use_the_model_default() {
        let f = frames(Some(BayerPattern::Rggb), [0.7, 1.0, 0.68], 2);
        let d = detect(
            &f,
            Hints {
                model_default: Some(BayerPattern::Rggb),
                ..Hints::default()
            },
        );
        assert_eq!(
            d.outcome,
            Outcome::Mosaic {
                pattern: BayerPattern::Rggb,
                basis: RbBasis::Model
            }
        );
    }

    #[test]
    fn dark_and_discless_frames_are_inconclusive() {
        let dark = Frame {
            width: 128,
            height: 128,
            data: vec![0.0; 128 * 128],
            bayer: None,
        };
        let d = detect(&[dark], Hints::default());
        assert!(matches!(d.outcome, Outcome::Inconclusive(_)));
        assert_eq!(d.frames_used, 0);
        assert!(matches!(
            detect(&[], Hints::default()).outcome,
            Outcome::Inconclusive(_)
        ));
    }

    #[test]
    fn frames_are_sampled_evenly() {
        assert_eq!(sample_indices(0), Vec::<usize>::new());
        assert_eq!(sample_indices(1), [0]);
        assert_eq!(sample_indices(3), [0, 1, 2]);
        assert_eq!(sample_indices(101), [0, 25, 50, 75, 100]);
    }
}
