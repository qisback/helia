// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Measuring how much granulation an image holds (`helia measure`).
//!
//! The figure is a band-pass contrast: the standard deviation of
//! `box(r_inner) - box(r_outer)` over a crop of the disc centre, divided by the
//! mean of the image over the same crop. `box(r)` is a mean over a
//! `(2r+1) x (2r+1)` window and `box(0)` is the image itself. Three bands are
//! reported:
//!
//! * fine, radii 0 to 1: pixel-scale structure, which noise dominates;
//! * granulation, radii 1 to 4: cells of a few pixels at typical amateur
//!   scales (a granule is about 1.5 arcsec across);
//! * meso, radii 4 to 12: larger groupings and slow brightness variation.
//!
//! Noise adds to every band, so a rising granulation figure is evidence, not
//! proof: look at a crop as well. The figure is only comparable between
//! images of the same scale, bit depth and region.

use crate::frame::{Frame, Rect};

/// Side of the disc-centre crop, in pixels, that [`granulation_contrast`] uses.
pub const DEFAULT_CROP: usize = 240;

/// Band-pass contrast (standard deviation over mean, a fraction) in the three
/// bands.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GranulationBands {
    /// Radii 0 to 1.
    pub fine: f32,
    /// Radii 1 to 4: the granulation band.
    pub granulation: f32,
    /// Radii 4 to 12.
    pub meso: f32,
    /// The crop measured.
    pub area: Rect,
}

/// Centre of the bright disc: the centroid of the pixels above half the
/// brightest, found on a 4x decimated copy. The frame's centre when nothing
/// stands out.
#[must_use]
pub fn disc_centre(frame: &Frame) -> (usize, usize) {
    let (_, hi) = frame.min_max();
    let level = 0.5 * hi;
    let (mut sx, mut sy, mut n) = (0.0f64, 0.0f64, 0u64);
    for y in (0..frame.height).step_by(4) {
        for (x, &v) in frame.row(y).iter().enumerate().step_by(4) {
            if v > level {
                sx += x as f64;
                sy += y as f64;
                n += 1;
            }
        }
    }
    if n == 0 || hi <= 0.0 {
        return (frame.width / 2, frame.height / 2);
    }
    ((sx / n as f64) as usize, (sy / n as f64) as usize)
}

/// A `side` x `side` crop centred on the disc, moved to lie inside the frame
/// (and shrunk if the frame is smaller).
#[must_use]
pub fn centre_crop(frame: &Frame, side: usize) -> Rect {
    let (cx, cy) = disc_centre(frame);
    crop_around(frame, side, cx, cy)
}

/// A `side` x `side` crop centred on `(cx, cy)`, moved to lie inside the frame
/// (and shrunk if the frame is smaller). For comparing stacks of one capture
/// over the same part of the Sun, with the centre taken from each disc fit.
#[must_use]
pub fn crop_around(frame: &Frame, side: usize, cx: usize, cy: usize) -> Rect {
    let w = side.min(frame.width).max(1);
    let h = side.min(frame.height).max(1);
    let x = cx.saturating_sub(w / 2).min(frame.width - w);
    let y = cy.saturating_sub(h / 2).min(frame.height - h);
    Rect::new(x, y, w, h)
}

/// Mean over `(2r+1)^2` windows for every pixel of `area`, borders clamped to
/// the frame edge. `r = 0` is the image.
fn box_mean(frame: &Frame, area: Rect, r: usize) -> Vec<f32> {
    let (aw, ah) = (area.width(), area.height());
    if r == 0 {
        return (area.y0..area.y1)
            .flat_map(|y| frame.row(y)[area.x0..area.x1].iter().copied())
            .collect();
    }
    // Integral image over the area grown by r, with clamped sampling.
    let (pw, ph) = (aw + 2 * r, ah + 2 * r);
    let at = |px: usize, py: usize| -> f64 {
        let x = (area.x0 + px).saturating_sub(r).min(frame.width - 1);
        let y = (area.y0 + py).saturating_sub(r).min(frame.height - 1);
        f64::from(frame.data[y * frame.width + x])
    };
    let stride = pw + 1;
    let mut sat = vec![0.0f64; stride * (ph + 1)];
    for py in 0..ph {
        let mut row = 0.0;
        for px in 0..pw {
            row += at(px, py);
            sat[(py + 1) * stride + px + 1] = sat[py * stride + px + 1] + row;
        }
    }
    let n = ((2 * r + 1) * (2 * r + 1)) as f64;
    let mut out = Vec::with_capacity(aw * ah);
    for y in 0..ah {
        for x in 0..aw {
            // The window around (x + r, y + r) in padded coordinates.
            let (x1, y1) = (x + 2 * r + 1, y + 2 * r + 1);
            let s = sat[y1 * stride + x1] - sat[y * stride + x1] - sat[y1 * stride + x]
                + sat[y * stride + x];
            out.push((s / n) as f32);
        }
    }
    out
}

/// Standard deviation of `box(r_inner) - box(r_outer)` over `area`, divided by
/// the mean of the image over `area`. Zero for an empty area or a black image.
#[must_use]
pub fn band_contrast(frame: &Frame, area: Rect, r_inner: usize, r_outer: usize) -> f32 {
    let area = area.clamp(frame.width, frame.height);
    if area.is_empty() {
        return 0.0;
    }
    let a = box_mean(frame, area, r_inner);
    let b = box_mean(frame, area, r_outer);
    let mean = box_mean(frame, area, 0)
        .iter()
        .map(|&v| f64::from(v))
        .sum::<f64>()
        / a.len() as f64;
    if mean <= 0.0 {
        return 0.0;
    }
    let d: Vec<f64> = a
        .iter()
        .zip(&b)
        .map(|(&p, &q)| f64::from(p) - f64::from(q))
        .collect();
    let dm = d.iter().sum::<f64>() / d.len() as f64;
    let var = d.iter().map(|v| (v - dm) * (v - dm)).sum::<f64>() / d.len() as f64;
    (var.sqrt() / mean) as f32
}

/// The three bands over `area`.
#[must_use]
pub fn granulation_contrast_in(frame: &Frame, area: Rect) -> GranulationBands {
    GranulationBands {
        fine: band_contrast(frame, area, 0, 1),
        granulation: band_contrast(frame, area, 1, 4),
        meso: band_contrast(frame, area, 4, 12),
        area: area.clamp(frame.width, frame.height),
    }
}

/// The three bands over a [`DEFAULT_CROP`] crop at the disc centre.
#[must_use]
pub fn granulation_contrast(frame: &Frame) -> GranulationBands {
    granulation_contrast_in(frame, centre_crop(frame, DEFAULT_CROP))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vertical stripes: `mean + amp * sin(2 pi x / period + 0.7)`; the phase
    /// keeps a period of 2 pixels off its zero crossings.
    fn stripes(w: usize, h: usize, mean: f32, amp: f32, period: f32) -> Frame {
        let data = (0..w * h)
            .map(|i| mean + amp * (std::f32::consts::TAU * (i % w) as f32 / period + 0.7).sin())
            .collect();
        Frame::new(w, h, data).unwrap_or_else(|e| panic!("{e}"))
    }

    /// Response of a box of width `n` to a sinusoid of frequency `f` cycles per pixel.
    fn box_response(n: f32, f: f32) -> f32 {
        (std::f32::consts::PI * n * f).sin() / (n * (std::f32::consts::PI * f).sin())
    }

    #[test]
    fn a_known_stripe_pattern_has_the_contrast_the_filters_predict() {
        let (mean, amp, period) = (0.5, 0.02, 8.0);
        let f = stripes(400, 400, mean, amp, period);
        let area = Rect::new(100, 100, 200, 200);
        let want = (box_response(3.0, 1.0 / period) - box_response(9.0, 1.0 / period)).abs() * amp
            / std::f32::consts::SQRT_2
            / mean;
        let got = band_contrast(&f, area, 1, 4);
        assert!(((got - want) / want).abs() < 0.01, "got {got}, want {want}");
        // Against a 25-wide box (radius 12) the same pattern gives another known figure.
        let want =
            (1.0 - box_response(25.0, 1.0 / period)).abs() * amp / std::f32::consts::SQRT_2 / mean;
        let got = band_contrast(&f, area, 0, 12);
        assert!(((got - want) / want).abs() < 0.01, "got {got}, want {want}");
    }

    #[test]
    fn contrast_scales_with_amplitude_and_inversely_with_brightness() {
        let a = granulation_contrast(&stripes(400, 400, 0.5, 0.01, 8.0));
        let b = granulation_contrast(&stripes(400, 400, 0.5, 0.02, 8.0));
        let c = granulation_contrast(&stripes(400, 400, 0.25, 0.01, 8.0));
        assert!((b.granulation / a.granulation - 2.0).abs() < 0.02);
        assert!((c.granulation / a.granulation - 2.0).abs() < 0.02);
    }

    #[test]
    fn each_scale_lands_in_its_own_band() {
        let fine = granulation_contrast(&stripes(400, 400, 0.5, 0.02, 2.0));
        assert!(
            fine.fine > 2.5 * fine.granulation.max(fine.meso),
            "{fine:?}"
        );
        let gran = granulation_contrast(&stripes(400, 400, 0.5, 0.02, 8.0));
        assert!(
            gran.granulation > 3.0 * gran.fine.max(gran.meso),
            "{gran:?}"
        );
        let meso = granulation_contrast(&stripes(400, 400, 0.5, 0.02, 40.0));
        assert!(
            meso.meso > 3.0 * meso.fine.max(meso.granulation),
            "{meso:?}"
        );
    }

    #[test]
    fn a_flat_or_black_image_has_no_contrast() {
        let flat = granulation_contrast(&stripes(300, 300, 0.5, 0.0, 8.0));
        assert!(flat.fine < 1e-6 && flat.granulation < 1e-6 && flat.meso < 1e-6);
        let black = granulation_contrast(&Frame::zeros(300, 300));
        assert!(black.fine == 0.0 && black.granulation == 0.0 && black.meso == 0.0);
    }

    #[test]
    fn the_crop_follows_the_disc() {
        let (w, h) = (600, 400);
        let mut f = Frame::zeros(w, h);
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = (x as f32 - 420.0, y as f32 - 150.0);
                if dx * dx + dy * dy < 100.0 * 100.0 {
                    f.data[y * w + x] = 0.8;
                }
            }
        }
        let (cx, cy) = disc_centre(&f);
        assert!(cx.abs_diff(420) < 6 && cy.abs_diff(150) < 6, "{cx},{cy}");
        let r = centre_crop(&f, 240);
        assert_eq!((r.width(), r.height()), (240, 240));
        assert!(r.x1 <= w && r.y1 <= h);
    }

    #[test]
    fn a_small_frame_shrinks_the_crop() {
        let f = stripes(100, 80, 0.5, 0.02, 8.0);
        let r = centre_crop(&f, 240);
        assert_eq!((r.width(), r.height()), (100, 80));
    }
}
