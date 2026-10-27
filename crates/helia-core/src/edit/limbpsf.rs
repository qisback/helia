// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! A point spread function measured from the solar limb.
//!
//! The limb is the sharpest edge in a full-disc picture: a step from the
//! disc to the dark sky. What the picture makes of that step, the edge spread
//! function, is the step blurred by the telescope, the seeing and the stack.
//! The model here is
//!
//! ```text
//! p(r) = a + (b + c (R - r)) * erfc((r - R) / (sigma sqrt 2)) / 2
//! ```
//!
//! sky `a` outside, the disc level `b` with a slope `c` (limb darkening) inside,
//! the edge at `R`, blurred by a Gaussian of standard deviation `sigma`. The
//! profile is the median over all angles of the picture sampled along the
//! radius, so a prominence or a sunspot at the limb does not move it. `sigma`
//! is found by a search over `sigma` and `R`, solving `a`, `b` and `c` by least
//! squares at each step, and the width of one pixel (a box of standard
//! deviation `1/sqrt 12`) is taken out in quadrature.
//!
//! It works for a limb that is a sharp edge: white light, the calcium lines.
//! In H-alpha the limb is a fringe of spicules and the width comes out larger
//! than the telescope's; the number is still a sensible size for the blur to
//! undo, but check it against the picture.

use crate::frame::Frame;
use crate::mask::{Disc, fit_disc};

/// Standard deviation of a one-pixel box, in pixels.
const PIXEL_SIGMA: f32 = 0.288_675_13;
/// The profile spans this many pixels either side of the fitted edge.
const REACH: f32 = 12.0;
/// Radial step of the profile, pixels.
const STEP: f32 = 0.25;
/// Angles sampled.
const ANGLES: usize = 360;

/// A measured limb blur.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LimbPsf {
    /// Gaussian sigma of the blur, pixels, with the pixel width taken out.
    pub sigma: f32,
    /// Radius of the edge, pixels.
    pub radius: f32,
    /// RMS of the fit as a share of the step from sky to disc: small for a
    /// clean edge, large when the limb is not the model's step.
    pub misfit: f32,
}

/// Complementary error function (Abramowitz and Stegun 7.1.26, error under
/// 1.5e-7).
fn erfc(x: f32) -> f32 {
    let z = x.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * z);
    let poly = t
        * (0.254_829_6
            + t * (-0.284_496_72 + t * (1.421_413_8 + t * (-1.453_152_1 + t * 1.061_405_4))));
    let e = poly * (-z * z).exp();
    if x >= 0.0 { e } else { 2.0 - e }
}

fn bilinear(f: &Frame, x: f32, y: f32) -> Option<f32> {
    if x < 0.0 || y < 0.0 || x > (f.width - 1) as f32 || y > (f.height - 1) as f32 {
        return None;
    }
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(f.width - 1), (y0 + 1).min(f.height - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let p = |xx: usize, yy: usize| f.data[yy * f.width + xx];
    Some(
        p(x0, y0) * (1.0 - fx) * (1.0 - fy)
            + p(x1, y0) * fx * (1.0 - fy)
            + p(x0, y1) * (1.0 - fx) * fy
            + p(x1, y1) * fx * fy,
    )
}

/// The median over angles of the picture along the radius, for radii
/// `disc.r - REACH` to `disc.r + REACH`. Radii with fewer than a twentieth of the
/// angles inside the frame are `None`.
fn edge_profile(frame: &Frame, disc: Disc) -> Vec<(f32, f32)> {
    let n = (2.0 * REACH / STEP) as usize + 1;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let r = disc.r - REACH + i as f32 * STEP;
        let mut v: Vec<f32> = (0..ANGLES)
            .filter_map(|k| {
                let a = std::f32::consts::TAU * k as f32 / ANGLES as f32;
                bilinear(frame, disc.cx + r * a.cos(), disc.cy + r * a.sin())
            })
            .filter(|s| s.is_finite())
            .collect();
        if v.len() < ANGLES / 20 {
            continue;
        }
        let m = v.len() / 2;
        out.push((r, *v.select_nth_unstable_by(m, f32::total_cmp).1));
    }
    out
}

/// Least squares of `a + (b + c (edge - r)) * s` for `a`, `b`, `c` with
/// `s = erfc((r - edge) / (sigma sqrt 2)) / 2`; returns the sum of squares of
/// the residual and the step `b - a` at the edge.
fn fit_at(profile: &[(f32, f32)], edge: f32, sigma: f32) -> Option<(f64, f64, [f64; 3])> {
    // Columns: 1, s, s (edge - r). Normal equations of 3 unknowns.
    let mut m = [[0.0f64; 3]; 3];
    let mut rhs = [0.0f64; 3];
    for &(r, y) in profile {
        let s = f64::from(erfc((r - edge) / (sigma * std::f32::consts::SQRT_2)) * 0.5);
        let col = [1.0, s, s * f64::from(edge - r)];
        for i in 0..3 {
            for j in 0..3 {
                m[i][j] += col[i] * col[j];
            }
            rhs[i] += col[i] * f64::from(y);
        }
    }
    let sol = solve3(m, rhs)?;
    let mut ss = 0.0f64;
    for &(r, y) in profile {
        let s = f64::from(erfc((r - edge) / (sigma * std::f32::consts::SQRT_2)) * 0.5);
        let p = sol[0] + s * (sol[1] + sol[2] * f64::from(edge - r));
        ss += (f64::from(y) - p).powi(2);
    }
    Some((ss, sol[1], sol))
}

fn solve3(mut m: [[f64; 3]; 3], mut b: [f64; 3]) -> Option<[f64; 3]> {
    for col in 0..3 {
        let piv = (col..3).max_by(|&i, &j| m[i][col].abs().total_cmp(&m[j][col].abs()))?;
        if m[piv][col].abs() < 1e-12 {
            return None;
        }
        m.swap(col, piv);
        b.swap(col, piv);
        for row in col + 1..3 {
            let f = m[row][col] / m[col][col];
            let pivot_row = m[col];
            for (dst, src) in m[row].iter_mut().zip(pivot_row).skip(col) {
                *dst -= f * src;
            }
            b[row] -= f * b[col];
        }
    }
    let mut x = [0.0f64; 3];
    for i in (0..3).rev() {
        let mut s = b[i];
        for k in i + 1..3 {
            s -= m[i][k] * x[k];
        }
        x[i] = s / m[i][i];
    }
    Some(x)
}

/// Measure the blur of the limb in `frame`. `None` when there is no disc with
/// a clear edge in view, or the edge is not the model's step (see
/// [`try_measure`] for why).
#[must_use]
pub fn measure(frame: &Frame) -> Option<LimbPsf> {
    try_measure(frame).ok()
}

/// The limb's edge spread function as measured and as fitted.
#[derive(Clone, Debug, PartialEq)]
pub struct EdgeFit {
    /// The blur, edge radius and misfit, as [`try_measure`] gives.
    pub psf: LimbPsf,
    /// Radius from the disc centre, pixels, of each profile sample.
    pub radius: Vec<f32>,
    /// The picture's median level at each radius.
    pub measured: Vec<f32>,
    /// The fitted model at each radius: sky, then the blurred edge of the disc.
    pub model: Vec<f32>,
    /// Gaussian sigma the model was blurred by, pixels, including the width
    /// of a pixel (`psf.sigma` has that taken out).
    pub sigma_total: f32,
}

/// As [`try_measure`], keeping the profile the fit was made on so that it
/// can be drawn against the model.
///
/// # Errors
/// As [`try_measure`].
pub fn edge_fit(frame: &Frame) -> Result<EdgeFit, String> {
    let disc = fit_disc(frame).ok_or("no solar disc with a clear edge was found")?;
    let profile = edge_profile(frame, disc);
    if profile.len() < 40 {
        return Err(format!(
            "too little of the limb is in the frame (disc radius {:.0} px at ({:.0}, {:.0}))",
            disc.r, disc.cx, disc.cy
        ));
    }
    let mut best: Option<(f64, f32, f32, f64, [f64; 3])> = None;
    let mut sigma = 0.3f32;
    while sigma <= 6.0 {
        let mut edge = disc.r - 3.0;
        while edge <= disc.r + 3.0 {
            if let Some((ss, step, sol)) = fit_at(&profile, edge, sigma)
                && step > 0.0
                && best.is_none_or(|(b, ..)| ss < b)
            {
                best = Some((ss, sigma, edge, step, sol));
            }
            edge += 0.1;
        }
        sigma += 0.05;
    }
    let (ss, sigma, edge, step, sol) =
        best.ok_or("the limb profile has no step from disc to sky")?;
    let misfit = ((ss / profile.len() as f64).sqrt() / step.max(1e-12)) as f32;
    if misfit > 0.1 {
        return Err(format!(
            "the limb is not a clean edge here (misfit {:.0}% of the step, edge near {edge:.0} px, blur {sigma:.1} px)",
            misfit * 100.0
        ));
    }
    let model = profile
        .iter()
        .map(|&(r, _)| {
            let s = f64::from(erfc((r - edge) / (sigma * std::f32::consts::SQRT_2)) * 0.5);
            (sol[0] + s * (sol[1] + sol[2] * f64::from(edge - r))) as f32
        })
        .collect();
    let blur = (sigma * sigma - PIXEL_SIGMA * PIXEL_SIGMA).max(0.09).sqrt();
    Ok(EdgeFit {
        psf: LimbPsf {
            sigma: blur,
            radius: edge,
            misfit,
        },
        radius: profile.iter().map(|p| p.0).collect(),
        measured: profile.iter().map(|p| p.1).collect(),
        model,
        sigma_total: sigma,
    })
}

/// As [`measure`], with the reason when there is no measurement.
///
/// # Errors
/// A sentence saying what was missing: no disc, too little of the limb in the
/// frame, or an edge the model does not fit.
pub fn try_measure(frame: &Frame) -> Result<LimbPsf, String> {
    edge_fit(frame).map(|f| f.psf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::gaussian_blur;

    /// A limb-darkened disc on a dark sky, then blurred by `sigma`.
    fn disc_image(w: usize, h: usize, r: f32, sigma: f32) -> Frame {
        let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
        let mut data = vec![0.02f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
                if d < r {
                    let mu = (1.0 - (d / r).powi(2)).max(0.0).sqrt();
                    data[y * w + x] = 0.35 + 0.6 * (0.4 + 0.6 * mu);
                }
            }
        }
        let f = Frame::new(w, h, data).expect("frame");
        gaussian_blur(&f, sigma)
    }

    #[test]
    fn a_blurred_limb_gives_its_blur() {
        for truth in [1.0f32, 1.6, 2.5] {
            let f = disc_image(400, 400, 150.0, truth);
            let m = measure(&f).expect("a limb");
            // A pixel-wide step is blurred by the Gaussian plus the sampling.
            assert!(
                (m.sigma - truth).abs() < 0.25,
                "truth {truth}, measured {m:?}"
            );
            assert!((m.radius - 150.0).abs() < 1.0, "{m:?}");
            assert!(m.misfit < 0.05, "{m:?}");
        }
    }

    #[test]
    fn a_limb_psf_in_an_edit_list_resolves_to_the_measured_blur() {
        use crate::edit::{Deconvolve, EditList, Op, Psf};
        let f = disc_image(400, 400, 150.0, 1.6);
        let list = EditList::new(vec![Op::Deconvolve(Deconvolve {
            psf: Psf::Limb,
            ..Deconvolve::default()
        })])
        .resolve(&f);
        let Op::Deconvolve(d) = &list.ops[0] else {
            panic!("deconvolve")
        };
        let Psf::Gaussian { sigma } = d.psf else {
            panic!("a Gaussian, got {:?}", d.psf)
        };
        assert!((sigma - 1.6).abs() < 0.25, "{sigma}");
        // With no limb it falls back to the scale's PSF, still a Gaussian.
        let flat = Frame::new(200, 200, vec![0.4; 200 * 200]).expect("frame");
        let list = EditList::new(vec![Op::Deconvolve(Deconvolve {
            psf: Psf::Limb,
            ..Deconvolve::default()
        })])
        .resolve(&flat);
        assert!(matches!(
            &list.ops[0],
            Op::Deconvolve(d) if matches!(d.psf, Psf::Gaussian { .. })
        ));
    }

    #[test]
    fn no_disc_no_measurement() {
        let f = Frame::new(200, 200, vec![0.4; 200 * 200]).expect("frame");
        assert!(measure(&f).is_none());
    }

    #[test]
    fn erfc_matches_known_values() {
        assert!((erfc(0.0) - 1.0).abs() < 1e-6);
        assert!((erfc(1.0) - 0.157_299_2).abs() < 1e-5);
        assert!((erfc(-1.0) - 1.842_700_8).abs() < 1e-5);
    }
}
