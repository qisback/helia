// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Frame-ranking metrics beyond gradient energy and Laplacian variance.
//!
//! Each is scale free (a brightness change does not move it) and higher means
//! sharper. They exist for the metric bake-off (#21): [`helmli_scherer`] and
//! [`dct_energy_ratio`] and [`mfgs`] follow the three measures Popowicz et al.
//! (2017) recommend for solar images, [`fourier_band`] is a band-limited
//! power with a noise-floor estimate, [`local_contrast`] is the mean local
//! RMS contrast. The definitions are ours where the papers leave room, and are
//! written out on each function. They run on the CPU only; a GPU backend
//! declines them and the pipeline scores the frames here.
//!
//! # References
//!
//! - Popowicz, A. et al. (2017), Review of image quality measures for solar imaging, Solar Phys. 292, 187,
//!   doi:10.1007/s11207-017-1211-3.
//! - Helmli, F. S. and Scherer, S. (2001), Adaptive shape from focus with an error estimation in light
//!   microscopy, Proc. ISPA 2001, 188-193, doi:10.1109/ISPA.2001.938626.
//! - The DCT energy ratio is after Su et al. as cited by Popowicz et al. (original reference to verify).

#![allow(
    clippy::many_single_char_names,
    reason = "x, y, w, h and u, v are the natural names in image and transform maths"
)]

use std::sync::Arc;

use rustfft::{Fft, FftPlanner, num_complex::Complex};

use crate::frame::{Frame, Rect};
use crate::quality::Metric;

/// Score of `metric` over `roi` (default the whole frame), for the metrics
/// this module implements. `None` for gradient energy and Laplacian variance.
#[must_use]
pub fn score(frame: &Frame, metric: Metric, roi: Option<Rect>) -> Option<f32> {
    let f: fn(&[f32], usize, usize) -> f32 = match metric {
        Metric::GradientEnergy | Metric::LaplacianVariance => return None,
        Metric::HelmliScherer => helmli_scherer,
        Metric::Mfgs => mfgs,
        Metric::DctEnergyRatio => dct_energy_ratio,
        Metric::FourierBand => fourier_band,
        Metric::LocalContrast => local_contrast,
    };
    let r = roi
        .unwrap_or_else(|| frame.rect())
        .clamp(frame.width, frame.height);
    if r.is_empty() {
        return Some(0.0);
    }
    let (w, h) = (r.width(), r.height());
    let mut data = Vec::with_capacity(w * h);
    for y in r.y0..r.y1 {
        data.extend_from_slice(&frame.row(y)[r.x0..r.x1]);
    }
    Some(f(&data, w, h))
}

fn mean_of(data: &[f32]) -> f64 {
    data.iter().map(|&v| f64::from(v)).sum::<f64>() / data.len().max(1) as f64
}

/// Box mean of side `2 r + 1`, edges clamped.
fn box_mean(data: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let n = (2 * r + 1) as f32;
    let mut hp = vec![0.0f32; w * h];
    for y in 0..h {
        let row = &data[y * w..(y + 1) * w];
        for x in 0..w {
            let mut s = 0.0;
            for k in 0..=2 * r {
                s += row[(x + k).saturating_sub(r).min(w - 1)];
            }
            hp[y * w + x] = s / n;
        }
    }
    let mut out = vec![0.0f32; w * h];
    for y in 0..h {
        for k in 0..=2 * r {
            let yy = (y + k).saturating_sub(r).min(h - 1);
            for (o, &v) in out[y * w..(y + 1) * w]
                .iter_mut()
                .zip(&hp[yy * w..(yy + 1) * w])
            {
                *o += v;
            }
        }
        for o in &mut out[y * w..(y + 1) * w] {
            *o /= n;
        }
    }
    out
}

/// Helmli and Scherer's mean: the mean over pixels of the ratio of a pixel to
/// the mean of its 5x5 neighbourhood, inverted where the pixel is the smaller,
/// so every term is at least 1 and a sharper frame (more pixels far from
/// their surroundings) scores higher. Values are lifted by 2% of the mean so
/// dark sky does not divide by zero.
#[must_use]
pub fn helmli_scherer(data: &[f32], w: usize, h: usize) -> f32 {
    let eps = (0.02 * mean_of(data)).max(1e-9) as f32;
    let m = box_mean(data, w, h, 2);
    let s: f64 = data
        .iter()
        .zip(&m)
        .map(|(&p, &mu)| {
            let (p, mu) = (p.max(0.0) + eps, mu.max(0.0) + eps);
            f64::from(if p >= mu { p / mu } else { mu / p })
        })
        .sum();
    (s / data.len() as f64 - 1.0) as f32
}

/// Median of nine values.
fn med9(mut v: [f32; 9]) -> f32 {
    v.sort_by(f32::total_cmp);
    v[4]
}

/// Scharr gradient magnitude of the interior of `data`.
fn scharr(data: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut g = vec![0.0f32; w * h];
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let p = |dx: usize, dy: usize| data[(y + dy - 1) * w + x + dx - 1];
            let gx =
                3.0 * (p(2, 0) - p(0, 0)) + 10.0 * (p(2, 1) - p(0, 1)) + 3.0 * (p(2, 2) - p(0, 2));
            let gy =
                3.0 * (p(0, 2) - p(0, 0)) + 10.0 * (p(1, 2) - p(1, 0)) + 3.0 * (p(2, 2) - p(2, 0));
            g[y * w + x] = (gx * gx + gy * gy).sqrt() / 32.0;
        }
    }
    g
}

/// Median-filter gradient similarity (after Deng et al. 2015): the Scharr
/// gradient magnitude of the frame is compared with that of its 3x3 median
/// filtered copy, pixel by pixel, as `(2 a b + c) / (a^2 + b^2 + c)`. The mean
/// similarity is 1 for a frame the median filter does not change, so the score
/// is `1 - mean`, which rises with sharpness (and with noise).
#[must_use]
pub fn mfgs(data: &[f32], w: usize, h: usize) -> f32 {
    if w < 3 || h < 3 {
        return 0.0;
    }
    let mut med = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut v = [0.0f32; 9];
            for (i, slot) in v.iter_mut().enumerate() {
                let (dx, dy) = (i % 3, i / 3);
                *slot = data[((y + dy).saturating_sub(1)).min(h - 1) * w
                    + ((x + dx).saturating_sub(1)).min(w - 1)];
            }
            med[y * w + x] = med9(v);
        }
    }
    let (a, b) = (scharr(data, w, h), scharr(&med, w, h));
    let mean = mean_of(data) as f32;
    let c = (1e-3 * mean).powi(2).max(1e-12);
    let mut s = 0.0f64;
    let mut n = 0usize;
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let (p, q) = (a[y * w + x], b[y * w + x]);
            s += f64::from((2.0 * p * q + c) / (p * p + q * q + c));
            n += 1;
        }
    }
    (1.0 - s / n.max(1) as f64) as f32
}

/// DCT-II basis of size 8: `B[u][x] = c(u) cos((2x + 1) u pi / 16)`.
fn dct8_basis() -> [[f32; 8]; 8] {
    let mut b = [[0.0f32; 8]; 8];
    for (u, row) in b.iter_mut().enumerate() {
        let c = if u == 0 {
            (1.0f64 / 8.0).sqrt()
        } else {
            (2.0f64 / 8.0).sqrt()
        };
        for (x, v) in row.iter_mut().enumerate() {
            *v = (c * ((2 * x + 1) as f64 * u as f64 * std::f64::consts::PI / 16.0).cos()) as f32;
        }
    }
    b
}

/// DCT energy ratio (after Su et al. and Popowicz et al.): over all 8x8 blocks,
/// the energy of the coefficients with `u + v >= 4` as a share of all the
/// alternating-current energy (`(u, v) != (0, 0)`). Block energies are summed
/// before the ratio, so flat sky blocks weigh nothing.
#[must_use]
pub fn dct_energy_ratio(data: &[f32], w: usize, h: usize) -> f32 {
    let b = dct8_basis();
    let (mut hi, mut ac) = (0.0f64, 0.0f64);
    for by in 0..h / 8 {
        for bx in 0..w / 8 {
            // Rows then columns.
            let mut t = [[0.0f32; 8]; 8];
            for y in 0..8 {
                let row = &data[(by * 8 + y) * w + bx * 8..][..8];
                for u in 0..8 {
                    t[y][u] = (0..8).map(|x| b[u][x] * row[x]).sum();
                }
            }
            for u in 0..8 {
                for (v, basis) in b.iter().enumerate() {
                    if u + v == 0 {
                        continue;
                    }
                    let c: f32 = basis.iter().zip(&t).map(|(&bv, trow)| bv * trow[u]).sum();
                    let e = f64::from(c * c);
                    ac += e;
                    if u + v >= 4 {
                        hi += e;
                    }
                }
            }
        }
    }
    if ac <= 0.0 { 0.0 } else { (hi / ac) as f32 }
}

/// Mean local RMS contrast: the standard deviation over the mean in a 5x5
/// window, averaged over the pixels whose neighbourhood is brighter than a
/// third of the mean of the frame (so the sky does not count).
#[must_use]
pub fn local_contrast(data: &[f32], w: usize, h: usize) -> f32 {
    let m1 = box_mean(data, w, h, 2);
    let sq: Vec<f32> = data.iter().map(|&v| v * v).collect();
    let m2 = box_mean(&sq, w, h, 2);
    let lim = (mean_of(data) / 3.0) as f32;
    let (mut s, mut n) = (0.0f64, 0usize);
    for (&a, &b) in m1.iter().zip(&m2) {
        if a > lim && a > 0.0 {
            s += f64::from((b - a * a).max(0.0).sqrt() / a);
            n += 1;
        }
    }
    if n == 0 { 0.0 } else { (s / n as f64) as f32 }
}

/// Tile side of [`fourier_band`].
const TILE: usize = 128;

fn fft2_power(plan: &Arc<dyn Fft<f32>>, tile: &mut [Complex<f32>]) -> Vec<f32> {
    for row in tile.as_chunks_mut::<TILE>().0 {
        plan.process(row);
    }
    let mut col = vec![Complex::new(0.0, 0.0); TILE];
    for x in 0..TILE {
        for y in 0..TILE {
            col[y] = tile[y * TILE + x];
        }
        plan.process(&mut col);
        for y in 0..TILE {
            tile[y * TILE + x] = col[y];
        }
    }
    tile.iter().map(Complex::norm_sqr).collect()
}

/// Band-limited Fourier power with a noise floor: Hann-windowed 128 px tiles
/// (those at least a third as bright as the frame mean, at most 36 on a
/// regular grid) are transformed; the power at radial frequencies from 1/16
/// to 1/4 cycle per pixel, less the mean power per bin between 0.4 and 0.5
/// cycle per pixel (shot and read noise, which is white) times the bins in
/// the band, is divided by the zero-frequency power. Detail finer than the
/// seeing allows sits in the band; the noise term stops a noisy frame from
/// outranking a clean one.
#[must_use]
pub fn fourier_band(data: &[f32], w: usize, h: usize) -> f32 {
    if w < TILE || h < TILE {
        return 0.0;
    }
    let plan = FftPlanner::<f32>::new().plan_fft_forward(TILE);
    let hann: Vec<f32> = (0..TILE)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * (i as f32 + 0.5) / TILE as f32).cos())
        .collect();
    let freq = |i: usize| (if i <= TILE / 2 { i } else { TILE - i }) as f32 / TILE as f32;
    let lim = (mean_of(data) / 3.0) as f32;
    let nx = (w / TILE).min(6);
    let ny = (h / TILE).min(6);
    let (mut band, mut noise_sum, mut dc) = (0.0f64, 0.0f64, 0.0f64);
    let (mut n_band, mut n_noise) = (0usize, 0usize);
    let mut used = 0usize;
    for ty in 0..ny {
        for tx in 0..nx {
            let x0 = if nx > 1 {
                tx * (w - TILE) / (nx - 1)
            } else {
                0
            };
            let y0 = if ny > 1 {
                ty * (h - TILE) / (ny - 1)
            } else {
                0
            };
            let mut tile = vec![Complex::new(0.0f32, 0.0); TILE * TILE];
            let mut sum = 0.0f64;
            for y in 0..TILE {
                for x in 0..TILE {
                    let v = data[(y0 + y) * w + x0 + x];
                    sum += f64::from(v);
                    tile[y * TILE + x] = Complex::new(v * hann[x] * hann[y], 0.0);
                }
            }
            if (sum / (TILE * TILE) as f64) < f64::from(lim) {
                continue;
            }
            let p = fft2_power(&plan, &mut tile);
            dc += f64::from(p[0]);
            for ky in 0..TILE {
                for kx in 0..TILE {
                    let f = freq(kx).hypot(freq(ky));
                    let v = f64::from(p[ky * TILE + kx]);
                    if (1.0 / 16.0..=0.25).contains(&f) {
                        band += v;
                        n_band += 1;
                    } else if (0.4..=0.5).contains(&f) {
                        noise_sum += v;
                        n_noise += 1;
                    }
                }
            }
            used += 1;
        }
    }
    if used == 0 || dc <= 0.0 || n_noise == 0 {
        return 0.0;
    }
    let noise_per_bin = noise_sum / n_noise as f64;
    ((band - noise_per_bin * n_band as f64).max(0.0) / dc) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::gaussian_blur;
    use crate::testutil::{SyntheticConfig, SyntheticSun};

    fn sun(side: usize) -> Frame {
        let cfg = SyntheticConfig {
            width: side,
            height: side,
            ..SyntheticConfig::default()
        };
        SyntheticSun::new(&cfg).truth().clone()
    }

    const ALL: [Metric; 5] = [
        Metric::HelmliScherer,
        Metric::Mfgs,
        Metric::DctEnergyRatio,
        Metric::FourierBand,
        Metric::LocalContrast,
    ];

    #[test]
    fn blur_lowers_every_metric() {
        let sharp = sun(256);
        let soft = gaussian_blur(&sharp, 2.0);
        for m in ALL {
            let (a, b) = (
                score(&sharp, m, None).expect("extra metric"),
                score(&soft, m, None).expect("extra metric"),
            );
            assert!(a > b, "{m:?}: sharp {a} should beat soft {b}");
        }
    }

    #[test]
    fn brightness_does_not_move_a_metric() {
        let f = sun(256);
        let mut g = f.clone();
        g.data.iter_mut().for_each(|v| *v *= 3.0);
        for m in ALL {
            let (a, b) = (
                score(&f, m, None).expect("extra metric"),
                score(&g, m, None).expect("extra metric"),
            );
            assert!(
                (a - b).abs() <= 2e-3 * a.abs().max(1e-6),
                "{m:?}: {a} vs {b}"
            );
        }
    }

    #[test]
    fn classic_metrics_are_not_handled_here() {
        let f = sun(64);
        assert!(score(&f, Metric::GradientEnergy, None).is_none());
        assert!(score(&f, Metric::LaplacianVariance, None).is_none());
    }
}
