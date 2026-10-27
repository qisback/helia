// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Synthetic bursts with a known Fried parameter for the tests: an object tile
//! imaged through independent Kolmogorov phase screens.

#![allow(
    clippy::many_single_char_names,
    reason = "x, y, n and the frequencies are the natural names in this maths"
)]

use realfft::num_complex::Complex32;

use crate::granulation::fft2;
use crate::seeing::screen::{
    GRID, OTF_GRID, PUPIL, Rng, kolmogorov_screen, otf, pupil_phase, pupil_pixels,
};
use crate::seeing::spectral::TILE;

/// What the burst is made of.
#[derive(Clone, Copy, Debug)]
pub struct BurstConfig {
    /// `D / r0` of the atmosphere.
    pub rho: f32,
    /// Diffraction cut-off in cycles per pixel of the tile.
    pub cutoff: f32,
    /// Frames.
    pub frames: usize,
    /// Pixel noise, standard deviation.
    pub noise: f32,
    /// Largest sub-pixel image motion applied (and reported) per frame.
    pub jitter: f32,
    /// Seed.
    pub seed: u64,
}

/// Kinds of object.
#[derive(Clone, Copy, Debug)]
pub enum Scene {
    /// Voronoi granulation, about 6 pixels across.
    Granulation,
    /// A limb: an edge with a bright side, darkening towards it, a little texture.
    Limb,
    /// A pore: a dark blob on granulation.
    Pore,
}

fn signed(k: usize, n: usize) -> f32 {
    if k <= n / 2 {
        k as f32
    } else {
        k as f32 - n as f32
    }
}

/// The object tile, mean about 1.
#[must_use]
pub fn scene(kind: Scene, seed: u64) -> Vec<f32> {
    scene_sized(kind, seed, 1)
}

/// The object tile drawn on a grid `factor` times finer than the tile's pixels
/// (`TILE * factor` pixels a side), for [`render_sampled`].
#[must_use]
pub fn scene_sized(kind: Scene, seed: u64, factor: usize) -> Vec<f32> {
    let n = TILE * factor;
    let fs = factor as f32;
    let mut rng = Rng::new(seed);
    let cells = 120;
    let seeds: Vec<(f32, f32, f32)> = (0..cells)
        .map(|_| {
            (
                rng.uniform() * n as f32,
                rng.uniform() * n as f32,
                1.0 + 0.12 * rng.gauss(),
            )
        })
        .collect();
    let wrap = |d: f32| {
        let d = d.abs();
        d.min(n as f32 - d)
    };
    let mut img = vec![0.0f32; n * n];
    for y in 0..n {
        for x in 0..n {
            let (mut d1, mut d2, mut v1) = (f32::MAX, f32::MAX, 1.0);
            for &(sx, sy, v) in &seeds {
                let d = wrap(sx - x as f32).hypot(wrap(sy - y as f32));
                if d < d1 {
                    d2 = d1;
                    d1 = d;
                    v1 = v;
                } else if d < d2 {
                    d2 = d;
                }
            }
            let lane = (-(0.5 * (d2 - d1)).powi(2) / (1.6 * fs * fs)).exp();
            img[y * n + x] = v1 * (1.0 - 0.3 * lane);
        }
    }
    match kind {
        Scene::Granulation => {}
        Scene::Limb => {
            for y in 0..n {
                for x in 0..n {
                    let edge = 0.5 + 0.5 * ((n as f32 / 2.0 - x as f32) / (4.0 * fs)).tanh();
                    let dark = 1.0 - 0.3 * (x as f32 / n as f32);
                    img[y * n + x] = (0.25 * img[y * n + x] + 0.75) * edge * dark;
                }
            }
        }
        Scene::Pore => {
            let c = n as f32 / 2.0;
            for y in 0..n {
                for x in 0..n {
                    let r2 = (x as f32 - c).powi(2) + (y as f32 - c).powi(2);
                    img[y * n + x] *= 1.0 - 0.7 * (-r2 / (2.0 * 7.0 * 7.0 * fs * fs)).exp();
                }
            }
        }
    }
    img
}

/// One frame's transfer function sampled at the tile's frequencies.
fn transfer(s: &[Complex32], cutoff: f32, n: usize) -> Vec<Complex32> {
    let at = |lx: i64, ly: i64| -> Complex32 {
        let g = OTF_GRID as i64;
        s[(ly.rem_euclid(g) * g + lx.rem_euclid(g)) as usize]
    };
    let mut out = vec![Complex32::new(0.0, 0.0); n * n];
    for ky in 0..n {
        let ly = signed(ky, n) / n as f32 / cutoff * PUPIL as f32;
        for kx in 0..n {
            let lx = signed(kx, n) / n as f32 / cutoff * PUPIL as f32;
            if lx.hypot(ly) >= PUPIL as f32 - 1.0 {
                continue;
            }
            let (x0, y0) = (lx.floor() as i64, ly.floor() as i64);
            let (tx, ty) = (lx - x0 as f32, ly - y0 as f32);
            out[ky * n + kx] = at(x0, y0) * ((1.0 - tx) * (1.0 - ty))
                + at(x0 + 1, y0) * (tx * (1.0 - ty))
                + at(x0, y0 + 1) * ((1.0 - tx) * ty)
                + at(x0 + 1, y0 + 1) * (tx * ty);
        }
    }
    out
}

/// A rendered frame: pixels of the tile and the sub-pixel image motion that was applied.
#[derive(Clone, Debug)]
pub struct Frame {
    /// `TILE` x `TILE` pixels.
    pub pixels: Vec<f32>,
    /// Image motion `(x, y)` in pixels: the content sits at `+shift`.
    pub shift: (f32, f32),
}

/// Render the burst of several objects through the same atmosphere: frame `k`
/// of every object uses the same transfer function and the same motion, as
/// tiles of one frame do. Returns `bursts[object][frame]`.
#[must_use]
pub fn render(objects: &[Vec<f32>], cfg: &BurstConfig) -> Vec<Vec<Frame>> {
    render_sampled(objects, cfg, 1)
}

/// As [`render`], from objects drawn `factor` times finer ([`scene_sized`]),
/// imaged and then integrated over `factor` x `factor` samples into the tile's
/// pixels, as a detector does. With `cfg.cutoff` above 0.5 cycles per pixel the
/// image is undersampled and detail aliases.
#[must_use]
pub fn render_sampled(objects: &[Vec<f32>], cfg: &BurstConfig, factor: usize) -> Vec<Vec<Frame>> {
    let n = TILE * factor;
    let fs = factor as f32;
    let pix = pupil_pixels();
    let r0 = PUPIL as f32 / cfg.rho;
    let mut rng = Rng::new(cfg.seed);
    let spectra: Vec<Vec<Complex32>> = objects
        .iter()
        .map(|o| {
            let mut b: Vec<Complex32> = o.iter().map(|&v| Complex32::new(v, 0.0)).collect();
            fft2(&mut b, n, n, false);
            b
        })
        .collect();
    let mut out: Vec<Vec<Frame>> = vec![Vec::new(); objects.len()];
    for _ in 0..cfg.frames {
        let screen = kolmogorov_screen(&mut rng, r0);
        let ox = (rng.uniform() * (GRID - PUPIL) as f32) as usize;
        let oy = (rng.uniform() * (GRID - PUPIL) as f32) as usize;
        let s = otf(&pupil_phase(&screen, ox, oy, &pix), &pix);
        let h = transfer(&s, cfg.cutoff / fs, n);
        let shift = (
            cfg.jitter * (2.0 * rng.uniform() - 1.0),
            cfg.jitter * (2.0 * rng.uniform() - 1.0),
        );
        for (o, f) in spectra.iter().enumerate() {
            let mut buf = vec![Complex32::new(0.0, 0.0); n * n];
            for ky in 0..n {
                let fy = signed(ky, n) / n as f32;
                for kx in 0..n {
                    let fx = signed(kx, n) / n as f32;
                    let ph = -std::f32::consts::TAU * (fx * shift.0 * fs + fy * shift.1 * fs);
                    buf[ky * n + kx] =
                        f[ky * n + kx] * h[ky * n + kx] * Complex32::new(ph.cos(), ph.sin());
                }
            }
            fft2(&mut buf, n, n, true);
            let scale = 1.0 / (n * n) as f32;
            let area = (factor * factor) as f32;
            let pixels = (0..TILE * TILE)
                .map(|i| {
                    let (x, y) = (i % TILE, i / TILE);
                    let mut sum = 0.0;
                    for dy in 0..factor {
                        for dx in 0..factor {
                            sum += buf[(y * factor + dy) * n + x * factor + dx].re;
                        }
                    }
                    sum * scale / area + cfg.noise * rng.gauss()
                })
                .collect();
            out[o].push(Frame { pixels, shift });
        }
    }
    out
}
