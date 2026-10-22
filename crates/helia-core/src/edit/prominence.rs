// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The prominence edit: a stretched off-limb layer composited with the disc.
//!
//! A narrow-band stack holds two pictures at once. The disc is bright and
//! the prominences above the limb are a few per cent of it, so one tone curve
//! cannot show both: lift the prominences and the disc burns out. This edit
//! makes the second exposure from the same stack. The disc is kept as it is;
//! outside the limb the scattered light is taken away, what is left is
//! stretched to read like the disc, and the two are joined by a mask whose
//! edge is feathered about the fitted limb ([`crate::mask::fit_disc`]).
//!
//! 1. **Background.** The scattered light and sky at each radius is the
//!    azimuthal median of the ring at that radius, outside the limb. A
//!    prominence covers a small part of a ring, so the median is the glow.
//! 2. **Signal.** The frame less that background, lightly blurred, less a
//!    multiple of the noise (measured as the median absolute deviation in
//!    the outer part of the field), floored at zero.
//! 3. **Stretch.** The signal as a share of its own 99.5th percentile just
//!    above the limb, through `asinh(k x) / asinh(k)` (held at 1), then scaled
//!    so the brightest prominences reach `level` of the disc's brightness.
//! 4. **Join.** Inside the limb the disc is untouched; across the feather the
//!    two are mixed; outside the layer replaces the picture, with the black
//!    level the stack had.
//!
//! A stack with no prominences stays dark: the stretch is referred to at
//! least four times the noise, so noise is not lifted to look like structure.
//!
//! # References
//!
//! - Lupton, R. et al. (2004), Preparing red-green-blue images from CCD data, Publ. Astron. Soc. Pac. 116,
//!   133-137, doi:10.1086/382245 (the `asinh` stretch). The layer design is Helia's own.

#![allow(
    clippy::cast_possible_wrap,
    reason = "pixel coordinates and sector numbers are far below isize::MAX"
)]

use serde::{Deserialize, Serialize};

use super::conv;
use crate::error::{Error, Result};
use crate::frame::Frame;
use crate::mask::{self, Disc};

/// Ring width in pixels when the background profile is measured.
const RING: f32 = 1.0;

/// Angular sectors the glow is modelled in.
const SECTORS: usize = 24;

/// Fewest samples a ring needs to give a median.
const MIN_SAMPLES: usize = 24;

/// Standard deviation in pixels of the blur the signal gets before the noise
/// gate.
const SMOOTH_SIGMA: f32 = 1.2;

/// Brightest prominence at least this many noise sigma, so that a field with
/// no prominence is not stretched into noise.
const MIN_REFERENCE_SIGMA: f32 = 8.0;

/// The gate is never below this share of the disc's brightness: below it the
/// background model's own error (the glow's curvature, a flat that is not
/// flat) is as big as the noise, and stretching it shows the model, not the
/// Sun.
const MIN_GATE_SHARE: f32 = 0.004;

/// Composite a stretched prominence layer with the disc.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prominence {
    /// How hard the layer is lifted: the signal is multiplied by this before
    /// the stretch. 1 refers it to the brightest prominence found.
    pub strength: f32,
    /// The `k` of the `asinh` stretch; 0 is linear. Larger lifts faint
    /// prominences more.
    pub stretch: f32,
    /// Brightness of the brightest prominence as a share of the disc's.
    pub level: f32,
    /// Half-width of the feathered edge, as a share of the disc radius.
    pub feather: f32,
    /// How far out the layer reaches, in disc radii; beyond it the picture
    /// is the black level.
    pub reach: f32,
    /// Noise sigmas taken off the signal (the gate).
    pub gate: f32,
    /// Where the join sits, as a share of the disc radius added to the fitted
    /// limb (negative moves it inside the limb, positive outside).
    pub offset: f32,
    /// Gamma of the disc's tone above the sky; 1 leaves the disc as it is.
    pub disc_gamma: f32,
    /// Gamma of the stretched layer's tone; above 1 darkens its faint parts.
    pub layer_gamma: f32,
}

impl Default for Prominence {
    fn default() -> Self {
        Self {
            strength: 1.0,
            stretch: 6.0,
            level: 0.75,
            feather: 0.012,
            reach: 1.4,
            gate: 3.0,
            offset: 0.0,
            disc_gamma: 1.0,
            layer_gamma: 1.0,
        }
    }
}

impl Prominence {
    /// Check the values.
    ///
    /// # Errors
    /// [`Error::Invalid`] for a value out of range.
    pub fn validate(&self) -> Result<()> {
        let bad = |m: &str| Err(Error::Invalid(m.into()));
        if !(self.strength.is_finite() && (0.0..=100.0).contains(&self.strength)) {
            return bad("strength must be 0 to 100");
        }
        if !(self.stretch.is_finite() && (0.0..=1000.0).contains(&self.stretch)) {
            return bad("stretch must be 0 to 1000");
        }
        if !(self.level.is_finite() && self.level > 0.0 && self.level <= 2.0) {
            return bad("level must be above 0 and at most 2");
        }
        if !(self.feather.is_finite() && (0.0..=0.2).contains(&self.feather)) {
            return bad("feather must be 0 to 0.2 of the radius");
        }
        if !(self.reach.is_finite() && (1.05..=3.0).contains(&self.reach)) {
            return bad("reach must be 1.05 to 3 radii");
        }
        if !(self.gate.is_finite() && (0.0..=20.0).contains(&self.gate)) {
            return bad("gate must be 0 to 20 sigma");
        }
        if !(self.offset.is_finite() && (-0.1..=0.2).contains(&self.offset)) {
            return bad("offset must be -0.1 to 0.2 of the radius");
        }
        for g in [self.disc_gamma, self.layer_gamma] {
            if !(g.is_finite() && (0.2..=5.0).contains(&g)) {
                return bad("gamma must be 0.2 to 5");
            }
        }
        Ok(())
    }

    /// One line saying what the edit does, for the file history.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "composite a stretched prominence layer outside the limb with the disc: \
             strength {:.2}, asinh stretch {:.1}, brightest at {:.0}% of the disc, \
             feather {:.1}% of the radius, reach {:.2} radii, gate {:.1} sigma, join offset {:+.1}% of the radius, \n             disc gamma {:.2}, layer gamma {:.2}",
            self.strength,
            self.stretch,
            self.level * 100.0,
            self.feather * 100.0,
            self.reach,
            self.gate,
            self.offset * 100.0,
            self.disc_gamma,
            self.layer_gamma
        )
    }

    /// Apply to `frame`: fit the limb, then composite. A frame with no clear
    /// disc is returned as it is.
    #[must_use]
    pub fn apply(&self, frame: &Frame) -> Frame {
        match mask::fit_disc(frame) {
            Some(disc) => self.composite(frame, disc),
            None => frame.clone(),
        }
    }

    /// Composite on a disc that is already known.
    #[must_use]
    #[allow(
        clippy::too_many_lines,
        clippy::similar_names,
        reason = "one pass from the frame to the composite, in the order the module docs give"
    )]
    pub fn composite(&self, frame: &Frame, disc: Disc) -> Frame {
        use rayon::prelude::*;
        let (w, h) = (frame.width, frame.height);
        let radius = disc.r;
        let reach_px = radius * self.reach;
        // The first radius that counts as outside: the pixels at the limb
        // itself are still disc, and the edge is blurred by the seeing.
        let feather_px = (self.feather * radius).max(0.5);
        let edge = radius + (1.5 * feather_px).max(2.5);
        let dist = |x: usize, y: usize| (x as f32 - disc.cx).hypot(y as f32 - disc.cy);

        // 1. The background: the median of the ring at each radius, in a
        // window of three angular sectors around each angle, so a glow that is
        // brighter on one side (a hazy sky, light from a bright limb) is
        // followed and a prominence, which fills a small part of the window,
        // is not.
        let rings = (((reach_px - edge) / RING).ceil() as usize + 2).max(4);
        let sector_of = |x: usize, y: usize| -> usize {
            let a = (y as f32 - disc.cy).atan2(x as f32 - disc.cx);
            let u = (a + std::f32::consts::PI) / std::f32::consts::TAU * SECTORS as f32;
            (u as usize).min(SECTORS - 1)
        };
        let mut cells: Vec<Vec<f32>> = vec![Vec::new(); rings * SECTORS];
        let mut whole: Vec<Vec<f32>> = vec![Vec::new(); rings];
        for y in 0..h {
            for (x, &v) in frame.row(y).iter().enumerate() {
                let r = dist(x, y);
                if r >= edge && v.is_finite() {
                    let k = ((r - edge) / RING) as usize;
                    if k < rings {
                        cells[k * SECTORS + sector_of(x, y)].push(v);
                        whole[k].push(v);
                    }
                }
            }
        }
        let median = |mut v: Vec<f32>| -> Option<f32> {
            (v.len() >= MIN_SAMPLES).then(|| {
                let k = v.len() / 2;
                *v.select_nth_unstable_by(k, f32::total_cmp).1
            })
        };
        let all: Vec<Option<f32>> = whole.into_iter().map(median).collect();
        let Some(profile) = fill_profile(&all) else {
            return frame.clone();
        };
        let mut table = vec![0.0f32; rings * SECTORS];
        for k in 0..rings {
            for sec in 0..SECTORS {
                let mut window = Vec::new();
                for d in [SECTORS - 1, 0, 1] {
                    window.extend_from_slice(&cells[k * SECTORS + (sec + d) % SECTORS]);
                }
                table[k * SECTORS + sec] = median(window).unwrap_or(profile[k]);
            }
        }
        let background = |x: usize, y: usize, r: f32| -> f32 {
            let t = ((r - edge) / RING - 0.5).max(0.0);
            let i = (t as usize).min(rings - 1);
            let f = (t - i as f32).min(1.0);
            let i1 = (i + 1).min(rings - 1);
            let a = (y as f32 - disc.cy).atan2(x as f32 - disc.cx);
            let u = (a + std::f32::consts::PI) / std::f32::consts::TAU * SECTORS as f32 - 0.5;
            let s0 = u.floor();
            let g = u - s0;
            let s0 = (s0 as isize).rem_euclid(SECTORS as isize) as usize;
            let s1 = (s0 + 1) % SECTORS;
            let at = |k: usize, sec: usize| table[k * SECTORS + sec];
            let lo = at(i, s0) * (1.0 - g) + at(i, s1) * g;
            let hi = at(i1, s0) * (1.0 - g) + at(i1, s1) * g;
            lo * (1.0 - f) + hi * f
        };
        // The black level: the median beyond the reach when the frame has
        // enough there, else the last ring.
        let mut beyond: Vec<f32> = (0..w * h)
            .filter(|&i| dist(i % w, i / w) > reach_px)
            .map(|i| frame.data[i])
            .filter(|v| v.is_finite())
            .collect();
        let sky = if beyond.len() >= 2000 {
            let k = beyond.len() / 2;
            *beyond.select_nth_unstable_by(k, f32::total_cmp).1
        } else {
            profile[profile.len() - 1]
        };

        // 2. The signal above the background, lightly blurred. Inside the
        // edge the disc is not signal: each pixel there takes the value of the
        // same ray at the edge, so the blur does not carry the disc outwards.
        let ray_to_edge = |x: usize, y: usize, r: f32| -> usize {
            let k = edge / r.max(1e-3);
            let sx = (disc.cx + (x as f32 - disc.cx) * k)
                .round()
                .clamp(0.0, (w - 1) as f32);
            let sy = (disc.cy + (y as f32 - disc.cy) * k)
                .round()
                .clamp(0.0, (h - 1) as f32);
            sy as usize * w + sx as usize
        };
        let mut excess: Vec<f32> = (0..w * h)
            .into_par_iter()
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let r = dist(x, y);
                if r >= edge {
                    frame.data[i] - background(x, y, r)
                } else {
                    0.0
                }
            })
            .collect();
        for i in 0..w * h {
            let (x, y) = (i % w, i / w);
            let r = dist(x, y);
            if r < edge {
                excess[i] = excess[ray_to_edge(x, y, r)];
            }
        }
        let taps = gaussian_taps(SMOOTH_SIGMA);
        let blurred = conv::separable(&excess, w, h, &taps, 1);
        // Noise: median absolute deviation in the outer part of the field.
        let outer_from = radius * (1.0 + 0.6 * (self.reach - 1.0));
        let mut dev: Vec<f32> = (0..w * h)
            .filter(|&i| {
                let r = dist(i % w, i / w);
                r >= outer_from && r <= reach_px
            })
            .map(|i| blurred[i])
            .collect();
        let sigma = mad_sigma(&mut dev);
        let disc_level = disc_level(frame, disc, sky);
        let gate = (self.gate * sigma).max(MIN_GATE_SHARE * disc_level);
        let signal: Vec<f32> = blurred.iter().map(|&e| (e - gate).max(0.0)).collect();

        // 3. The reference: the brightest prominences just above the limb.
        let mut near: Vec<f32> = (0..w * h)
            .filter(|&i| {
                let r = dist(i % w, i / w);
                r >= edge && r <= radius * 1.1
            })
            .map(|i| signal[i])
            .collect();
        let p99 = if near.is_empty() {
            0.0
        } else {
            let k = ((near.len() - 1) as f32 * 0.995) as usize;
            *near.select_nth_unstable_by(k, f32::total_cmp).1
        };
        let reference = p99
            .max(MIN_REFERENCE_SIGMA * sigma)
            .max(2.0 * MIN_GATE_SHARE * disc_level)
            .max(1e-9);
        let k = self.stretch;
        let norm = if k > 0.0 { k.asinh() } else { 1.0 };
        let lift = |s: f32| -> f32 {
            let x = (s / reference * self.strength).max(0.0);
            let y = if k > 0.0 { (k * x).asinh() / norm } else { x };
            y.min(1.0)
        };

        // 4. The join.
        let feather = feather_px;
        let join = radius * (1.0 + self.offset);
        let mut out = frame.clone();
        out.bayer = None;
        out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, v) in row.iter_mut().enumerate() {
                let r = dist(x, y);
                let inside = 1.0 - smoothstep(join - feather, join + feather, r);
                let toned = if (self.disc_gamma - 1.0).abs() > 1e-6 && r < join + feather {
                    sky + disc_level * ((*v - sky) / disc_level).max(0.0).powf(self.disc_gamma)
                } else {
                    *v
                };
                if inside >= 1.0 {
                    *v = toned;
                    continue;
                }
                let faded = 1.0 - smoothstep(reach_px * 0.9, reach_px, r);
                let s = signal[y * w + x];
                let layer = sky + self.level * disc_level * lift(s).powf(self.layer_gamma) * faded;
                *v = inside * toned + (1.0 - inside) * layer;
            }
        });
        out
    }
}

/// Brightness of the quiet disc above the sky: the median of the disc between
/// 0.3 and 0.9 of the radius.
fn disc_level(frame: &Frame, disc: Disc, sky: f32) -> f32 {
    let (lo, hi) = (0.3 * disc.r, 0.9 * disc.r);
    let mut v: Vec<f32> = Vec::new();
    for y in 0..frame.height {
        let dy = y as f32 - disc.cy;
        for (x, &s) in frame.row(y).iter().enumerate() {
            let r = (x as f32 - disc.cx).hypot(dy);
            if r >= lo && r <= hi && s.is_finite() {
                v.push(s);
            }
        }
    }
    if v.is_empty() {
        return 1.0;
    }
    let k = v.len() / 2;
    (*v.select_nth_unstable_by(k, f32::total_cmp).1 - sky).max(1e-6)
}

/// Fill the gaps of a ring profile by the nearest neighbours; `None` when
/// there is no ring at all.
fn fill_profile(m: &[Option<f32>]) -> Option<Vec<f32>> {
    let first = m.iter().position(Option::is_some)?;
    let mut out = Vec::with_capacity(m.len());
    let mut last = m[first]?;
    for v in m {
        if let Some(v) = v {
            last = *v;
        }
        out.push(last);
    }
    Some(out)
}

/// Robust standard deviation: 1.4826 times the median absolute deviation.
fn mad_sigma(v: &mut [f32]) -> f32 {
    if v.len() < MIN_SAMPLES {
        return 0.0;
    }
    let mid = v.len() / 2;
    let med = *v.select_nth_unstable_by(mid, f32::total_cmp).1;
    for x in v.iter_mut() {
        *x = (*x - med).abs();
    }
    1.4826 * *v.select_nth_unstable_by(mid, f32::total_cmp).1
}

/// Gaussian taps to three sigma.
fn gaussian_taps(sigma: f32) -> Vec<f32> {
    let reach = (3.0 * sigma).ceil().max(1.0) as isize;
    let mut taps: Vec<f32> = (-reach..=reach)
        .map(|i| (-((i * i) as f32) / (2.0 * sigma * sigma)).exp())
        .collect();
    let sum: f32 = taps.iter().sum();
    for t in &mut taps {
        *t /= sum;
    }
    taps
}

/// 0 below `a`, 1 above `b`, a smooth ramp between.
fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    if b <= a {
        return f32::from(x >= b);
    }
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use super::*;
    use crate::testutil::Rng;

    const N: usize = 360;
    const R: f32 = 110.0;
    const SKY: f32 = 0.02;

    /// A limb-darkened disc, a glow that falls off outside the limb, noise,
    /// and prominences of `amp` above the glow at the given angles (degrees,
    /// from the top, clockwise) and heights (radii above the limb).
    fn scene(amp: f32, proms: &[(f32, f32)], noise: f32, seed: u64) -> Frame {
        let c = N as f32 / 2.0;
        let mut rng = Rng::new(seed);
        let mut data = vec![0.0; N * N];
        for y in 0..N {
            for x in 0..N {
                let (dx, dy) = (x as f32 - c, y as f32 - c);
                let r = dx.hypot(dy);
                let mut v = if r < R {
                    let mu = (1.0 - (r / R).powi(2)).sqrt();
                    0.8 * (1.0 - 0.5 * (1.0 - mu))
                } else {
                    SKY + 0.05 * (-(r - R) / 20.0).exp()
                };
                let ang = dx.atan2(-dy).to_degrees();
                for &(a, height) in proms {
                    let da = (ang - a + 540.0).rem_euclid(360.0) - 180.0;
                    let rh = (r - R) / R;
                    if r >= R {
                        v += amp * (-(da / 4.0).powi(2)).exp() * (-(rh / height).powi(2)).exp();
                    }
                }
                data[y * N + x] = v + noise * rng.gauss();
            }
        }
        Frame::new(N, N, data).unwrap()
    }

    fn at(f: &Frame, r: f32, ang: f32) -> f32 {
        let c = N as f32 / 2.0;
        let (s, co) = ang.to_radians().sin_cos();
        f.get((c + r * s).round() as usize, (c - r * co).round() as usize)
    }

    #[test]
    fn the_disc_is_left_alone_and_the_prominences_are_lifted() {
        let proms = [(40.0, 0.06), (140.0, 0.1), (250.0, 0.04)];
        let img = scene(0.04, &proms, 0.003, 5);
        let out = Prominence::default().apply(&img);
        let c = N as f32 / 2.0;
        // Well inside the limb: identical.
        for y in 0..N {
            for x in 0..N {
                let r = (x as f32 - c).hypot(y as f32 - c);
                if r < R - 6.0 {
                    assert!((out.get(x, y) - img.get(x, y)).abs() < 1e-6);
                }
            }
        }
        // The brightest prominence, as a share of the disc, before and after.
        let disc = 0.8 * 0.9;
        let before = (at(&img, R + 4.0, 140.0) - SKY) / disc;
        let after = (at(&out, R + 4.0, 140.0) - SKY) / disc;
        assert!(before < 0.15, "{before}");
        assert!(after > 3.0 * before && after > 0.4, "{before} -> {after}");
        // A faint one is lifted too, and the order of brightness is kept.
        let faint_after = at(&out, R + 4.0, 250.0) - SKY;
        let bright_after = at(&out, R + 4.0, 140.0) - SKY;
        assert!(faint_after > 0.05 * disc, "{faint_after}");
        assert!(bright_after > faint_after);
    }

    #[test]
    fn the_glow_is_taken_off_and_the_sky_is_black() {
        let img = scene(0.04, &[(140.0, 0.1)], 0.003, 6);
        let out = Prominence::default().apply(&img);
        // Just above the limb where there is no prominence, the glow was 0.05.
        assert!(at(&img, R + 3.0, 300.0) > SKY + 0.03);
        let glow_after = at(&out, R + 3.0, 300.0) - SKY;
        assert!(glow_after.abs() < 0.03, "{glow_after}");
        // Far out, the black level.
        let mut far = Vec::new();
        let c = N as f32 / 2.0;
        for y in 0..N {
            for x in 0..N {
                if (x as f32 - c).hypot(y as f32 - c) > R * 1.6 {
                    far.push(out.get(x, y));
                }
            }
        }
        let mean = far.iter().sum::<f32>() / far.len() as f32;
        // The black level is the glow's last ring, a little above the true sky.
        assert!((mean - SKY).abs() < 0.008, "{mean}");
    }

    #[test]
    fn a_field_with_no_prominence_is_not_stretched_into_noise() {
        let img = scene(0.0, &[], 0.004, 7);
        let out = Prominence::default().apply(&img);
        let disc = 0.8 * 0.9;
        let c = N as f32 / 2.0;
        let (mut n, mut bright) = (0usize, 0usize);
        for y in 0..N {
            for x in 0..N {
                let r = (x as f32 - c).hypot(y as f32 - c);
                if r > R + 4.0 {
                    n += 1;
                    if out.get(x, y) - SKY > 0.05 * disc {
                        bright += 1;
                    }
                }
            }
        }
        assert!(bright * 200 < n, "{bright} of {n} pixels lifted");
    }

    #[test]
    fn the_join_has_no_rim_and_no_overshoot_across_the_limb() {
        let img = scene(0.04, &[(140.0, 0.1)], 0.002, 8);
        let out = Prominence::default().apply(&img);
        // Along a ray with no prominence the picture falls from the disc to
        // the sky and never rises or overshoots on the way.
        let mut prev = at(&out, R - 10.0, 300.0);
        for k in -9..14 {
            let v = at(&out, R + k as f32, 300.0);
            assert!(v <= prev + 0.02, "rises by {} at {k}", v - prev);
            assert!(v >= SKY - 0.01, "below the sky at {k}");
            prev = v;
        }
    }

    #[test]
    fn the_tone_and_the_join_can_be_moved() {
        let img = scene(0.04, &[(140.0, 0.1)], 0.002, 9);
        let base = Prominence::default().apply(&img);
        // A layer gamma above 1 darkens the faint parts of the layer.
        let dark = Prominence {
            layer_gamma: 2.0,
            ..Prominence::default()
        }
        .apply(&img);
        assert!(at(&dark, R + 14.0, 140.0) < at(&base, R + 14.0, 140.0));
        // A disc gamma above 1 darkens the disc's mid tones and leaves the sky.
        let toned = Prominence {
            disc_gamma: 2.0,
            ..Prominence::default()
        }
        .apply(&img);
        assert!(at(&toned, R * 0.95, 20.0) < at(&base, R * 0.95, 20.0) - 0.05);
        assert!((at(&toned, R * 1.3, 300.0) - at(&base, R * 1.3, 300.0)).abs() < 1e-6);
        // Moving the join out keeps more of the disc's own edge.
        let out = Prominence {
            offset: 0.04,
            ..Prominence::default()
        }
        .apply(&img);
        // Out to the new join the picture is still the original, glow included.
        assert!(at(&out, R + 2.0, 300.0) > at(&base, R + 2.0, 300.0) + 0.02);
    }

    #[test]
    fn a_frame_without_a_disc_passes_unchanged() {
        let flat = Frame::new(64, 64, vec![0.3; 64 * 64]).unwrap();
        assert_eq!(Prominence::default().apply(&flat), flat);
    }

    #[test]
    fn values_are_validated() {
        assert!(Prominence::default().validate().is_ok());
        for p in [
            Prominence {
                strength: -1.0,
                ..Prominence::default()
            },
            Prominence {
                level: 0.0,
                ..Prominence::default()
            },
            Prominence {
                reach: 1.0,
                ..Prominence::default()
            },
            Prominence {
                feather: 0.5,
                ..Prominence::default()
            },
            Prominence {
                gate: f32::NAN,
                ..Prominence::default()
            },
        ] {
            assert!(p.validate().is_err(), "{p:?}");
        }
    }
}
