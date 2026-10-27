// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The relative position of two panels from their overlap.
//!
//! Two panels of one mosaic share a strip of the Sun. Both are first turned
//! into a picture of their detail only: the difference of two Gaussian blurs,
//! divided by its own local size, so that a difference of gain or a sloping
//! background does not matter. The strip is then found in two steps.
//!
//! 1. **A search.** On pictures shrunk to about 128 pixels, for each of a few
//!    trial rotations of the second panel, the masked normalised
//!    cross-correlation (Padfield 2012: the correlation of only the pixels the
//!    two panels share, normalised by their own sums) over every shift is
//!    computed with FFTs. The best peak gives a rotation and a shift to within a
//!    pixel or two of the shrunk picture.
//! 2. **A refinement.** From there, a Gauss-Newton fit of shift and rotation
//!    (the Lucas-Kanade method) on the pyramid of the pictures, from a factor of
//!    8 down to full size, brings the agreement to a small fraction of a pixel.
//!
//! The transform found maps a pixel `p` of the second panel to the first as
//! `R(theta) (p - c) + c + t`, with `c` the centre of the second panel.
//!
//! # References
//!
//! - Padfield, D. (2012), Masked object registration in the Fourier domain, IEEE Trans. Image Process. 21,
//!   2706-2718, doi:10.1109/TIP.2011.2181402.
//! - Lucas, B. D. and Kanade, T. (1981), An iterative image registration technique with an application to
//!   stereo vision, Proc. 7th Int. Joint Conf. on Artificial Intelligence, 674-679 (no DOI).

#![allow(
    clippy::many_single_char_names,
    reason = "x, y, u, v, t and the matrix entries are the natural names in this geometry"
)]
#![allow(
    clippy::similar_names,
    reason = "a, b and their masks and squares are the names of the formulas"
)]

use rayon::prelude::*;
use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};
use std::sync::Arc;

use crate::align::global::next_fast_len;
use crate::filter::gaussian_blur;
use crate::frame::Frame;

/// Trial rotations are this far apart in the search, degrees.
const ANGLE_STEP_DEG: f64 = 0.75;
/// The shrunk pictures of the search are about this many pixels on their short side.
const SEARCH_SIZE: usize = 128;
/// Peaks kept at each trial rotation.
const PEAKS_PER_ANGLE: usize = 3;
/// Candidates refined in full.
const CANDIDATES: usize = 6;
/// Gauss-Newton iterations at each level of the pyramid.
const LK_ITERATIONS: usize = 12;
/// Pixels read for the fit at each level are at most this many.
const LK_SAMPLES: usize = 120_000;

/// A rotation about the centre of the second panel and a shift.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rigid {
    /// Shift in x, pixels.
    pub tx: f64,
    /// Shift in y, pixels.
    pub ty: f64,
    /// Rotation in radians (clockwise on screen, with y down).
    pub theta: f64,
}

impl Rigid {
    /// Where pixel `(x, y)` of the second panel falls in the first.
    #[must_use]
    pub fn apply(&self, x: f64, y: f64, centre: (f64, f64)) -> (f64, f64) {
        let (s, c) = self.theta.sin_cos();
        let (u, v) = (x - centre.0, y - centre.1);
        (
            c * u - s * v + centre.0 + self.tx,
            s * u + c * v + centre.1 + self.ty,
        )
    }
}

/// A panel ready to be matched: its detail, normalised.
#[derive(Clone, Debug)]
pub struct Prepared {
    /// Width.
    pub width: usize,
    /// Height.
    pub height: usize,
    /// Local-contrast-normalised detail.
    pub data: Vec<f32>,
}

impl Prepared {
    /// Centre of the panel in pixel coordinates.
    #[must_use]
    pub fn centre(&self) -> (f64, f64) {
        (
            (self.width as f64 - 1.0) / 2.0,
            (self.height as f64 - 1.0) / 2.0,
        )
    }
}

/// The detail of a panel: a difference of Gaussians over its local size.
#[must_use]
pub fn prepare(luma: &Frame) -> Prepared {
    let fine = gaussian_blur(luma, 1.2);
    let coarse = gaussian_blur(luma, 7.0);
    let dog = Frame {
        width: luma.width,
        height: luma.height,
        data: fine
            .data
            .iter()
            .zip(&coarse.data)
            .map(|(a, b)| a - b)
            .collect(),
        bayer: None,
    };
    let sq = Frame {
        width: dog.width,
        height: dog.height,
        data: dog.data.iter().map(|v| v * v).collect(),
        bayer: None,
    };
    let var = gaussian_blur(&sq, 12.0);
    let mean_var = var.data.iter().map(|&v| f64::from(v)).sum::<f64>() / var.data.len() as f64;
    let eps = (0.1 * mean_var) as f32 + 1e-12;
    let data = dog
        .data
        .iter()
        .zip(&var.data)
        .map(|(d, v)| (d / (v + eps).sqrt()).clamp(-5.0, 5.0))
        .collect();
    Prepared {
        width: luma.width,
        height: luma.height,
        data,
    }
}

/// Box-average by an integer factor.
fn shrink(data: &[f32], w: usize, h: usize, f: usize) -> (Vec<f32>, usize, usize) {
    if f <= 1 {
        return (data.to_vec(), w, h);
    }
    let (nw, nh) = (w / f, h / f);
    let mut out = vec![0.0f32; nw * nh];
    for y in 0..nh {
        for x in 0..nw {
            let mut s = 0.0f32;
            for j in 0..f {
                let row = (y * f + j) * w + x * f;
                s += data[row..row + f].iter().sum::<f32>();
            }
            out[y * nw + x] = s / (f * f) as f32;
        }
    }
    (out, nw, nh)
}

fn bilinear(d: &[f32], w: usize, h: usize, x: f64, y: f64) -> f32 {
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = ((x - x0) as f32, (y - y0) as f32);
    let (xi, yi) = (x0 as usize, y0 as usize);
    let (x1, y1) = ((xi + 1).min(w - 1), (yi + 1).min(h - 1));
    let a = d[yi * w + xi] * (1.0 - fx) + d[yi * w + x1] * fx;
    let b = d[y1 * w + xi] * (1.0 - fx) + d[y1 * w + x1] * fx;
    a * (1.0 - fy) + b * fy
}

// ----------------------------------------------------------------- FFT

struct Fft2 {
    nx: usize,
    ny: usize,
    row_fwd: Arc<dyn Fft<f32>>,
    row_inv: Arc<dyn Fft<f32>>,
    col_fwd: Arc<dyn Fft<f32>>,
    col_inv: Arc<dyn Fft<f32>>,
}

impl Fft2 {
    fn new(nx: usize, ny: usize) -> Self {
        let mut p = FftPlanner::<f32>::new();
        Self {
            nx,
            ny,
            row_fwd: p.plan_fft_forward(nx),
            row_inv: p.plan_fft_inverse(nx),
            col_fwd: p.plan_fft_forward(ny),
            col_inv: p.plan_fft_inverse(ny),
        }
    }

    fn run(&self, data: &mut [Complex32], inverse: bool) {
        let (nx, ny) = (self.nx, self.ny);
        let (row, col) = if inverse {
            (&self.row_inv, &self.col_inv)
        } else {
            (&self.row_fwd, &self.col_fwd)
        };
        for r in data.chunks_exact_mut(nx) {
            row.process(r);
        }
        let mut t = vec![Complex32::new(0.0, 0.0); nx * ny];
        for y in 0..ny {
            for x in 0..nx {
                t[x * ny + y] = data[y * nx + x];
            }
        }
        for c in t.chunks_exact_mut(ny) {
            col.process(c);
        }
        for y in 0..ny {
            for x in 0..nx {
                data[y * nx + x] = t[x * ny + y];
            }
        }
    }

    /// Transform of `src` (size `w` x `h`) placed at the origin, zero-padded.
    fn forward(&self, src: &[f32], w: usize, h: usize) -> Vec<Complex32> {
        let mut d = vec![Complex32::new(0.0, 0.0); self.nx * self.ny];
        for y in 0..h {
            for x in 0..w {
                d[y * self.nx + x] = Complex32::new(src[y * w + x], 0.0);
            }
        }
        self.run(&mut d, false);
        d
    }

    /// `corr(s) = sum_x a(x + s) b(x)` from two transforms, real part, scaled.
    fn correlate(&self, fa: &[Complex32], fb: &[Complex32]) -> Vec<f32> {
        let mut d: Vec<Complex32> = fa.iter().zip(fb).map(|(a, b)| a * b.conj()).collect();
        self.run(&mut d, true);
        let k = 1.0 / (self.nx * self.ny) as f32;
        d.iter().map(|c| c.re * k).collect()
    }
}

/// The best shift of a trial rotation of `b` against `a`, by the masked
/// normalised cross-correlation, and its value.
struct Peak {
    sx: f64,
    sy: f64,
    ncc: f32,
}

#[allow(
    clippy::too_many_arguments,
    reason = "the two pictures, their masks and the limits"
)]
fn masked_ncc(
    fft: &Fft2,
    a: &[f32],
    ma: &[f32],
    wa: usize,
    ha: usize,
    b: &[f32],
    mb: &[f32],
    wb: usize,
    hb: usize,
    min_overlap: f32,
) -> Vec<Peak> {
    let sq = |v: &[f32]| v.iter().map(|x| x * x).collect::<Vec<f32>>();
    let a_masked: Vec<f32> = a.iter().zip(ma).map(|(x, m)| x * m).collect();
    let b_masked: Vec<f32> = b.iter().zip(mb).map(|(x, m)| x * m).collect();
    let (fa, fma, fa2) = (
        fft.forward(&a_masked, wa, ha),
        fft.forward(ma, wa, ha),
        fft.forward(&sq(&a_masked), wa, ha),
    );
    let (fb, fmb, fb2) = (
        fft.forward(&b_masked, wb, hb),
        fft.forward(mb, wb, hb),
        fft.forward(&sq(&b_masked), wb, hb),
    );
    let ab = fft.correlate(&fa, &fb);
    let a_mb = fft.correlate(&fa, &fmb);
    let ma_b = fft.correlate(&fma, &fb);
    let a2_mb = fft.correlate(&fa2, &fmb);
    let ma_b2 = fft.correlate(&fma, &fb2);
    let n = fft.correlate(&fma, &fmb);
    let (nx, ny) = (fft.nx, fft.ny);
    let mut found: Vec<Peak> = Vec::new();
    for y in 0..ny {
        // Shifts run from -(h_b - 1) to h_a - 1.
        let sy = if y < ha {
            y as f64
        } else {
            y as f64 - ny as f64
        };
        if sy < -((hb - 1) as f64) {
            continue;
        }
        for x in 0..nx {
            let sx = if x < wa {
                x as f64
            } else {
                x as f64 - nx as f64
            };
            if sx < -((wb - 1) as f64) {
                continue;
            }
            let i = y * nx + x;
            let cnt = n[i];
            if cnt < min_overlap {
                continue;
            }
            let num = ab[i] - a_mb[i] * ma_b[i] / cnt;
            let da = a2_mb[i] - a_mb[i] * a_mb[i] / cnt;
            let db = ma_b2[i] - ma_b[i] * ma_b[i] / cnt;
            // Windows that are nearly flat (sky) give a ratio of rounding errors.
            if da <= 0.02 * cnt || db <= 0.02 * cnt {
                continue;
            }
            let ncc = (num / (da * db).sqrt()).clamp(-1.0, 1.0);
            if ncc.is_finite() && ncc > 0.05 {
                found.push(Peak { sx, sy, ncc });
            }
        }
    }
    // The best few peaks, with a little room between them.
    found.sort_by(|a, b| b.ncc.total_cmp(&a.ncc));
    let mut keep: Vec<Peak> = Vec::new();
    for p in found {
        if keep
            .iter()
            .all(|k| (k.sx - p.sx).abs() > 4.0 || (k.sy - p.sy).abs() > 4.0)
        {
            keep.push(p);
            if keep.len() == PEAKS_PER_ANGLE {
                break;
            }
        }
    }
    keep
}

/// Rotate `src` about its centre by `theta` into a picture of the same size;
/// the mask is 1 where the rotated picture has data.
fn rotated(src: &[f32], w: usize, h: usize, theta: f64) -> (Vec<f32>, Vec<f32>) {
    let (s, c) = theta.sin_cos();
    let (cx, cy) = ((w as f64 - 1.0) / 2.0, (h as f64 - 1.0) / 2.0);
    let mut out = vec![0.0f32; w * h];
    let mut mask = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            // The rotated picture at (x, y) is the source at R(-theta)(p - c) + c.
            let (u, v) = (x as f64 - cx, y as f64 - cy);
            let (sx, sy) = (c * u + s * v + cx, -s * u + c * v + cy);
            if sx >= 0.0 && sy >= 0.0 && sx <= (w - 1) as f64 && sy <= (h - 1) as f64 {
                out[y * w + x] = bilinear(src, w, h, sx, sy);
                mask[y * w + x] = 1.0;
            }
        }
    }
    (out, mask)
}

/// First estimates of where `b` lies in `a`, best first, searching shifts and
/// rotations of up to `max_rot_deg`: a few candidates, because the best peak
/// of the coarse correlation is sometimes a chance one. Empty when no placement
/// shares at least `min_overlap` (a share of the smaller panel) and 400 shrunk
/// pixels.
#[must_use]
pub fn search(a: &Prepared, b: &Prepared, max_rot_deg: f64, min_overlap: f32) -> Vec<(Rigid, f32)> {
    let short = a.width.min(a.height).min(b.width).min(b.height);
    let f = (short / SEARCH_SIZE).max(1);
    let (da, wa, ha) = shrink(&a.data, a.width, a.height, f);
    let (db, wb, hb) = shrink(&b.data, b.width, b.height, f);
    if wa < 16 || ha < 16 || wb < 16 || hb < 16 {
        return Vec::new();
    }
    let min_px = (min_overlap * (wa * ha).min(wb * hb) as f32).max(400.0);
    let (nx, ny) = (next_fast_len(wa + wb), next_fast_len(ha + hb));
    let fft = Fft2::new(nx, ny);
    let ma = vec![1.0f32; wa * ha];
    let steps = if max_rot_deg > 0.0 {
        (max_rot_deg / ANGLE_STEP_DEG).ceil() as i32
    } else {
        0
    };
    let angles: Vec<f64> = (-steps..=steps)
        .map(|k| max_rot_deg * f64::from(k) / f64::from(steps.max(1)))
        .collect();
    let mut all: Vec<(Rigid, f32)> = angles
        .par_iter()
        .flat_map_iter(|&deg| {
            let theta = deg.to_radians();
            let (rb, mb) = rotated(&db, wb, hb, theta);
            masked_ncc(&fft, &da, &ma, wa, ha, &rb, &mb, wb, hb, min_px)
                .into_iter()
                .map(move |p| {
                    (
                        Rigid {
                            tx: p.sx * f as f64,
                            ty: p.sy * f as f64,
                            theta,
                        },
                        p.ncc,
                    )
                })
        })
        .collect();
    all.sort_by(|x, y| y.1.total_cmp(&x.1));
    all.truncate(CANDIDATES);
    all
}

/// What the refinement found.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fit {
    /// The transform of the second panel into the first.
    pub rigid: Rigid,
    /// Normalised correlation of the detail over the overlap.
    pub ncc: f32,
    /// Pixels of overlap (full size).
    pub overlap: usize,
}

/// Normalised correlation and overlap of `b` placed in `a` by `r`, from every
/// `stride`th pixel of `b`.
fn agreement(a: &Prepared, b: &Prepared, r: &Rigid, stride: usize) -> (f32, usize) {
    let c = b.centre();
    let (mut sa, mut sb, mut saa, mut sbb, mut sab, mut n) =
        (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64, 0usize);
    for y in (0..b.height).step_by(stride) {
        for x in (0..b.width).step_by(stride) {
            let (qx, qy) = r.apply(x as f64, y as f64, c);
            if qx < 1.0 || qy < 1.0 || qx > (a.width - 2) as f64 || qy > (a.height - 2) as f64 {
                continue;
            }
            let va = f64::from(bilinear(&a.data, a.width, a.height, qx, qy));
            let vb = f64::from(b.data[y * b.width + x]);
            sa += va;
            sb += vb;
            saa += va * va;
            sbb += vb * vb;
            sab += va * vb;
            n += 1;
        }
    }
    if n < 50 {
        return (0.0, 0);
    }
    let nf = n as f64;
    let cov = sab - sa * sb / nf;
    let (va, vb) = (saa - sa * sa / nf, sbb - sb * sb / nf);
    if va <= 1e-9 || vb <= 1e-9 {
        return (0.0, n * stride * stride);
    }
    ((cov / (va * vb).sqrt()) as f32, n * stride * stride)
}

/// Refine `init` by Gauss-Newton on the pyramid. `None` when the pictures do
/// not overlap enough to fit.
#[must_use]
pub fn refine(a: &Prepared, b: &Prepared, init: Rigid) -> Option<Fit> {
    let mut r = init;
    let short = a.width.min(a.height).min(b.width).min(b.height);
    let mut factors: Vec<usize> = [8usize, 4, 2, 1]
        .into_iter()
        .filter(|&f| short / f >= 48)
        .collect();
    if factors.is_empty() {
        factors.push(1);
    }
    for &f in &factors {
        let (da, wa, ha) = shrink(&a.data, a.width, a.height, f);
        let (db, wb, hb) = shrink(&b.data, b.width, b.height, f);
        let (cx, cy) = ((wb as f64 - 1.0) / 2.0, (hb as f64 - 1.0) / 2.0);
        let stride = ((wb * hb) as f64 / LK_SAMPLES as f64)
            .sqrt()
            .ceil()
            .max(1.0) as usize;
        let mut p = Rigid {
            tx: r.tx / f as f64,
            ty: r.ty / f as f64,
            theta: r.theta,
        };
        for _ in 0..LK_ITERATIONS {
            let (s, c) = p.theta.sin_cos();
            let mut ata = [[0.0f64; 3]; 3];
            let mut atb = [0.0f64; 3];
            let mut n = 0usize;
            for y in (0..hb).step_by(stride) {
                for x in (0..wb).step_by(stride) {
                    let (u, v) = (x as f64 - cx, y as f64 - cy);
                    let (qx, qy) = (c * u - s * v + cx + p.tx, s * u + c * v + cy + p.ty);
                    if qx < 2.0 || qy < 2.0 || qx > (wa - 3) as f64 || qy > (ha - 3) as f64 {
                        continue;
                    }
                    let gx = f64::from(
                        bilinear(&da, wa, ha, qx + 1.0, qy) - bilinear(&da, wa, ha, qx - 1.0, qy),
                    ) / 2.0;
                    let gy = f64::from(
                        bilinear(&da, wa, ha, qx, qy + 1.0) - bilinear(&da, wa, ha, qx, qy - 1.0),
                    ) / 2.0;
                    let e = f64::from(db[y * wb + x]) - f64::from(bilinear(&da, wa, ha, qx, qy));
                    let dq = (-s * u - c * v, c * u - s * v);
                    let j = [gx, gy, gx * dq.0 + gy * dq.1];
                    // A light Huber weight keeps a bad patch from dragging the fit.
                    let wgt = if e.abs() < 1.5 { 1.0 } else { 1.5 / e.abs() };
                    for i in 0..3 {
                        for k in 0..3 {
                            ata[i][k] += wgt * j[i] * j[k];
                        }
                        atb[i] += wgt * j[i] * e;
                    }
                    n += 1;
                }
            }
            if n < 200 {
                return None;
            }
            // Damp the rotation a little: its column is large.
            for (i, row) in ata.iter_mut().enumerate() {
                row[i] += 1e-6 * (1.0 + row[i]);
            }
            let Some(d) = solve3(ata, atb) else { break };
            p.tx += d[0];
            p.ty += d[1];
            p.theta += d[2];
            if d[0].abs() < 1e-3 && d[1].abs() < 1e-3 && d[2].abs() < 1e-6 {
                break;
            }
        }
        r = Rigid {
            tx: p.tx * f as f64,
            ty: p.ty * f as f64,
            theta: p.theta,
        };
    }
    let (ncc, overlap) = agreement(a, b, &r, 2);
    if overlap == 0 {
        return None;
    }
    Some(Fit {
        rigid: r,
        ncc,
        overlap,
    })
}

fn solve3(m: [[f64; 3]; 3], b: [f64; 3]) -> Option<[f64; 3]> {
    let det = |m: &[[f64; 3]; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let d = det(&m);
    if d.abs() < 1e-18 {
        return None;
    }
    let mut out = [0.0; 3];
    for (k, o) in out.iter_mut().enumerate() {
        let mut mk = m;
        for (r, row) in mk.iter_mut().enumerate() {
            row[k] = b[r];
        }
        *o = det(&mk) / d;
    }
    Some(out)
}

/// Estimate where `b` lies in `a`: the search, then the refinement of each
/// candidate, keeping the one that agrees best.
///
/// `init`, when given, replaces the search (a second pass over panels that are
/// already placed). `None` when they do not overlap.
#[must_use]
pub fn match_pair(
    a: &Prepared,
    b: &Prepared,
    max_rot_deg: f64,
    min_overlap: f32,
    init: Option<Rigid>,
) -> Option<Fit> {
    if let Some(r) = init {
        return refine(a, b, r);
    }
    search(a, b, max_rot_deg, min_overlap)
        .into_iter()
        .filter_map(|(r, _)| refine(a, b, r))
        .max_by(|x, y| x.ncc.total_cmp(&y.ncc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{SyntheticConfig, SyntheticSun};

    fn sun() -> Frame {
        SyntheticSun::new(&SyntheticConfig {
            width: 400,
            height: 300,
            radius_frac: 0.42,
            granulation: 0.2,
            ..SyntheticConfig::default()
        })
        .truth()
        .clone()
    }

    /// A `w` x `h` window of `f` whose top-left is `(x0, y0)`, turned by `deg`
    /// about its own centre.
    fn window(f: &Frame, x0: f64, y0: f64, w: usize, h: usize, deg: f64) -> Frame {
        let mut out = Frame::zeros(w, h);
        let r = Rigid {
            tx: x0,
            ty: y0,
            theta: deg.to_radians(),
        };
        let c = ((w as f64 - 1.0) / 2.0, (h as f64 - 1.0) / 2.0);
        for y in 0..h {
            for x in 0..w {
                let (qx, qy) = r.apply(x as f64, y as f64, c);
                out.data[y * w + x] = crate::resample::sample(
                    f,
                    qx as f32,
                    qy as f32,
                    crate::resample::Kernel::Lanczos3,
                );
            }
        }
        out
    }

    #[test]
    fn the_rigid_transform_turns_about_the_centre() {
        let r = Rigid {
            tx: 3.0,
            ty: -2.0,
            theta: std::f64::consts::FRAC_PI_2,
        };
        let (x, y) = r.apply(10.0, 5.0, (5.0, 5.0));
        assert!((x - 8.0).abs() < 1e-9 && (y - 8.0).abs() < 1e-9, "{x} {y}");
    }

    #[test]
    fn a_known_shift_is_found_by_the_search_and_refined() {
        let sun = sun();
        // Two 220 x 200 windows of the same Sun, 110 px apart in x and 14 in y.
        let a = window(&sun, 20.0, 40.0, 220, 200, 0.0);
        let b = window(&sun, 130.0, 54.0, 220, 200, 0.0);
        let (pa, pb) = (prepare(&a), prepare(&b));
        let fit = match_pair(&pa, &pb, 3.0, 0.05, None).unwrap_or_else(|| panic!("no fit"));
        // B's pixel (x, y) is the Sun's (x + 130, y + 54); A's is (x + 20, y + 40).
        assert!((fit.rigid.tx - 110.0).abs() < 0.15, "{:?}", fit.rigid);
        assert!((fit.rigid.ty - 14.0).abs() < 0.15, "{:?}", fit.rigid);
        assert!(fit.rigid.theta.abs() < 0.001, "{:?}", fit.rigid);
        assert!(fit.ncc > 0.9, "{}", fit.ncc);
    }

    #[test]
    fn a_rotation_of_a_degree_and_a_half_is_found() {
        let sun = sun();
        let a = window(&sun, 20.0, 40.0, 220, 200, 0.0);
        let b = window(&sun, 130.0, 54.0, 220, 200, 1.5);
        let (pa, pb) = (prepare(&a), prepare(&b));
        let fit = match_pair(&pa, &pb, 4.0, 0.05, None).unwrap_or_else(|| panic!("no fit"));
        // B's pixels are turned 1.5 degrees from A's: that is the transform that carries them over.
        assert!(
            (fit.rigid.theta.to_degrees() - 1.5).abs() < 0.05,
            "{:?} {}",
            fit.rigid,
            fit.rigid.theta.to_degrees()
        );
        assert!(fit.ncc > 0.85, "{}", fit.ncc);
    }

    #[test]
    fn panels_that_share_nothing_do_not_match() {
        let sun = sun();
        let a = window(&sun, 0.0, 0.0, 100, 100, 0.0);
        let b = window(&sun, 290.0, 190.0, 100, 100, 0.0);
        // Nothing is shared, so whatever it finds correlates poorly.
        let m = match_pair(&prepare(&a), &prepare(&b), 3.0, 0.05, None);
        assert!(m.is_none_or(|f| f.ncc < 0.45), "{m:?}");
    }
}
