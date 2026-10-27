// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Kolmogorov phase screens and the optical transfer function they give, used
//! to make the table of [`super::table`] and to build synthetic bursts with a
//! known Fried parameter for the tests.
//!
//! Lengths are in pupil pixels: the pupil is [`PUPIL`] pixels across, so a
//! Fried parameter `r0` of `PUPIL / rho` pixels means `D / r0 = rho`.
//!
//! # References
//!
//! - Noll, R. J. (1976), Zernike polynomials and atmospheric turbulence, J. Opt. Soc. Am. 66, 207-211,
//!   doi:10.1364/JOSA.66.000207 (the Kolmogorov phase spectrum).
//! - Lane, R. G., Glindemann, A. and Dainty, J. C. (1992), Simulation of a Kolmogorov phase screen, Waves
//!   in Random Media 2, 209-224, doi:10.1088/0959-7174/2/3/003 (FFT screens and subharmonics).

#![allow(
    clippy::many_single_char_names,
    reason = "x, y, n, m and the frequencies are the natural names in this maths"
)]

use realfft::num_complex::Complex32;
use rustfft::FftPlanner;

/// Pupil diameter in grid pixels.
pub const PUPIL: usize = 64;
/// Side of the phase screen grid.
pub const GRID: usize = 256;
/// Side of the grid the transfer function is computed on (twice the pupil).
pub const OTF_GRID: usize = 2 * PUPIL;

/// Deterministic xorshift64* generator with Box-Muller normals.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    /// Seeded generator.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `[0, 1)`.
    pub fn uniform(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Standard normal.
    pub fn gauss(&mut self) -> f32 {
        let u1 = self.uniform().max(1e-9);
        let u2 = self.uniform();
        (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos()
    }
}

/// Phase power spectral density at spatial frequency `f` (cycles per pixel)
/// for a Fried parameter of `r0` pixels.
fn psd(f: f32, r0: f32) -> f32 {
    0.023 * r0.powf(-5.0 / 3.0) * f.powf(-11.0 / 3.0)
}

/// A Kolmogorov phase screen of [`GRID`] x [`GRID`] pixels, in radians, with a
/// Fried parameter of `r0` pixels. The FFT screen is completed at low
/// frequencies with three levels of subharmonics (Lane et al. 1992), so that
/// the structure function holds out to the size of the pupil.
#[must_use]
pub fn kolmogorov_screen(rng: &mut Rng, r0: f32) -> Vec<f32> {
    let n = GRID;
    let df = 1.0 / n as f32;
    let mut spec = vec![Complex32::new(0.0, 0.0); n * n];
    for ky in 0..n {
        let fy = signed(ky, n) as f32 * df;
        for kx in 0..n {
            let fx = signed(kx, n) as f32 * df;
            let f = fx.hypot(fy);
            if f == 0.0 {
                continue;
            }
            let a = psd(f, r0).sqrt() * df;
            spec[ky * n + kx] = Complex32::new(rng.gauss() * a, rng.gauss() * a);
        }
    }
    let mut planner = FftPlanner::<f32>::new();
    let ifft = planner.plan_fft_inverse(n);
    for row in spec.chunks_mut(n) {
        ifft.process(row);
    }
    let mut col = vec![Complex32::new(0.0, 0.0); n];
    for x in 0..n {
        for y in 0..n {
            col[y] = spec[y * n + x];
        }
        ifft.process(&mut col);
        for y in 0..n {
            spec[y * n + x] = col[y];
        }
    }
    let mut screen: Vec<f32> = spec.iter().map(|c| c.re).collect();
    // Subharmonics: a 3 x 3 grid of frequencies at a third of the previous step.
    let mut sub = vec![0.0f32; n * n];
    for p in 1..=3 {
        let dfp = df / 3.0f32.powi(p);
        for jy in -1i32..=1 {
            for jx in -1i32..=1 {
                if jx == 0 && jy == 0 {
                    continue;
                }
                let (fx, fy) = (jx as f32 * dfp, jy as f32 * dfp);
                let a = psd(fx.hypot(fy), r0).sqrt() * dfp;
                let (cr, ci) = (rng.gauss() * a, rng.gauss() * a);
                for y in 0..n {
                    for x in 0..n {
                        let ph = std::f32::consts::TAU * (fx * x as f32 + fy * y as f32);
                        sub[y * n + x] += cr * ph.cos() - ci * ph.sin();
                    }
                }
            }
        }
    }
    let m = sub.iter().sum::<f32>() / (n * n) as f32;
    for (s, b) in screen.iter_mut().zip(&sub) {
        *s += b - m;
    }
    screen
}

fn signed(k: usize, n: usize) -> i64 {
    if k <= n / 2 {
        k as i64
    } else {
        k as i64 - n as i64
    }
}

/// Pixels of the circular pupil as `(x, y)` offsets from its centre, with the
/// pupil `PUPIL` pixels across.
#[must_use]
pub fn pupil_pixels() -> Vec<(usize, usize)> {
    let c = (PUPIL as f32 - 1.0) / 2.0;
    let r2 = (PUPIL as f32 / 2.0).powi(2);
    (0..PUPIL)
        .flat_map(|y| (0..PUPIL).map(move |x| (x, y)))
        .filter(|&(x, y)| (x as f32 - c).powi(2) + (y as f32 - c).powi(2) <= r2)
        .collect()
}

/// The pupil's phase for one screen: the `PUPIL` x `PUPIL` window at
/// `(ox, oy)`, with piston and tilt removed over the pupil (the picture is
/// registered on its features, which removes the tilt).
#[must_use]
pub fn pupil_phase(screen: &[f32], ox: usize, oy: usize, pix: &[(usize, usize)]) -> Vec<f32> {
    let c = (PUPIL as f32 - 1.0) / 2.0;
    let at = |x: usize, y: usize| screen[(oy + y) * GRID + ox + x];
    let n = pix.len() as f32;
    let mean = pix.iter().map(|&(x, y)| at(x, y)).sum::<f32>() / n;
    let (mut sx, mut sy, mut sxx, mut syy) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    for &(x, y) in pix {
        let (dx, dy) = (x as f32 - c, y as f32 - c);
        let v = at(x, y) - mean;
        sx += v * dx;
        sy += v * dy;
        sxx += dx * dx;
        syy += dy * dy;
    }
    let (gx, gy) = (sx / sxx, sy / syy);
    let mut out = vec![0.0f32; PUPIL * PUPIL];
    for &(x, y) in pix {
        let (dx, dy) = (x as f32 - c, y as f32 - c);
        out[y * PUPIL + x] = at(x, y) - mean - gx * dx - gy * dy;
    }
    out
}

/// Optical transfer function of the pupil with `phase`, as an
/// [`OTF_GRID`] x [`OTF_GRID`] array indexed by lag (cyclic, zero lag at 0),
/// normalised to 1 at zero lag. The autocorrelation of the pupil function.
#[must_use]
pub fn otf(phase: &[f32], pix: &[(usize, usize)]) -> Vec<Complex32> {
    let n = OTF_GRID;
    let mut u = vec![Complex32::new(0.0, 0.0); n * n];
    for &(x, y) in pix {
        let p = phase[y * PUPIL + x];
        u[y * n + x] = Complex32::new(p.cos(), p.sin());
    }
    let mut planner = FftPlanner::<f32>::new();
    let (fwd, inv) = (planner.plan_fft_forward(n), planner.plan_fft_inverse(n));
    fft2(&mut u, n, &*fwd);
    for c in &mut u {
        *c = Complex32::new(c.norm_sqr(), 0.0);
    }
    fft2(&mut u, n, &*inv);
    let norm = u[0].re.max(1e-12);
    u.iter().map(|c| *c / norm).collect()
}

fn fft2(buf: &mut [Complex32], n: usize, plan: &dyn rustfft::Fft<f32>) {
    for row in buf.chunks_mut(n) {
        plan.process(row);
    }
    let mut col = vec![Complex32::new(0.0, 0.0); n];
    for x in 0..n {
        for y in 0..n {
            col[y] = buf[y * n + x];
        }
        plan.process(&mut col);
        for y in 0..n {
            buf[y * n + x] = col[y];
        }
    }
}

/// Bins of the radial frequency `u = f / f_c` between 0 and 1 in the table.
pub const U_BINS: usize = 32;

/// Radial bin (`0..U_BINS`) of the lag `(kx, ky)` of an [`OTF_GRID`] array, or
/// `None` beyond the cut-off.
#[must_use]
pub fn lag_bin(kx: usize, ky: usize) -> Option<usize> {
    let r = (signed(kx, OTF_GRID) as f32).hypot(signed(ky, OTF_GRID) as f32);
    let u = r / PUPIL as f32;
    (u < 1.0).then_some((u * U_BINS as f32) as usize)
}

/// The expected ratio `|<S>|^2 / <|S|^2>` of the tilt-free transfer function
/// of a Kolmogorov atmosphere with `D / r0 = rho`, by Monte Carlo over `n`
/// screens, per radial bin of `u = f / f_c` ([`U_BINS`] of them).
#[must_use]
pub fn ratio_curve(rho: f32, n: usize, seed: u64) -> Vec<f32> {
    let pix = pupil_pixels();
    let r0 = PUPIL as f32 / rho;
    let mut rng = Rng::new(seed);
    let len = OTF_GRID * OTF_GRID;
    let mut mean = vec![Complex32::new(0.0, 0.0); len];
    let mut power = vec![0.0f64; len];
    for _ in 0..n {
        let screen = kolmogorov_screen(&mut rng, r0);
        let ox = (rng.uniform() * (GRID - PUPIL) as f32) as usize;
        let oy = (rng.uniform() * (GRID - PUPIL) as f32) as usize;
        let s = otf(&pupil_phase(&screen, ox, oy, &pix), &pix);
        for (i, c) in s.iter().enumerate() {
            mean[i] += *c;
            power[i] += f64::from(c.norm_sqr());
        }
    }
    let nf = n as f64;
    let (mut num, mut den, mut cnt) = (
        vec![0.0f64; U_BINS],
        vec![0.0f64; U_BINS],
        vec![0u32; U_BINS],
    );
    for ky in 0..OTF_GRID {
        for kx in 0..OTF_GRID {
            let Some(b) = lag_bin(kx, ky) else { continue };
            let i = ky * OTF_GRID + kx;
            let p = power[i] / nf;
            let a = (nf * f64::from((mean[i] / n as f32).norm_sqr()) - p) / (nf - 1.0);
            num[b] += a;
            den[b] += p;
            cnt[b] += 1;
        }
    }
    (0..U_BINS)
        .map(|b| (num[b] / den[b].max(1e-30)).max(0.0) as f32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilt_removed_phase_variance_matches_noll() {
        // Noll (1976): 0.134 (D / r0)^(5/3) rad^2 with piston and tilt removed.
        let pix = pupil_pixels();
        for rho in [2.0f32, 6.0, 12.0] {
            let r0 = PUPIL as f32 / rho;
            let mut rng = Rng::new(5);
            let (mut var, n) = (0.0f64, 160);
            for _ in 0..n {
                let s = kolmogorov_screen(&mut rng, r0);
                let ox = (rng.uniform() * (GRID - PUPIL) as f32) as usize;
                let oy = (rng.uniform() * (GRID - PUPIL) as f32) as usize;
                let ph = pupil_phase(&s, ox, oy, &pix);
                var += pix
                    .iter()
                    .map(|&(x, y)| f64::from(ph[y * PUPIL + x].powi(2)))
                    .sum::<f64>()
                    / pix.len() as f64;
            }
            let (got, want) = (var / f64::from(n), 0.134 * f64::from(rho).powf(5.0 / 3.0));
            eprintln!("rho {rho}: {got:.3} vs Noll {want:.3}");
            assert!(
                (got - want).abs() / want < 0.05,
                "rho {rho}: {got} vs {want}"
            );
        }
    }

    #[test]
    fn a_flat_pupil_has_unit_transfer_inside_the_cutoff() {
        let pix = pupil_pixels();
        let s = otf(&vec![0.0; PUPIL * PUPIL], &pix);
        assert!((s[0].re - 1.0).abs() < 1e-5);
        // Diffraction-limited OTF of a circle at half the cut-off: about 0.39.
        let half = s[PUPIL / 2].re;
        assert!((half - 0.39).abs() < 0.03, "{half}");
        assert!(s[PUPIL].re.abs() < 1e-3, "{}", s[PUPIL].re);
    }

    #[test]
    fn the_ratio_curve_falls_with_the_turbulence_and_with_frequency() {
        let weak = ratio_curve(2.0, 120, 1);
        let strong = ratio_curve(8.0, 120, 1);
        eprintln!("rho 2: {:?}", &weak[..12]);
        eprintln!("rho 8: {:?}", &strong[..12]);
        assert!(weak[6] > 5.0 * strong[6]);
        assert!(weak[1] > 0.5);
        assert!(strong[10] < strong[2]);
    }
}
