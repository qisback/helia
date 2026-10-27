// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Global translation by fitting the solar limb.
//!
//! A full-disc frame has no small patch to correlate, but it has a sharp
//! circular edge. This module finds that edge to sub-pixel accuracy and fits a
//! circle; the shift of a frame is the difference between its disc centre and
//! the reference's.
//!
//! The edge is located along rays. From a first guess of the disc, 720 rays
//! leave the centre; on each, the image (lightly blurred to suppress noise) is
//! sampled every half pixel and the fall in brightness, the limb, is located as
//! the centroid of the steepest stretch of the profile (which averages the noise
//! over the whole edge, where a single maximum would not). A ray that leaves the
//! frame, or whose edge is much weaker than the rest, is dropped. The circle is then fitted to
//! the edge points by iteratively reweighted least squares with a Tukey
//! biweight, so prominences, flares and sunspot edges that pull single points
//! away from the circle are ignored. A second pass repeats this with a narrow
//! window around the first result.
//!
//! The fit also says how far to trust itself. [`LimbFit::centre_sigma`] is the
//! predicted error of the centre in pixels: the larger of the estimate from the
//! scatter of the points and a jackknife (refit without each eighth of the limb
//! in turn), which also reflects noise shared by neighbouring rays and how much
//! of the limb is in view (a short arc fixes the centre badly along its own
//! direction). [`LimbFit::confidence`] turns that, and how many
//! points agreed with the circle, into a number from 0 to 1.
//!
//! When the limb is cut by the frame edge the radius and centre trade off
//! against each other, so the radius of the reference is held fixed.
//!
//! # References
//!
//! - Beaton, A. E. and Tukey, J. W. (1974), Technometrics 16, 147-185, doi:10.1080/00401706.1974.10489171
//!   (the biweight).
//! - Huber, P. J. and Ronchetti, E. M. (2009), Robust Statistics, 2nd ed., Wiley, doi:10.1002/9780470434697
//!   (iteratively reweighted least squares).

#![allow(
    clippy::many_single_char_names,
    reason = "x, y, w, h, r and the matrix entries are the natural names in image maths"
)]

use crate::align::Shift;
use crate::filter::gaussian_blur;
use crate::frame::Frame;
use crate::mask::{self, Disc};

/// Rays cast per pass.
const RAYS: usize = 720;
/// Step along a ray, pixels.
const STEP: f64 = 0.5;
/// Blur applied before sampling, pixels.
const BLUR: f32 = 1.5;
/// Points on a ray closer than this to the frame edge make the ray unusable.
const MARGIN: f64 = 2.0;
/// Fewest edge points a fit may rest on.
const MIN_POINTS: usize = 60;
/// Centre error at which the confidence is one half, pixels.
const SIGMA_HALF: f64 = 0.15;
/// Least confidence at which a frame's limb fit is used instead of phase
/// correlation.
pub const MIN_CONFIDENCE: f32 = 0.5;

/// A circle fitted to the limb, with a measure of how good the fit is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LimbFit {
    /// Centre, x, in pixels (pixel centres are at integers).
    pub cx: f64,
    /// Centre, y.
    pub cy: f64,
    /// Radius in pixels, to the steepest part of the edge.
    pub r: f64,
    /// Whether the radius was fitted (`true`) or held at the reference's.
    pub radius_fitted: bool,
    /// Edge points found.
    pub points: usize,
    /// Edge points that agree with the circle.
    pub inliers: usize,
    /// Robust scatter of the agreeing points about the circle, pixels.
    pub rms: f64,
    /// Fraction of the limb circumference that lies inside the frame.
    pub in_frame: f32,
    /// Predicted 1-sigma error of the centre, pixels.
    pub centre_sigma: f64,
    /// 0 to 1: 1 is a precisely located, consistent, well-spread limb.
    pub confidence: f32,
}

impl LimbFit {
    /// The whole limb is inside the frame (a full disc is visible).
    #[must_use]
    pub fn is_full_disc(&self) -> bool {
        self.in_frame >= 0.999
    }

    /// The circle as a [`Disc`].
    #[must_use]
    pub fn disc(&self) -> Disc {
        Disc {
            cx: self.cx as f32,
            cy: self.cy as f32,
            r: self.r as f32,
        }
    }
}

/// A circle used as a starting point.
#[derive(Clone, Copy, Debug)]
struct Circle {
    cx: f64,
    cy: f64,
    r: f64,
}

impl From<Disc> for Circle {
    fn from(d: Disc) -> Self {
        Self {
            cx: f64::from(d.cx),
            cy: f64::from(d.cy),
            r: f64::from(d.r),
        }
    }
}

/// Bilinear sample; the caller keeps the point at least one pixel inside.
fn bilinear(f: &Frame, x: f64, y: f64) -> f64 {
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (fx, fy) = (x - x0 as f64, y - y0 as f64);
    let at = |xx: usize, yy: usize| f64::from(f.data[yy * f.width + xx]);
    let top = at(x0, y0) * (1.0 - fx) + at(x0 + 1, y0) * fx;
    let bot = at(x0, y0 + 1) * (1.0 - fx) + at(x0 + 1, y0 + 1) * fx;
    top * (1.0 - fy) + bot * fy
}

fn inside(f: &Frame, x: f64, y: f64) -> bool {
    x >= MARGIN
        && y >= MARGIN
        && x <= f.width as f64 - 1.0 - MARGIN
        && y <= f.height as f64 - 1.0 - MARGIN
}

/// A point on the limb.
#[derive(Clone, Copy)]
struct Edge {
    x: f64,
    y: f64,
    /// Index of the ray, for grouping points by angle.
    ray: usize,
}

/// What one pass of ray casting found.
struct Edges {
    pts: Vec<Edge>,
    /// Rays whose limb point (at the guess radius) is inside the frame.
    in_frame: usize,
    /// Typical half-width of the edge, pixels.
    half_width: f64,
}

/// Cast the rays from `c` and find the edge on each.
fn find_edges(blurred: &Frame, c: Circle, half_window: f64, contrast_floor: f32) -> Edges {
    let n_steps = (2.0 * half_window / STEP) as usize + 1;
    let mut found: Vec<(Edge, f64, f64)> = Vec::new();
    let mut in_frame = 0usize;
    let mut profile = vec![0.0f64; n_steps];
    for k in 0..RAYS {
        let theta = std::f64::consts::TAU * k as f64 / RAYS as f64;
        let (ux, uy) = (theta.cos(), theta.sin());
        if inside(blurred, c.cx + c.r * ux, c.cy + c.r * uy) {
            in_frame += 1;
        }
        let r0 = c.r - half_window;
        let mut ok = true;
        for (t, v) in profile.iter_mut().enumerate() {
            let rho = r0 + t as f64 * STEP;
            let (x, y) = (c.cx + rho * ux, c.cy + rho * uy);
            if !inside(blurred, x, y) {
                ok = false;
                break;
            }
            *v = bilinear(blurred, x, y);
        }
        if !ok {
            continue;
        }
        // Steepest fall outwards: the disc is brighter than the sky.
        let mut best = (0usize, f64::INFINITY);
        for t in 1..n_steps - 1 {
            let d = (profile[t + 1] - profile[t - 1]) / (2.0 * STEP);
            if d < best.1 {
                best = (t, d);
            }
        }
        let (t, d) = best;
        if t < 2 || t + 3 > n_steps || d > -f64::from(contrast_floor) {
            continue;
        }
        let g = |i: usize| (profile[i + 1] - profile[i - 1]) / (2.0 * STEP);
        // The edge is the centroid of the fall in brightness over the stretch
        // where it exceeds a quarter of its peak: for a symmetric blur this is
        // the edge position, and it averages the noise over the whole stretch
        // where the position of a single maximum would not.
        let thr = 0.25 * -d;
        let (mut lo, mut hi) = (t, t);
        while lo > 1 && -g(lo - 1) > thr {
            lo -= 1;
        }
        while hi + 2 < n_steps && -g(hi + 1) > thr {
            hi += 1;
        }
        // A fall that runs into the end of the window is cut short and would
        // bias the centroid: drop the ray.
        if lo <= 1 || hi + 2 >= n_steps {
            continue;
        }
        let (mut num, mut den) = (0.0, 0.0);
        for i in lo..=hi {
            let wgt = -g(i) - thr;
            num += wgt * i as f64;
            den += wgt;
        }
        let pos = if den > 0.0 { num / den } else { t as f64 };
        let rho = r0 + pos * STEP;
        found.push((
            Edge {
                x: c.cx + rho * ux,
                y: c.cy + rho * uy,
                ray: k,
            },
            -d,
            (hi - lo) as f64 * STEP / 2.0,
        ));
    }
    // Drop rays whose edge is far weaker than the typical one: a faint
    // shoulder, a gap in the limb or a cut-off ray.
    if found.is_empty() {
        return Edges {
            pts: Vec::new(),
            in_frame,
            half_width: 0.0,
        };
    }
    let mut strengths: Vec<f64> = found.iter().map(|f| f.1).collect();
    strengths.sort_by(f64::total_cmp);
    let floor = 0.3 * strengths[strengths.len() / 2];
    let mut widths: Vec<f64> = found.iter().map(|f| f.2).collect();
    widths.sort_by(f64::total_cmp);
    Edges {
        pts: found
            .into_iter()
            .filter(|f| f.1 >= floor)
            .map(|f| f.0)
            .collect(),
        in_frame,
        half_width: widths[widths.len() / 2],
    }
}

/// Result of the robust circle fit.
struct CircleFit {
    cx: f64,
    cy: f64,
    r: f64,
    inliers: usize,
    rms: f64,
    /// Trace of the inverse normal matrix for the centre (x and y only).
    centre_trace: f64,
}

/// Solve the symmetric positive-definite system `a x = b` (n <= 3) by
/// Gaussian elimination; `None` when singular. Returns the inverse's
/// diagonal too, for the error estimate.
fn solve(a: &[[f64; 3]; 3], b: [f64; 3], n: usize) -> Option<([f64; 3], [f64; 3])> {
    let mut m = [[0.0f64; 6]; 3];
    for i in 0..n {
        m[i][..n].copy_from_slice(&a[i][..n]);
        m[i][n + i] = 1.0;
    }
    for col in 0..n {
        let piv = (col..n).max_by(|&p, &q| m[p][col].abs().total_cmp(&m[q][col].abs()))?;
        if m[piv][col].abs() < 1e-12 {
            return None;
        }
        m.swap(col, piv);
        let d = m[col][col];
        for v in &mut m[col][..2 * n] {
            *v /= d;
        }
        for row in 0..n {
            if row != col {
                let f = m[row][col];
                if f != 0.0 {
                    let pivot_row = m[col];
                    for (dst, s) in m[row][..2 * n].iter_mut().zip(&pivot_row[..2 * n]) {
                        *dst -= f * s;
                    }
                }
            }
        }
    }
    let mut x = [0.0; 3];
    let mut diag = [0.0; 3];
    for i in 0..n {
        // After elimination the left block is the identity; x = inv(a) b.
        x[i] = (0..n).map(|j| m[i][n + j] * b[j]).sum();
        diag[i] = m[i][n + i];
    }
    Some((x, diag))
}

/// Tukey biweight weights for residuals `e` with scale `s`.
fn tukey(e: f64, s: f64) -> f64 {
    let u = e / (4.685 * s);
    if u.abs() >= 1.0 {
        0.0
    } else {
        (1.0 - u * u).powi(2)
    }
}

/// Fit a circle to `pts` by iteratively reweighted Gauss-Newton, starting at
/// `start`. With `fixed_r` the radius is not fitted.
fn fit_circle(pts: &[Edge], start: Circle, fixed_r: Option<f64>) -> Option<CircleFit> {
    let n_par = if fixed_r.is_some() { 2 } else { 3 };
    let mut c = start;
    if let Some(r) = fixed_r {
        c.r = r;
    }
    let mut scale = 1.0f64;
    for iter in 0..12 {
        let res: Vec<f64> = pts
            .iter()
            .map(|p| (p.x - c.cx).hypot(p.y - c.cy) - c.r)
            .collect();
        // Robust scale from the median absolute residual; the first pass is
        // wide so a poor start is not mistaken for outliers.
        let mut abs: Vec<f64> = res.iter().map(|e| e.abs()).collect();
        abs.sort_by(f64::total_cmp);
        let mad = abs[abs.len() / 2];
        scale = (1.4826 * mad).max(0.1);
        if iter == 0 {
            scale = scale.max(2.0);
        }
        let mut a = [[0.0f64; 3]; 3];
        let mut b = [0.0f64; 3];
        let mut live = 0;
        for (p, &e) in pts.iter().zip(&res) {
            let w = tukey(e, scale);
            if w <= 0.0 {
                continue;
            }
            live += 1;
            let d = (p.x - c.cx).hypot(p.y - c.cy).max(1e-9);
            // d(residual)/d(centre) = -(unit vector to the point); d/dr = -1.
            let j = [-(p.x - c.cx) / d, -(p.y - c.cy) / d, -1.0];
            for r in 0..n_par {
                for q in 0..n_par {
                    a[r][q] += w * j[r] * j[q];
                }
                b[r] -= w * j[r] * e;
            }
        }
        if live < MIN_POINTS / 2 {
            return None;
        }
        let (step, _) = solve(&a, b, n_par)?;
        c.cx += step[0];
        c.cy += step[1];
        if fixed_r.is_none() {
            c.r += step[2];
        }
        if step[0].abs().max(step[1].abs()) < 1e-5 && iter > 2 {
            break;
        }
    }
    // Final statistics on the agreeing points with unit weights.
    let tol = 4.685 * scale;
    let mut sum2 = 0.0;
    let mut cnt = 0usize;
    let mut a = [[0.0f64; 3]; 3];
    for p in pts {
        let d = (p.x - c.cx).hypot(p.y - c.cy).max(1e-9);
        let e = d - c.r;
        if e.abs() > tol {
            continue;
        }
        cnt += 1;
        sum2 += e * e;
        let j = [-(p.x - c.cx) / d, -(p.y - c.cy) / d, -1.0];
        for r in 0..n_par {
            for q in 0..n_par {
                a[r][q] += j[r] * j[q];
            }
        }
    }
    let (_, diag) = solve(&a, [0.0; 3], n_par)?;
    Some(CircleFit {
        cx: c.cx,
        cy: c.cy,
        r: c.r,
        inliers: cnt,
        rms: (sum2 / cnt.max(1) as f64).sqrt(),
        centre_trace: diag[0] + diag[1],
    })
}

/// One pass: rays within `half_window` pixels of the guess radius, and the
/// circle through their edges.
fn pass(
    blurred: &Frame,
    guess: Circle,
    half_window: f64,
    fixed_r: Option<f64>,
    contrast_floor: f32,
) -> Option<(CircleFit, Edges)> {
    let edges = find_edges(blurred, guess, half_window, contrast_floor);
    if edges.pts.len() < MIN_POINTS {
        return None;
    }
    let fit = fit_circle(&edges.pts, guess, fixed_r)?;
    Some((fit, edges))
}

/// Delete-one-block jackknife error of the centre: refit without each eighth
/// of the limb in turn. Unlike the scatter of the points, it is honest about
/// neighbouring rays sharing noise, and about a short arc.
fn jackknife_sigma(pts: &[Edge], fit: &CircleFit, fixed_r: Option<f64>) -> f64 {
    const BLOCKS: usize = 8;
    let start = Circle {
        cx: fit.cx,
        cy: fit.cy,
        r: fit.r,
    };
    let mut centres = Vec::with_capacity(BLOCKS);
    for b in 0..BLOCKS {
        let keep: Vec<Edge> = pts
            .iter()
            .filter(|p| p.ray * BLOCKS / RAYS != b)
            .copied()
            .collect();
        if keep.len() == pts.len() {
            continue; // no points in this block (a limb cut by the frame)
        }
        match fit_circle(&keep, start, fixed_r) {
            Some(f) => centres.push((f.cx, f.cy)),
            None => return f64::INFINITY,
        }
    }
    if centres.len() < 3 {
        return f64::INFINITY;
    }
    let n = centres.len() as f64;
    let (mx, my) = centres
        .iter()
        .fold((0.0, 0.0), |a, c| (a.0 + c.0 / n, a.1 + c.1 / n));
    let ss: f64 = centres
        .iter()
        .map(|c| (c.0 - mx).powi(2) + (c.1 - my).powi(2))
        .sum();
    ((n - 1.0) / n * ss).sqrt()
}

/// The points with the second harmonic of the radius about the fitted centre
/// (the ellipse, `a cos 2t + b sin 2t`) taken out. Only when the points cover
/// at least 90% of the rays: a short arc cannot tell an ellipse from a shifted
/// circle, and keeps its honest, large error.
fn remove_ellipticity(pts: &[Edge], fit: &CircleFit, coverage: f64) -> Vec<Edge> {
    if coverage < 0.9 {
        return pts.to_vec();
    }
    let polar: Vec<(f64, f64)> = pts
        .iter()
        .map(|p| {
            let (dx, dy) = (p.x - fit.cx, p.y - fit.cy);
            (dy.atan2(dx), dx.hypot(dy) - fit.r)
        })
        .collect();
    let n = polar.len() as f64;
    let a = 2.0 * polar.iter().map(|&(t, e)| e * (2.0 * t).cos()).sum::<f64>() / n;
    let b = 2.0 * polar.iter().map(|&(t, e)| e * (2.0 * t).sin()).sum::<f64>() / n;
    pts.iter()
        .zip(&polar)
        .map(|(p, &(t, e))| {
            let d = fit.r + e - (a * (2.0 * t).cos() + b * (2.0 * t).sin());
            Edge {
                x: fit.cx + d * t.cos(),
                y: fit.cy + d * t.sin(),
                ray: p.ray,
            }
        })
        .collect()
}

/// Fit the limb of `image`, starting from `guess` (typically
/// [`mask::fit_disc`] on the same image, or the reference's disc). With
/// `fixed_radius` only the centre is fitted, which is what a limb cut by the
/// frame edge needs. `None` when there is no usable limb.
#[must_use]
pub fn fit_limb(image: &Frame, guess: Disc, fixed_radius: Option<f64>) -> Option<LimbFit> {
    if image.width < 32 || image.height < 32 {
        return None;
    }
    let blurred = gaussian_blur(image, BLUR);
    // Contrast: brightness step between disc and sky, from the guess.
    let g: Circle = guess.into();
    let level = |rho: f64| -> Option<f64> {
        let mut v = Vec::new();
        for k in 0..64 {
            let t = std::f64::consts::TAU * f64::from(k) / 64.0;
            let (x, y) = (g.cx + rho * t.cos(), g.cy + rho * t.sin());
            if inside(&blurred, x, y) {
                v.push(bilinear(&blurred, x, y));
            }
        }
        v.sort_by(f64::total_cmp);
        v.get(v.len() / 2).copied()
    };
    let (inner, outer) = (level(0.9 * g.r)?, level(1.1 * g.r)?);
    let step = inner - outer;
    if step < 0.05 {
        return None;
    }
    // A limb is a fall of about `step` over a few pixels; accept a quarter of
    // that gradient at the least.
    let floor = (0.25 * step / 4.0) as f32;
    let wide = (0.12 * g.r).max(10.0);
    let (first, wide_edges) = pass(&blurred, g, wide, fixed_radius, floor)?;
    let g2 = Circle {
        cx: first.cx,
        cy: first.cy,
        r: first.r,
    };
    // The window must hold the whole fall in brightness with room to spare,
    // however blurred the frame is.
    let narrow = (0.025 * g.r)
        .max(5.0)
        .max(3.0 * wide_edges.half_width + 2.0);
    let (fit, edges) = pass(&blurred, g2, narrow, fixed_radius, floor)?;
    let points = edges.pts.len();
    let in_frame = edges.in_frame as f32 / RAYS as f32;
    let model = fit.rms * fit.centre_trace.sqrt();
    // A real limb is slightly elliptical (refraction at a low Sun, a video
    // resampled to unequal pixels). That shape is the same in every frame and
    // does not move the centre of a whole limb, but it makes the blocks of the
    // jackknife disagree, so it is taken out of the points first.
    let round_pts = remove_ellipticity(&edges.pts, &fit, edges.pts.len() as f64 / RAYS as f64);
    let sigma = model.max(jackknife_sigma(&round_pts, &fit, fixed_radius));
    let precision = 1.0 / (1.0 + (sigma / SIGMA_HALF).powi(2));
    let consistency = fit.inliers as f64 / points.max(1) as f64;
    let seen = (points as f64 / (f64::from(in_frame) * RAYS as f64).max(1.0)).min(1.0);
    let confidence = (precision * consistency * (seen / 0.8).min(1.0)) as f32;
    Some(LimbFit {
        cx: fit.cx,
        cy: fit.cy,
        r: fit.r,
        radius_fitted: fixed_radius.is_none(),
        points,
        inliers: fit.inliers,
        rms: fit.rms,
        in_frame,
        centre_sigma: sigma,
        confidence: confidence.clamp(0.0, 1.0),
    })
}

/// Centre of mass of the bright disc, a coarse fallback when the edge fit has
/// no starting point. The disc must lie wholly inside the frame (a cut disc
/// has a biased centroid) and must not be the whole frame.
#[must_use]
pub fn disc_centroid(image: &Frame) -> Option<Disc> {
    let (w, h) = (image.width, image.height);
    if w < 32 || h < 32 {
        return None;
    }
    let blurred = gaussian_blur(image, 2.0);
    let mut sample: Vec<f32> = blurred.data.iter().copied().step_by(7).collect();
    sample.sort_by(f32::total_cmp);
    let at = |p: f32| sample[((sample.len() - 1) as f32 * p) as usize];
    let (sky, disc) = (at(0.05), at(0.95));
    if disc - sky < 0.05 {
        return None;
    }
    let level = f32::midpoint(sky, disc);
    let (mut sx, mut sy, mut n) = (0.0f64, 0.0f64, 0.0f64);
    let mut touches_edge = false;
    for y in 0..h {
        for (x, &v) in blurred.row(y).iter().enumerate() {
            if v > level {
                sx += x as f64;
                sy += y as f64;
                n += 1.0;
                touches_edge |= x == 0 || y == 0 || x + 1 == w || y + 1 == h;
            }
        }
    }
    if touches_edge || n < 0.01 * (w * h) as f64 || n > 0.9 * (w * h) as f64 {
        return None;
    }
    Some(Disc {
        cx: (sx / n) as f32,
        cy: (sy / n) as f32,
        r: (n / std::f64::consts::PI).sqrt() as f32,
    })
}

/// The reference image's limb, against which frames are measured.
#[derive(Clone, Debug, PartialEq)]
pub struct LimbReference {
    /// The reference's own limb fit.
    pub fit: LimbFit,
}

impl LimbReference {
    /// Fit the reference's limb. `None` without a usable limb.
    #[must_use]
    pub fn new(reference: &Frame) -> Option<Self> {
        let guess = mask::fit_disc(reference).or_else(|| disc_centroid(reference))?;
        fit_limb(reference, guess, None).map(|fit| Self { fit })
    }

    /// Shift of `frame` relative to the reference, with the fit it came from.
    /// `None` when the frame has no usable limb.
    ///
    /// The radius is fitted too when enough of the limb is visible to fix it;
    /// otherwise the reference's radius is held.
    #[must_use]
    pub fn estimate(&self, frame: &Frame) -> Option<(Shift, LimbFit)> {
        let r = &self.fit;
        let start: Disc = r.disc();
        let free = |guess: Disc| {
            fit_limb(frame, guess, None)
                .filter(|f| f.is_full_disc() && f.confidence >= MIN_CONFIDENCE)
        };
        let held = |guess: Disc| fit_limb(frame, guess, Some(r.r));
        let attempt = |guess: Disc| {
            if r.is_full_disc() {
                free(guess).or_else(|| held(guess))
            } else {
                held(guess)
            }
        };
        let good = |f: &LimbFit| f.confidence >= MIN_CONFIDENCE;
        let best = attempt(start).filter(good).or_else(|| {
            // A shift too large for the search window: find the disc afresh.
            let guess = mask::fit_disc(frame).or_else(|| disc_centroid(frame))?;
            attempt(guess).filter(good)
        })?;
        Some((
            Shift::new((best.cx - r.cx) as f32, (best.cy - r.cy) as f32),
            best,
        ))
    }
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "test data generation"
)]
mod tests {
    use super::*;
    use crate::align::method::{AlignMethod, AlignPlan, GlobalMethod};
    use crate::testutil::Rng;

    /// A limb-darkened disc at an exact sub-pixel position, area-sampled 6x6
    /// per pixel, blurred by `blur` and with Gaussian noise `noise`.
    pub(super) fn disc_image(
        size: (usize, usize),
        disc: (f64, f64, f64),
        blur: f32,
        noise: f32,
        seed: u64,
    ) -> Frame {
        elliptical_disc_image(size, disc, 1.0, blur, noise, seed)
    }

    /// As [`disc_image`], with the disc's height scaled by `squash` (1 is round).
    pub(super) fn elliptical_disc_image(
        (w, h): (usize, usize),
        (cx, cy, r): (f64, f64, f64),
        squash: f64,
        blur: f32,
        noise: f32,
        seed: u64,
    ) -> Frame {
        const SS: usize = 6;
        let mut f = Frame::zeros(w, h);
        for y in 0..h {
            for x in 0..w {
                let mut acc = 0.0f64;
                for j in 0..SS {
                    for i in 0..SS {
                        let px = x as f64 + (i as f64 + 0.5) / SS as f64 - 0.5;
                        let py = y as f64 + (j as f64 + 0.5) / SS as f64 - 0.5;
                        let rho = (px - cx).hypot((py - cy) / squash) / r;
                        acc += if rho < 1.0 {
                            let mu = (1.0 - rho * rho).sqrt();
                            0.35 + 0.65 * (1.0 - 0.6 * (1.0 - mu))
                        } else {
                            0.02
                        };
                    }
                }
                f.data[y * w + x] = (acc / (SS * SS) as f64) as f32;
            }
        }
        let mut f = gaussian_blur(&f, blur);
        let mut rng = Rng::new(seed);
        for v in &mut f.data {
            *v += noise * rng.gauss();
        }
        f
    }

    fn guess(cx: f64, cy: f64, r: f64) -> Disc {
        Disc {
            cx: cx as f32,
            cy: cy as f32,
            r: r as f32,
        }
    }

    #[test]
    fn centre_of_a_clean_disc_is_found_to_a_few_hundredths_of_a_pixel() {
        let mut worst = 0.0f64;
        let mut rng = Rng::new(5);
        for k in 0..12 {
            let cx = 128.0 + f64::from(rng.uniform() - 0.5) * 24.0;
            let cy = 100.0 + f64::from(rng.uniform() - 0.5) * 24.0;
            let img = disc_image((256, 200), (cx, cy, 70.0), 2.0, 0.02, 100 + k);
            let fit = fit_limb(&img, guess(cx + 3.0, cy - 2.0, 66.0), None).expect("fit");
            let err = (fit.cx - cx).hypot(fit.cy - cy);
            worst = worst.max(err);
            assert!(fit.is_full_disc() && fit.confidence > 0.9, "{fit:?}");
            assert!((fit.r - 70.0).abs() < 1.0, "{fit:?}");
        }
        eprintln!("worst centre error on 12 noisy discs: {worst:.4} px");
        assert!(worst < 0.05, "{worst}");
    }

    #[test]
    fn known_shifts_are_recovered_within_a_tenth_of_a_pixel() {
        let reference = disc_image((256, 200), (128.3, 100.7, 70.0), 1.0, 0.0, 1);
        let lr = LimbReference::new(&reference).expect("reference limb");
        assert!(lr.fit.is_full_disc());
        let mut rng = Rng::new(9);
        let mut worst = 0.0f32;
        let mut sum = 0.0f32;
        for k in 0..40 {
            let (dx, dy) = ((rng.uniform() - 0.5) * 30.0, (rng.uniform() - 0.5) * 30.0);
            let blur = 1.5 + 2.5 * rng.uniform();
            let img = disc_image(
                (256, 200),
                (128.3 + f64::from(dx), 100.7 + f64::from(dy), 70.0),
                blur,
                0.03,
                200 + k,
            );
            let (s, fit) = lr.estimate(&img).expect("estimate");
            let err = (s.dx - dx).hypot(s.dy - dy);
            worst = worst.max(err);
            sum += err;
            assert!(fit.confidence > MIN_CONFIDENCE, "{fit:?}");
            assert!(
                f64::from(err) < 3.0 * fit.centre_sigma.max(0.02),
                "error {err} vs predicted {}",
                fit.centre_sigma
            );
        }
        eprintln!(
            "40 frames, shifts up to 15 px, blur 1.5-4, noise 0.03: mean error {:.4} px, worst {worst:.4} px",
            sum / 40.0
        );
        assert!(worst < 0.1, "{worst}");
    }

    #[test]
    fn a_limb_cut_by_the_frame_is_still_located() {
        // Disc centred 30 px from the left edge: about a third of the limb is out.
        let truth = (30.0, 100.0, 70.0);
        let reference = disc_image((256, 200), truth, 1.2, 0.0, 3);
        let lr = LimbReference::new(&reference).expect("reference limb");
        assert!(!lr.fit.is_full_disc(), "{:?}", lr.fit);
        assert!(lr.fit.in_frame > 0.5 && lr.fit.in_frame < 0.9);
        let mut rng = Rng::new(21);
        let mut worst = 0.0f32;
        for k in 0..20 {
            let (dx, dy) = ((rng.uniform() - 0.5) * 10.0, (rng.uniform() - 0.5) * 20.0);
            let img = disc_image(
                (256, 200),
                (truth.0 + f64::from(dx), truth.1 + f64::from(dy), truth.2),
                1.2,
                0.02,
                300 + k,
            );
            let (s, fit) = lr.estimate(&img).expect("estimate");
            worst = worst.max((s.dx - dx).hypot(s.dy - dy));
            assert!(!fit.radius_fitted, "a cut limb holds the reference radius");
        }
        eprintln!("cut limb (about 40% outside), 20 frames: worst error {worst:.4} px");
        assert!(worst < 0.1, "{worst}");
    }

    #[test]
    fn a_prominence_and_a_bright_spot_do_not_move_the_centre() {
        let truth = (128.0, 100.0, 70.0);
        let mut img = disc_image((256, 200), truth, 1.5, 0.02, 7);
        // A prominence: bright blob just outside the limb on the right.
        for y in 90..112 {
            for x in 196..212 {
                img.data[y * 256 + x] = 0.75;
            }
        }
        // And a bright flare inside the disc near the limb.
        for y in 60..70 {
            for x in 150..160 {
                img.data[y * 256 + x] = 1.0;
            }
        }
        let fit = fit_limb(&img, guess(125.0, 103.0, 68.0), None).expect("fit");
        let err = (fit.cx - truth.0).hypot(fit.cy - truth.1);
        eprintln!("prominence case: centre error {err:.4} px, {fit:?}");
        assert!(err < 0.1, "{err}");
        assert!(fit.inliers < fit.points, "the blob's points were rejected");
    }

    #[test]
    fn confidence_falls_with_noise_and_with_a_short_arc() {
        let clean = fit_limb(
            &disc_image((256, 200), (128.0, 100.0, 70.0), 2.0, 0.01, 1),
            guess(128.0, 100.0, 70.0),
            None,
        )
        .expect("clean");
        let noisy = fit_limb(
            &disc_image((256, 200), (128.0, 100.0, 70.0), 2.0, 0.25, 1),
            guess(128.0, 100.0, 70.0),
            None,
        );
        // A disc whose limb shows only as a short arc in one corner.
        let arc = fit_limb(
            &disc_image((256, 200), (-30.0, -20.0, 70.0), 2.0, 0.01, 1),
            guess(-30.0, -20.0, 70.0),
            Some(70.0),
        );
        eprintln!(
            "confidence: clean {:.3}, noisy {:?}, arc {:?}",
            clean.confidence,
            noisy.map(|n| n.confidence),
            arc.map(|a| a.confidence)
        );
        assert!(clean.confidence > 0.95);
        if let Some(n) = noisy {
            assert!(n.confidence < clean.confidence, "{n:?}");
        }
        assert!(arc.is_none_or(|a| a.confidence < 0.6), "{arc:?}");
    }

    #[test]
    fn a_slightly_elliptical_full_limb_is_still_trusted() {
        // A 1% flatter disc (0.35 px of second harmonic at radius 70): a real
        // Sun seen through refraction, or a video resampled to unequal pixels.
        let (cx, cy) = (128.3, 100.4);
        let round = fit_limb(
            &disc_image((256, 200), (cx, cy, 70.0), 1.5, 0.01, 2),
            guess(cx, cy, 70.0),
            None,
        )
        .expect("round");
        let flat = fit_limb(
            &elliptical_disc_image((256, 200), (cx, cy, 70.0), 0.99, 1.5, 0.01, 2),
            guess(cx, cy, 70.0),
            None,
        )
        .expect("flat");
        eprintln!(
            "ellipse: round {:?}, flat {:?}",
            round.confidence, flat.confidence
        );
        assert!(flat.confidence >= MIN_CONFIDENCE, "{flat:?}");
        // The ellipse does not move the centre.
        assert!((flat.cx - cx).hypot(flat.cy - cy) < 0.1, "{flat:?}");
    }

    #[test]
    fn predicted_error_covers_the_real_error() {
        // The centre sigma should be an honest (not optimistic) estimate.
        let mut covered = 0;
        for k in 0..30 {
            let (cx, cy) = (128.0 + 0.037 * k as f64, 100.0 - 0.021 * k as f64);
            let img = disc_image((256, 200), (cx, cy, 70.0), 2.0, 0.08, 500 + k);
            let fit = fit_limb(&img, guess(cx + 2.0, cy + 2.0, 68.0), None).expect("fit");
            let err = (fit.cx - cx).hypot(fit.cy - cy);
            if err <= 3.0 * fit.centre_sigma.max(0.01) {
                covered += 1;
            }
        }
        assert!(
            covered >= 28,
            "only {covered}/30 within three predicted sigma"
        );
    }

    #[test]
    fn no_limb_means_no_fit() {
        let flat = Frame::new(200, 150, vec![0.3; 200 * 150]).expect("frame");
        assert!(fit_limb(&flat, guess(100.0, 75.0, 50.0), None).is_none());
        assert!(LimbReference::new(&flat).is_none());
    }

    #[test]
    fn centroid_finds_a_whole_disc_and_declines_a_cut_one() {
        let img = disc_image((256, 200), (120.0, 90.0, 60.0), 1.5, 0.01, 2);
        let d = disc_centroid(&img).expect("centroid");
        assert!((f64::from(d.cx) - 120.0).abs() < 0.5 && (f64::from(d.cy) - 90.0).abs() < 0.5);
        let cut = disc_image((256, 200), (20.0, 90.0, 60.0), 1.5, 0.01, 2);
        assert!(disc_centroid(&cut).is_none());
    }

    #[test]
    fn a_frame_far_from_the_reference_is_found_afresh() {
        let reference = disc_image((400, 300), (150.0, 150.0, 60.0), 1.0, 0.0, 1);
        let lr = LimbReference::new(&reference).expect("reference");
        // 100 px away: outside the first search window.
        let img = disc_image((400, 300), (250.0, 160.0, 60.0), 1.5, 0.02, 4);
        let (s, _) = lr.estimate(&img).expect("estimate");
        assert!((s.dx - 100.0).hypot(s.dy - 10.0) < 0.1, "{s:?}");
    }

    #[test]
    fn auto_chooses_the_limb_for_a_full_disc_and_phase_for_a_cut_one() {
        let full = disc_image((256, 200), (128.0, 100.0, 70.0), 1.0, 0.0, 1);
        let plan = AlignPlan::choose(AlignMethod::Auto, &full).expect("plan");
        assert_eq!(plan.method, GlobalMethod::Limb, "{}", plan.reason);
        assert!(plan.reason.contains("full disc"), "{}", plan.reason);

        let cut = disc_image((256, 200), (40.0, 100.0, 70.0), 1.0, 0.0, 1);
        let plan = AlignPlan::choose(AlignMethod::Auto, &cut).expect("plan");
        assert_eq!(plan.method, GlobalMethod::Phase);
        assert!(plan.reason.contains("cuts the limb"), "{}", plan.reason);
        let forced = AlignPlan::choose(AlignMethod::Limb, &cut).expect("plan");
        assert_eq!(forced.method, GlobalMethod::Limb);

        let flat = Frame::new(200, 150, vec![0.3; 200 * 150]).expect("frame");
        let plan = AlignPlan::choose(AlignMethod::Auto, &flat).expect("plan");
        assert_eq!(plan.method, GlobalMethod::Phase);
        assert!(AlignPlan::choose(AlignMethod::Limb, &flat).is_err());
        let phase = AlignPlan::choose(AlignMethod::Phase, &full).expect("plan");
        assert_eq!(phase.method, GlobalMethod::Phase);
        assert!(phase.limb.is_none());
    }

    #[test]
    fn method_names_parse() {
        assert_eq!("LIMB".parse::<AlignMethod>().ok(), Some(AlignMethod::Limb));
        assert_eq!("auto".parse::<AlignMethod>().ok(), Some(AlignMethod::Auto));
        assert_eq!(
            "phase".parse::<AlignMethod>().ok(),
            Some(AlignMethod::Phase)
        );
        assert!("fft".parse::<AlignMethod>().is_err());
    }
}
