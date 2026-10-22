// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Limb darkening: a fitted law, and a flattening of the disc.
//!
//! The Sun is brighter at the centre of the disc than near the limb, because
//! the line of sight reaches a shallower, cooler layer there. With `mu` the
//! cosine of the angle between the line of sight and the local vertical, which
//! for a point at radius `r` on a disc of radius `R` is `sqrt(1 - (r/R)^2)`,
//! the intensity relative to the centre is `L(mu)`:
//!
//! | Law | `L(mu)` | Coefficients |
//! |---|---|---|
//! | linear | `1 - u (1 - mu)` | `u` |
//! | quadratic | `1 - u1 (1 - mu) - u2 (1 - mu)^2` | `u1`, `u2` |
//! | Neckel and Labs | `1 - sum_k a_k (1 - mu^k)`, k = 1 to 5 | `a1` to `a5` |
//!
//! The quadratic law is the usual one for the Sun in the visible (Allen's
//! Astrophysical Quantities lists it); the five-term form is the polynomial of
//! Neckel and Labs (1994), which also fits the steepening near the limb.
//!
//! The fit starts from the azimuthal median of the image in rings about the
//! fitted disc centre. A median ignores sunspots, granulation, plage and
//! filaments, which cover a minority of each ring. It also takes in whatever
//! else is radial, so an uncorrected vignette or a filter's transmission
//! gradient reads as limb darkening: the fit measures the image, not the Sun.
//!
//! Flattening divides the disc by the profile (the fitted law, or the measured
//! profile itself) so that sharpening and compositing see flat contrast. The
//! gain is a table over the radius that the CPU and the GPU read the same way.
//! Outside the limb nothing changes (prominences have their own brightness),
//! and the gain eases to 1 over the last 1.5% of the radius, where the limb is
//! a blur of disc and sky that a gain of three would turn into a bright rim.
//!
//! # References
//!
//! - Neckel, H. and Labs, D. (1994), Solar limb darkening 1986-1990 (lambda 303 to 1099 nm), Solar Phys.
//!   153, 91-114, doi:10.1007/BF00712494.
//! - Cox, A. N. (ed.) (2000), Allen's Astrophysical Quantities, 4th ed., Springer (ISBN to verify).

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::frame::Frame;
use crate::mask::{self, Disc};

/// Rings in the profile and nodes in the gain table (the table has one more).
pub const BINS: usize = 512;

/// Rings beyond this share of the radius are not fitted: the limb is blurred
/// by the seeing and the optics there.
pub const FIT_EDGE: f32 = 0.97;

/// The gain eases to 1 between this share of the radius and the limb.
pub const TAPER_FROM: f32 = 0.985;

/// Smallest relative intensity the gain divides by.
pub const MIN_LEVEL: f32 = 0.2;

/// Ridge weight of the five-term fit, per ring.
const RIDGE: f64 = 1e-5;

/// Rings with fewer samples than this are left out of the fit.
const MIN_SAMPLES: usize = 24;

/// A limb-darkening law.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Law {
    /// `1 - u (1 - mu)`.
    Linear,
    /// `1 - u1 (1 - mu) - u2 (1 - mu)^2`.
    #[default]
    Quadratic,
    /// Neckel and Labs: five terms of `1 - mu^k`.
    Neckel,
}

impl Law {
    /// Every law.
    pub const ALL: [Law; 3] = [Law::Linear, Law::Quadratic, Law::Neckel];

    /// Number of coefficients.
    #[must_use]
    pub fn terms(self) -> usize {
        match self {
            Self::Linear => 1,
            Self::Quadratic => 2,
            Self::Neckel => 5,
        }
    }

    /// Short id: `linear`, `quadratic` or `neckel`.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Quadratic => "quadratic",
            Self::Neckel => "neckel",
        }
    }

    /// Name for display.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Quadratic => "quadratic",
            Self::Neckel => "Neckel and Labs (5 terms)",
        }
    }

    /// Names of the coefficients, in order.
    #[must_use]
    pub fn coefficient_names(self) -> Vec<String> {
        match self {
            Self::Linear => vec!["u".into()],
            Self::Quadratic => vec!["u1".into(), "u2".into()],
            Self::Neckel => (1..=5).map(|k| format!("a{k}")).collect(),
        }
    }

    /// The power of `mu` that term `k` (from 0) is built on.
    fn power(k: usize) -> i32 {
        i32::try_from(k + 1).unwrap_or(i32::MAX)
    }

    /// The basis function of term `k` (from 0) at `mu`: what its coefficient
    /// multiplies, subtracted from one.
    fn basis(self, k: usize, mu: f64) -> f64 {
        match self {
            Self::Linear | Self::Quadratic => (1.0 - mu).powi(Self::power(k)),
            Self::Neckel => 1.0 - mu.powi(Self::power(k)),
        }
    }

    /// `L(mu)` for coefficients `c`.
    #[must_use]
    pub fn eval(self, c: &[f32], mu: f32) -> f32 {
        let mu = f64::from(mu.clamp(0.0, 1.0));
        let drop: f64 = c
            .iter()
            .take(self.terms())
            .enumerate()
            .map(|(k, &v)| f64::from(v) * self.basis(k, mu))
            .sum();
        (1.0 - drop) as f32
    }
}

impl std::str::FromStr for Law {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "linear" => Ok(Self::Linear),
            "quadratic" | "quad" => Ok(Self::Quadratic),
            "neckel" | "neckel-labs" | "nl" | "polynomial" => Ok(Self::Neckel),
            _ => Err(Error::Invalid(format!(
                "unknown limb-darkening law {s:?}; use linear, quadratic or neckel"
            ))),
        }
    }
}

/// `mu` at a share `x` of the radius.
#[must_use]
pub fn mu_at(x: f32) -> f32 {
    (1.0 - x * x).max(0.0).sqrt()
}

/// A fitted law.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LimbFit {
    /// The law fitted.
    pub law: Law,
    /// Coefficients of the law, see [`Law::coefficient_names`].
    pub coefficients: Vec<f32>,
    /// Intensity at the disc centre above the background, in the image's units.
    pub centre: f32,
    /// Sky level subtracted before fitting.
    pub background: f32,
    /// Root mean square of the rings' departure from the law, as a share of
    /// the centre intensity.
    pub rms: f32,
    /// Rings that took part.
    pub rings: usize,
    /// Centre of the disc, x.
    pub cx: f32,
    /// Centre of the disc, y.
    pub cy: f32,
    /// Radius of the disc, pixels.
    pub radius: f32,
}

impl LimbFit {
    /// `L(mu)`, the intensity relative to the centre.
    #[must_use]
    pub fn level(&self, mu: f32) -> f32 {
        self.law.eval(&self.coefficients, mu)
    }

    /// The law at the limb, `L(0)`, which the visible-light Sun puts near 0.3.
    #[must_use]
    pub fn limb_level(&self) -> f32 {
        self.level(0.0)
    }

    /// The disc the fit was made on.
    #[must_use]
    pub fn disc(&self) -> Disc {
        Disc {
            cx: self.cx,
            cy: self.cy,
            r: self.radius,
        }
    }

    /// The coefficients as `u1 = 0.470, u2 = 0.230`.
    #[must_use]
    pub fn coefficients_text(&self) -> String {
        self.law
            .coefficient_names()
            .iter()
            .zip(&self.coefficients)
            .map(|(n, v)| format!("{n} = {v:.3}"))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// One line for a log: the law, its coefficients, the limb level and the fit.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "{}: {}; limb at {:.0}% of the centre; rings differ from the law by {:.2}% rms",
            self.law.id(),
            self.coefficients_text(),
            self.limb_level() * 100.0,
            self.rms * 100.0
        )
    }
}

/// Azimuthal median of `image` in rings one pixel wide (at most `BINS`, at
/// least 16) over `0..=1` radii of `disc`. `None` for a ring with too few
/// samples.
#[must_use]
pub fn radial_profile(image: &Frame, disc: Disc) -> Vec<Option<f32>> {
    let count = (disc.r.round() as usize).clamp(16, BINS);
    let mut rings: Vec<Vec<f32>> = vec![Vec::new(); count];
    let r2 = disc.r * disc.r;
    let (x0, x1) = (
        (disc.cx - disc.r).floor().max(0.0) as usize,
        ((disc.cx + disc.r).ceil() as usize + 1).min(image.width),
    );
    let (y0, y1) = (
        (disc.cy - disc.r).floor().max(0.0) as usize,
        ((disc.cy + disc.r).ceil() as usize + 1).min(image.height),
    );
    for y in y0..y1 {
        let dy = y as f32 - disc.cy;
        let row = image.row(y);
        for (x, &v) in row.iter().enumerate().take(x1).skip(x0) {
            let dx = x as f32 - disc.cx;
            let d2 = dx * dx + dy * dy;
            if d2 < r2 && v.is_finite() {
                let b = ((d2 / r2).sqrt() * count as f32) as usize;
                rings[b.min(count - 1)].push(v);
            }
        }
    }
    rings.into_iter().map(|v| median(v, MIN_SAMPLES)).collect()
}

fn median(mut v: Vec<f32>, min: usize) -> Option<f32> {
    if v.len() < min {
        return None;
    }
    let k = v.len() / 2;
    Some(*v.select_nth_unstable_by(k, f32::total_cmp).1)
}

/// Median of the sky in a ring just outside the limb, or 0 when the frame
/// holds too little of it.
#[must_use]
pub fn sky_level(image: &Frame, disc: Disc) -> f32 {
    let (lo, hi) = ((disc.r * 1.15).powi(2), (disc.r * 1.4).powi(2));
    let mut v = Vec::new();
    for y in 0..image.height {
        let dy = y as f32 - disc.cy;
        for (x, &s) in image.row(y).iter().enumerate() {
            let dx = x as f32 - disc.cx;
            let d2 = dx * dx + dy * dy;
            if d2 >= lo && d2 <= hi && s.is_finite() {
                v.push(s);
            }
        }
    }
    median(v, 2000).unwrap_or(0.0)
}

/// Solve the normal equations `a x = b` by Gaussian elimination with partial
/// pivoting. `None` when singular.
fn solve(mut m: Vec<Vec<f64>>, mut rhs: Vec<f64>) -> Option<Vec<f64>> {
    let n = rhs.len();
    for i in 0..n {
        let piv = (i..n).max_by(|&p, &q| m[p][i].abs().total_cmp(&m[q][i].abs()))?;
        if m[piv][i].abs() < 1e-14 {
            return None;
        }
        m.swap(i, piv);
        rhs.swap(i, piv);
        for row in i + 1..n {
            let f = m[row][i] / m[i][i];
            let top = m[i].clone();
            for (cell, t) in m[row][i..].iter_mut().zip(&top[i..]) {
                *cell -= f * t;
            }
            rhs[row] -= f * rhs[i];
        }
    }
    let mut x = vec![0.0; n];
    for i in (0..n).rev() {
        let s: f64 = (i + 1..n).map(|c| m[i][c] * x[c]).sum();
        x[i] = (rhs[i] - s) / m[i][i];
    }
    Some(x)
}

/// Fit `law` to the rings of `profile` (see [`radial_profile`]) of a disc,
/// after subtracting `background`. `None` when too few rings are usable.
#[must_use]
pub fn fit_profile(
    profile: &[Option<f32>],
    disc: Disc,
    background: f32,
    law: Law,
) -> Option<LimbFit> {
    let n = law.terms() + 1;
    let mut ata = vec![vec![0.0f64; n]; n];
    let mut atb = vec![0.0f64; n];
    let mut used: Vec<(f64, f64)> = Vec::new();
    let count = profile.len();
    for (i, p) in profile.iter().enumerate() {
        let x = (i as f32 + 0.5) / count as f32;
        let Some(v) = p.filter(|_| x <= FIT_EDGE) else {
            continue;
        };
        let mu = f64::from(mu_at(x));
        // v - background = centre - sum_k c_k basis_k(mu)
        let mut row = vec![1.0f64];
        row.extend((0..law.terms()).map(|k| -law.basis(k, mu)));
        let y = f64::from(v - background);
        for r in 0..n {
            for c in 0..n {
                ata[r][c] += row[r] * row[c];
            }
            atb[r] += row[r] * y;
        }
        used.push((mu, y));
    }
    if used.len() < 4 * n {
        return None;
    }
    // The powers of mu are nearly collinear, so five terms are barely
    // determined by the rings. A light ridge on the coefficients (not on the
    // centre) keeps them from running off to values that cancel each other.
    if law == Law::Neckel {
        let ridge = RIDGE * used.len() as f64;
        for (k, row) in ata.iter_mut().enumerate().skip(1) {
            row[k] += ridge;
        }
    }
    let sol = solve(ata, atb)?;
    let centre = sol[0];
    if !(centre.is_finite() && centre > 0.0) {
        return None;
    }
    let coefficients: Vec<f32> = sol[1..].iter().map(|c| (c / centre) as f32).collect();
    let mut sq = 0.0;
    for &(mu, y) in &used {
        let m = f64::from(law.eval(&coefficients, mu as f32));
        sq += (y / centre - m).powi(2);
    }
    Some(LimbFit {
        law,
        coefficients,
        centre: centre as f32,
        background,
        rms: (sq / used.len() as f64).sqrt() as f32,
        rings: used.len(),
        cx: disc.cx,
        cy: disc.cy,
        radius: disc.r,
    })
}

/// Fit `law` to `image`, finding the disc first. `None` without a clear disc
/// or with too little of it in the frame to fit.
#[must_use]
pub fn fit_image(image: &Frame, law: Law) -> Option<LimbFit> {
    let disc = mask::fit_disc(image)?;
    fit_with_disc(image, disc, law)
}

/// As [`fit_image`] on a disc that is already known.
#[must_use]
pub fn fit_with_disc(image: &Frame, disc: Disc, law: Law) -> Option<LimbFit> {
    let profile = radial_profile(image, disc);
    fit_profile(&profile, disc, sky_level(image, disc), law)
}

/// What flattening divides by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// The fitted law: smooth, and blind to everything but the law.
    #[default]
    Model,
    /// The measured azimuthal median profile, smoothed: follows whatever is
    /// radial in the image, a vignette or a filter gradient included.
    Profile,
}

impl Source {
    /// Short id.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Profile => "profile",
        }
    }
}

impl std::str::FromStr for Source {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "model" | "law" | "fit" => Ok(Self::Model),
            "profile" | "median" => Ok(Self::Profile),
            _ => Err(Error::Invalid(format!(
                "unknown flatten source {s:?}; use model or profile"
            ))),
        }
    }
}

/// A flattening ready to apply: the disc, the sky level and the gain by radius.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FlattenPlan {
    /// Centre of the disc, x.
    pub cx: f32,
    /// Centre of the disc, y.
    pub cy: f32,
    /// Radius, pixels.
    pub radius: f32,
    /// Sky level, left alone.
    pub background: f32,
    /// Gain at `BINS + 1` evenly spaced shares of the radius, 0 to 1.
    pub gain: Vec<f32>,
}

impl FlattenPlan {
    /// The gain at `x` shares of the radius: linear between the table's nodes.
    #[must_use]
    pub fn gain_at(&self, x: f32) -> f32 {
        let t = (x * BINS as f32).clamp(0.0, BINS as f32);
        let i = (t as usize).min(BINS - 1);
        let f = t - i as f32;
        self.gain[i] * (1.0 - f) + self.gain[i + 1] * f
    }

    /// Divide the disc of `image` by the profile; everything outside the
    /// limb is returned as it was.
    #[must_use]
    pub fn apply(&self, image: &Frame) -> Frame {
        use rayon::prelude::*;
        let mut out = image.clone();
        out.bayer = None;
        let w = image.width;
        let inv_r = 1.0 / self.radius;
        out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            let dy = y as f32 - self.cy;
            for (x, v) in row.iter_mut().enumerate() {
                let dx = x as f32 - self.cx;
                let u = dx.hypot(dy) * inv_r;
                if u < 1.0 {
                    *v = self.background + (*v - self.background) * self.gain_at(u);
                }
            }
        });
        out
    }
}

/// Gain eased to 1 at the limb.
fn tapered(g: f32, x: f32) -> f32 {
    if x <= TAPER_FROM {
        g
    } else {
        let t = ((x - TAPER_FROM) / (1.0 - TAPER_FROM)).clamp(0.0, 1.0);
        g + (1.0 - g) * t
    }
}

/// The flattening of `image` by `source`: the law of `law` fitted to the disc,
/// or the profile itself. `None` when there is no disc to fit.
#[must_use]
pub fn plan(image: &Frame, source: Source, law: Law) -> Option<(FlattenPlan, LimbFit)> {
    let disc = mask::fit_disc(image)?;
    let profile = radial_profile(image, disc);
    let background = sky_level(image, disc);
    let fit = fit_profile(&profile, disc, background, law)?;
    let level: Vec<f32> = match source {
        Source::Model => (0..=BINS)
            .map(|i| fit.level(mu_at(i as f32 / BINS as f32)))
            .collect(),
        Source::Profile => measured_levels(&profile, background, fit.centre),
    };
    let raw: Vec<f32> = level.iter().map(|&l| 1.0 / l.max(MIN_LEVEL)).collect();
    let mut plan = FlattenPlan {
        cx: disc.cx,
        cy: disc.cy,
        radius: disc.r,
        background,
        gain: raw.clone(),
    };
    // Dividing out the darkening brightens the limb side, and an image with a
    // brighter side (a filter's sweet spot) would run past white. The gain is
    // scaled down just enough to keep the disc's bright end where it was.
    let scale = bright_end(image, &plan, false) / bright_end(image, &plan, true).max(1e-9);
    let scale = scale.clamp(0.1, 1.0);
    plan.gain = raw
        .iter()
        .enumerate()
        .map(|(i, &g)| tapered(g * scale, i as f32 / BINS as f32))
        .collect();
    Some((plan, fit))
}

/// The 99.9th percentile of the disc above the background, as it is or as
/// `plan` would leave it, from every second pixel of every second row.
fn bright_end(image: &Frame, plan: &FlattenPlan, flattened: bool) -> f32 {
    let mut v = Vec::new();
    let inv_r = 1.0 / plan.radius;
    for y in (0..image.height).step_by(2) {
        let dy = y as f32 - plan.cy;
        for x in (0..image.width).step_by(2) {
            let u = (x as f32 - plan.cx).hypot(dy) * inv_r;
            if u < 1.0 {
                let d = image.get(x, y) - plan.background;
                v.push(if flattened { d * plan.gain_at(u) } else { d });
            }
        }
    }
    if v.is_empty() {
        return 1.0;
    }
    let k = ((v.len() - 1) as f32 * 0.999) as usize;
    *v.select_nth_unstable_by(k, f32::total_cmp).1
}

/// The profile as relative levels at the table's nodes: gaps filled, the limb
/// rings held at the last good ring, a short smoothing and the centre at one.
fn measured_levels(profile: &[Option<f32>], background: f32, centre: f32) -> Vec<f32> {
    let n = profile.len();
    let edge = (FIT_EDGE * n as f32) as usize;
    let mut rings: Vec<f32> = Vec::with_capacity(n);
    let mut last = None;
    for (i, p) in profile.iter().enumerate() {
        let v = p.filter(|_| i < edge).map(|p| (p - background) / centre);
        last = v.or(last);
        rings.push(last.unwrap_or(f32::NAN));
    }
    // The rings nearest the centre may hold too few samples; start from the first good one.
    if let Some(first) = rings.iter().copied().find(|v| v.is_finite()) {
        for v in &mut rings {
            if !v.is_finite() {
                *v = first;
            }
        }
    } else {
        return vec![1.0; BINS + 1];
    }
    // Light smoothing over a few rings, then read at the table's nodes.
    let half = (n / 100).max(1);
    let smooth: Vec<f32> = (0..n)
        .map(|i| {
            let (a, b) = (i.saturating_sub(half), (i + half + 1).min(n));
            rings[a..b].iter().sum::<f32>() / (b - a) as f32
        })
        .collect();
    let mut v: Vec<f32> = (0..=BINS)
        .map(|j| {
            let t = (j as f32 / BINS as f32 * n as f32 - 0.5).clamp(0.0, (n - 1) as f32);
            let i0 = t as usize;
            let i1 = (i0 + 1).min(n - 1);
            let f = t - i0 as f32;
            smooth[i0] * (1.0 - f) + smooth[i1] * f
        })
        .collect();
    // The centre is one by definition.
    let c = v[0];
    if c > 0.0 {
        for x in &mut v {
            *x /= c;
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Rng;

    /// A disc of radius 100 about (128, 120) with the given law, a sky pedestal
    /// and fine detail on top that a radial gain must leave alone.
    fn disc_image(law: Law, c: &[f32], sky: f32, detail: f32) -> Frame {
        let (w, h) = (256usize, 240usize);
        let mut rng = Rng::new(5);
        let mut data = vec![sky; w * h];
        for y in 0..h {
            for x in 0..w {
                let d = (x as f32 - 128.0).hypot(y as f32 - 120.0);
                if d < 100.0 {
                    let mu = mu_at(d / 100.0);
                    let grain = detail * ((x as f32 * 1.9).sin() * (y as f32 * 2.3).cos());
                    data[y * w + x] =
                        sky + 0.8 * law.eval(c, mu) * (1.0 + grain) + 0.002 * rng.gauss();
                }
            }
        }
        Frame::new(w, h, data).expect("frame")
    }

    #[test]
    fn the_quadratic_law_is_recovered() {
        let img = disc_image(Law::Quadratic, &[0.47, 0.23], 0.02, 0.01);
        let fit = fit_image(&img, Law::Quadratic).expect("fit");
        assert!((fit.coefficients[0] - 0.47).abs() < 0.02, "{fit:?}");
        assert!((fit.coefficients[1] - 0.23).abs() < 0.03, "{fit:?}");
        assert!((fit.centre - 0.8).abs() < 0.01, "{fit:?}");
        assert!((fit.background - 0.02).abs() < 0.003, "{fit:?}");
        assert!((fit.limb_level() - 0.30).abs() < 0.03, "{fit:?}");
        assert!(fit.rms < 0.01, "{fit:?}");
        assert!((fit.radius - 100.0).abs() < 2.0);
    }

    #[test]
    fn the_linear_and_neckel_laws_are_recovered() {
        let img = disc_image(Law::Linear, &[0.6], 0.0, 0.0);
        let fit = fit_image(&img, Law::Linear).expect("fit");
        assert!((fit.coefficients[0] - 0.6).abs() < 0.02, "{fit:?}");
        let a = [0.30, 0.30, 0.15, -0.05, 0.05];
        let img = disc_image(Law::Neckel, &a, 0.0, 0.0);
        let fit = fit_image(&img, Law::Neckel).expect("fit");
        // Five terms in the powers of mu are nearly collinear: compare the law, not the terms.
        for mu in [0.3f32, 0.5, 0.7, 0.9, 1.0] {
            let (got, want) = (fit.level(mu), Law::Neckel.eval(&a, mu));
            assert!((got - want).abs() < 0.015, "mu {mu}: {got} against {want}");
        }
    }

    #[test]
    fn a_richer_law_fits_no_worse() {
        let img = disc_image(Law::Neckel, &[0.3, 0.3, 0.15, -0.05, 0.05], 0.0, 0.0);
        let lin = fit_image(&img, Law::Linear).expect("linear");
        let quad = fit_image(&img, Law::Quadratic).expect("quadratic");
        let nl = fit_image(&img, Law::Neckel).expect("neckel");
        assert!(
            lin.rms > quad.rms && quad.rms >= nl.rms,
            "{lin:?} {quad:?} {nl:?}"
        );
    }

    #[test]
    fn no_disc_means_no_fit() {
        let img = Frame::new(100, 100, vec![0.4; 10_000]).expect("frame");
        assert!(fit_image(&img, Law::Quadratic).is_none());
        assert!(plan(&img, Source::Model, Law::Quadratic).is_none());
    }

    /// Spread of the disc interior (inside 90% of the radius) over its mean.
    fn flatness(img: &Frame, sky: f32) -> f32 {
        let mut v = Vec::new();
        for y in 0..img.height {
            for x in 0..img.width {
                if (x as f32 - 128.0).hypot(y as f32 - 120.0) < 90.0 {
                    v.push(img.get(x, y) - sky);
                }
            }
        }
        let m = v.iter().sum::<f32>() / v.len() as f32;
        (v.iter().map(|a| (a - m).powi(2)).sum::<f32>() / v.len() as f32).sqrt() / m
    }

    #[test]
    fn flattening_makes_the_disc_flat_and_keeps_the_detail() {
        let sky = 0.02;
        let img = disc_image(Law::Quadratic, &[0.47, 0.23], sky, 0.02);
        let before = flatness(&img, sky);
        for source in [Source::Model, Source::Profile] {
            let (p, _) = plan(&img, source, Law::Quadratic).expect("plan");
            let out = p.apply(&img);
            let after = flatness(&out, sky);
            // The detail alone is 0.02 / sqrt(2) = 1.4%.
            assert!(
                before > 0.1 && after < 0.03,
                "{source:?}: {before} to {after}"
            );
            // The sky and everything outside the limb is untouched.
            assert_eq!(out.get(5, 5), img.get(5, 5));
            // A radial gain keeps small-scale detail: compare it at the centre.
            let hp = |f: &Frame| {
                let (mut s, mut m) = (0.0, 0.0);
                for y in 100..140 {
                    for x in 108..148 {
                        s += (f.get(x, y) - f.get(x + 1, y)).powi(2);
                        m += f.get(x, y) - sky;
                    }
                }
                // Relative to the level, since the gain may scale the whole disc.
                s / (m * m)
            };
            let ratio = hp(&out) / hp(&img);
            assert!(
                (0.9..1.1).contains(&ratio),
                "{source:?}: detail ratio {ratio}"
            );
        }
    }

    #[test]
    fn the_gain_is_one_at_the_centre_and_at_the_limb() {
        let img = disc_image(Law::Quadratic, &[0.47, 0.23], 0.02, 0.0);
        let (p, _) = plan(&img, Source::Model, Law::Quadratic).expect("plan");
        assert!((0.7..=1.0).contains(&p.gain_at(0.0)), "{}", p.gain_at(0.0));
        assert!((p.gain_at(1.0) - 1.0).abs() < 1e-6);
        assert!(
            p.gain_at(0.95) > 1.5 * p.gain_at(0.0),
            "the gain rises towards the limb"
        );
        assert_eq!(p.gain.len(), BINS + 1);
    }

    #[test]
    fn the_law_parses() {
        assert_eq!("neckel".parse::<Law>().ok(), Some(Law::Neckel));
        assert_eq!("Quadratic".parse::<Law>().ok(), Some(Law::Quadratic));
        assert!("cubic".parse::<Law>().is_err());
        assert_eq!("profile".parse::<Source>().ok(), Some(Source::Profile));
        assert!("x".parse::<Source>().is_err());
    }
}
