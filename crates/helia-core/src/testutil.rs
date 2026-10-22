// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Synthetic solar captures with known ground truth.
//!
//! The model: a limb-darkened disc with band-limited granulation and a few
//! sunspots (the "truth"). Each frame is the truth displaced by a known
//! global shift plus a smooth local warp, blurred by a known Gaussian and
//! degraded with Gaussian noise. Frame `i` satisfies
//! `frame(x) = truth(x - shift - warp(x))`, so the content of frame `i` sits at
//! `shift + warp` relative to the truth, matching [`Shift`]'s convention.
//!
//! Seeing correlates blur with distortion: the warp amplitude grows with the
//! blur sigma, so the sharpest frames are also the least distorted.

#![allow(
    clippy::many_single_char_names,
    reason = "x, y, w, h, dx, dy and r, g, b are the natural names in image and kernel maths"
)]

use std::io::Cursor;

use crate::align::Shift;
use crate::filter::gaussian_blur;
use crate::frame::{BayerPattern, Frame, Rect};
use crate::io::ser::{ColorId, SerHeader, SerWriter};
use crate::resample::{Kernel, sample};

/// 3x5 bitmaps of the digits, row-major, one bit per pixel.
const DIGITS: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111],
    [0b010, 0b110, 0b010, 0b010, 0b111],
    [0b111, 0b001, 0b111, 0b100, 0b111],
    [0b111, 0b001, 0b111, 0b001, 0b111],
    [0b101, 0b101, 0b111, 0b001, 0b001],
    [0b111, 0b100, 0b111, 0b001, 0b111],
    [0b111, 0b100, 0b111, 0b101, 0b111],
    [0b111, 0b001, 0b001, 0b001, 0b001],
    [0b111, 0b101, 0b111, 0b101, 0b111],
    [0b111, 0b101, 0b111, 0b001, 0b111],
];

/// Burn a `SharpCap`-style timestamp into the top-left corner of `frame`: ten
/// fixed digits and four that change with `index`, white (1.0) pixels on the
/// existing sky, each glyph scaled 2x. Returns the rectangle the text covers.
pub fn burn_timestamp(frame: &mut Frame, index: usize) -> Rect {
    const CELL: usize = 8;
    const FIXED: [usize; 10] = [2, 0, 2, 2, 0, 7, 1, 7, 1, 4];
    let changing = (index * 37 + 1234) % 10_000;
    let digits: Vec<usize> = FIXED
        .iter()
        .copied()
        .chain([1000, 100, 10, 1].map(|d| changing / d % 10))
        .collect();
    let (x0, y0) = (4, 4);
    for (g, &d) in digits.iter().enumerate() {
        for (row, bits) in DIGITS[d].iter().enumerate() {
            for col in 0..3 {
                if bits >> (2 - col) & 1 == 1 {
                    for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                        let (x, y) = (x0 + g * CELL + col * 2 + dx, y0 + row * 2 + dy);
                        if x < frame.width && y < frame.height {
                            frame.data[y * frame.width + x] = 1.0;
                        }
                    }
                }
            }
        }
    }
    Rect::new(x0, y0, digits.len() * CELL - 2, 10)
}

/// Deterministic xorshift64* generator.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    /// Seeded generator.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    /// Next raw value.
    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `[0, 1)`.
    pub fn uniform(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Standard normal (Box-Muller).
    pub fn gauss(&mut self) -> f32 {
        let u1 = self.uniform().max(1e-9);
        let u2 = self.uniform();
        (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos()
    }
}

/// Parameters of the clean disc image.
#[derive(Clone, Debug)]
pub struct SyntheticConfig {
    /// Image width.
    pub width: usize,
    /// Image height.
    pub height: usize,
    /// Random seed.
    pub seed: u64,
    /// Disc radius as a fraction of the shorter side.
    pub radius_frac: f32,
    /// Limb darkening coefficient.
    pub limb_u: f32,
    /// Granulation contrast.
    pub granulation: f32,
    /// Granulation correlation length (Gaussian sigma, pixels).
    pub granule_sigma: f32,
    /// Number of sunspots.
    pub spots: usize,
    /// Sky background level.
    pub background: f32,
}

impl Default for SyntheticConfig {
    fn default() -> Self {
        Self {
            width: 192,
            height: 160,
            seed: 7,
            radius_frac: 0.35,
            limb_u: 0.6,
            granulation: 0.12,
            granule_sigma: 1.8,
            spots: 3,
            background: 0.02,
        }
    }
}

/// Smooth displacement field applied on top of a global shift.
#[derive(Clone, Copy, Debug, Default)]
pub struct Warp {
    /// Peak x displacement in pixels.
    pub ax: f32,
    /// Peak y displacement in pixels.
    pub ay: f32,
    /// Spatial wavelength in pixels (0 disables the warp).
    pub wavelength: f32,
    /// Phases of the two components.
    pub phase: [f32; 2],
}

impl Warp {
    /// Displacement at `(x, y)`.
    #[must_use]
    pub fn at(&self, x: f32, y: f32) -> (f32, f32) {
        if self.wavelength <= 0.0 {
            return (0.0, 0.0);
        }
        let k = std::f32::consts::TAU / self.wavelength;
        (
            self.ax * (k * (0.7 * x + 0.3 * y) + self.phase[0]).sin(),
            self.ay * (k * (-0.3 * x + 0.7 * y) + self.phase[1]).sin(),
        )
    }
}

/// The clean, continuous-looking ground truth.
#[derive(Clone, Debug)]
pub struct SyntheticSun {
    truth: Frame,
    /// Disc centre.
    pub centre: (f32, f32),
    /// Disc radius in pixels.
    pub radius: f32,
}

impl SyntheticSun {
    /// Build the truth image.
    #[must_use]
    pub fn new(cfg: &SyntheticConfig) -> Self {
        let (w, h) = (cfg.width, cfg.height);
        let mut rng = Rng::new(cfg.seed);
        let radius = cfg.radius_frac * w.min(h) as f32;
        let centre = (w as f32 / 2.0, h as f32 / 2.0);

        let mut noise = Frame::zeros(w, h);
        noise.data.iter_mut().for_each(|v| *v = rng.gauss());
        let mut g = gaussian_blur(&noise, cfg.granule_sigma);
        let mean = g.mean();
        let var = g.data.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / g.data.len() as f32;
        let inv = 1.0 / var.sqrt().max(1e-9);
        g.data.iter_mut().for_each(|v| *v = (*v - mean) * inv);

        let spots: Vec<(f32, f32, f32, f32)> = (0..cfg.spots)
            .map(|_| {
                let ang = rng.uniform() * std::f32::consts::TAU;
                let rr = rng.uniform().sqrt() * 0.6 * radius;
                (
                    centre.0 + rr * ang.cos(),
                    centre.1 + rr * ang.sin(),
                    3.0 + 3.0 * rng.uniform(),
                    0.5 + 0.3 * rng.uniform(),
                )
            })
            .collect();

        let mut truth = Frame::zeros(w, h);
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = (x as f32 - centre.0, y as f32 - centre.1);
                let rho = (dx * dx + dy * dy).sqrt() / radius;
                let edge = ((1.0 - rho) * radius / 3.0 + 0.5).clamp(0.0, 1.0);
                let mask = edge * edge * (3.0 - 2.0 * edge);
                let mu = (1.0 - rho * rho).max(0.0).sqrt();
                let mut v = 1.0 - cfg.limb_u * (1.0 - mu);
                v *= 1.0 + cfg.granulation * g.data[y * w + x] * (0.4 + 0.6 * mu);
                for &(sx, sy, s, depth) in &spots {
                    let d2 = (x as f32 - sx).powi(2) + (y as f32 - sy).powi(2);
                    v *= 1.0 - depth * (-d2 / (2.0 * s * s)).exp();
                    v *= 1.0 - 0.25 * (-d2 / (2.0 * (2.2 * s).powi(2))).exp();
                }
                truth.data[y * w + x] = cfg.background + (1.0 - cfg.background) * mask * v;
            }
        }
        Self {
            truth,
            centre,
            radius,
        }
    }

    /// The unshifted, unblurred truth image.
    #[must_use]
    pub fn truth(&self) -> &Frame {
        &self.truth
    }

    /// Truth displaced by `shift` and `warp`, with no blur or noise.
    #[must_use]
    pub fn render_clean(&self, shift: Shift, warp: Option<&Warp>) -> Frame {
        let (w, h) = (self.truth.width, self.truth.height);
        let mut out = Frame::zeros(w, h);
        for y in 0..h {
            for x in 0..w {
                let (wx, wy) = warp.map_or((0.0, 0.0), |p| p.at(x as f32, y as f32));
                out.data[y * w + x] = sample(
                    &self.truth,
                    x as f32 - shift.dx - wx,
                    y as f32 - shift.dy - wy,
                    Kernel::Lanczos3,
                );
            }
        }
        out
    }
}

/// Parameters of a synthetic capture.
#[derive(Clone, Debug)]
pub struct CaptureConfig {
    /// Number of frames.
    pub frames: usize,
    /// Smallest blur sigma.
    pub blur_min: f32,
    /// Largest blur sigma.
    pub blur_max: f32,
    /// Global shifts are uniform in `[-max_shift, max_shift]` on each axis.
    pub max_shift: f32,
    /// Warp amplitude at `blur_max`; scales linearly down to `warp_floor` times that at `blur_min`.
    pub warp_amp: f32,
    /// Warp wavelength in pixels.
    pub warp_wavelength: f32,
    /// Fraction of `warp_amp` applied even to the sharpest frame; the rest
    /// scales with blur.
    pub warp_floor: f32,
    /// The sharpest `warp_free` frames get no warp (good seeing is also
    /// undistorted), so a reference built from them has clean geometry.
    pub warp_free: usize,
    /// Gaussian noise sigma.
    pub noise: f32,
    /// Random seed.
    pub seed: u64,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            frames: 40,
            blur_min: 0.6,
            blur_max: 3.0,
            max_shift: 6.0,
            warp_amp: 0.0,
            warp_wavelength: 110.0,
            warp_floor: 0.0,
            warp_free: 0,
            noise: 0.01,
            seed: 11,
        }
    }
}

/// Frames plus everything needed to check results against known answers.
#[derive(Clone, Debug)]
pub struct SyntheticCapture {
    /// The clean source.
    pub sun: SyntheticSun,
    /// Degraded frames.
    pub frames: Vec<Frame>,
    /// Global shift of each frame relative to the truth.
    pub shifts: Vec<Shift>,
    /// Local warp of each frame.
    pub warps: Vec<Warp>,
    /// Blur sigma of each frame.
    pub sigmas: Vec<f32>,
}

impl SyntheticCapture {
    /// Generate frames; deterministic for a given configuration.
    #[must_use]
    pub fn generate(sun: &SyntheticSun, cfg: &CaptureConfig) -> Self {
        let mut rng = Rng::new(cfg.seed);
        let n = cfg.frames;
        // Evenly spread sigmas, shuffled so quality is not ordered by index.
        let mut sigmas: Vec<f32> = (0..n)
            .map(|i| {
                cfg.blur_min + (cfg.blur_max - cfg.blur_min) * i as f32 / (n.max(2) - 1) as f32
            })
            .collect();
        for i in (1..n).rev() {
            let j = (rng.next_u64() % (i as u64 + 1)) as usize;
            sigmas.swap(i, j);
        }
        let mut frames = Vec::with_capacity(n);
        let mut shifts = Vec::with_capacity(n);
        let mut warps = Vec::with_capacity(n);
        let mut sorted = sigmas.clone();
        sorted.sort_by(f32::total_cmp);
        let free_below = sorted.get(cfg.warp_free).copied().unwrap_or(f32::INFINITY);
        for &sigma in &sigmas {
            let shift = Shift {
                dx: (rng.uniform() * 2.0 - 1.0) * cfg.max_shift,
                dy: (rng.uniform() * 2.0 - 1.0) * cfg.max_shift,
            };
            let t = (sigma - cfg.blur_min) / (cfg.blur_max - cfg.blur_min).max(1e-6);
            let amp = if sigma < free_below {
                0.0
            } else {
                cfg.warp_amp * (cfg.warp_floor + (1.0 - cfg.warp_floor) * t)
            };
            let sgn = |r: f32| if r < 0.5 { -1.0 } else { 1.0 };
            let warp = Warp {
                ax: amp * sgn(rng.uniform()) * (0.6 + 0.4 * rng.uniform()),
                ay: amp * sgn(rng.uniform()) * (0.6 + 0.4 * rng.uniform()),
                wavelength: if cfg.warp_amp > 0.0 {
                    cfg.warp_wavelength
                } else {
                    0.0
                },
                phase: [
                    rng.uniform() * std::f32::consts::TAU,
                    rng.uniform() * std::f32::consts::TAU,
                ],
            };
            let clean = sun.render_clean(shift, Some(&warp));
            let mut f = gaussian_blur(&clean, sigma);
            f.data
                .iter_mut()
                .for_each(|v| *v += cfg.noise * rng.gauss());
            frames.push(f);
            shifts.push(shift);
            warps.push(warp);
        }
        Self {
            sun: sun.clone(),
            frames,
            shifts,
            warps,
            sigmas,
        }
    }

    /// Index of the least blurred frame.
    #[must_use]
    pub fn sharpest(&self) -> usize {
        self.sigmas
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.total_cmp(b.1))
            .map_or(0, |(i, _)| i)
    }
}

/// Root-mean-square difference ignoring a `border` of pixels.
///
/// # Panics
/// If the frames differ in size.
#[must_use]
pub fn rmse(a: &Frame, b: &Frame, border: usize) -> f32 {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let (mut s, mut n) = (0.0f64, 0usize);
    for y in border..a.height.saturating_sub(border) {
        for x in border..a.width.saturating_sub(border) {
            let d = f64::from(a.get(x, y) - b.get(x, y));
            s += d * d;
            n += 1;
        }
    }
    (s / n.max(1) as f64).sqrt() as f32
}

/// Encode frames as a 16-bit mono SER file in memory.
///
/// # Panics
/// Never for frames of equal size; panics on a writer error, which cannot
/// occur for an in-memory cursor and a valid header.
#[must_use]
pub fn ser_bytes(frames: &[Frame]) -> Vec<u8> {
    let first = &frames[0];
    let hdr = SerHeader::new(first.width as u32, first.height as u32, ColorId::Mono, 16);
    let mut w = SerWriter::new(Cursor::new(Vec::new()), hdr).expect("valid header");
    for f in frames {
        w.write_frame(f, None).expect("in-memory write");
    }
    w.finish().expect("in-memory finish").into_inner()
}

/// Deterministic noise in -1..1.
fn noise(state: &mut u64) -> f32 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    ((*state >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
}

/// A limb-darkened disc on a dark sky, mosaicked with per-channel gains.
/// `gains` are (R, G, B).
#[must_use]
pub fn mosaic_disc(
    w: usize,
    h: usize,
    pattern: Option<BayerPattern>,
    gains: [f32; 3],
    seed: u64,
) -> Frame {
    let mut s = seed;
    let (cx, cy, rad) = (w as f32 * 0.5, h as f32 * 0.5, h as f32 * 0.38);
    let mut data = vec![0.0; w * h];
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (x as f32 - cx, y as f32 - cy);
            let d = (dx * dx + dy * dy).sqrt() / rad;
            let scene = if d < 1.0 {
                0.9 * (1.0 - 0.4 * d * d) + 0.02 * (x as f32 * 0.7).sin()
            } else {
                0.01
            };
            let g = pattern.map_or(1.0, |p| gains[p.channel_at(x, y)]);
            data[y * w + x] = (scene * g + 0.004 * noise(&mut s)).clamp(0.0, 1.0);
        }
    }
    Frame {
        width: w,
        height: h,
        data,
        bayer: None,
    }
}

/// As [`ser_bytes_with`], with a trailer of timestamps: frame `i` is stamped
/// `start + i * step` (100 ns ticks since 0001-01-01).
///
/// # Panics
/// Never for frames of equal size matching the header.
#[must_use]
pub fn ser_bytes_stamped(frames: &[Frame], header: SerHeader, start: i64, step: i64) -> Vec<u8> {
    let mut w = SerWriter::new(Cursor::new(Vec::new()), header).expect("valid header");
    for (i, f) in frames.iter().enumerate() {
        w.write_frame(
            f,
            Some(start + i64::try_from(i).expect("small index") * step),
        )
        .expect("in-memory write");
    }
    w.finish().expect("in-memory finish").into_inner()
}

/// Encode frames as an in-memory SER file with `header` (its frame count is
/// filled in). Use it to make captures whose header claims something about
/// the data, true or not.
///
/// # Panics
/// Never for frames of equal size matching the header.
#[must_use]
pub fn ser_bytes_with(frames: &[Frame], header: SerHeader) -> Vec<u8> {
    let mut w = SerWriter::new(Cursor::new(Vec::new()), header).expect("valid header");
    for f in frames {
        w.write_frame(f, None).expect("in-memory write");
    }
    w.finish().expect("in-memory finish").into_inner()
}

/// What is wrong with a synthetic sensor.
#[derive(Clone, Debug)]
pub struct DefectConfig {
    /// Fall-off of the gain at the corners (0.3 leaves 70 per cent there).
    pub vignette: f32,
    /// Dust motes: shadows of random size and position.
    pub dust: usize,
    /// Depth of the deepest mote (0.15 removes 15 per cent of the light).
    pub dust_depth: f32,
    /// Amplitude of concentric fringes in the gain (0 for none).
    pub rings: f32,
    /// Spacing of the fringes in pixels.
    pub ring_period: f32,
    /// Pixel-to-pixel gain spread (photo-response non-uniformity).
    pub prnu: f32,
    /// Pixel-to-pixel spread of the offset (fixed-pattern noise).
    pub fpn: f32,
    /// Column-to-column spread of the offset.
    pub column: f32,
    /// Hot pixels, and how far above their surroundings they read.
    pub hot: usize,
    /// Excess of a hot pixel.
    pub hot_level: f32,
    /// Cold (dead) pixels that read their offset whatever the light.
    pub cold: usize,
}

impl Default for DefectConfig {
    fn default() -> Self {
        Self {
            vignette: 0.30,
            dust: 6,
            dust_depth: 0.15,
            rings: 0.0,
            ring_period: 14.0,
            prnu: 0.01,
            fpn: 0.004,
            column: 0.002,
            hot: 12,
            hot_level: 0.25,
            cold: 6,
        }
    }
}

/// A sensor with a fixed gain and offset at every pixel, and the frames it
/// would record of a dark and of a flat field. `raw = light * gain + offset`.
#[derive(Clone, Debug)]
pub struct SensorDefects {
    /// Width.
    pub width: usize,
    /// Height.
    pub height: usize,
    /// Multiplicative pattern (vignetting, dust, fringes, response).
    pub gain: Vec<f32>,
    /// Additive pattern (fixed pattern, column pattern, hot pixels).
    pub offset: Vec<f32>,
    /// Indices of the hot pixels.
    pub hot: Vec<usize>,
    /// Indices of the cold pixels.
    pub cold: Vec<usize>,
}

impl SensorDefects {
    /// Draw a sensor; deterministic for a given seed.
    #[must_use]
    pub fn new(w: usize, h: usize, seed: u64, cfg: &DefectConfig) -> Self {
        let mut rng = Rng::new(seed);
        let (cx, cy) = ((w as f32 - 1.0) / 2.0, (h as f32 - 1.0) / 2.0);
        let rmax = (cx * cx + cy * cy).sqrt();
        let motes: Vec<(f32, f32, f32, f32)> = (0..cfg.dust)
            .map(|_| {
                (
                    rng.uniform() * w as f32,
                    rng.uniform() * h as f32,
                    3.0 + 5.0 * rng.uniform(),
                    cfg.dust_depth * (0.3 + 0.7 * rng.uniform()),
                )
            })
            .collect();
        let columns: Vec<f32> = (0..w).map(|_| cfg.column * rng.gauss()).collect();
        let mut gain = vec![1.0f32; w * h];
        let mut offset = vec![0.0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = (x as f32 - cx, y as f32 - cy);
                let r = (dx * dx + dy * dy).sqrt();
                let rho = r / rmax;
                let mut g = 1.0 - cfg.vignette * rho * rho;
                if cfg.rings > 0.0 {
                    g *= 1.0 + cfg.rings * (std::f32::consts::TAU * r / cfg.ring_period).cos();
                }
                for &(mx, my, s, depth) in &motes {
                    let d2 = (x as f32 - mx).powi(2) + (y as f32 - my).powi(2);
                    g *= 1.0 - depth * (-d2 / (2.0 * s * s)).exp();
                }
                g *= 1.0 + cfg.prnu * rng.gauss();
                gain[y * w + x] = g;
                offset[y * w + x] = 0.02 + cfg.fpn * rng.gauss() + columns[x];
            }
        }
        let pick = |n: usize, rng: &mut Rng| -> Vec<usize> {
            let mut v: Vec<usize> = Vec::new();
            while v.len() < n {
                let (x, y) = (
                    2 + (rng.next_u64() as usize) % (w - 4),
                    2 + (rng.next_u64() as usize) % (h - 4),
                );
                let i = y * w + x;
                // Keep them apart: a cluster is a different problem.
                if v.iter().all(|&j| {
                    let (jx, jy) = (j % w, j / w);
                    jx.abs_diff(x) > 4 || jy.abs_diff(y) > 4
                }) {
                    v.push(i);
                }
            }
            v.sort_unstable();
            v
        };
        let hot = pick(cfg.hot, &mut rng);
        let cold = pick(cfg.cold, &mut rng);
        for &i in &hot {
            offset[i] += cfg.hot_level;
        }
        for &i in &cold {
            gain[i] = 0.0;
        }
        Self {
            width: w,
            height: h,
            gain,
            offset,
            hot,
            cold,
        }
    }

    /// What the sensor records of `light` (a frame of the same size).
    pub fn apply(&self, light: &mut Frame) {
        for ((v, &g), &o) in light.data.iter_mut().zip(&self.gain).zip(&self.offset) {
            *v = (*v * g + o).clamp(0.0, 1.0);
        }
    }

    /// `n` frames with the lens capped: the offset and read noise.
    #[must_use]
    pub fn dark_frames(&self, n: usize, noise: f32, seed: u64) -> Vec<Frame> {
        let mut rng = Rng::new(seed);
        (0..n)
            .map(|_| Frame {
                width: self.width,
                height: self.height,
                data: self
                    .offset
                    .iter()
                    .map(|&o| (o + noise * rng.gauss()).clamp(0.0, 1.0))
                    .collect(),
                bayer: None,
            })
            .collect()
    }

    /// `n` frames of an even field at `level`, whose brightness drifts by up
    /// to 3 per cent from frame to frame as a lamp or the sky does.
    #[must_use]
    pub fn flat_frames(&self, n: usize, level: f32, noise: f32, seed: u64) -> Vec<Frame> {
        let mut rng = Rng::new(seed);
        (0..n)
            .map(|_| {
                let l = level * (1.0 + 0.03 * (rng.uniform() * 2.0 - 1.0));
                Frame {
                    width: self.width,
                    height: self.height,
                    data: self
                        .gain
                        .iter()
                        .zip(&self.offset)
                        .map(|(&g, &o)| (l * g + o + noise * rng.gauss()).clamp(0.0, 1.0))
                        .collect(),
                    bayer: None,
                }
            })
            .collect()
    }
}

/// Turn mono frames into raw colour ones: the scene as each of red, green and
/// blue sees it, which is the frame scaled by `colours` and moved by
/// `dispersion` (a plane whose content sits that many pixels to the right and
/// down), sampled through a Bayer mosaic of `pattern`.
#[must_use]
pub fn colour_mosaic(
    frames: &[Frame],
    colours: [f32; 3],
    dispersion: [crate::align::Shift; 3],
    pattern: BayerPattern,
) -> Vec<Frame> {
    use crate::align::Shift;
    use crate::resample::{Kernel, shifted};
    frames
        .iter()
        .map(|f| {
            let planes: Vec<Frame> = (0..3)
                .map(|c| {
                    let d = dispersion[c];
                    let mut p = if d.dx == 0.0 && d.dy == 0.0 {
                        f.clone()
                    } else {
                        shifted(f, Shift::new(-d.dx, -d.dy), Kernel::Lanczos3)
                    };
                    for v in &mut p.data {
                        *v *= colours[c];
                    }
                    p
                })
                .collect();
            let mut data = vec![0.0f32; f.width * f.height];
            for y in 0..f.height {
                for x in 0..f.width {
                    data[y * f.width + x] = planes[pattern.channel_at(x, y)].data[y * f.width + x];
                }
            }
            Frame {
                width: f.width,
                height: f.height,
                data,
                bayer: Some(pattern),
            }
        })
        .collect()
}
