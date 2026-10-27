// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Richardson-Lucy deconvolution with total-variation regularisation and a
//! limb-aware ringing guard.
//!
//! Richardson-Lucy (RL) undoes a known blur, the point spread function (PSF)
//! `h`, by iterating
//!
//! ```text
//! u(k+1) = u(k) / (1 - lambda * div(grad u / |grad u|)) * ( h^T * ( f / (h * u(k)) ) )
//! ```
//!
//! from `u(0) = f`, the observed image. The bracket is plain RL; the divisor
//! is the total-variation (TV) term of Dey et al. (2006), which keeps noise
//! from being amplified into speckle. `lambda` is [`Deconvolve::tv`]. The
//! PSFs here are symmetric, so `h^T` is `h`.
//!
//! # Ringing
//!
//! Deconvolving a sharp, bright edge such as the solar limb leaves a bright
//! halo inside it and a dark one outside it; sunspots do the same on a smaller
//! scale. The guard measures, for every pixel, how large a step lies within
//! reach of the PSF (the contrast of the blurred image over a window of about
//! five PSF widths, as a share of the image's range) and turns that into a
//! weight between 0 (granulation, filaments: free) and 1 (limb, umbra: guarded).
//! Where the weight is high the result is clamped to the range of the
//! *observed* image in a window around that pixel. A true sharpening of a
//! step has values on the two plateaus, which are inside that range, so the
//! edge still steepens, but a halo beyond a plateau cannot form.
//!
//! # References
//!
//! - Richardson, W. H. (1972), Bayesian-based iterative method of image restoration, J. Opt. Soc. Am. 62,
//!   55-59, doi:10.1364/JOSA.62.000055.
//! - Lucy, L. B. (1974), An iterative technique for the rectification of observed distributions, Astron. J.
//!   79, 745, doi:10.1086/111605.
//! - Dey, N. et al. (2006), Richardson-Lucy algorithm with total variation regularization for 3D confocal
//!   microscope deconvolution, Microsc. Res. Tech. 69, 260-266, doi:10.1002/jemt.20294.

#![allow(
    clippy::cast_possible_wrap,
    reason = "pixel coordinates are far below isize::MAX and the TV stencil needs signed offsets"
)]

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use super::conv;
use crate::error::{Error, Result};
use crate::filter::{gaussian_blur, gaussian_taps};
use crate::frame::Frame;
use crate::mask::fit_disc;

/// Apparent radius of the Sun in arc seconds, for the image scale.
pub const SOLAR_RADIUS_ARCSEC: f32 = 960.0;
/// Typical full width at half maximum of a seeing-limited stack, arc seconds.
pub const TYPICAL_FWHM_ARCSEC: f32 = 2.0;
/// Division guard of the RL ratio.
pub const RATIO_EPS: f32 = 1e-4;
/// Squared gradient floor of the TV term.
pub const TV_EPS2: f32 = 1e-6;
/// Smallest the TV divisor may be.
pub const TV_FLOOR: f32 = 0.5;
/// Largest Airy first-null radius, pixels.
pub const MAX_AIRY_RADIUS: f32 = 6.0;
/// Most iterations.
pub const MAX_ITERATIONS: u32 = 200;
/// Zero of the Bessel function J1 that bounds the Airy disc.
const J1_FIRST_ZERO: f64 = 3.831_705_97;

/// The blur to undo.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Psf {
    /// Chosen from the image scale when the edit runs (see [`auto_sigma`]) and
    /// recorded as a Gaussian.
    #[default]
    Auto,
    /// Measured from the solar limb when the edit runs (see
    /// [`super::limbpsf`]) and recorded as a Gaussian; the same as
    /// [`Psf::Auto`] when the picture has no clear limb.
    Limb,
    /// A Gaussian of this sigma in pixels: seeing and a sloppy focus.
    Gaussian {
        /// Standard deviation, pixels.
        sigma: f32,
    },
    /// An Airy disc: the diffraction pattern of a circular aperture.
    Airy {
        /// Radius of the first dark ring, pixels (`1.22 * wavelength / aperture`
        /// divided by the pixel scale; see [`airy_radius_px`]).
        radius: f32,
    },
}

impl Psf {
    /// One phrase for the file history.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Auto => "auto".into(),
            Self::Limb => "measured from the limb".into(),
            Self::Gaussian { sigma } => format!("Gaussian sigma {sigma:.2} px"),
            Self::Airy { radius } => format!("Airy first null {radius:.2} px"),
        }
    }
}

/// First dark ring of an Airy pattern in pixels.
///
/// `wavelength_nm` and `aperture_mm` describe the telescope, `arcsec_per_px`
/// the sampling: `1.22 * lambda / D` in arc seconds, over the pixel scale.
#[must_use]
pub fn airy_radius_px(wavelength_nm: f32, aperture_mm: f32, arcsec_per_px: f32) -> f32 {
    let rad = 1.22 * f64::from(wavelength_nm) * 1e-9 / (f64::from(aperture_mm) * 1e-3);
    let arcsec = rad.to_degrees() * 3600.0;
    (arcsec / f64::from(arcsec_per_px)) as f32
}

/// Richardson-Lucy deconvolution with total-variation regularisation.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Deconvolve {
    /// The blur to undo.
    pub psf: Psf,
    /// RL iterations. More is sharper and noisier.
    pub iterations: u32,
    /// Total-variation weight `lambda`; 0 is plain RL.
    pub tv: f32,
    /// Strength of the limb and sunspot ringing guard, 0 (off) to 1 (full).
    pub protect: f32,
}

impl Default for Deconvolve {
    fn default() -> Self {
        Self {
            psf: Psf::Limb,
            iterations: 25,
            tv: 0.0,
            protect: 1.0,
        }
    }
}

impl Deconvolve {
    /// Check the values.
    ///
    /// # Errors
    /// [`Error::Invalid`] for a PSF outside 0.3..=6 pixels, no or too many
    /// iterations, a negative or too large TV weight, or a guard outside 0..=1.
    pub fn validate(&self) -> Result<()> {
        let psf_ok = match self.psf {
            Psf::Auto | Psf::Limb => true,
            Psf::Gaussian { sigma } => (0.3..=8.0).contains(&sigma),
            Psf::Airy { radius } => (0.5..=MAX_AIRY_RADIUS).contains(&radius),
        };
        if !psf_ok {
            return Err(Error::Invalid(format!(
                "deconvolution PSF out of range (Gaussian sigma 0.3 to 8 px, Airy radius 0.5 to {MAX_AIRY_RADIUS} px)"
            )));
        }
        if self.iterations == 0
            || self.iterations > MAX_ITERATIONS
            || !self.tv.is_finite()
            || !(0.0..=0.05).contains(&self.tv)
            || !(0.0..=1.0).contains(&self.protect)
        {
            return Err(Error::Invalid(format!(
                "deconvolution needs 1 to {MAX_ITERATIONS} iterations, a TV weight of 0 to 0.05 and a guard of 0 to 1"
            )));
        }
        Ok(())
    }

    /// One line for the file history.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "Richardson-Lucy deconvolution, PSF {}, {} iterations, TV {:.4}, limb guard {:.2}",
            self.psf.describe(),
            self.iterations,
            self.tv,
            self.protect
        )
    }
}

/// PSF sigma (pixels) for a disc of `radius_px`, assuming a seeing-limited
/// stack of [`TYPICAL_FWHM_ARCSEC`]. Never below 0.7 px: a smaller PSF is
/// below the sampling and there is nothing to recover.
#[must_use]
pub fn sigma_for_disc_radius(radius_px: f32) -> f32 {
    let arcsec_per_px = SOLAR_RADIUS_ARCSEC / radius_px.max(1.0);
    let fwhm_px = TYPICAL_FWHM_ARCSEC / arcsec_per_px;
    (fwhm_px / 2.3548).clamp(0.7, 4.0)
}

/// PSF sigma guessed from the frame size alone, assuming the disc fills about
/// three quarters of the short side.
#[must_use]
pub fn heuristic_sigma(width: usize, height: usize) -> f32 {
    sigma_for_disc_radius(0.375 * width.min(height) as f32)
}

/// PSF sigma for `frame`: from the fitted disc when there is one, else from
/// the frame size.
#[must_use]
pub fn auto_sigma(frame: &Frame) -> f32 {
    fit_disc(frame).map_or_else(
        || heuristic_sigma(frame.width, frame.height),
        |d| sigma_for_disc_radius(d.r),
    )
}

/// Bessel function of the first kind, order one (Abramowitz and Stegun
/// rational approximations, error below 1e-7).
fn bessel_j1(x: f64) -> f64 {
    let ax = x.abs();
    if ax < 8.0 {
        let y = x * x;
        let n = x
            * (72_362_614_232.0
                + y * (-7_895_059_235.0
                    + y * (242_396_853.1
                        + y * (-2_972_611.439 + y * (15_704.482_60 + y * -30.160_366_06)))));
        let d = 144_725_228_442.0
            + y * (2_300_535_178.0
                + y * (18_583_304.74 + y * (99_447.433_94 + y * (376.999_139_7 + y))));
        n / d
    } else {
        let z = 8.0 / ax;
        let y = z * z;
        let xx = ax - 2.356_194_491;
        let a = 1.0
            + y * (0.183_105e-2
                + y * (-0.351_639_649_6e-4 + y * (0.245_752_017_4e-5 + y * -0.240_337_019e-6)));
        let b = 0.046_874_999_95
            + y * (-0.200_269_087_3e-3
                + y * (0.844_919_909_6e-5 + y * (-0.882_289_87e-6 + y * 0.105_787_412e-6)));
        let ans = (std::f64::consts::FRAC_2_PI / ax).sqrt() * (xx.cos() * a - z * xx.sin() * b);
        if x < 0.0 { -ans } else { ans }
    }
}

/// Airy intensity `(2 J1(x) / x)^2` at distance `r` for a first null at `null`.
fn airy_intensity(r: f64, null: f64) -> f64 {
    if r < 1e-9 {
        return 1.0;
    }
    let x = J1_FIRST_ZERO * r / null;
    let v = 2.0 * bessel_j1(x) / x;
    v * v
}

/// A PSF as a convolution kernel.
#[derive(Clone, Debug, PartialEq)]
pub struct PsfKernel {
    /// Half-width of the square support.
    pub radius: usize,
    /// One-dimensional taps when the PSF is separable (Gaussian).
    pub separable: Option<Vec<f32>>,
    /// The `(2 * radius + 1)^2` kernel, row-major, summing to one.
    pub full: Vec<f32>,
}

impl PsfKernel {
    /// Kernel of a concrete PSF.
    ///
    /// # Errors
    /// [`Error::Invalid`] for [`Psf::Auto`], which must be resolved first.
    pub fn new(psf: Psf) -> Result<Self> {
        match psf {
            Psf::Auto | Psf::Limb => Err(Error::Invalid("the PSF must be resolved first".into())),
            Psf::Gaussian { sigma } => {
                let taps = gaussian_taps(sigma);
                let radius = taps.len() / 2;
                let side = taps.len();
                let mut full = vec![0.0f32; side * side];
                for j in 0..side {
                    for i in 0..side {
                        full[j * side + i] = taps[j] * taps[i];
                    }
                }
                Ok(Self {
                    radius,
                    separable: Some(taps),
                    full,
                })
            }
            Psf::Airy { radius: null } => {
                let radius = (3.5 * null).ceil().max(2.0) as usize;
                let side = 2 * radius + 1;
                let mut full = vec![0.0f32; side * side];
                let mut sum = 0.0f64;
                for j in 0..side {
                    for i in 0..side {
                        // 3 x 3 samples per pixel, so a small disc is integrated, not point-sampled.
                        let mut acc = 0.0f64;
                        for sj in 0..3 {
                            for si in 0..3 {
                                let dx = i as f64 - radius as f64 + (f64::from(si) - 1.0) / 3.0;
                                let dy = j as f64 - radius as f64 + (f64::from(sj) - 1.0) / 3.0;
                                acc += airy_intensity(dx.hypot(dy), f64::from(null));
                            }
                        }
                        full[j * side + i] = (acc / 9.0) as f32;
                        sum += acc / 9.0;
                    }
                }
                for v in &mut full {
                    *v = (f64::from(*v) / sum) as f32;
                }
                Ok(Self {
                    radius,
                    separable: None,
                    full,
                })
            }
        }
    }

    /// Convolve a `w` x `h` image with the kernel (clamped borders).
    #[must_use]
    pub fn apply(&self, src: &[f32], w: usize, h: usize) -> Vec<f32> {
        match &self.separable {
            Some(t) => conv::separable(src, w, h, t, 1),
            None => conv::direct(src, w, h, &self.full, self.radius),
        }
    }
}

/// Where the result must be held to the range of the observed image.
#[derive(Clone, Debug, PartialEq)]
pub struct Protection {
    /// Guard weight per pixel, 0 to 1.
    pub weight: Vec<f32>,
    /// Smallest observed value in a window around each pixel.
    pub lo: Vec<f32>,
    /// Largest observed value in a window around each pixel.
    pub hi: Vec<f32>,
}

/// Contrast (share of the image range) at which the guard starts.
const GUARD_LOW: f32 = 0.25;
/// Contrast at which the guard is complete.
const GUARD_HIGH: f32 = 0.55;

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Robust range of `data`: the 0.5 and 99.5 percentiles of a sparse sample.
fn robust_range(data: &[f32]) -> (f32, f32) {
    let mut v: Vec<f32> = data.iter().copied().step_by(5).collect();
    if v.is_empty() {
        return (0.0, 1.0);
    }
    v.sort_by(f32::total_cmp);
    let at = |p: f32| v[((v.len() - 1) as f32 * p) as usize];
    (at(0.005), at(0.995))
}

/// Compute the ringing guard for the observed image `obs`, for a PSF of
/// width `sigma` pixels and a guard strength in `0..=1`.
#[must_use]
pub fn protection(obs: &[f32], w: usize, h: usize, sigma: f32, strength: f32) -> Protection {
    let reach = (5.0 * sigma).ceil().max(2.0) as usize;
    let limit = (2.0 * sigma).ceil().max(1.0) as usize;
    let smooth = conv::separable(obs, w, h, &gaussian_taps((0.5 * sigma).max(0.7)), 1);
    let (lo_v, hi_v) = robust_range(&smooth);
    let range = (hi_v - lo_v).max(1e-6);
    let wide_max = conv::window_extreme(&smooth, w, h, reach, true);
    let wide_min = conv::window_extreme(&smooth, w, h, reach, false);
    let raw: Vec<f32> = wide_max
        .iter()
        .zip(&wide_min)
        .map(|(&a, &b)| smoothstep(GUARD_LOW, GUARD_HIGH, (a - b) / range) * strength)
        .collect();
    // Soften the weight so the clamp fades in rather than switching on.
    let weight = conv::separable(&raw, w, h, &gaussian_taps((0.6 * sigma).max(0.7)), 1);
    Protection {
        weight,
        lo: conv::window_extreme(obs, w, h, limit, false),
        hi: conv::window_extreme(obs, w, h, limit, true),
    }
}

impl Protection {
    /// Hold `est` to the observed range where the guard says so.
    pub fn apply(&self, est: &mut [f32]) {
        est.par_iter_mut()
            .zip(self.weight.par_iter().zip(self.lo.par_iter().zip(&self.hi)))
            .for_each(|(e, (&p, (&lo, &hi)))| {
                let c = e.clamp(lo, hi);
                *e += p * (c - *e);
            });
    }
}

/// Unit gradient of `u` at `(x, y)` with forward differences and clamped
/// borders: `grad / sqrt(|grad|^2 + eps)`.
fn unit_gradient(u: &[f32], w: usize, h: usize, x: isize, y: isize) -> (f32, f32) {
    let at = |xx: isize, yy: isize| {
        u[yy.clamp(0, h as isize - 1) as usize * w + xx.clamp(0, w as isize - 1) as usize]
    };
    let c = at(x, y);
    let (gx, gy) = (at(x + 1, y) - c, at(x, y + 1) - c);
    let n = (gx * gx + gy * gy + TV_EPS2).sqrt();
    (gx / n, gy / n)
}

/// Divergence of the unit gradient at pixel `(x, y)`.
fn tv_divergence(u: &[f32], w: usize, h: usize, x: usize, y: usize) -> f32 {
    let (xi, yi) = (x as isize, y as isize);
    let (nx, ny) = unit_gradient(u, w, h, xi, yi);
    let (px, _) = unit_gradient(u, w, h, xi - 1, yi);
    let (_, py) = unit_gradient(u, w, h, xi, yi - 1);
    (nx - px) + (ny - py)
}

/// Run `iterations` of RL-TV on `obs` (see the module documentation) and hold
/// the result to `guard`.
#[must_use]
pub fn richardson_lucy(
    obs: &[f32],
    w: usize,
    h: usize,
    kernel: &PsfKernel,
    params: &Deconvolve,
    guard: Option<&Protection>,
) -> Vec<f32> {
    let mut est = obs.to_vec();
    let tv = params.tv;
    for _ in 0..params.iterations {
        let blur = kernel.apply(&est, w, h);
        let ratio: Vec<f32> = obs
            .par_iter()
            .zip(&blur)
            .map(|(&o, &b)| o / (b + RATIO_EPS))
            .collect();
        let corr = kernel.apply(&ratio, w, h);
        let prev = est.clone();
        est.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, e) in row.iter_mut().enumerate() {
                let c = corr[y * w + x];
                let denom = if tv > 0.0 {
                    (1.0 - tv * tv_divergence(&prev, w, h, x, y)).max(TV_FLOOR)
                } else {
                    1.0
                };
                *e = (prev[y * w + x] * c / denom).max(0.0);
            }
        });
    }
    if let Some(g) = guard {
        g.apply(&mut est);
    }
    est
}

/// The observed image as deconvolution sees it: negative and non-finite
/// samples become zero.
#[must_use]
pub fn observed(frame: &Frame) -> Vec<f32> {
    frame
        .data
        .iter()
        .map(|&v| if v.is_finite() { v.max(0.0) } else { 0.0 })
        .collect()
}

/// Width in pixels the ringing guard works to for a concrete PSF.
///
/// # Errors
/// [`Error::Invalid`] for [`Psf::Auto`].
pub fn guard_sigma(psf: Psf) -> Result<f32> {
    match psf {
        Psf::Gaussian { sigma } => Ok(sigma),
        // The core of an Airy disc is about 0.42 of its first-null radius wide.
        Psf::Airy { radius } => Ok(0.42 * radius),
        Psf::Auto | Psf::Limb => Err(Error::Invalid("the PSF must be resolved first".into())),
    }
}

/// Deconvolve a frame (the CPU reference of the `deconvolve` edit). `params.psf`
/// must be concrete.
///
/// # Errors
/// [`Error::Invalid`] for an unresolved PSF or invalid parameters.
pub fn deconvolve(frame: &Frame, params: &Deconvolve) -> Result<Frame> {
    params.validate()?;
    let kernel = PsfKernel::new(params.psf)?;
    let (w, h) = (frame.width, frame.height);
    let obs = observed(frame);
    let sigma = guard_sigma(params.psf)?;
    let guard = (params.protect > 0.0).then(|| protection(&obs, w, h, sigma, params.protect));
    let data = richardson_lucy(&obs, w, h, &kernel, params, guard.as_ref());
    Ok(Frame {
        width: w,
        height: h,
        data,
        bayer: None,
    })
}

/// The Gaussian blur used to build test images and to report resolved PSFs.
#[must_use]
pub fn blur(frame: &Frame, psf: Psf) -> Option<Frame> {
    match psf {
        Psf::Gaussian { sigma } => Some(gaussian_blur(frame, sigma)),
        Psf::Airy { .. } => {
            let k = PsfKernel::new(psf).ok()?;
            Some(Frame {
                width: frame.width,
                height: frame.height,
                data: k.apply(&frame.data, frame.width, frame.height),
                bayer: None,
            })
        }
        Psf::Auto | Psf::Limb => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Rng, SyntheticConfig, SyntheticSun, rmse};

    #[test]
    fn bessel_j1_matches_known_values() {
        assert!((bessel_j1(1.0) - 0.440_050_6).abs() < 1e-6);
        assert!((bessel_j1(5.0) - (-0.327_579_1)).abs() < 1e-6);
        assert!((bessel_j1(10.0) - 0.043_472_75).abs() < 1e-6);
        assert!(bessel_j1(J1_FIRST_ZERO).abs() < 1e-6);
    }

    #[test]
    fn airy_kernel_is_normalised_symmetric_and_has_a_dark_ring() {
        let k = PsfKernel::new(Psf::Airy { radius: 3.0 }).unwrap_or_else(|e| panic!("{e}"));
        let s: f32 = k.full.iter().sum();
        assert!((s - 1.0).abs() < 1e-5);
        let side = 2 * k.radius + 1;
        let c = k.radius;
        assert!(k.full[c * side + c] > k.full[c * side + c + 1]);
        assert!((k.full[c * side + c + 2] - k.full[c * side + c - 2]).abs() < 1e-7);
        // Intensity at the first null is far below the peak.
        assert!(k.full[c * side + c + 3] < 0.02 * k.full[c * side + c]);
    }

    #[test]
    fn airy_radius_from_the_telescope() {
        // 656 nm through 100 mm: 1.22 * 656e-9 / 0.1 rad = 1.65 arcsec.
        let a = airy_radius_px(656.3, 100.0, 1.0);
        assert!((a - 1.65).abs() < 0.02, "{a}");
    }

    #[test]
    fn default_sigma_follows_the_image_scale() {
        let small = heuristic_sigma(640, 480);
        let large = heuristic_sigma(4000, 3000);
        assert!(small <= large);
        assert!((0.7..=4.0).contains(&small) && (0.7..=4.0).contains(&large));
    }

    #[test]
    fn auto_must_be_resolved_before_use() {
        let f = Frame::zeros(8, 8);
        assert!(deconvolve(&f, &Deconvolve::default()).is_err());
    }

    fn textured() -> (Frame, Frame) {
        let sun = SyntheticSun::new(&SyntheticConfig {
            width: 160,
            height: 160,
            ..SyntheticConfig::default()
        });
        let truth = sun.truth().clone();
        let mut blurred = gaussian_blur(&truth, 1.8);
        let mut rng = Rng::new(7);
        for v in &mut blurred.data {
            *v += 0.002 * rng.gauss();
        }
        (truth, blurred)
    }

    #[test]
    fn rl_recovers_sharpness_against_the_clean_truth() {
        let (truth, blurred) = textured();
        let before = rmse(&blurred, &truth, 12);
        let params = Deconvolve {
            psf: Psf::Gaussian { sigma: 1.8 },
            iterations: 25,
            tv: 0.002,
            protect: 1.0,
        };
        let out = deconvolve(&blurred, &params).unwrap_or_else(|e| panic!("{e}"));
        let after = rmse(&out, &truth, 12);
        eprintln!("gaussian RL: RMSE against the truth {before:.5} -> {after:.5}");
        assert!(after < 0.8 * before, "RMSE {before} -> {after}");
    }

    #[test]
    fn airy_deconvolution_also_improves_the_truth_match() {
        let (truth, _) = textured();
        let psf = Psf::Airy { radius: 2.2 };
        let mut blurred = blur(&truth, psf).unwrap_or_else(|| panic!("no blur"));
        let mut rng = Rng::new(9);
        for v in &mut blurred.data {
            *v += 0.001 * rng.gauss();
        }
        let before = rmse(&blurred, &truth, 16);
        let out = deconvolve(
            &blurred,
            &Deconvolve {
                psf,
                iterations: 20,
                ..Deconvolve::default()
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let after = rmse(&out, &truth, 16);
        eprintln!("airy RL: RMSE against the truth {before:.5} -> {after:.5}");
        assert!(after < 0.9 * before);
    }

    /// A flat disc on a dark sky: the cleanest possible limb.
    fn limb(blur_sigma: f32) -> (Frame, Frame, f32, f32) {
        let (w, h) = (128usize, 128usize);
        let (sky, disc) = (0.05f32, 0.85f32);
        let mut truth = Frame::zeros(w, h);
        for y in 0..h {
            for x in 0..w {
                let r = (x as f32 - 64.0).hypot(y as f32 - 64.0);
                truth.data[y * w + x] = if r < 40.0 { disc } else { sky };
            }
        }
        let mut obs = gaussian_blur(&truth, blur_sigma);
        let mut rng = Rng::new(11);
        for v in &mut obs.data {
            *v += 0.002 * rng.gauss();
        }
        (truth, obs, sky, disc)
    }

    /// Largest excursion beyond either plateau, as a share of the step, within
    /// 12 pixels of the limb.
    fn overshoot(img: &Frame, sky: f32, disc: f32) -> f32 {
        let mut worst = 0.0f32;
        for y in 0..img.height {
            for x in 0..img.width {
                let r = (x as f32 - 64.0).hypot(y as f32 - 64.0);
                if (r - 40.0).abs() <= 12.0 {
                    let v = img.get(x, y);
                    worst = worst.max((v - disc).max(sky - v) / (disc - sky));
                }
            }
        }
        worst
    }

    #[test]
    fn the_limb_guard_keeps_overshoot_below_two_percent_of_the_step() {
        let (_, obs, sky, disc) = limb(2.0);
        let base = Deconvolve {
            psf: Psf::Gaussian { sigma: 2.0 },
            iterations: 30,
            tv: 0.0,
            protect: 0.0,
        };
        let free = deconvolve(&obs, &base).unwrap_or_else(|e| panic!("{e}"));
        let guarded = deconvolve(
            &obs,
            &Deconvolve {
                protect: 1.0,
                ..base
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let (free_os, guarded_os) = (overshoot(&free, sky, disc), overshoot(&guarded, sky, disc));
        eprintln!("limb overshoot: unguarded {free_os:.4}, guarded {guarded_os:.4}");
        assert!(free_os > 0.04, "unguarded RL should ring, got {free_os}");
        assert!(
            guarded_os < 0.02,
            "guarded overshoot {guarded_os} (unguarded {free_os})"
        );
    }

    /// A dark umbra with a penumbra on a bright photosphere, well inside the disc.
    #[test]
    fn the_guard_also_holds_a_sunspot_edge() {
        let (w, h) = (128usize, 128usize);
        let (photo, umbra) = (0.85f32, 0.30f32);
        let mut truth = Frame::zeros(w, h);
        for y in 0..h {
            for x in 0..w {
                let r = (x as f32 - 64.0).hypot(y as f32 - 64.0);
                truth.data[y * w + x] = if r < 14.0 { umbra } else { photo };
            }
        }
        let mut obs = gaussian_blur(&truth, 2.0);
        // A bright limb elsewhere sets the range, as in a real frame.
        for y in 0..h {
            for x in 0..w {
                if x > 116 {
                    obs.data[y * w + x] = 0.05;
                }
            }
        }
        let mut rng = Rng::new(5);
        for v in &mut obs.data {
            *v += 0.002 * rng.gauss();
        }
        let base = Deconvolve {
            psf: Psf::Gaussian { sigma: 2.0 },
            iterations: 30,
            tv: 0.0,
            protect: 0.0,
        };
        let spot = |img: &Frame| {
            let mut worst = 0.0f32;
            for y in 30..100 {
                for x in 30..100 {
                    let r = (x as f32 - 64.0).hypot(y as f32 - 64.0);
                    if (r - 14.0).abs() <= 10.0 {
                        let v = img.get(x, y);
                        worst = worst.max((v - photo).max(umbra - v) / (photo - umbra));
                    }
                }
            }
            worst
        };
        let free = spot(&deconvolve(&obs, &base).unwrap_or_else(|e| panic!("{e}")));
        let held = spot(
            &deconvolve(
                &obs,
                &Deconvolve {
                    protect: 1.0,
                    ..base
                },
            )
            .unwrap_or_else(|e| panic!("{e}")),
        );
        eprintln!("sunspot overshoot: unguarded {free:.4}, guarded {held:.4}");
        assert!(free > 0.03, "unguarded {free}");
        assert!(held < 0.02, "guarded {held} (unguarded {free})");
    }

    #[test]
    fn the_guard_still_sharpens_the_limb() {
        let (truth, obs, ..) = limb(2.0);
        let out = deconvolve(
            &obs,
            &Deconvolve {
                psf: Psf::Gaussian { sigma: 2.0 },
                iterations: 30,
                tv: 0.002,
                protect: 1.0,
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
        // The 10-90% edge width falls.
        let width = |img: &Frame| {
            let row = img.row(64);
            let (lo, hi) = (0.05 + 0.1 * 0.8, 0.05 + 0.9 * 0.8);
            let a = row[64..].iter().position(|&v| v < hi).unwrap_or(0) as f32;
            let b = row[64..].iter().position(|&v| v < lo).unwrap_or(0) as f32;
            b - a
        };
        assert!(
            width(&out) < width(&obs) - 1.0,
            "{} vs {}",
            width(&out),
            width(&obs)
        );
        assert!(rmse(&out, &truth, 4) < rmse(&obs, &truth, 4));
    }

    #[test]
    fn flat_images_stay_flat() {
        let mut f = Frame::zeros(40, 40);
        f.data.fill(0.4);
        let out = deconvolve(
            &f,
            &Deconvolve {
                psf: Psf::Gaussian { sigma: 1.5 },
                ..Deconvolve::default()
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(out.data.iter().all(|&v| (v - 0.4).abs() < 1e-3));
    }
}
