// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Did the sharpening add detail, or mostly noise?
//!
//! Sharpening cannot tell the two apart, so it is judged afterwards from the
//! bands of [`crate::measure`] over one crop of the disc centre (taken from the
//! picture before, so the sharpening cannot move the area it is judged on):
//!
//! * the **noise gain**, the pixel-scale band (radii 0 to 1) after over before;
//! * the **detail gain**, the 1 to 4 pixel band after over before.
//!
//! Real detail lifts the 1 to 4 pixel band at least as fast as it lifts the
//! pixel-scale band; amplified noise lifts the pixel-scale band and leaves the
//! 1 to 4 pixel band where it was. The check calls it noise when the noise gain
//! is at least [`NOISE_MIN`] and exceeds [`RATIO`] times the detail gain. On the
//! real H-alpha and calcium K stacks (see the user guide), deconvolution with
//! the recommended settings gives a noise gain of 2.8 for a detail gain of 1.0
//! on the H-alpha stack that is the known case, and 4.4 against 1.2 on calcium K;
//! the recommended wavelets give 2.1 against 2.3 and are not flagged.
//!
//! For a deconvolution the residual is reported as well: the root mean square
//! of the blurred result minus the picture, as a share of the mean, against the
//! noise level of the picture. A residual below the noise means the iterations
//! are fitting the noise itself.

use super::deconv::{Psf, PsfKernel};
use super::wavelet::{mad_sigma_of_difference, smooth};
use crate::frame::Frame;
use crate::measure::{DEFAULT_CROP, centre_crop, granulation_contrast_in};

/// The noise gain below which nothing is flagged.
pub const NOISE_MIN: f32 = 1.4;
/// The noise gain must exceed this times the detail gain to be flagged.
pub const RATIO: f32 = 1.5;
/// A residual under this share of the noise level means noise is being fitted.
pub const OVERFIT: f32 = 0.7;

/// What was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Detail rose in step with the noise or faster.
    Fine,
    /// The noise rose and the detail did not: mostly amplified noise.
    NoiseDominated,
    /// The deconvolution residual is below the noise: the iterations are
    /// fitting the noise.
    Overfit,
}

/// The result of [`check`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SharpenCheck {
    /// 1 to 4 pixel band, after over before.
    pub detail_gain: f32,
    /// Pixel-scale band, after over before.
    pub noise_gain: f32,
    /// Deconvolution residual (share of the mean), when a PSF was given.
    pub residual: Option<f32>,
    /// Noise level of the picture before (share of the mean).
    pub noise_level: f32,
    /// The verdict.
    pub verdict: Verdict,
}

fn ratio(after: f32, before: f32) -> f32 {
    if before > 1e-9 { after / before } else { 1.0 }
}

/// Noise level of a picture as a share of its mean: the median-based sigma of
/// the finest wavelet layer.
fn noise_level(f: &Frame) -> f32 {
    let mean = f.data.iter().map(|&v| f64::from(v)).sum::<f64>() / f.data.len().max(1) as f64;
    if mean <= 0.0 {
        return 0.0;
    }
    let s = smooth(&f.data, f.width, f.height, 1);
    (f64::from(mad_sigma_of_difference(&f.data, &s, f.width, f.height)) / mean) as f32
}

/// Judge `after` against `before` (the same size), and with a concrete `psf`
/// also the residual of the deconvolution. `None` when the sizes differ.
#[must_use]
pub fn check(before: &Frame, after: &Frame, psf: Option<Psf>) -> Option<SharpenCheck> {
    if (before.width, before.height) != (after.width, after.height) {
        return None;
    }
    let area = centre_crop(before, DEFAULT_CROP);
    let (b, a) = (
        granulation_contrast_in(before, area),
        granulation_contrast_in(after, area),
    );
    let detail_gain = ratio(a.granulation, b.granulation);
    let noise_gain = ratio(a.fine, b.fine);
    let level = noise_level(before);
    let residual = psf
        .and_then(|p| PsfKernel::new(p).ok())
        .map(|k| residual_of(before, after, &k, area));
    let mut verdict = Verdict::Fine;
    if noise_gain >= NOISE_MIN && noise_gain > RATIO * detail_gain {
        verdict = Verdict::NoiseDominated;
    } else if residual.is_some_and(|r| level > 0.0 && r < OVERFIT * level) {
        verdict = Verdict::Overfit;
    }
    Some(SharpenCheck {
        detail_gain,
        noise_gain,
        residual,
        noise_level: level,
        verdict,
    })
}

/// RMS of `kernel * after - before` over `area`, as a share of the mean of
/// `before` there.
fn residual_of(before: &Frame, after: &Frame, kernel: &PsfKernel, area: crate::frame::Rect) -> f32 {
    let blurred = kernel.apply(&after.data, after.width, after.height);
    let (mut ss, mut sum, mut n) = (0.0f64, 0.0f64, 0usize);
    for y in area.y0..area.y1 {
        for x in area.x0..area.x1 {
            let i = y * before.width + x;
            let d = f64::from(blurred[i]) - f64::from(before.data[i]);
            ss += d * d;
            sum += f64::from(before.data[i]);
            n += 1;
        }
    }
    if n == 0 || sum <= 0.0 {
        return 0.0;
    }
    ((ss / n as f64).sqrt() / (sum / n as f64)) as f32
}

impl SharpenCheck {
    /// A sentence for a person when the result is mostly noise, else `None`.
    #[must_use]
    pub fn warning(&self) -> Option<String> {
        match self.verdict {
            Verdict::Fine => None,
            Verdict::NoiseDominated => Some(format!(
                "This mostly amplifies noise: the pixel-scale noise is up {:.1}x and the detail only {:.1}x. Use fewer iterations or a smaller boost, a higher regularisation (TV) weight or noise cut, or denoise afterwards.",
                self.noise_gain, self.detail_gain
            )),
            Verdict::Overfit => Some(
                "The deconvolution fits the noise: its residual is below the noise level of the picture. Use fewer iterations.".to_string(),
            ),
        }
    }

    /// One line for the log: the gains, the residual, the verdict.
    #[must_use]
    pub fn summary(&self) -> String {
        let res = self.residual.map_or(String::new(), |r| {
            format!(
                ", residual {:.3}% against a noise level of {:.3}%",
                r * 100.0,
                self.noise_level * 100.0
            )
        });
        format!(
            "detail x{:.2}, noise x{:.2}{}{}",
            self.detail_gain,
            self.noise_gain,
            res,
            match self.verdict {
                Verdict::Fine => "",
                Verdict::NoiseDominated => " (mostly noise)",
                Verdict::Overfit => " (fitting the noise)",
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::wavelet::{Layer, Wavelet, transform_with};
    use crate::testutil::Rng;

    /// Smooth blobs with a little noise.
    fn scene(w: usize, h: usize, noise: f32, seed: u64) -> Frame {
        let mut rng = Rng::new(seed);
        let mut data = vec![0.0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let s = (x as f32 * 0.21).sin() * (y as f32 * 0.17).cos();
                data[y * w + x] = 0.5 + 0.05 * s + noise * rng.gauss();
            }
        }
        Frame::new(w, h, data).expect("frame")
    }

    #[test]
    fn amplified_noise_is_flagged_and_real_detail_is_not() {
        let (w, h) = (260, 260);
        // Pure noise boosted at the finest layer: the pixel band rises, the
        // 1 to 4 pixel band does not.
        let noisy = scene(w, h, 0.02, 1);
        let fine_only = Wavelet::from_layers(vec![Layer::from_gain(6.0, 0.0)]);
        let boosted = Frame::new(w, h, transform_with(&noisy.data, w, h, &fine_only)).expect("f");
        let c = check(&noisy, &boosted, None).expect("check");
        assert_eq!(c.verdict, Verdict::NoiseDominated, "{c:?}");
        assert!(c.warning().is_some());
        // Real structure boosted at the 2 to 4 pixel layers: the detail band rises.
        let clean = scene(w, h, 0.0005, 2);
        let mid = Wavelet::from_layers(vec![
            Layer::default(),
            Layer::from_gain(3.0, 0.0),
            Layer::from_gain(3.0, 0.0),
        ]);
        let lifted = Frame::new(w, h, transform_with(&clean.data, w, h, &mid)).expect("f");
        let c = check(&clean, &lifted, None).expect("check");
        assert_eq!(c.verdict, Verdict::Fine, "{c:?}");
    }

    #[test]
    fn different_sizes_are_not_compared() {
        let a = scene(64, 64, 0.01, 3);
        let b = scene(32, 32, 0.01, 3);
        assert!(check(&a, &b, None).is_none());
    }
}
