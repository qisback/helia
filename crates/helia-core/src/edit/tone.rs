// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Tone edits: levels (black point, white point, gamma) and a monotone curve.
//!
//! Both are pointwise and rise monotonically, so a darker input never gets a
//! brighter output. Samples are clamped to `0..=1` on the way in.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Levels: black point, white point and gamma, as in any photo editor.
///
/// ```text
/// t   = clamp((v - black) / (white - black), 0, 1)
/// out = out_black + (out_white - out_black) * t ^ (1 / gamma)
/// ```
///
/// A gamma above 1 brightens the mid-tones. With the defaults the edit does
/// nothing.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Levels {
    /// Input value that becomes black.
    pub black: f32,
    /// Input value that becomes white.
    pub white: f32,
    /// Mid-tone exponent (above 1 brightens).
    pub gamma: f32,
    /// Output value of black.
    pub out_black: f32,
    /// Output value of white.
    pub out_white: f32,
}

impl Default for Levels {
    fn default() -> Self {
        Self {
            black: 0.0,
            white: 1.0,
            gamma: 1.0,
            out_black: 0.0,
            out_white: 1.0,
        }
    }
}

impl Levels {
    /// Reject values that make the mapping meaningless.
    ///
    /// # Errors
    /// [`Error::Invalid`] when the white point is not above the black point,
    /// the gamma is not positive or a value is not finite.
    pub fn validate(&self) -> Result<()> {
        let finite = [
            self.black,
            self.white,
            self.gamma,
            self.out_black,
            self.out_white,
        ]
        .iter()
        .all(|v| v.is_finite());
        if !finite || self.white <= self.black || self.gamma <= 0.0 {
            return Err(Error::Invalid(
                "levels need white > black and a positive gamma".into(),
            ));
        }
        Ok(())
    }

    /// True when the edit leaves every sample alone.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        *self == Self::default()
    }

    /// The mapping of one sample (the arithmetic the GPU kernel repeats).
    #[must_use]
    pub fn apply(&self, v: f32) -> f32 {
        let v = if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let t = ((v - self.black) / (self.white - self.black)).clamp(0.0, 1.0);
        let t = if (self.gamma - 1.0).abs() < f32::EPSILON {
            t
        } else {
            t.powf(1.0 / self.gamma)
        };
        self.out_black + (self.out_white - self.out_black) * t
    }

    /// The edit that undoes this one for samples between the black and white
    /// points (what lies outside was clipped and cannot come back). `None`
    /// when the output range is empty.
    #[must_use]
    pub fn inverse(&self) -> Option<Levels> {
        if (self.out_white - self.out_black).abs() < 1e-9 || self.out_white < self.out_black {
            return None;
        }
        // out = ob + (ow - ob) t^(1/g)  =>  t = ((out - ob) / (ow - ob))^g
        // v = black + (white - black) t. Written as levels on the output
        // domain: black' = ob, white' = ow, gamma' = 1 / g, out' = black..white.
        Some(Levels {
            black: self.out_black,
            white: self.out_white,
            gamma: 1.0 / self.gamma,
            out_black: self.black,
            out_white: self.white,
        })
    }

    /// Black and white points at two percentiles of `data` (for an "auto
    /// levels" button), gamma 1.
    #[must_use]
    pub fn auto(data: &[f32], low: f32, high: f32) -> Self {
        let mut v: Vec<f32> = data
            .iter()
            .copied()
            .filter(|x| x.is_finite())
            .step_by(3)
            .collect();
        if v.len() < 2 {
            return Self::default();
        }
        v.sort_by(f32::total_cmp);
        let at = |p: f32| v[((v.len() - 1) as f32 * p.clamp(0.0, 1.0)) as usize];
        let (b, w) = (at(low), at(high));
        if w - b < 1e-4 {
            return Self::default();
        }
        Self {
            black: b.clamp(0.0, 1.0),
            white: w.clamp(0.0, 1.0),
            ..Self::default()
        }
    }

    /// One line for the file history.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "levels black {:.4} white {:.4} gamma {:.3} output {:.3}..{:.3}",
            self.black, self.white, self.gamma, self.out_black, self.out_white
        )
    }
}

/// A tone curve through control points `(input, output)`, interpolated as a
/// monotone cubic (Fritsch-Carlson / PCHIP), so it never overshoots between
/// points. The ends `(0, 0)` and `(1, 1)` are added when missing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Curve {
    /// Control points, in any order; inputs must be distinct.
    pub points: Vec<[f32; 2]>,
}

impl Default for Curve {
    fn default() -> Self {
        Self {
            points: vec![[0.0, 0.0], [1.0, 1.0]],
        }
    }
}

/// Knots of a curve in the form the GPU kernel evaluates.
#[derive(Clone, Debug, PartialEq)]
pub struct Knots {
    /// Inputs, strictly rising from 0 to 1.
    pub x: Vec<f32>,
    /// Outputs.
    pub y: Vec<f32>,
    /// Slope at each knot.
    pub m: Vec<f32>,
}

impl Curve {
    /// Most control points a curve may have.
    pub const MAX_POINTS: usize = 16;

    /// Check the points.
    ///
    /// # Errors
    /// [`Error::Invalid`] for too many points, values outside `0..=1`,
    /// repeated inputs or an output that falls as the input rises.
    pub fn validate(&self) -> Result<()> {
        self.knots().map(|_| ())
    }

    /// True when the curve is the straight line.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.knots()
            .is_ok_and(|k| k.x.len() == 2 && k.y == [0.0, 1.0])
    }

    /// Sorted knots with end points and PCHIP slopes.
    ///
    /// # Errors
    /// As [`Curve::validate`].
    pub fn knots(&self) -> Result<Knots> {
        if self.points.len() > Self::MAX_POINTS {
            return Err(Error::Invalid(format!(
                "a curve takes at most {} points",
                Self::MAX_POINTS
            )));
        }
        let mut pts = self.points.clone();
        if pts
            .iter()
            .any(|p| !p[0].is_finite() || !p[1].is_finite() || !(0.0..=1.0).contains(&p[0]))
            || pts.iter().any(|p| !(0.0..=1.0).contains(&p[1]))
        {
            return Err(Error::Invalid(
                "curve points must lie inside 0..1 on both axes".into(),
            ));
        }
        pts.sort_by(|a, b| a[0].total_cmp(&b[0]));
        if pts.first().is_none_or(|p| p[0] > 0.0) {
            pts.insert(0, [0.0, 0.0]);
        }
        if pts.last().is_none_or(|p| p[0] < 1.0) {
            pts.push([1.0, 1.0]);
        }
        if pts.windows(2).any(|w| w[1][0] <= w[0][0]) {
            return Err(Error::Invalid("curve inputs must be distinct".into()));
        }
        if pts.windows(2).any(|w| w[1][1] < w[0][1]) {
            return Err(Error::Invalid(
                "a curve must not fall as the input rises".into(),
            ));
        }
        let n = pts.len();
        let x: Vec<f32> = pts.iter().map(|p| p[0]).collect();
        let y: Vec<f32> = pts.iter().map(|p| p[1]).collect();
        let d: Vec<f32> = (0..n - 1)
            .map(|k| (y[k + 1] - y[k]) / (x[k + 1] - x[k]))
            .collect();
        let mut m = vec![0.0f32; n];
        m[0] = d[0];
        m[n - 1] = d[n - 2];
        for k in 1..n - 1 {
            if d[k - 1] * d[k] > 0.0 {
                let (h0, h1) = (x[k] - x[k - 1], x[k + 1] - x[k]);
                let (w1, w2) = (2.0 * h1 + h0, h1 + 2.0 * h0);
                m[k] = (w1 + w2) / (w1 / d[k - 1] + w2 / d[k]);
            }
        }
        Ok(Knots { x, y, m })
    }

    /// The curve at `v` (clamped to `0..=1`).
    #[must_use]
    pub fn apply_knots(k: &Knots, v: f32) -> f32 {
        let v = if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let last = k.x.len() - 1;
        let mut i = 0;
        while i + 1 < last && v >= k.x[i + 1] {
            i += 1;
        }
        let h = k.x[i + 1] - k.x[i];
        let t = ((v - k.x[i]) / h).clamp(0.0, 1.0);
        let (t2, t3) = (t * t, t * t * t);
        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + t;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;
        h00 * k.y[i] + h10 * h * k.m[i] + h01 * k.y[i + 1] + h11 * h * k.m[i + 1]
    }

    /// One line for the file history.
    #[must_use]
    pub fn describe(&self) -> String {
        let pts: Vec<String> = self
            .points
            .iter()
            .map(|p| format!("({:.3},{:.3})", p[0], p[1]))
            .collect();
        format!("curve through {}", pts.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_levels_do_nothing() {
        let l = Levels::default();
        for i in 0..=100 {
            let v = i as f32 / 100.0;
            assert_eq!(l.apply(v), v);
        }
        assert!(l.is_identity());
    }

    #[test]
    fn levels_are_monotone_for_any_valid_setting() {
        for (b, w, g, ob, ow) in [
            (0.1, 0.9, 0.5, 0.0, 1.0),
            (0.0, 0.5, 2.2, 0.1, 0.8),
            (0.3, 0.31, 1.0, 0.0, 1.0),
            (0.2, 1.0, 3.0, 0.0, 1.0),
        ] {
            let l = Levels {
                black: b,
                white: w,
                gamma: g,
                out_black: ob,
                out_white: ow,
            };
            l.validate().unwrap_or_else(|e| panic!("{e}"));
            let mut prev = f32::NEG_INFINITY;
            for i in 0..=1000 {
                let o = l.apply(i as f32 / 1000.0);
                assert!(o >= prev, "{l:?} falls at {i}");
                prev = o;
            }
        }
    }

    #[test]
    fn levels_invert_between_the_black_and_white_points() {
        let l = Levels {
            black: 0.1,
            white: 0.8,
            gamma: 1.8,
            out_black: 0.05,
            out_white: 0.95,
        };
        let inv = l.inverse().unwrap_or_else(|| panic!("no inverse"));
        let mut worst = 0.0f32;
        for i in 0..=700 {
            let v = 0.1 + 0.7 * i as f32 / 700.0;
            worst = worst.max((inv.apply(l.apply(v)) - v).abs());
        }
        assert!(worst < 2e-5, "round trip error {worst}");
    }

    #[test]
    fn levels_reject_nonsense() {
        let bad = Levels {
            black: 0.5,
            white: 0.4,
            ..Levels::default()
        };
        assert!(bad.validate().is_err());
        let bad = Levels {
            gamma: 0.0,
            ..Levels::default()
        };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn auto_levels_pick_the_percentiles() {
        let data: Vec<f32> = (0..1000).map(|i| 0.2 + 0.6 * i as f32 / 999.0).collect();
        let l = Levels::auto(&data, 0.0, 1.0);
        assert!((l.black - 0.2).abs() < 0.01 && (l.white - 0.8).abs() < 0.01);
    }

    #[test]
    fn straight_curve_is_the_identity() {
        let c = Curve::default();
        assert!(c.is_identity());
        let k = c.knots().unwrap_or_else(|e| panic!("{e}"));
        for i in 0..=50 {
            let v = i as f32 / 50.0;
            assert!((Curve::apply_knots(&k, v) - v).abs() < 1e-6);
        }
    }

    #[test]
    fn curve_is_monotone_and_passes_through_its_points() {
        let c = Curve {
            points: vec![[0.5, 0.2], [0.2, 0.05], [0.8, 0.9], [0.85, 0.9]],
        };
        let k = c.knots().unwrap_or_else(|e| panic!("{e}"));
        for p in &c.points {
            assert!((Curve::apply_knots(&k, p[0]) - p[1]).abs() < 1e-5);
        }
        let mut prev = -1.0;
        for i in 0..=2000 {
            let o = Curve::apply_knots(&k, i as f32 / 2000.0);
            assert!(o >= prev - 1e-6, "falls at {i}");
            assert!((0.0..=1.0 + 1e-6).contains(&o));
            prev = o;
        }
    }

    #[test]
    fn a_rising_curve_can_be_inverted_by_bisection() {
        // Strictly rising control points make a strictly rising curve, so each
        // output has exactly one input that made it.
        let c = Curve {
            points: vec![[0.2, 0.1], [0.5, 0.35], [0.8, 0.9]],
        };
        let k = c.knots().unwrap_or_else(|e| panic!("{e}"));
        let mut worst = 0.0f32;
        for i in 1..200 {
            let v = i as f32 / 200.0;
            let out = Curve::apply_knots(&k, v);
            let (mut lo, mut hi) = (0.0f32, 1.0f32);
            for _ in 0..40 {
                let mid = f32::midpoint(lo, hi);
                if Curve::apply_knots(&k, mid) < out {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            worst = worst.max((f32::midpoint(lo, hi) - v).abs());
        }
        assert!(worst < 1e-4, "inverse error {worst}");
    }

    #[test]
    fn falling_or_repeated_points_are_refused() {
        let falling = Curve {
            points: vec![[0.3, 0.6], [0.6, 0.4]],
        };
        assert!(falling.validate().is_err());
        let repeated = Curve {
            points: vec![[0.3, 0.3], [0.3, 0.4]],
        };
        assert!(repeated.validate().is_err());
    }
}
