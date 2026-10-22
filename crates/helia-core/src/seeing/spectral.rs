// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The spectral ratio (von der Luehe 1984).
//!
//! A tile of an aligned burst has the Fourier transform `F_k = O S_k` of a
//! fixed object `O` and the transfer function `S_k` of frame `k`. The ratio
//! `|<F>|^2 / <|F|^2>` is therefore `|<S>|^2 / <|S|^2>` whatever the object, and
//! depends only on `D / r0` and on the frequency in units of the diffraction
//! cut-off `f_c = D / lambda`. The tables of [`super::table`] give the
//! expectation for a Kolmogorov atmosphere; [`fit_rho`] finds the `D / r0`
//! that matches the measured ratio.
//!
//! # References
//!
//! - von der Luehe, O. (1984), J. Opt. Soc. Am. A 1, 510-519, doi:10.1364/josaa.1.000510.
//! - Fried, D. L. (1966), J. Opt. Soc. Am. 56, 1372-1379, doi:10.1364/JOSA.56.001372.

use realfft::num_complex::Complex32;

use crate::granulation::fft2;
use crate::seeing::screen::U_BINS;
use crate::seeing::{RHO_GRID, table};

/// Tile side the accumulators use.
pub const TILE: usize = 64;

/// Expected `|<S>|^2 / <|S|^2>` at `u = f / f_c` for `D / r0 = rho`.
#[must_use]
pub fn theory_ratio(rho: f32, u: f32) -> f32 {
    let x = (u * U_BINS as f32 - 0.5).clamp(0.0, U_BINS as f32 - 1.0);
    let (i0, t) = (x.floor() as usize, x.fract());
    let i1 = (i0 + 1).min(U_BINS - 1);
    let at = |r: usize| table::RATIO[r][i0] * (1.0 - t) + table::RATIO[r][i1] * t;
    let r = rho.clamp(RHO_GRID[0], RHO_GRID[RHO_GRID.len() - 1]);
    let k = RHO_GRID
        .iter()
        .position(|&g| g >= r)
        .unwrap_or(RHO_GRID.len() - 1)
        .max(1);
    let (lo, hi) = (RHO_GRID[k - 1], RHO_GRID[k]);
    let w = (r - lo) / (hi - lo);
    // Interpolate the logarithm: the ratio falls roughly exponentially with D / r0.
    let (a, b) = (at(k - 1).max(1e-4).ln(), at(k).max(1e-4).ln());
    (a * (1.0 - w) + b * w).exp()
}

/// One ring of the radially binned tile spectrum.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ring {
    /// Frequency in cycles per pixel.
    pub freq: f32,
    /// Sum over the ring of the unbiased `|<F>|^2` estimate.
    pub mean_power: f64,
    /// Sum over the ring of `<|F|^2>` less the noise power.
    pub signal_power: f64,
    /// Noise power in the ring (the same noise level at every pixel).
    pub noise_power: f64,
    /// Pixels in the ring.
    pub count: u32,
}

impl Ring {
    /// Signal to noise of the ring's power.
    #[must_use]
    pub fn snr(&self) -> f32 {
        (self.signal_power / self.noise_power.max(1e-30)) as f32
    }
}

/// Running sums of one tile over a burst of aligned frames.
#[derive(Clone, Debug)]
pub struct TileSpectrum {
    size: usize,
    window: Vec<f32>,
    win_pow: f64,
    sum_f: Vec<Complex32>,
    sum_p: Vec<f64>,
    noise_var: f64,
    n: usize,
}

fn signed(k: usize, n: usize) -> f32 {
    if k <= n / 2 {
        k as f32
    } else {
        k as f32 - n as f32
    }
}

impl TileSpectrum {
    /// Empty accumulator for `size` x `size` tiles.
    #[must_use]
    pub fn new(size: usize) -> Self {
        let hann: Vec<f32> = (0..size)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * (i as f32 + 0.5) / size as f32).cos())
            .collect();
        let window: Vec<f32> = (0..size * size)
            .map(|i| hann[i % size] * hann[i / size])
            .collect();
        let win_pow = window.iter().map(|&w| f64::from(w * w)).sum();
        Self {
            size,
            window,
            win_pow,
            sum_f: vec![Complex32::new(0.0, 0.0); size * size],
            sum_p: vec![0.0; size * size],
            noise_var: 0.0,
            n: 0,
        }
    }

    /// Frames added.
    #[must_use]
    pub fn frames(&self) -> usize {
        self.n
    }

    /// Add one frame's tile: `crop` is the `size` x `size` pixels cut at the
    /// whole-pixel position of the tile in this frame, and `(rx, ry)` the
    /// sub-pixel remainder of the frame's shift (the content sits at `+r`).
    /// `sigma` is the noise standard deviation of the pixels.
    pub fn add(&mut self, crop: &[f32], rx: f32, ry: f32, sigma: f32) {
        let n = self.size;
        let mean = crop.iter().sum::<f32>() / crop.len() as f32;
        let mut buf: Vec<Complex32> = crop
            .iter()
            .zip(&self.window)
            .map(|(&v, &w)| Complex32::new((v - mean) * w, 0.0))
            .collect();
        fft2(&mut buf, n, n, false);
        for ky in 0..n {
            let fy = signed(ky, n) / n as f32;
            for kx in 0..n {
                let fx = signed(kx, n) / n as f32;
                let ph = std::f32::consts::TAU * (fx * rx + fy * ry);
                let c = buf[ky * n + kx] * Complex32::new(ph.cos(), ph.sin());
                self.sum_f[ky * n + kx] += c;
                self.sum_p[ky * n + kx] += f64::from(c.norm_sqr());
            }
        }
        self.noise_var += f64::from(sigma * sigma);
        self.n += 1;
    }

    /// The rings of the radially binned spectrum, with the noise power taken
    /// from the noise levels given to [`Self::add`].
    #[must_use]
    pub fn rings(&self) -> Vec<Ring> {
        let n = self.size;
        let frames = self.n.max(2) as f64;
        let noise = self.noise_var / self.n.max(1) as f64 * self.win_pow;
        let half = n / 2;
        let mut rings = vec![
            Ring {
                freq: 0.0,
                mean_power: 0.0,
                signal_power: 0.0,
                noise_power: 0.0,
                count: 0,
            };
            half
        ];
        for ky in 0..n {
            let fy = signed(ky, n);
            for kx in 0..n {
                let fx = signed(kx, n);
                let b = (fx.hypot(fy)).round() as usize;
                if b == 0 || b >= half {
                    continue;
                }
                let i = ky * n + kx;
                let total = self.sum_p[i] / frames;
                let mean = (self.sum_f[i] / frames as f32).norm_sqr();
                let a = (frames * f64::from(mean) - total) / (frames - 1.0);
                let r = &mut rings[b];
                r.mean_power += a;
                r.signal_power += total - noise;
                r.noise_power += noise;
                r.count += 1;
            }
        }
        for (b, r) in rings.iter_mut().enumerate() {
            r.freq = b as f32 / n as f32;
        }
        rings.retain(|r| r.count > 0);
        rings
    }
}

/// Add the rings of several tiles into one (their ring sums add).
#[must_use]
pub fn pool(tiles: &[Vec<Ring>]) -> Vec<Ring> {
    let Some(first) = tiles.first() else {
        return Vec::new();
    };
    let mut out = first.clone();
    for t in &tiles[1..] {
        for (o, r) in out.iter_mut().zip(t) {
            o.mean_power += r.mean_power;
            o.signal_power += r.signal_power;
            o.noise_power += r.noise_power;
            o.count += r.count;
        }
    }
    out
}

/// Settings of the fit.
#[derive(Clone, Copy, Debug)]
pub struct FitRange {
    /// Cut-off `f_c` in cycles per pixel.
    pub cutoff: f32,
    /// Lowest `u` used (the lowest frequencies are leaked from the mean).
    pub u_min: f32,
    /// Highest `u` used.
    pub u_max: f32,
    /// Fewest signal-to-noise a ring needs.
    pub min_snr: f32,
}

impl FitRange {
    /// The usual range for a cut-off of `cutoff` cycles per pixel: `u` from
    /// 0.05 to 0.9 (and below the sampling limit), rings with signal over three
    /// times the noise.
    #[must_use]
    pub fn new(cutoff: f32) -> Self {
        Self {
            cutoff,
            u_min: 0.05,
            u_max: 0.9f32.min(0.48 / cutoff.max(1e-3)),
            min_snr: 3.0,
        }
    }
}

/// The `D / r0` whose theoretical ratio best matches `rings`, least squares
/// over the rings in range weighted by their pixel counts, with the number of
/// rings used. `None` with fewer than three usable rings.
#[must_use]
pub fn fit_rho(rings: &[Ring], range: &FitRange) -> Option<(f32, usize)> {
    let used: Vec<(f32, f32, f32)> = rings
        .iter()
        .filter_map(|r| {
            let u = r.freq / range.cutoff;
            (u >= range.u_min
                && u <= range.u_max
                && r.snr() >= range.min_snr
                && r.signal_power > 0.0)
                .then(|| (u, (r.mean_power / r.signal_power) as f32, r.count as f32))
        })
        .collect();
    if used.len() < 3 {
        return None;
    }
    let cost = |rho: f32| -> f32 {
        used.iter()
            .map(|&(u, r, w)| w * (r - theory_ratio(rho, u)).powi(2))
            .sum()
    };
    let (lo, hi) = (RHO_GRID[0], RHO_GRID[RHO_GRID.len() - 1]);
    let steps = 240;
    let at = |i: usize| lo * (hi / lo).powf(i as f32 / steps as f32);
    let best = (0..=steps)
        .map(|i| (i, cost(at(i))))
        .min_by(|a, b| a.1.total_cmp(&b.1))?
        .0;
    Some((at(best), used.len()))
}

/// Noise standard deviation of an image from the response to a Laplacian-like
/// kernel that cancels smooth structure (Immerkaer 1996).
#[must_use]
pub fn noise_sigma(crop: &[f32], size: usize) -> f32 {
    let at = |x: usize, y: usize| crop[y * size + x];
    let mut sum = 0.0f64;
    for y in 1..size - 1 {
        for x in 1..size - 1 {
            let v = at(x - 1, y - 1) - 2.0 * at(x, y - 1) + at(x + 1, y - 1) - 2.0 * at(x - 1, y)
                + 4.0 * at(x, y)
                - 2.0 * at(x + 1, y)
                + at(x - 1, y + 1)
                - 2.0 * at(x, y + 1)
                + at(x + 1, y + 1);
            sum += f64::from(v.abs());
        }
    }
    let n = ((size - 2) * (size - 2)) as f64;
    (sum / n * (std::f64::consts::FRAC_PI_2).sqrt() / 6.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seeing::synth::{BurstConfig, Scene, render, scene};

    const CUTOFF: f32 = 0.35;

    fn burst(rho: f32, frames: usize, noise: f32, seed: u64) -> BurstConfig {
        BurstConfig {
            rho,
            cutoff: CUTOFF,
            frames,
            noise,
            jitter: 0.5,
            seed,
        }
    }

    /// Estimate `D / r0` from tiles of the given scenes seen through one atmosphere.
    fn estimate(cfg: &BurstConfig, scenes: &[(Scene, u64)], known_noise: bool) -> Option<f32> {
        let objects: Vec<Vec<f32>> = scenes.iter().map(|&(k, s)| scene(k, s)).collect();
        let bursts = render(&objects, cfg);
        let tiles: Vec<Vec<Ring>> = bursts
            .iter()
            .map(|frames| {
                let mut t = TileSpectrum::new(TILE);
                for f in frames {
                    let sigma = if known_noise {
                        cfg.noise
                    } else {
                        noise_sigma(&f.pixels, TILE)
                    };
                    t.add(&f.pixels, f.shift.0, f.shift.1, sigma);
                }
                t.rings()
            })
            .collect();
        fit_rho(&pool(&tiles), &FitRange::new(CUTOFF)).map(|(r, _)| r)
    }

    const MIX: [(Scene, u64); 4] = [
        (Scene::Granulation, 1),
        (Scene::Granulation, 2),
        (Scene::Pore, 3),
        (Scene::Limb, 4),
    ];

    #[test]
    fn the_theory_ratio_is_one_without_turbulence_and_falls_with_it() {
        assert!(theory_ratio(0.5, 0.3) > 0.99);
        assert!(theory_ratio(8.0, 0.3) < theory_ratio(4.0, 0.3));
        assert!(theory_ratio(4.0, 0.3) < theory_ratio(2.0, 0.3));
        // Between table rows it is between them.
        let mid = theory_ratio(5.0, 0.25);
        assert!(mid < theory_ratio(4.0, 0.25) && mid > theory_ratio(6.0, 0.25));
    }

    #[test]
    fn d_over_r0_is_recovered_within_twenty_percent_across_the_sweep() {
        for rho in [3.0f32, 5.0, 8.0, 12.0] {
            let got = estimate(&burst(rho, 160, 0.01, 7), &MIX, true).expect("fit");
            eprintln!(
                "D/r0 {rho}: estimated {got:.2} ({:+.0}%)",
                (got / rho - 1.0) * 100.0
            );
            assert!((got / rho - 1.0).abs() < 0.20, "{rho}: {got}");
        }
    }

    #[test]
    fn the_object_does_not_change_the_estimate() {
        let rho = 6.0;
        let each: Vec<f32> = [
            (Scene::Granulation, 11u64),
            (Scene::Limb, 12),
            (Scene::Pore, 13),
        ]
        .iter()
        .map(|&sc| estimate(&burst(rho, 160, 0.01, 21), &[sc], true).expect("fit"))
        .collect();
        eprintln!(
            "granulation {:.2}, limb {:.2}, pore {:.2}",
            each[0], each[1], each[2]
        );
        let mean = each.iter().sum::<f32>() / 3.0;
        for e in &each {
            assert!((e / mean - 1.0).abs() < 0.10, "{each:?}");
        }
    }

    #[test]
    fn noise_is_corrected_for_and_its_estimate_is_good_enough() {
        let rho = 6.0;
        let clean = estimate(&burst(rho, 160, 0.002, 5), &MIX, true).expect("clean");
        for noise in [0.02f32, 0.05] {
            let known = estimate(&burst(rho, 160, noise, 5), &MIX, true).expect("known");
            let blind = estimate(&burst(rho, 160, noise, 5), &MIX, false).expect("blind");
            eprintln!("noise {noise}: clean {clean:.2}, known {known:.2}, estimated {blind:.2}");
            assert!((known / rho - 1.0).abs() < 0.25, "known {known}");
            assert!((blind / rho - 1.0).abs() < 0.30, "blind {blind}");
        }
    }

    #[test]
    fn r0_scales_with_wavelength_to_the_six_fifths() {
        // The same atmosphere at 656 nm and at 393 nm: D / r0 grows by (656 / 393)^1.2.
        let rho_656 = 4.0f32;
        let rho_393 = rho_656 * (656.0f32 / 393.0).powf(1.2);
        let a = estimate(&burst(rho_656, 160, 0.01, 3), &MIX, true).expect("656");
        let b = estimate(&burst(rho_393, 160, 0.01, 4), &MIX, true).expect("393");
        let (r_a, r_b) = (
            crate::seeing::r0_at_500(100.0 / a, 656.0),
            crate::seeing::r0_at_500(100.0 / b, 393.0),
        );
        eprintln!("r0 at 500 nm: {r_a:.2} from 656 nm, {r_b:.2} from 393 nm");
        assert!((r_a / r_b - 1.0).abs() < 0.20, "{r_a} vs {r_b}");
    }

    #[test]
    fn undersampling_does_not_bias_the_estimate_of_a_synthetic_scene() {
        // Drawn three times finer, imaged and integrated into pixels, with the
        // cut-off at 1.05 cycles per pixel (twice the sampling limit): detail
        // aliases, and the estimate keeps its accuracy.
        use crate::seeing::synth::{render_sampled, scene_sized};
        let rho = 5.0;
        let cfg = BurstConfig {
            cutoff: 1.05,
            ..burst(rho, 140, 0.01, 9)
        };
        let objects: Vec<Vec<f32>> = MIX.iter().map(|&(k, s)| scene_sized(k, s, 3)).collect();
        let tiles: Vec<Vec<Ring>> = render_sampled(&objects, &cfg, 3)
            .iter()
            .map(|frames| {
                let mut t = TileSpectrum::new(TILE);
                for f in frames {
                    t.add(&f.pixels, f.shift.0, f.shift.1, cfg.noise);
                }
                t.rings()
            })
            .collect();
        let (got, _) = fit_rho(&pool(&tiles), &FitRange::new(1.05)).expect("fit");
        eprintln!("undersampled: D/r0 {rho} estimated {got:.2}");
        assert!((got / rho - 1.0).abs() < 0.20, "{got}");
    }

    #[test]
    fn independent_patches_in_one_tile_do_not_bias_the_estimate() {
        // Left and right halves of the tile look through independent turbulence,
        // the limiting case of a tile wider than the isoplanatic patch. Real
        // neighbouring patches are partly correlated, which this does not
        // cover, so the method stays validated for isoplanatic tiles only.
        let rho = 4.0;
        let cfg = BurstConfig {
            jitter: 0.0,
            ..burst(rho, 160, 0.01, 31)
        };
        let objects: Vec<Vec<f32>> = MIX.iter().map(|&(k, s)| scene(k, s)).collect();
        let a = render(&objects, &cfg);
        let b = render(&objects, &BurstConfig { seed: 77, ..cfg });
        let (mut mixed, mut single) = (Vec::new(), Vec::new());
        for (fa, fb) in a.iter().zip(&b) {
            let (mut tm, mut ts) = (TileSpectrum::new(TILE), TileSpectrum::new(TILE));
            for (x, y) in fa.iter().zip(fb) {
                let px: Vec<f32> = (0..TILE * TILE)
                    .map(|i| {
                        if i % TILE < TILE / 2 {
                            x.pixels[i]
                        } else {
                            y.pixels[i]
                        }
                    })
                    .collect();
                tm.add(&px, 0.0, 0.0, cfg.noise);
                ts.add(&x.pixels, 0.0, 0.0, cfg.noise);
            }
            mixed.push(tm.rings());
            single.push(ts.rings());
        }
        let range = FitRange::new(CUTOFF);
        let (m, _) = fit_rho(&pool(&mixed), &range).expect("mixed");
        let (s, _) = fit_rho(&pool(&single), &range).expect("single");
        eprintln!("one patch: {s:.2}, two independent patches: {m:.2} (truth {rho})");
        assert!(
            (s / rho - 1.0).abs() < 0.2 && (m / rho - 1.0).abs() < 0.2,
            "{s} {m}"
        );
    }

    #[test]
    fn noise_sigma_reads_white_noise() {
        let mut rng = crate::seeing::screen::Rng::new(9);
        let img: Vec<f32> = (0..TILE * TILE).map(|_| 1.0 + 0.03 * rng.gauss()).collect();
        let s = noise_sigma(&img, TILE);
        assert!((s - 0.03).abs() < 0.003, "{s}");
    }
}
