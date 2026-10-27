// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Global translation by FFT phase correlation.
//!
//! The frame is mean-subtracted, Hann-windowed and zero-padded to a 5-smooth
//! size. The whitened cross-power spectrum is band-passed (a mild Gaussian
//! high-pass removes residual illumination gradients, a low-pass suppresses
//! noise; both are linear, so applying them to the cross-power spectrum equals
//! filtering each image first) and
//! inverse-transformed to find the integer peak; the position is then refined
//! by evaluating the correlation surface directly on a 3x3 grid around the
//! current estimate and fitting a parabola, repeated a few times so that the
//! parabola's bias vanishes as the residual shrinks.
//!
//! Frames are real, so only the half of the spectrum with non-negative
//! horizontal frequency is computed and stored (the other half is its complex
//! conjugate). It is kept transposed, indexed `[kx][ky]`, so the column
//! transforms and the refinement sums run over contiguous memory. Plans, the
//! reference spectrum, the filters and the working buffers are built once and
//! shared by every [`PhaseCorrelator::estimate`] call, from any thread.
//!
//! # References
//!
//! - Kuglin, C. D. and Hines, D. C. (1975), The phase correlation image alignment method, Proc. IEEE Int.
//!   Conf. on Cybernetics and Society, 163-165 (no DOI).
//! - Harris, F. J. (1978), On the use of windows for harmonic analysis with the discrete Fourier transform,
//!   Proc. IEEE 66, 51-83, doi:10.1109/PROC.1978.10837 (the Hann window).

use std::f32::consts::TAU;
use std::sync::{Arc, Mutex};

use rayon::prelude::*;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

use crate::align::Shift;
use crate::error::{Error, Result};
use crate::frame::Frame;
use crate::lanes::{sum_and_square_lanes, sum_lanes};
use crate::resample::{Kernel, shifted};

/// Tuning for [`PhaseCorrelator`].
#[derive(Clone, Copy, Debug)]
pub struct PhaseOptions {
    /// Gaussian low-pass sigma on the cross-power spectrum, cycles per pixel.
    pub lowpass: f32,
    /// Gaussian high-pass sigma complement, cycles per pixel (0 disables).
    pub highpass: f32,
    /// Whitening floor as a fraction of the mean cross-power magnitude.
    pub whiten_eps: f32,
    /// Parabolic refinement passes after the integer peak.
    pub refine_iterations: usize,
}

impl Default for PhaseOptions {
    fn default() -> Self {
        Self {
            lowpass: 0.12,
            highpass: 0.01,
            whiten_eps: 0.02,
            refine_iterations: 3,
        }
    }
}

/// Result of one registration.
#[derive(Clone, Copy, Debug)]
pub struct Estimate {
    /// Frame shift relative to the reference.
    pub shift: Shift,
    /// Peak height relative to the total cross-power (0..1).
    pub peak: f32,
    /// Peak-to-sidelobe ratio: (peak - mean) / std of the correlation surface
    /// outside an 11x11 box around the peak. Above roughly 8 is a solid lock;
    /// below 4 the estimate is not trustworthy.
    pub psr: f32,
}

/// Smallest 2-, 3-, 5-smooth integer at least `n`.
#[must_use]
pub fn next_fast_len(n: usize) -> usize {
    let mut m = n.max(1);
    loop {
        let mut r = m;
        for p in [2, 3, 5] {
            while r.is_multiple_of(p) {
                r /= p;
            }
        }
        if r == 1 {
            return m;
        }
        m += 1;
    }
}

/// Out-of-place transpose of a `rows` x `cols` row-major matrix.
fn transpose(src: &[Complex32], dst: &mut [Complex32], rows: usize, cols: usize) {
    const B: usize = 32;
    for r0 in (0..rows).step_by(B) {
        for c0 in (0..cols).step_by(B) {
            for r in r0..(r0 + B).min(rows) {
                for c in c0..(c0 + B).min(cols) {
                    dst[c * rows + r] = src[r * cols + c];
                }
            }
        }
    }
}

fn hann(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| 0.5 * (1.0 - (TAU * (i as f32 + 0.5) / n as f32).cos()))
        .collect()
}

/// Signed frequency index for bin `i` of an `n`-point transform.
fn signed(i: usize, n: usize) -> f32 {
    if i <= n / 2 {
        i as f32
    } else {
        i as f32 - n as f32
    }
}

/// Working memory for one estimate; a correlator keeps a few and lends them out.
struct Scratch {
    /// Row transforms, `[y][kx]`; later the inverse's column output.
    rows: Vec<Complex32>,
    /// Spectrum, `[kx][ky]`.
    cols: Vec<Complex32>,
    /// One zero-padded real row.
    real_row: Vec<f32>,
    fft: Vec<Complex32>,
    /// Cross-power magnitudes, `[kx][ky]`.
    mags: Vec<f32>,
    /// Filtered cross-power spectrum as separate real and imaginary planes, `[kx][ky]`.
    planes: [Vec<f32>; 2],
    /// Correlation surface, `[y][x]`, unnormalised.
    surface: Vec<f32>,
}

/// The tables and sizes of a [`PhaseCorrelator`], for a backend that runs the
/// same registration elsewhere. All spectra are over the half plane of
/// non-negative horizontal frequency, `nx / 2 + 1` columns of `ny` values,
/// stored column by column (`[kx][ky]`).
#[derive(Clone, Copy, Debug)]
pub struct PhaseGeometry<'a> {
    /// Frame width.
    pub width: usize,
    /// Frame height.
    pub height: usize,
    /// Transform length along x (5-smooth, at least the width).
    pub nx: usize,
    /// Transform length along y.
    pub ny: usize,
    /// Hann window along x (`width` values).
    pub win_x: &'a [f32],
    /// Hann window along y (`height` values).
    pub win_y: &'a [f32],
    /// Gaussian low-pass per column (`nx / 2 + 1` values).
    pub low_x: &'a [f32],
    /// Gaussian low-pass per row (`ny` values).
    pub low_y: &'a [f32],
    /// Complement of the high-pass per column: 1 at DC, falling away.
    pub stop_x: &'a [f32],
    /// Complement of the high-pass per row.
    pub stop_y: &'a [f32],
    /// Whitening floor as a fraction of the mean cross-power magnitude.
    pub whiten_eps: f32,
    /// Parabolic refinement passes after the integer peak.
    pub refine_iterations: usize,
}

/// Registers frames against a fixed reference.
pub struct PhaseCorrelator {
    width: usize,
    height: usize,
    nx: usize,
    ny: usize,
    /// Columns of the half spectrum, `nx / 2 + 1`.
    nxh: usize,
    row_fwd: Arc<dyn RealToComplex<f32>>,
    row_inv: Arc<dyn ComplexToReal<f32>>,
    col_fwd: Arc<dyn Fft<f32>>,
    col_inv: Arc<dyn Fft<f32>>,
    win_x: Vec<f32>,
    win_y: Vec<f32>,
    /// Low-pass and "one minus high-pass" factors per half-plane column / per row.
    low_x: Vec<f32>,
    low_y: Vec<f32>,
    stop_x: Vec<f32>,
    stop_y: Vec<f32>,
    ref_spec: Vec<Complex32>,
    pool: Mutex<Vec<Scratch>>,
    opts: PhaseOptions,
}

impl PhaseCorrelator {
    /// Prepare the reference spectrum.
    ///
    /// # Errors
    /// Never currently; reserved for size limits.
    pub fn new(reference: &Frame, opts: PhaseOptions) -> Result<Self> {
        let (w, h) = (reference.width, reference.height);
        let (nx, ny) = (next_fast_len(w), next_fast_len(h));
        let nxh = nx / 2 + 1;
        let mut real = RealFftPlanner::<f32>::new();
        let mut cplx = FftPlanner::<f32>::new();
        let inv_two_s2 = 1.0 / (2.0 * opts.lowpass * opts.lowpass);
        let hp = opts.highpass;
        let low = |n: usize, count: usize| -> Vec<f32> {
            (0..count)
                .map(|i| (-(signed(i, n) / n as f32).powi(2) * inv_two_s2).exp())
                .collect()
        };
        let stop = |n: usize, count: usize| -> Vec<f32> {
            (0..count)
                .map(|i| {
                    if hp > 0.0 {
                        // Complement of the high-pass: 1 at DC, falling away.
                        (-(signed(i, n) / n as f32).powi(2) / (2.0 * hp * hp)).exp()
                    } else {
                        0.0
                    }
                })
                .collect()
        };
        let mut c = Self {
            width: w,
            height: h,
            nx,
            ny,
            nxh,
            row_fwd: real.plan_fft_forward(nx),
            row_inv: real.plan_fft_inverse(nx),
            col_fwd: cplx.plan_fft_forward(ny),
            col_inv: cplx.plan_fft_inverse(ny),
            win_x: hann(w),
            win_y: hann(h),
            low_x: low(nx, nxh),
            low_y: low(ny, ny),
            stop_x: stop(nx, nxh),
            stop_y: stop(ny, ny),
            ref_spec: Vec::new(),
            pool: Mutex::new(Vec::new()),
            opts,
        };
        let mut s = c.new_scratch();
        c.spectrum(reference, &mut s);
        c.ref_spec = s.cols.clone();
        c.give_back(s);
        Ok(c)
    }

    /// Sizes and filter tables, see [`PhaseGeometry`].
    #[must_use]
    pub fn geometry(&self) -> PhaseGeometry<'_> {
        PhaseGeometry {
            width: self.width,
            height: self.height,
            nx: self.nx,
            ny: self.ny,
            win_x: &self.win_x,
            win_y: &self.win_y,
            low_x: &self.low_x,
            low_y: &self.low_y,
            stop_x: &self.stop_x,
            stop_y: &self.stop_y,
            whiten_eps: self.opts.whiten_eps,
            refine_iterations: self.opts.refine_iterations,
        }
    }

    /// The reference's windowed half spectrum as interleaved (re, im) pairs,
    /// `[kx][ky]`.
    #[must_use]
    pub fn reference_spectrum(&self) -> Vec<f32> {
        self.ref_spec.iter().flat_map(|c| [c.re, c.im]).collect()
    }

    fn new_scratch(&self) -> Scratch {
        let half = self.nxh * self.ny;
        let zero = Complex32::new(0.0, 0.0);
        let fft_len = [
            self.row_fwd.get_scratch_len(),
            self.row_inv.get_scratch_len(),
            self.col_fwd.get_inplace_scratch_len(),
            self.col_inv.get_inplace_scratch_len(),
        ]
        .into_iter()
        .max()
        .unwrap_or(0);
        Scratch {
            rows: vec![zero; half],
            cols: vec![zero; half],
            real_row: vec![0.0; self.nx],
            fft: vec![zero; fft_len],
            mags: vec![0.0; half],
            planes: [vec![0.0; half], vec![0.0; half]],
            surface: vec![0.0; self.nx * self.ny],
        }
    }

    fn take(&self) -> Scratch {
        let reused = self.pool.lock().ok().and_then(|mut p| p.pop());
        reused.unwrap_or_else(|| self.new_scratch())
    }

    fn give_back(&self, s: Scratch) {
        if let Ok(mut p) = self.pool.lock() {
            p.push(s);
        }
    }

    /// Weight of half-plane column `kx` in sums over the whole plane: columns
    /// that are their own mirror image count once, the rest twice.
    fn column_weight(&self, kx: usize) -> f32 {
        if kx == 0 || (self.nx.is_multiple_of(2) && kx == self.nx / 2) {
            1.0
        } else {
            2.0
        }
    }

    /// Windowed half spectrum of `frame` into `s.cols`, `[kx][ky]`.
    fn spectrum(&self, frame: &Frame, s: &mut Scratch) {
        let (nxh, ny) = (self.nxh, self.ny);
        let mean = frame.mean();
        let Scratch {
            rows,
            cols,
            real_row,
            fft,
            ..
        } = s;
        for (y, out) in rows.chunks_exact_mut(nxh).enumerate() {
            if y >= self.height {
                out.fill(Complex32::new(0.0, 0.0));
                continue;
            }
            let wy = self.win_y[y];
            for ((o, &v), &wx) in real_row.iter_mut().zip(frame.row(y)).zip(&self.win_x) {
                *o = (v - mean) * wy * wx;
            }
            real_row[self.width..].fill(0.0);
            // Cannot fail: the buffers have the planned lengths.
            let _ = self.row_fwd.process_with_scratch(real_row, out, fft);
        }
        transpose(rows, cols, ny, nxh);
        self.col_fwd.process_with_scratch(cols, fft);
    }

    /// Estimate the shift of `frame` relative to the reference.
    ///
    /// # Errors
    /// [`Error::Invalid`] if the frame size differs from the reference.
    pub fn estimate(&self, frame: &Frame) -> Result<Estimate> {
        if frame.width != self.width || frame.height != self.height {
            return Err(Error::Invalid(format!(
                "frame {}x{} does not match reference {}x{}",
                frame.width, frame.height, self.width, self.height
            )));
        }
        let mut s = self.take();
        let est = self.estimate_in(frame, &mut s);
        self.give_back(s);
        est
    }

    fn estimate_in(&self, frame: &Frame, s: &mut Scratch) -> Result<Estimate> {
        let (nx, ny, nxh) = (self.nx, self.ny, self.nxh);
        self.spectrum(frame, s);
        let total = self.whiten(s);

        // Integer peak from the inverse transform.
        self.col_inv.process_with_scratch(&mut s.cols, &mut s.fft);
        transpose(&s.cols, &mut s.rows, nxh, ny);
        for (row, out) in s
            .rows
            .chunks_exact_mut(nxh)
            .zip(s.surface.chunks_exact_mut(nx))
        {
            // Conjugate-symmetric input: these bins are real up to rounding.
            row[0].im = 0.0;
            if nx.is_multiple_of(2) {
                row[nxh - 1].im = 0.0;
            }
            self.row_inv
                .process_with_scratch(row, out, &mut s.fft)
                .map_err(|e| Error::Invalid(format!("inverse transform failed: {e}")))?;
        }
        let best_i = argmax(&s.surface);
        let psr = peak_to_sidelobe(&s.surface, nx, ny, best_i);
        let (bx, by) = (best_i % nx, best_i / nx);
        let mut est = Shift::new(signed(bx, nx), signed(by, ny));

        // Parabolic refinement. The first step reads the integer neighbours
        // from the surface itself; later steps sum the spectrum directly.
        let at = |dx: isize, dy: isize| {
            let x = (bx.cast_signed() + dx).rem_euclid(nx.cast_signed()) as usize;
            let y = (by.cast_signed() + dy).rem_euclid(ny.cast_signed()) as usize;
            s.surface[y * nx + x]
        };
        let mut q = Cross {
            centre: at(0, 0),
            left: at(-1, 0),
            right: at(1, 0),
            up: at(0, -1),
            down: at(0, 1),
        };
        for it in 0..self.opts.refine_iterations.max(1) {
            if it > 0 {
                q = self.sample_cross(&s.planes, est);
            }
            est.dx += parabola(q.left, q.centre, q.right);
            est.dy += parabola(q.up, q.centre, q.down);
        }
        let centre = self.sample_centre(&s.planes, est);
        Ok(Estimate {
            shift: est,
            peak: (centre / total.max(f32::MIN_POSITIVE)).clamp(0.0, 1.0),
            psr,
        })
    }

    /// Cross-power spectrum with the reference, whitened and band-passed, in
    /// place in `s.cols` and as planes in `s.planes`. Returns the summed
    /// magnitude over the whole (not just the half) plane.
    fn whiten(&self, s: &mut Scratch) -> f32 {
        let ny = self.ny;
        let Scratch {
            cols, mags, planes, ..
        } = s;
        let mut sum = 0.0f64;
        for (kx, ((col, rcol), mcol)) in cols
            .chunks_exact_mut(ny)
            .zip(self.ref_spec.chunks_exact(ny))
            .zip(mags.chunks_exact_mut(ny))
            .enumerate()
        {
            for ((c, r), m) in col.iter_mut().zip(rcol).zip(mcol.iter_mut()) {
                // F * conj(R)
                let (a, b) = (c.re, c.im);
                c.re = a * r.re + b * r.im;
                c.im = b * r.re - a * r.im;
                *m = (c.re * c.re + c.im * c.im).sqrt();
            }
            sum += f64::from(self.column_weight(kx)) * sum_lanes(mcol);
        }
        let mean_mag = sum / (self.nx * ny) as f64;
        let floor = self.opts.whiten_eps * mean_mag as f32;
        let [pre, pim] = planes;
        let mut total = 0.0f64;
        for (kx, (((col, mcol), re), im)) in cols
            .chunks_exact_mut(ny)
            .zip(mags.chunks_exact(ny))
            .zip(pre.chunks_exact_mut(ny))
            .zip(pim.chunks_exact_mut(ny))
            .enumerate()
        {
            let (gx, stop_x) = (self.low_x[kx], self.stop_x[kx]);
            let mut col_total = 0.0f32;
            for ((((c, &m), re), im), (&gy, &stop_y)) in col
                .iter_mut()
                .zip(mcol)
                .zip(re.iter_mut())
                .zip(im.iter_mut())
                .zip(self.low_y.iter().zip(&self.stop_y))
            {
                // 2-D high-pass from the separable pieces: 1 - stop_x * stop_y is
                // isotropic enough and zero at DC.
                let high = 1.0 - stop_x * stop_y;
                let scale = gy * gx * high / (m + floor).max(f32::MIN_POSITIVE);
                c.re *= scale;
                c.im *= scale;
                *re = c.re;
                *im = c.im;
                col_total += m * scale;
            }
            total += f64::from(self.column_weight(kx)) * f64::from(col_total);
        }
        total as f32
    }

    /// Real part of the inverse DFT of the cross-power spectrum at
    /// `est + (dx, dy)` for the five points of a cross: centre, left and right
    /// (offsets of one pixel in x), up and down.
    ///
    /// The sum over the half plane, columns weighted by
    /// [`Self::column_weight`], equals the sum over the whole plane.
    fn sample_cross(&self, planes: &[Vec<f32>; 2], est: Shift) -> Cross {
        let (nxh, ny) = (self.nxh, self.ny);
        let [pre, pim] = planes;
        // Row phasors for the centre row, and for one row up and down.
        let ey = |d: f32| -> (Vec<f32>, Vec<f32>) {
            (0..ny)
                .map(|k| {
                    let a = TAU * signed(k, ny) / ny as f32 * (est.dy + d);
                    (a.cos(), a.sin())
                })
                .unzip()
        };
        let (ey_re, ey_im) = ey(0.0);
        let (up_re, up_im) = ey(-1.0);
        let (down_re, down_im) = ey(1.0);
        let mut centre_row = [0.0f64; 3]; // x - 1, x, x + 1 on the centre row
        let (mut acc_re, mut acc_im) = (vec![0.0f32; ny], vec![0.0f32; ny]);
        for kx in 0..nxh {
            let (cre, cim) = (&pre[kx * ny..(kx + 1) * ny], &pim[kx * ny..(kx + 1) * ny]);
            let wk = self.column_weight(kx);
            // Pass A: collapse this column against the centre-row phasor.
            let (s_re, s_im) = complex_dot(cre, cim, &ey_re, &ey_im);
            let f = TAU * signed(kx, self.nx) / self.nx as f32;
            for (slot, d) in centre_row.iter_mut().zip([-1.0f32, 0.0, 1.0]) {
                let a = f * (est.dx + d);
                let (c, sn) = (a.cos(), a.sin());
                *slot += f64::from(wk * (s_re * c - s_im * sn));
            }
            // Pass B: accumulate columns against the centre-column phasor.
            let a = f * est.dx;
            let (xr, xi) = (wk * a.cos(), wk * a.sin());
            for (((ur, ui), &cr), &ci) in acc_re.iter_mut().zip(acc_im.iter_mut()).zip(cre).zip(cim)
            {
                *ur += cr * xr - ci * xi;
                *ui += cr * xi + ci * xr;
            }
        }
        Cross {
            centre: centre_row[1] as f32,
            left: centre_row[0] as f32,
            right: centre_row[2] as f32,
            up: complex_dot(&acc_re, &acc_im, &up_re, &up_im).0,
            down: complex_dot(&acc_re, &acc_im, &down_re, &down_im).0,
        }
    }

    /// Correlation surface value at `est` alone.
    fn sample_centre(&self, planes: &[Vec<f32>; 2], est: Shift) -> f32 {
        let ny = self.ny;
        let [pre, pim] = planes;
        let (ey_re, ey_im): (Vec<f32>, Vec<f32>) = (0..ny)
            .map(|k| {
                let a = TAU * signed(k, ny) / ny as f32 * est.dy;
                (a.cos(), a.sin())
            })
            .unzip();
        let mut sum = 0.0f64;
        for kx in 0..self.nxh {
            let (cre, cim) = (&pre[kx * ny..(kx + 1) * ny], &pim[kx * ny..(kx + 1) * ny]);
            let (s_re, s_im) = complex_dot(cre, cim, &ey_re, &ey_im);
            let a = TAU * signed(kx, self.nx) / self.nx as f32 * est.dx;
            sum += f64::from(self.column_weight(kx) * (s_re * a.cos() - s_im * a.sin()));
        }
        sum as f32
    }
}

/// Correlation surface values on a cross around the current estimate.
#[derive(Clone, Copy)]
struct Cross {
    centre: f32,
    left: f32,
    right: f32,
    up: f32,
    down: f32,
}

/// `sum(a * b)` for complex `a = (ar, ai)` and `b = (br, bi)`, as (re, im).
fn complex_dot(ar: &[f32], ai: &[f32], br: &[f32], bi: &[f32]) -> (f32, f32) {
    let mut lanes = [[0.0f32; 8]; 2];
    let n = ar.len() - ar.len() % 8;
    for (((a_r, a_i), b_r), b_i) in ar[..n]
        .as_chunks::<8>()
        .0
        .iter()
        .zip(ai[..n].as_chunks::<8>().0)
        .zip(br[..n].as_chunks::<8>().0)
        .zip(bi[..n].as_chunks::<8>().0)
    {
        let [re, im] = &mut lanes;
        for j in 0..8 {
            re[j] += a_r[j] * b_r[j] - a_i[j] * b_i[j];
            im[j] += a_r[j] * b_i[j] + a_i[j] * b_r[j];
        }
    }
    let (mut re, mut im) = (lanes[0].iter().sum::<f32>(), lanes[1].iter().sum::<f32>());
    for k in n..ar.len() {
        re += ar[k] * br[k] - ai[k] * bi[k];
        im += ar[k] * bi[k] + ai[k] * br[k];
    }
    (re, im)
}

/// Index of the first maximum of `x`.
fn argmax(x: &[f32]) -> usize {
    let mut lanes = [f32::NEG_INFINITY; 8];
    let (chunks, rest) = x.as_chunks::<8>();
    for c in chunks {
        for (l, &v) in lanes.iter_mut().zip(c) {
            *l = l.max(v);
        }
    }
    let tail = rest.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let best = lanes.into_iter().fold(tail, f32::max);
    x.iter().position(|&v| v >= best).unwrap_or(0)
}

/// Peak-to-sidelobe ratio of `surface` (row-major `ny` rows of `nx`) around
/// index `peak`: the statistics are those of everything outside the 11x11 box
/// (with wrap-around) centred on the peak.
fn peak_to_sidelobe(surface: &[f32], nx: usize, ny: usize, peak: usize) -> f32 {
    let (px, py) = (peak % nx, peak / nx);
    // Distinct box indices along one axis; small transforms wrap onto themselves.
    let axis = |p: usize, n: usize| -> Vec<usize> {
        let mut v: Vec<usize> = (0..11).map(|d| (p + n * 8 + d - 5) % n).collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    let (bx, by) = (axis(px, nx), axis(py, ny));
    let (all, all_sq) = sum_and_square_lanes(surface);
    let (mut s, mut s2) = (all, all_sq);
    for &y in &by {
        for &x in &bx {
            let v = f64::from(surface[y * nx + x]);
            s -= v;
            s2 -= v * v;
        }
    }
    let cnt = surface.len() - bx.len() * by.len();
    if cnt == 0 {
        return 0.0;
    }
    let mean = s / cnt as f64;
    let sd = (s2 / cnt as f64 - mean * mean).max(0.0).sqrt();
    ((f64::from(surface[peak]) - mean) / sd.max(1e-12)) as f32
}

/// Vertex offset of the parabola through three equally spaced samples around a maximum.
fn parabola(l: f32, c: f32, r: f32) -> f32 {
    let denom = l - 2.0 * c + r;
    if denom >= -1e-12 {
        return 0.0;
    }
    (0.5 * (l - r) / denom).clamp(-0.5, 0.5)
}

/// Several frames after registering them to the first one, combined into one:
/// their per-pixel median for three or more (so an artefact in one or two does
/// not reach the reference, issue #172), their mean for two. Used to
/// build the reference from the best few frames.
///
/// # Errors
/// [`Error::Invalid`] for an empty list or mismatched sizes.
pub fn build_reference(frames: &[&Frame], opts: PhaseOptions, kernel: Kernel) -> Result<Frame> {
    let Some(first) = frames.first() else {
        return Err(Error::Invalid("no frames to build a reference from".into()));
    };
    let mut acc = (*first).clone();
    if frames.len() == 1 {
        return Ok(acc);
    }
    let corr = PhaseCorrelator::new(first, opts)?;
    // Register in parallel, add in order so the result does not depend on scheduling.
    let aligned: Vec<Frame> = frames[1..]
        .par_iter()
        .map(|f| corr.estimate(f).map(|est| shifted(f, est.shift, kernel)))
        .collect::<Result<_>>()?;
    if frames.len() >= 3 {
        // Median: an artefact in one or two of the frames does not reach it.
        let mut column = vec![0.0f32; frames.len()];
        for (i, out) in acc.data.iter_mut().enumerate() {
            column[0] = first.data[i];
            for (c, a) in column[1..].iter_mut().zip(&aligned) {
                *c = a.data[i];
            }
            column.sort_by(f32::total_cmp);
            let m = column.len() / 2;
            *out = if column.len() % 2 == 1 {
                column[m]
            } else {
                f32::midpoint(column[m - 1], column[m])
            };
        }
        return Ok(acc);
    }
    for a in &aligned {
        acc.data
            .iter_mut()
            .zip(&a.data)
            .for_each(|(acc, v)| *acc += v);
    }
    let inv = 1.0 / frames.len() as f32;
    acc.data.iter_mut().for_each(|v| *v *= inv);
    Ok(acc)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The original full-complex implementation, kept as the oracle.
    #[allow(
        clippy::unnecessary_wraps,
        clippy::many_single_char_names,
        clippy::cast_possible_wrap,
        reason = "verbatim copy of the previous implementation"
    )]
    mod oracle {
        use std::f32::consts::TAU;
        use std::sync::Arc;

        use rustfft::num_complex::Complex32;
        use rustfft::{Fft, FftPlanner};

        use super::super::{Estimate, PhaseOptions, next_fast_len, parabola};
        use crate::align::Shift;
        use crate::error::{Error, Result};
        use crate::frame::Frame;

        /// Row-column 2-D FFT of an `ny` x `nx` row-major buffer.
        struct OracleFft2d {
            nx: usize,
            ny: usize,
            row: [Arc<dyn Fft<f32>>; 2],
            col: [Arc<dyn Fft<f32>>; 2],
        }

        impl OracleFft2d {
            fn new(nx: usize, ny: usize) -> Self {
                let mut p = FftPlanner::<f32>::new();
                Self {
                    nx,
                    ny,
                    row: [p.plan_fft_forward(nx), p.plan_fft_inverse(nx)],
                    col: [p.plan_fft_forward(ny), p.plan_fft_inverse(ny)],
                }
            }

            fn transpose(src: &[Complex32], dst: &mut [Complex32], rows: usize, cols: usize) {
                const B: usize = 32;
                for r0 in (0..rows).step_by(B) {
                    for c0 in (0..cols).step_by(B) {
                        for r in r0..(r0 + B).min(rows) {
                            for c in c0..(c0 + B).min(cols) {
                                dst[c * rows + r] = src[r * cols + c];
                            }
                        }
                    }
                }
            }

            /// Unnormalised transform in place; `tmp` is scratch of equal length.
            fn process(&self, buf: &mut [Complex32], tmp: &mut [Complex32], inverse: bool) {
                let i = usize::from(inverse);
                let mut scratch = vec![
                    Complex32::new(0.0, 0.0);
                    self.row[i]
                        .get_inplace_scratch_len()
                        .max(self.col[i].get_inplace_scratch_len())
                ];
                self.row[i].process_with_scratch(buf, &mut scratch);
                Self::transpose(buf, tmp, self.ny, self.nx);
                self.col[i].process_with_scratch(tmp, &mut scratch);
                Self::transpose(tmp, buf, self.nx, self.ny);
            }
        }

        fn oracle_hann(n: usize) -> Vec<f32> {
            (0..n)
                .map(|i| 0.5 * (1.0 - (TAU * (i as f32 + 0.5) / n as f32).cos()))
                .collect()
        }

        /// Signed frequency index for bin `i` of an `n`-point transform.
        fn oracle_signed(i: usize, n: usize) -> f32 {
            if i <= n / 2 {
                i as f32
            } else {
                i as f32 - n as f32
            }
        }

        /// Registers frames against a fixed reference.
        pub struct OraclePhase {
            width: usize,
            height: usize,
            fft: OracleFft2d,
            win_x: Vec<f32>,
            win_y: Vec<f32>,
            ref_spec: Vec<Complex32>,
            opts: PhaseOptions,
        }

        impl OraclePhase {
            /// Prepare the reference spectrum.
            ///
            /// # Errors
            /// Never currently; reserved for size limits.
            pub fn new(reference: &Frame, opts: PhaseOptions) -> Result<Self> {
                let (w, h) = (reference.width, reference.height);
                let fft = OracleFft2d::new(next_fast_len(w), next_fast_len(h));
                let mut c = Self {
                    width: w,
                    height: h,
                    fft,
                    win_x: oracle_hann(w),
                    win_y: oracle_hann(h),
                    ref_spec: Vec::new(),
                    opts,
                };
                c.ref_spec = c.spectrum(reference);
                Ok(c)
            }

            fn spectrum(&self, frame: &Frame) -> Vec<Complex32> {
                let (nx, ny) = (self.fft.nx, self.fft.ny);
                let mean = frame.mean();
                let mut buf = vec![Complex32::new(0.0, 0.0); nx * ny];
                for y in 0..self.height {
                    let wy = self.win_y[y];
                    let row = frame.row(y);
                    let out = &mut buf[y * nx..y * nx + self.width];
                    for ((o, &v), &wx) in out.iter_mut().zip(row).zip(&self.win_x) {
                        o.re = (v - mean) * wy * wx;
                    }
                }
                let mut tmp = vec![Complex32::new(0.0, 0.0); nx * ny];
                self.fft.process(&mut buf, &mut tmp, false);
                buf
            }

            /// Estimate the shift of `frame` relative to the reference.
            ///
            /// # Errors
            /// [`Error::Invalid`] if the frame size differs from the reference.
            pub fn estimate(&self, frame: &Frame) -> Result<Estimate> {
                if frame.width != self.width || frame.height != self.height {
                    return Err(Error::Invalid(format!(
                        "frame {}x{} does not match reference {}x{}",
                        frame.width, frame.height, self.width, self.height
                    )));
                }
                let (nx, ny) = (self.fft.nx, self.fft.ny);
                let mut cross = self.spectrum(frame);

                // Whiten and low-pass the cross-power spectrum F * conj(R).
                let mut mean_mag = 0.0f64;
                for (c, r) in cross.iter_mut().zip(&self.ref_spec) {
                    *c *= r.conj();
                    mean_mag += f64::from(c.norm());
                }
                let floor = self.opts.whiten_eps * (mean_mag / cross.len() as f64) as f32;
                let inv_two_s2 = 1.0 / (2.0 * self.opts.lowpass * self.opts.lowpass);
                let gx: Vec<f32> = (0..nx)
                    .map(|i| (-(oracle_signed(i, nx) / nx as f32).powi(2) * inv_two_s2).exp())
                    .collect();
                let gy: Vec<f32> = (0..ny)
                    .map(|i| (-(oracle_signed(i, ny) / ny as f32).powi(2) * inv_two_s2).exp())
                    .collect();
                let hp = self.opts.highpass;
                let hx: Vec<f32> = (0..nx)
                    .map(|i| {
                        if hp > 0.0 {
                            1.0 - (-(oracle_signed(i, nx) / nx as f32).powi(2) / (2.0 * hp * hp))
                                .exp()
                        } else {
                            1.0
                        }
                    })
                    .collect();
                let hy: Vec<f32> = (0..ny)
                    .map(|i| {
                        if hp > 0.0 {
                            1.0 - (-(oracle_signed(i, ny) / ny as f32).powi(2) / (2.0 * hp * hp))
                                .exp()
                        } else {
                            1.0
                        }
                    })
                    .collect();
                for (iy, row) in cross.chunks_exact_mut(nx).enumerate() {
                    for (ix, c) in row.iter_mut().enumerate() {
                        let m = c.norm();
                        // 2-D radial high-pass from the separable pieces: 1 - (1-hx)(1-hy) keeps it isotropic enough and zero at DC.
                        let high = 1.0 - (1.0 - hx[ix]) * (1.0 - hy[iy]);
                        *c *= gy[iy] * gx[ix] * high / (m + floor).max(f32::MIN_POSITIVE);
                    }
                }

                // Integer peak from the inverse transform.
                let mut surface = cross.clone();
                let mut tmp = vec![Complex32::new(0.0, 0.0); nx * ny];
                self.fft.process(&mut surface, &mut tmp, true);
                drop(tmp);
                let (mut best, mut best_i) = (f32::NEG_INFINITY, 0usize);
                for (i, c) in surface.iter().enumerate() {
                    if c.re > best {
                        best = c.re;
                        best_i = i;
                    }
                }
                let psr = oracle_psr(&surface, nx, ny, best_i);
                drop(surface);
                let mut est = Shift::new(
                    oracle_signed(best_i % nx, nx),
                    oracle_signed(best_i / nx, ny),
                );

                // Direct evaluation on a 3x3 grid, parabolic update.
                for _ in 0..self.opts.refine_iterations.max(1) {
                    let q = self.sample_grid(&cross, est);
                    est.dx += parabola(q[1][0], q[1][1], q[1][2]);
                    est.dy += parabola(q[0][1], q[1][1], q[2][1]);
                }
                let q = self.sample_grid(&cross, est);
                let total: f32 = cross.iter().map(|c| c.norm()).sum();
                Ok(Estimate {
                    shift: est,
                    peak: (q[1][1] / total.max(f32::MIN_POSITIVE)).clamp(0.0, 1.0),
                    psr,
                })
            }

            /// Real part of the inverse DFT of `cross` at `est + (i-1, j-1)`; indexed `[j][i]`.
            fn sample_grid(&self, cross: &[Complex32], est: Shift) -> [[f32; 3]; 3] {
                let (nx, ny) = (self.fft.nx, self.fft.ny);
                let phasors = |n: usize, centre: f32| -> [Vec<Complex32>; 3] {
                    [-1.0f32, 0.0, 1.0].map(|d| {
                        (0..n)
                            .map(|k| {
                                let a = TAU * oracle_signed(k, n) / n as f32 * (centre + d);
                                Complex32::new(a.cos(), a.sin())
                            })
                            .collect()
                    })
                };
                let ex = phasors(nx, est.dx);
                let ey = phasors(ny, est.dy);
                let mut q = [[0.0f32; 3]; 3];
                for (iy, row) in cross.chunks_exact(nx).enumerate() {
                    for (i, e) in ex.iter().enumerate() {
                        let s: Complex32 = row.iter().zip(e).map(|(c, p)| c * p).sum();
                        for (j, ey_j) in ey.iter().enumerate() {
                            q[j][i] += (s * ey_j[iy]).re;
                        }
                    }
                }
                q
            }
        }

        /// Peak-to-sidelobe ratio of the real part of `surface` around index `peak`.
        fn oracle_psr(surface: &[Complex32], nx: usize, ny: usize, peak: usize) -> f32 {
            let (px, py) = (peak % nx, peak / nx);
            let wrap = |a: usize, b: usize, n: usize| {
                let d = a.abs_diff(b);
                d.min(n - d)
            };
            let (mut s, mut s2, mut cnt) = (0.0f64, 0.0f64, 0u64);
            for (i, c) in surface.iter().enumerate() {
                let (x, y) = (i % nx, i / nx);
                if wrap(x, px, nx) <= 5 && wrap(y, py, ny) <= 5 {
                    continue;
                }
                let v = f64::from(c.re);
                s += v;
                s2 += v * v;
                cnt += 1;
            }
            if cnt == 0 {
                return 0.0;
            }
            let mean = s / cnt as f64;
            let sd = (s2 / cnt as f64 - mean * mean).max(0.0).sqrt();
            ((f64::from(surface[peak].re) - mean) / sd.max(1e-12)) as f32
        }
    }

    use crate::filter::gaussian_blur;
    use crate::testutil::{CaptureConfig, Rng, SyntheticCapture, SyntheticConfig, SyntheticSun};

    #[test]
    fn fast_len_is_smooth() {
        assert_eq!(next_fast_len(1), 1);
        assert_eq!(next_fast_len(7), 8);
        assert_eq!(next_fast_len(1936), 1944);
        assert_eq!(next_fast_len(1216), 1250);
    }

    #[test]
    fn recovers_known_shifts_within_a_tenth_of_a_pixel() {
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let reference = sun.render_clean(Shift::default(), None);
        let corr = PhaseCorrelator::new(&reference, PhaseOptions::default()).expect("correlator");
        let mut rng = Rng::new(5);
        let mut worst = 0.0f32;
        for i in 0..24 {
            let s = Shift::new(
                (rng.uniform() * 2.0 - 1.0) * 7.0,
                (rng.uniform() * 2.0 - 1.0) * 7.0,
            );
            let mut f = gaussian_blur(&sun.render_clean(s, None), 0.5 + 0.1 * i as f32);
            f.data.iter_mut().for_each(|v| *v += 0.01 * rng.gauss());
            let e = corr.estimate(&f).expect("estimate");
            let err = (e.shift - s).length();
            worst = worst.max(err);
            assert!(err < 0.1, "shift {s:?} got {:?} err {err}", e.shift);
        }
        eprintln!("worst global shift error: {worst:.4} px");
    }

    #[test]
    fn shift_sign_matches_convention() {
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let reference = sun.render_clean(Shift::default(), None);
        let corr = PhaseCorrelator::new(&reference, PhaseOptions::default()).expect("correlator");
        let s = Shift::new(3.0, -2.0);
        let e = corr.estimate(&sun.render_clean(s, None)).expect("estimate");
        assert!((e.shift - s).length() < 0.05, "{:?}", e.shift);
        assert!(e.peak > 0.0);
        assert!(e.psr > 8.0, "psr {}", e.psr);
        let noise = Frame {
            width: reference.width,
            height: reference.height,
            data: (0..reference.data.len())
                .map(|i| ((i * 7919 % 1013) as f32) / 1013.0)
                .collect(),
            bayer: None,
        };
        let n = corr.estimate(&noise).expect("estimate");
        assert!(n.psr < e.psr * 0.7, "noise psr {} vs lock {}", n.psr, e.psr);
    }

    /// Textured, shifted copies of one scene (some with noise).
    fn pairs() -> Vec<(Frame, Frame, Shift)> {
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let reference = sun.render_clean(Shift::default(), None);
        let mut rng = Rng::new(11);
        (0..6)
            .map(|i| {
                let s = Shift::new(
                    (rng.uniform() * 2.0 - 1.0) * 9.0,
                    (rng.uniform() * 2.0 - 1.0) * 9.0,
                );
                let mut f = gaussian_blur(&sun.render_clean(s, None), 0.4 + 0.3 * i as f32);
                f.data.iter_mut().for_each(|v| *v += 0.02 * rng.gauss());
                (reference.clone(), f, s)
            })
            .collect()
    }

    #[test]
    fn half_spectrum_path_agrees_with_the_full_complex_oracle() {
        for (reference, frame, truth) in pairs() {
            let new = PhaseCorrelator::new(&reference, PhaseOptions::default())
                .expect("new")
                .estimate(&frame)
                .expect("estimate");
            let old = oracle::OraclePhase::new(&reference, PhaseOptions::default())
                .expect("oracle")
                .estimate(&frame)
                .expect("oracle estimate");
            let d = (new.shift - old.shift).length();
            eprintln!(
                "truth {truth:?}: new {:?} old {:?} diff {d:.5}; peak {:.5} vs {:.5}; psr {:.3} vs {:.3}",
                new.shift, old.shift, new.peak, old.peak, new.psr, old.psr
            );
            assert!(d < 2e-3, "shift differs by {d}");
            assert!((new.peak - old.peak).abs() < 1e-3 * old.peak.max(1e-3));
            assert!((new.psr - old.psr).abs() < 1e-2 * old.psr.abs().max(1.0));
        }
    }

    #[test]
    fn odd_and_unequal_sizes_agree_with_the_oracle() {
        // 5-smooth sizes with odd padded lengths exercise the odd real transform.
        for (w, h) in [(45usize, 27usize), (50, 36), (75, 45)] {
            let mk = |dx: f32, dy: f32| Frame {
                width: w,
                height: h,
                data: (0..w * h)
                    .map(|i| {
                        let (x, y) = ((i % w) as f32 - dx, (i / w) as f32 - dy);
                        let r =
                            ((x - w as f32 / 2.0).powi(2) + (y - h as f32 / 2.0).powi(2)).sqrt();
                        (-(r * r) / 90.0).exp() * (1.0 + 0.3 * (x * 0.7).sin() * (y * 0.5).cos())
                    })
                    .collect(),
                bayer: None,
            };
            let reference = mk(0.0, 0.0);
            let frame = mk(1.7, -2.2);
            let opts = PhaseOptions::default();
            let new = PhaseCorrelator::new(&reference, opts)
                .expect("new")
                .estimate(&frame)
                .expect("estimate");
            let old = oracle::OraclePhase::new(&reference, opts)
                .expect("oracle")
                .estimate(&frame)
                .expect("oracle estimate");
            assert!(
                (new.shift - old.shift).length() < 2e-3,
                "{w}x{h}: {:?} vs {:?}",
                new.shift,
                old.shift
            );
        }
    }

    #[test]
    fn concurrent_estimates_share_one_correlator() {
        let ps = pairs();
        let corr = PhaseCorrelator::new(&ps[0].0, PhaseOptions::default()).expect("new");
        let serial: Vec<Shift> = ps
            .iter()
            .map(|(_, f, _)| corr.estimate(f).expect("estimate").shift)
            .collect();
        let parallel: Vec<Shift> = ps
            .par_iter()
            .map(|(_, f, _)| corr.estimate(f).expect("estimate").shift)
            .collect();
        assert_eq!(serial, parallel);
    }

    #[test]
    fn rejects_mismatched_sizes() {
        let a = Frame::zeros(16, 16);
        let corr = PhaseCorrelator::new(&a, PhaseOptions::default()).expect("correlator");
        assert!(corr.estimate(&Frame::zeros(16, 17)).is_err());
    }

    #[test]
    fn reference_from_top_frames_is_sharper_than_noise_level() {
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let cap = SyntheticCapture::generate(
            &sun,
            &CaptureConfig {
                frames: 12,
                ..CaptureConfig::default()
            },
        );
        let mut order: Vec<usize> = (0..12).collect();
        order.sort_by(|&a, &b| cap.sigmas[a].total_cmp(&cap.sigmas[b]));
        let top: Vec<&Frame> = order[..5].iter().map(|&i| &cap.frames[i]).collect();
        let r =
            build_reference(&top, PhaseOptions::default(), Kernel::Lanczos3).expect("reference");
        // Registered mean sits at the first frame's position; a naive mean of
        // the same frames (shifts up to 6 px) must be much further from it.
        let truth = sun.render_clean(cap.shifts[order[0]], None);
        let mut naive = top[0].clone();
        for f in &top[1..] {
            naive
                .data
                .iter_mut()
                .zip(&f.data)
                .for_each(|(a, b)| *a += b);
        }
        naive.data.iter_mut().for_each(|v| *v /= top.len() as f32);
        let (ref_err, naive_err) = (
            crate::testutil::rmse(&r, &truth, 12),
            crate::testutil::rmse(&naive, &truth, 12),
        );
        assert!(
            ref_err < 0.5 * naive_err && ref_err < 0.03,
            "ref {ref_err} naive {naive_err}"
        );
        assert!(build_reference(&[], PhaseOptions::default(), Kernel::Lanczos3).is_err());
    }

    #[test]
    fn reference_median_leaves_out_an_artefact_in_one_frame() {
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let clean = sun.render_clean(Shift::default(), None);
        let mut hit = clean.clone();
        let w = hit.width;
        for y in 0..hit.height {
            for x in 100..104 {
                hit.data[y * w + x] = 1.0;
            }
        }
        let frames = [&clean, &clean, &hit, &clean, &clean];
        let r =
            build_reference(&frames, PhaseOptions::default(), Kernel::Lanczos3).expect("reference");
        let worst = r
            .data
            .iter()
            .zip(&clean.data)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(worst < 1e-3, "the line reached the reference: {worst}");
    }
}
