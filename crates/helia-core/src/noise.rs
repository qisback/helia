// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Noise and signal-to-noise of an image, estimated from the image alone.
//!
//! The noise is the one of Immerkaer (1996, "Fast noise variance estimation",
//! Computer Vision and Image Understanding 64, 300): the image is convolved
//! with the mask
//!
//! ```text
//!  1 -2  1
//! -2  4 -2
//!  1 -2  1
//! ```
//!
//! which removes any locally planar or quadratic structure, so what is left is
//! noise, and for white noise of standard deviation `sigma` the response has
//! variance `36 sigma^2`. Granulation and fibrils leave a response too, so the
//! spread is taken as 1.4826 times the median absolute deviation of the
//! response, which a minority of structured pixels cannot move, rather than as
//! the standard deviation. The signal is the median level of the same area.
//!
//! The estimate reads fine-grained noise. Noise that is correlated over
//! several pixels (a stack that has been resampled and sharpened) is
//! under-read, so read it as a comparison between images of one capture, not as
//! the camera's read noise.

use crate::frame::{Frame, Rect};

/// Signal and noise of one area of an image.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoiseEstimate {
    /// Median level of the area.
    pub signal: f32,
    /// Standard deviation of the noise, in the image's units.
    pub sigma: f32,
    /// `signal / sigma`.
    pub snr: f32,
    /// Pixels measured.
    pub pixels: usize,
}

/// Estimate the noise of `area` of `frame`. `None` when the area is smaller
/// than 8 pixels a side, or flat (no noise to measure).
#[must_use]
pub fn estimate(frame: &Frame, area: Rect) -> Option<NoiseEstimate> {
    let area = area.clamp(frame.width, frame.height);
    let (w, h) = (area.width(), area.height());
    if w < 8 || h < 8 {
        return None;
    }
    let at = |x: usize, y: usize| f64::from(frame.row(area.y0 + y)[area.x0 + x]);
    let mut response = Vec::with_capacity((w - 2) * (h - 2));
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let r = at(x - 1, y - 1) - 2.0 * at(x, y - 1) + at(x + 1, y - 1) - 2.0 * at(x - 1, y)
                + 4.0 * at(x, y)
                - 2.0 * at(x + 1, y)
                + at(x - 1, y + 1)
                - 2.0 * at(x, y + 1)
                + at(x + 1, y + 1);
            if r.is_finite() {
                response.push(r);
            }
        }
    }
    let n = response.len();
    if n < 16 {
        return None;
    }
    let median = |v: &mut Vec<f64>| {
        let k = v.len() / 2;
        *v.select_nth_unstable_by(k, f64::total_cmp).1
    };
    let centre = median(&mut response.clone());
    let mut dev: Vec<f64> = response.iter().map(|r| (r - centre).abs()).collect();
    let mad = median(&mut dev);
    let sigma = (1.4826 * mad / 6.0) as f32;
    let mut levels: Vec<f64> = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| at(x, y))
        .filter(|v| v.is_finite())
        .collect();
    let signal = median(&mut levels) as f32;
    (sigma > 0.0 && sigma.is_finite()).then_some(NoiseEstimate {
        signal,
        sigma,
        snr: signal / sigma,
        pixels: n,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Rng;

    fn noisy(w: usize, h: usize, level: f32, sigma: f32, slope: f32) -> Frame {
        let mut rng = Rng::new(7);
        let data = (0..w * h)
            .map(|i| level + slope * (i % w) as f32 + sigma * rng.gauss())
            .collect();
        Frame::new(w, h, data).expect("frame")
    }

    #[test]
    fn white_noise_is_recovered_on_a_gradient() {
        let f = noisy(200, 200, 0.5, 0.02, 0.001);
        let e = estimate(&f, Rect::new(0, 0, 200, 200)).expect("estimate");
        assert!((e.sigma - 0.02).abs() < 0.001, "{e:?}");
        assert!((e.snr - e.signal / e.sigma).abs() < 1e-3);
    }

    #[test]
    fn structure_does_not_inflate_the_estimate() {
        let mut f = noisy(200, 200, 0.5, 0.02, 0.0);
        // A sprinkling of bright dots, 1% of the pixels (each reaches nine
        // pixels of the response).
        for i in (0..f.data.len()).step_by(100) {
            f.data[i] += 0.5;
        }
        let e = estimate(&f, Rect::new(0, 0, 200, 200)).expect("estimate");
        assert!((e.sigma - 0.02).abs() < 0.002, "{e:?}");
    }

    #[test]
    fn a_flat_or_tiny_area_has_none() {
        let f = Frame::new(50, 50, vec![0.5; 2500]).expect("frame");
        assert!(estimate(&f, Rect::new(0, 0, 50, 50)).is_none());
        assert!(estimate(&noisy(50, 50, 0.5, 0.02, 0.0), Rect::new(0, 0, 5, 5)).is_none());
    }
}
