// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Multi-point (alignment point) registration.
//!
//! A grid of overlapping square windows covers the reference. For every frame
//! and every window with enough contrast, [`measure`] finds the window's
//! displacement by zero-normalised cross-correlation within a small search
//! radius around the global shift, refined to sub-pixel with a parabola, and
//! scores the sharpness of that window in the frame. Each window then stacks
//! only its own best frames, and [`blend`] merges the per-window stacks with
//! raised-cosine weights that sum to one where windows overlap by half.
//!
//! A point whose measurement is missing or below the correlation threshold does
//! not use its own noisy estimate: [`fill_missing`] replaces it with the
//! inverse-distance average of the shifts measured at neighbouring points
//! (or the global shift when none are near).
//!
//! The same fallback handles ridge-like points such as a straight stretch of
//! limb, whose correlation surface fixes the shift across the edge but not along it.
//!
//! Analysis works on the raw frame (no resampling), so the measured shift is
//! free of interpolation bias.

#![allow(
    clippy::cast_possible_wrap,
    reason = "pixel coordinates are bounded by the 65535 dimension limit and signed offsets are needed for clamping"
)]
#![allow(
    clippy::many_single_char_names,
    reason = "x, y, w, h, dx, dy and r, g, b are the natural names in image and kernel maths"
)]

use crate::align::Shift;
use crate::error::{Error, Result};
use crate::filter::gaussian_blur;
use crate::frame::{Frame, Rect};
use crate::mask::{Disc, overlaps};
use crate::quality::{WindowScorer, noise_floor};

/// Alignment point grid settings.
#[derive(Clone, Copy, Debug)]
pub struct MultipointConfig {
    /// Window edge in pixels.
    pub ap_size: usize,
    /// Distance between window centres; 0 means half the window (50 % overlap).
    pub step: usize,
    /// Minimum reference contrast (standard deviation of the smoothed,
    /// background-subtracted window over the reference peak) for a window to be
    /// measured. The default suits low-contrast H-alpha disc detail; raise it
    /// for noisy captures so empty sky is skipped.
    pub min_contrast: f32,
    /// Search radius around the global shift, in pixels.
    pub search_radius: usize,
    /// Correlation below this marks a measurement unreliable.
    pub min_correlation: f32,
    /// Gaussian sigma applied to every frame before correlating and before
    /// scoring a window's sharpness. Pixel noise otherwise makes small windows
    /// correlate on noise and pick frames by noise rather than detail.
    pub smooth: f32,
    /// Sigma of the slow background removed before correlating (0 disables).
    /// Without it the limb-darkening gradient dominates a window and the
    /// correlation peak is too flat to locate on low-contrast detail.
    pub highpass: f32,
    /// Smallest eigenvalue of the correlation surface's curvature (per pixel
    /// squared) below which the peak is too flat to locate.
    pub min_curvature: f32,
    /// Smallest ratio of the two curvature eigenvalues; below it the peak is a
    /// ridge (a straight limb) that fixes the shift across the edge only.
    pub min_isotropy: f32,
}

impl Default for MultipointConfig {
    fn default() -> Self {
        Self {
            ap_size: 64,
            step: 0,
            min_contrast: 0.001,
            search_radius: 4,
            min_correlation: 0.3,
            smooth: 1.0,
            highpass: 4.0,
            min_curvature: 0.002,
            min_isotropy: 0.25,
        }
    }
}

/// Where alignment points may not go, see [`ApGrid::build_masked`].
#[derive(Clone, Debug, Default)]
pub struct ApMask {
    /// Fitted solar disc; points need their centre inside it.
    pub disc: Option<Disc>,
    /// Pixels outside the limb where points are still allowed.
    pub band: f32,
    /// Rectangles no window may overlap.
    pub exclude: Vec<Rect>,
}

impl ApMask {
    fn allows(&self, cx: usize, cy: usize, rect: Rect) -> bool {
        self.disc.is_none_or(|d| d.contains(cx, cy, self.band))
            && !self.exclude.iter().any(|&e| overlaps(rect, e))
    }
}

/// A frame prepared for alignment point analysis, see [`ApGrid::prepare`].
pub struct Prepared {
    /// Smoothed and background-subtracted: input to [`measure`].
    pub correlate: Frame,
    /// Smoothed only: used to score window sharpness.
    pub scored: Frame,
    /// Constant-time window sharpness over `scored`.
    pub scorer: WindowScorer,
    /// [`noise_floor`] of `scored`.
    pub floor: f32,
}

/// One alignment point.
#[derive(Clone, Copy, Debug)]
pub struct Ap {
    /// Window centre, x.
    pub cx: usize,
    /// Window centre, y.
    pub cy: usize,
    /// Window clipped to the image.
    pub rect: Rect,
    /// Measured reference contrast.
    pub contrast: f32,
    /// False when the window was skipped as featureless.
    pub active: bool,
}

/// The full grid.
#[derive(Clone, Debug)]
pub struct ApGrid {
    /// Image width.
    pub width: usize,
    /// Image height.
    pub height: usize,
    /// Window edge.
    pub size: usize,
    /// Points, row-major.
    pub aps: Vec<Ap>,
    smooth: f32,
    highpass: f32,
}

fn centres(extent: usize, step: usize) -> impl Iterator<Item = usize> {
    let n = extent.div_ceil(step) + 1;
    (0..n).map(move |i| i * step)
}

impl ApGrid {
    /// Lay out the grid over `reference` and mark featureless points inactive.
    ///
    /// # Errors
    /// [`Error::Invalid`] for a window smaller than 8 pixels.
    pub fn build(reference: &Frame, cfg: &MultipointConfig) -> Result<Self> {
        Self::build_masked(reference, cfg, &ApMask::default())
    }

    /// As [`Self::build`], and mark inactive every point whose centre is
    /// outside the disc (plus its prominence band) or whose window overlaps an
    /// excluded rectangle.
    ///
    /// # Errors
    /// [`Error::Invalid`] for a window smaller than 8 pixels.
    pub fn build_masked(reference: &Frame, cfg: &MultipointConfig, mask: &ApMask) -> Result<Self> {
        if cfg.ap_size < 8 {
            return Err(Error::Invalid(
                "alignment point size must be at least 8 pixels".into(),
            ));
        }
        let step = if cfg.step == 0 {
            cfg.ap_size / 2
        } else {
            cfg.step
        };
        let (w, h) = (reference.width, reference.height);
        let probe = Self {
            width: w,
            height: h,
            size: cfg.ap_size,
            aps: Vec::new(),
            smooth: cfg.smooth,
            highpass: cfg.highpass,
        };
        let prepared = probe.prepare(reference);
        let smooth = &prepared.correlate;
        let peak = prepared.scored.min_max().1.max(1e-9);
        let half = cfg.ap_size / 2;
        let mut aps = Vec::new();
        for cy in centres(h, step) {
            for cx in centres(w, step) {
                let rect = Rect {
                    x0: cx.saturating_sub(half),
                    y0: cy.saturating_sub(half),
                    x1: (cx + half).min(w),
                    y1: (cy + half).min(h),
                };
                let contrast = if rect.area() >= 16 {
                    std_dev(smooth, rect) / peak
                } else {
                    0.0
                };
                aps.push(Ap {
                    cx,
                    cy,
                    rect,
                    contrast,
                    active: contrast >= cfg.min_contrast && mask.allows(cx, cy, rect),
                });
            }
        }
        Ok(Self {
            width: w,
            height: h,
            size: cfg.ap_size,
            aps,
            smooth: cfg.smooth,
            highpass: cfg.highpass,
        })
    }

    /// The two views of a frame that analysis needs: `scored` is the frame
    /// smoothed by `smooth` (used to score window sharpness) and `correlate`
    /// is that minus a `highpass`-sigma background (used to find shifts).
    #[must_use]
    pub fn prepare(&self, frame: &Frame) -> Prepared {
        let scored = if self.smooth > 0.0 {
            gaussian_blur(frame, self.smooth)
        } else {
            frame.clone()
        };
        let mut correlate = scored.clone();
        if self.highpass > 0.0 {
            let low = gaussian_blur(frame, self.highpass);
            correlate
                .data
                .iter_mut()
                .zip(&low.data)
                .for_each(|(c, l)| *c -= l);
        }
        let floor = noise_floor(&scored);
        let windows = WindowScorer::new(&scored);
        Prepared {
            correlate,
            scored,
            scorer: windows,
            floor,
        }
    }

    /// Number of points that will be used.
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.aps.iter().filter(|a| a.active).count()
    }

    /// Raised-cosine weights of `ap` over its clipped rectangle.
    #[must_use]
    pub fn window(&self, ap: &Ap) -> Vec<f32> {
        let size = self.size as f32;
        let axis = |c: usize, lo: usize, hi: usize| -> Vec<f32> {
            (lo..hi)
                .map(|p| {
                    let t = (p as f32 + 0.5 - c as f32) / size;
                    if t.abs() < 0.5 {
                        1.0f32.midpoint((std::f32::consts::TAU * t).cos())
                    } else {
                        0.0
                    }
                })
                .collect()
        };
        let wx = axis(ap.cx, ap.rect.x0, ap.rect.x1);
        let wy = axis(ap.cy, ap.rect.y0, ap.rect.y1);
        wy.iter()
            .flat_map(|y| wx.iter().map(move |x| x * y))
            .collect()
    }

    /// Zero-mean correlation templates for the active points (`None` when inactive).
    #[must_use]
    pub fn templates(&self, reference: &Frame) -> Vec<Option<ApTemplate>> {
        let reference = &self.prepare(reference).correlate;
        self.aps
            .iter()
            .map(|ap| {
                if !ap.active {
                    return None;
                }
                let r = ap.rect;
                let mut data = Vec::with_capacity(r.area());
                for y in r.y0..r.y1 {
                    data.extend_from_slice(&reference.row(y)[r.x0..r.x1]);
                }
                let mean = data.iter().sum::<f32>() / data.len() as f32;
                for v in &mut data {
                    *v -= mean;
                }
                let norm = data.iter().map(|v| v * v).sum::<f32>().sqrt();
                (norm > 1e-9).then_some(ApTemplate {
                    rect: r,
                    data,
                    norm,
                })
            })
            .collect()
    }
}

fn std_dev(f: &Frame, r: Rect) -> f32 {
    let (mut s, mut s2, n) = (0.0f64, 0.0f64, r.area() as f64);
    for y in r.y0..r.y1 {
        for &v in &f.row(y)[r.x0..r.x1] {
            s += f64::from(v);
            s2 += f64::from(v) * f64::from(v);
        }
    }
    ((s2 / n - (s / n).powi(2)).max(0.0)).sqrt() as f32
}

/// Zero-mean reference patch of one point.
pub struct ApTemplate {
    rect: Rect,
    data: Vec<f32>,
    norm: f32,
}

impl ApTemplate {
    /// Window of the reference this patch was cut from.
    #[must_use]
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Zero-mean patch samples, row-major over [`Self::rect`].
    #[must_use]
    pub fn data(&self) -> &[f32] {
        &self.data
    }

    /// Euclidean norm of [`Self::data`].
    #[must_use]
    pub fn norm(&self) -> f32 {
        self.norm
    }
}

/// Outcome of measuring one point in one frame.
#[derive(Clone, Copy, Debug)]
pub struct ApMeasure {
    /// Total displacement of the point's content relative to the reference.
    pub shift: Shift,
    /// Sharpness of the window in the frame.
    pub quality: f32,
    /// Peak zero-normalised correlation (0 for interpolated entries).
    pub correlation: f32,
    /// True when `shift` was interpolated from neighbouring points.
    pub interpolated: bool,
}

struct QuadFit {
    dx: f32,
    dy: f32,
    lambda_min: f32,
    lambda_max: f32,
}

/// Peak offset and smallest curvature eigenvalue of a 3x3 patch (row-major),
/// from a second-order Taylor fit at the centre. `None` if not a maximum.
fn quadratic_fit(nb: &[f32; 9]) -> Option<QuadFit> {
    let c = nb[4];
    let (gx, gy) = (0.5 * (nb[5] - nb[3]), 0.5 * (nb[7] - nb[1]));
    let (hxx, hyy) = (nb[3] - 2.0 * c + nb[5], nb[1] - 2.0 * c + nb[7]);
    let hxy = 0.25 * (nb[8] - nb[6] - nb[2] + nb[0]);
    let tr = -(hxx + hyy);
    let det = hxx * hyy - hxy * hxy;
    let disc = (tr * tr / 4.0 - det).max(0.0).sqrt();
    let lambda_min = tr / 2.0 - disc;
    if lambda_min.is_nan() || lambda_min <= 0.0 || !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let dx = -(hyy * gx - hxy * gy) / det;
    let dy = -(hxx * gy - hxy * gx) / det;
    Some(QuadFit {
        dx: dx.clamp(-0.5, 0.5),
        dy: dy.clamp(-0.5, 0.5),
        lambda_min,
        lambda_max: tr / 2.0 + disc,
    })
}

/// Zero-normalised correlation of the template against `frame` at every
/// integer offset within `r` of `(gx, gy)`, row-major `(2r + 1)^2`. Offsets
/// whose window leaves the frame are `NaN`.
fn correlation_surface(t: &ApTemplate, frame: &Frame, gx: i64, gy: i64, r: i64) -> Vec<f32> {
    let side = (2 * r + 1) as usize;
    let (fw, fh) = (frame.width as i64, frame.height as i64);
    let (rw, rh) = (t.rect.width(), t.rect.height());
    let n = (rw * rh) as f64;
    let mut surface = vec![f32::NAN; side * side];
    // Frame window for offset (ox, oy) starts at (ax + ox, ay + oy), ox, oy in 0..side.
    let (ax, ay) = (t.rect.x0 as i64 + gx - r, t.rect.y0 as i64 + gy - r);
    let (lo_x, lo_y) = (ax.max(0) - ax, ay.max(0) - ay);
    let hi_x = (side as i64).min(fw - rw as i64 - ax + 1);
    let hi_y = (side as i64).min(fh - rh as i64 - ay + 1);
    if lo_x >= hi_x || lo_y >= hi_y {
        return surface;
    }
    let (lo_x, hi_x, lo_y, hi_y) = (lo_x as usize, hi_x as usize, lo_y as usize, hi_y as usize);
    // Window sums of p and p^2 for every valid offset from running sums: the
    // template-independent terms of the normalisation cost O(1) per offset.
    let span_x = hi_x - lo_x + rw - 1;
    let span_y = hi_y - lo_y + rh - 1;
    let (x_start, y_start) = ((ax + lo_x as i64) as usize, (ay + lo_y as i64) as usize);
    let mut col_sum = vec![0.0f64; span_y * (hi_x - lo_x)];
    let mut col_sq = vec![0.0f64; span_y * (hi_x - lo_x)];
    for yy in 0..span_y {
        let row = &frame.data[(y_start + yy) * frame.width + x_start..][..span_x];
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for &v in &row[..rw] {
            s1 += f64::from(v);
            s2 += f64::from(v) * f64::from(v);
        }
        for ox in 0..hi_x - lo_x {
            col_sum[yy * (hi_x - lo_x) + ox] = s1;
            col_sq[yy * (hi_x - lo_x) + ox] = s2;
            if let (Some(&out), Some(&inn)) = (row.get(ox), row.get(ox + rw)) {
                s1 += f64::from(inn) - f64::from(out);
                s2 += f64::from(inn) * f64::from(inn) - f64::from(out) * f64::from(out);
            }
        }
    }
    let cols = hi_x - lo_x;
    for oy in lo_y..hi_y {
        for ox in lo_x..hi_x {
            let c = ox - lo_x;
            let (mut sp, mut spp) = (0.0f64, 0.0f64);
            for row in oy - lo_y..oy - lo_y + rh {
                sp += col_sum[row * cols + c];
                spp += col_sq[row * cols + c];
            }
            let var = spp - sp * sp / n;
            if var > 1e-12 {
                let base = (ay + oy as i64) as usize * frame.width + (ax + ox as i64) as usize;
                let stp = dot_rows(&frame.data[base..], frame.width, &t.data, rw, rh);
                surface[oy * side + ox] = (stp / (f64::from(t.norm) * var.sqrt())) as f32;
            }
        }
    }
    surface
}

/// `sum(p[row][col] * t[row][col])` over an `rw` x `rh` window of `p` (whose
/// rows are `pitch` apart) and the template `t`. Eight f32 lanes per row
/// block keep the loop vectorised; partial sums are widened every few rows.
fn dot_rows(p: &[f32], pitch: usize, t: &[f32], rw: usize, rh: usize) -> f64 {
    const LANES: usize = 8;
    let mut total = 0.0f64;
    let mut lanes = [0.0f32; LANES];
    for row in 0..rh {
        let pr = &p[row * pitch..][..rw];
        let tr = &t[row * rw..][..rw];
        let ((pc, p_rest), (tc, t_rest)) = (pr.as_chunks::<LANES>(), tr.as_chunks::<LANES>());
        for (a, b) in pc.iter().zip(tc) {
            for ((l, &x), &y) in lanes.iter_mut().zip(a).zip(b) {
                *l += x * y;
            }
        }
        for (&x, &y) in p_rest.iter().zip(t_rest) {
            lanes[0] += x * y;
        }
        if row % 16 == 15 {
            total += lanes.iter().map(|&l| f64::from(l)).sum::<f64>();
            lanes = [0.0; LANES];
        }
    }
    total + lanes.iter().map(|&l| f64::from(l)).sum::<f64>()
}

/// Where the peak of one correlation surface lies.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PeakFit {
    /// Peak correlation at the best integer offset.
    pub best: f32,
    /// Best integer offset from the search centre, in x.
    pub ox: i64,
    /// Best integer offset from the search centre, in y.
    pub oy: i64,
    /// Sub-pixel correction to the offset, in x (within half a pixel).
    pub dx: f32,
    /// Sub-pixel correction to the offset, in y.
    pub dy: f32,
}

/// Locate the peak of `surface`, a `(2r + 1)^2` row-major grid of correlations
/// (`NaN` where the window left the frame), `r` being
/// [`MultipointConfig::search_radius`]. `None` when the correlation is weak,
/// the best match sits on the search boundary or the peak is flat or a ridge.
#[must_use]
pub fn fit_peak(surface: &[f32], cfg: &MultipointConfig) -> Option<PeakFit> {
    let r = cfg.search_radius as i64;
    let side = (2 * r + 1) as usize;
    let (mut best, mut bi) = (f32::NEG_INFINITY, usize::MAX);
    for (i, &v) in surface.iter().enumerate() {
        if v > best {
            best = v;
            bi = i;
        }
    }
    if bi == usize::MAX || best < cfg.min_correlation {
        return None;
    }
    let (bx, by) = (bi % side, bi / side);
    if bx == 0 || by == 0 || bx == side - 1 || by == side - 1 {
        return None;
    }
    let at = |x: usize, y: usize| surface[y * side + x];
    let nb = [
        at(bx - 1, by - 1),
        at(bx, by - 1),
        at(bx + 1, by - 1),
        at(bx - 1, by),
        best,
        at(bx + 1, by),
        at(bx - 1, by + 1),
        at(bx, by + 1),
        at(bx + 1, by + 1),
    ];
    let (dx, dy) = fit_patch(&nb, cfg)?;
    Some(PeakFit {
        best,
        ox: bx as i64 - r,
        oy: by as i64 - r,
        dx,
        dy,
    })
}

/// Sub-pixel correction `(dx, dy)` from the 3x3 neighbourhood `nb` (row-major)
/// of an interior correlation peak, `nb[4]` being the peak. `None` when a
/// value is not finite, or the peak is flat or a ridge. This is the part of
/// [`fit_peak`] after the search for the maximum, so that a backend which finds
/// the maximum elsewhere can send back the nine values and nothing else.
#[must_use]
pub fn fit_patch(nb: &[f32; 9], cfg: &MultipointConfig) -> Option<(f32, f32)> {
    if nb.iter().any(|v| !v.is_finite()) {
        return None;
    }
    // 2-D quadratic fit on the 3x3 neighbourhood: gradient g and Hessian h of
    // the correlation surface at the best integer offset.
    let raw = quadratic_fit(nb)?;
    // A ridge (a straight limb) constrains only one axis and gives an
    // arbitrary shift along it, so both curvature eigenvalues must be clear and
    // of similar size.
    if raw.lambda_min < cfg.min_curvature || raw.lambda_min < cfg.min_isotropy * raw.lambda_max {
        return None;
    }
    // Texture autocorrelation is close to Gaussian, for which a parabola on
    // the logarithm is exact; use it when every neighbour is positive.
    Some(if nb.iter().all(|&v| v > 1e-3) {
        let ln = nb.map(f32::ln);
        quadratic_fit(&ln).map_or((raw.dx, raw.dy), |f| (f.dx, f.dy))
    } else {
        (raw.dx, raw.dy)
    })
}

/// The measurement of a point whose peak a backend found at offset `(ox, oy)`
/// from the integer global shift `(gx, gy)`, with the 3x3 neighbourhood `nb`
/// of the peak. As [`measurement_from_surface`]; the quality is left at 0.
#[must_use]
pub fn measurement_from_patch(
    t: &ApTemplate,
    nb: &[f32; 9],
    (ox, oy): (i64, i64),
    (gx, gy): (i64, i64),
    cfg: &MultipointConfig,
) -> Option<(ApMeasure, Rect)> {
    let (dx, dy) = fit_patch(nb, cfg)?;
    let (ix, iy) = (gx + ox, gy + oy);
    Some((
        ApMeasure {
            shift: Shift::new(ix as f32 + dx, iy as f32 + dy),
            quality: 0.0,
            correlation: nb[4],
            interpolated: false,
        },
        displaced(t, ix, iy),
    ))
}

/// The window of template `t` at integer displacement `(ix, iy)`.
fn displaced(t: &ApTemplate, ix: i64, iy: i64) -> Rect {
    Rect::new(
        (t.rect.x0 as i64 + ix) as usize,
        (t.rect.y0 as i64 + iy) as usize,
        t.rect.width(),
        t.rect.height(),
    )
}

/// The measurement of a point from its correlation `surface` (computed around
/// the integer global shift `(gx, gy)`), with the sharpness of the matched
/// window supplied by `score`. Shared by the CPU and by backends that compute
/// the surface elsewhere. Returns the matched window with the result so the
/// caller can score it.
#[must_use]
pub fn measurement_from_surface(
    t: &ApTemplate,
    surface: &[f32],
    gx: i64,
    gy: i64,
    cfg: &MultipointConfig,
) -> Option<(ApMeasure, Rect)> {
    let fit = fit_peak(surface, cfg)?;
    let (ix, iy) = (gx + fit.ox, gy + fit.oy);
    let win = displaced(t, ix, iy);
    Some((
        ApMeasure {
            shift: Shift::new(ix as f32 + fit.dx, iy as f32 + fit.dy),
            quality: 0.0,
            correlation: fit.best,
            interpolated: false,
        },
        win,
    ))
}

/// Measure one point in a prepared frame, searching around the global shift
/// `guess`. Returns `None` when the window leaves the frame, the correlation is
/// weak or the best match sits on the search boundary. The window is
/// correlated on [`Prepared::correlate`] (smoothed, background removed) and its
/// sharpness scored on [`Prepared::scored`] (smoothed, see
/// [`MultipointConfig::smooth`]) against [`Prepared::floor`].
#[must_use]
pub fn measure(
    t: &ApTemplate,
    prep: &Prepared,
    guess: Shift,
    cfg: &MultipointConfig,
) -> Option<ApMeasure> {
    let (gx, gy) = (guess.dx.round() as i64, guess.dy.round() as i64);
    let surface = correlation_surface(t, &prep.correlate, gx, gy, cfg.search_radius as i64);
    let (mut m, win) = measurement_from_surface(t, &surface, gx, gy, cfg)?;
    m.quality = prep.scorer.score(win, prep.floor);
    Some(m)
}

/// An interpolated measurement to be scored: [`plan_fill`].
#[derive(Clone, Copy, Debug)]
pub struct FillPlan {
    /// Index of the alignment point.
    pub ap: usize,
    /// Inverse-distance average of the neighbouring shifts (or the guess).
    pub shift: Shift,
    /// The matched window, to score.
    pub window: Rect,
}

/// The points of `row` that are active but unmeasured, with the shift they
/// take from their measured neighbours (the inverse-square-distance average
/// within 1.5 window sizes, else `guess`) and the window to score at that
/// position in a `width` x `height` frame. Points whose window would leave the
/// frame are left out.
#[must_use]
pub fn plan_fill(
    grid: &ApGrid,
    templates: &[Option<ApTemplate>],
    row: &[Option<ApMeasure>],
    guess: Shift,
    width: usize,
    height: usize,
) -> Vec<FillPlan> {
    let measured: Vec<(usize, ApMeasure)> = row
        .iter()
        .enumerate()
        .filter_map(|(a, m)| m.map(|m| (a, m)))
        .collect();
    let reach = (grid.size as f32 * 1.5).powi(2);
    let mut plans = Vec::new();
    for (a, slot) in row.iter().enumerate() {
        let Some(t) = templates[a].as_ref().filter(|_| slot.is_none()) else {
            continue;
        };
        let ap = &grid.aps[a];
        let (mut sx, mut sy, mut sw) = (0.0f32, 0.0f32, 0.0f32);
        for (b, m) in &measured {
            let o = &grid.aps[*b];
            let d2 = (o.cx as f32 - ap.cx as f32).powi(2) + (o.cy as f32 - ap.cy as f32).powi(2);
            if d2 <= reach {
                let w = 1.0 / d2.max(1.0);
                sx += w * m.shift.dx;
                sy += w * m.shift.dy;
                sw += w;
            }
        }
        let shift = if sw > 0.0 {
            Shift::new(sx / sw, sy / sw)
        } else {
            guess
        };
        let (x0, y0) = (
            t.rect.x0 as i64 + shift.dx.round() as i64,
            t.rect.y0 as i64 + shift.dy.round() as i64,
        );
        let (rw, rh) = (t.rect.width() as i64, t.rect.height() as i64);
        if x0 < 0 || y0 < 0 || x0 + rw > width as i64 || y0 + rh > height as i64 {
            continue;
        }
        plans.push(FillPlan {
            ap: a,
            shift,
            window: Rect::new(x0 as usize, y0 as usize, rw as usize, rh as usize),
        });
    }
    plans
}

/// Replace missing measurements of active points by the inverse-square-distance
/// average of the measured neighbours within 1.5 window sizes (else `guess`),
/// and score the window at that position. Points whose window would leave the
/// frame stay `None`.
pub fn fill_missing(
    grid: &ApGrid,
    templates: &[Option<ApTemplate>],
    row: &mut [Option<ApMeasure>],
    prep: &Prepared,
    guess: Shift,
) {
    let plans = plan_fill(
        grid,
        templates,
        row,
        guess,
        prep.scored.width,
        prep.scored.height,
    );
    for p in plans {
        row[p.ap] = Some(ApMeasure {
            shift: p.shift,
            quality: prep.scorer.score(p.window, prep.floor),
            correlation: 0.0,
            interpolated: true,
        });
    }
}

/// Per-frame, per-point measurements, flat `[frame * n_aps + ap]`.
pub struct LocalTable {
    /// Points per frame.
    pub n_aps: usize,
    /// Measured shifts (valid where `quality` is finite).
    pub shifts: Vec<Shift>,
    /// Window sharpness, `NaN` where the measurement failed.
    pub quality: Vec<f32>,
}

impl LocalTable {
    /// Empty table.
    #[must_use]
    pub fn new(n_frames: usize, n_aps: usize) -> Self {
        Self {
            n_aps,
            shifts: vec![Shift::default(); n_frames * n_aps],
            quality: vec![f32::NAN; n_frames * n_aps],
        }
    }

    /// Store one frame's measurements.
    pub fn set_row(&mut self, frame: usize, row: &[Option<ApMeasure>]) {
        for (a, m) in row.iter().enumerate() {
            if let Some(m) = m {
                self.shifts[frame * self.n_aps + a] = m.shift;
                self.quality[frame * self.n_aps + a] = m.quality;
            }
        }
    }

    /// The `count` best frames for point `ap`, best first.
    #[must_use]
    pub fn best_frames(&self, n_frames: usize, ap: usize, count: usize) -> Vec<usize> {
        let mut v: Vec<(f32, usize)> = (0..n_frames)
            .map(|f| (self.quality[f * self.n_aps + ap], f))
            .filter(|(q, _)| q.is_finite())
            .collect();
        v.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        v.truncate(count);
        v.into_iter().map(|(_, f)| f).collect()
    }
}

/// Merge per-point stacks. `tiles[i]` is the stack of `grid.aps[i]` over its
/// rectangle (`NaN` = no data) or `None` for unused points. Where the summed
/// weight is below one, `fallback` fills the remainder.
#[must_use]
pub fn blend(grid: &ApGrid, tiles: &[Option<Vec<f32>>], fallback: &Frame) -> Frame {
    let (w, h) = (grid.width, grid.height);
    let mut num = vec![0.0f32; w * h];
    let mut den = vec![0.0f32; w * h];
    for (ap, tile) in grid.aps.iter().zip(tiles) {
        let Some(tile) = tile else { continue };
        let win = grid.window(ap);
        let rw = ap.rect.width();
        for y in ap.rect.y0..ap.rect.y1 {
            for x in ap.rect.x0..ap.rect.x1 {
                let i = (y - ap.rect.y0) * rw + (x - ap.rect.x0);
                let (v, wt) = (tile[i], win[i]);
                if !v.is_nan() && wt > 0.0 {
                    num[y * w + x] += wt * v;
                    den[y * w + x] += wt;
                }
            }
        }
    }
    let data = (0..w * h)
        .map(|i| (num[i] + (1.0 - den[i]).max(0.0) * fallback.data[i]) / den[i].max(1.0))
        .collect();
    Frame {
        width: w,
        height: h,
        data,
        bayer: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{SyntheticConfig, SyntheticSun};

    #[test]
    fn windows_sum_to_one_in_the_interior_and_background_is_skipped() {
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let r = sun.render_clean(Shift::default(), None);
        let cfg = MultipointConfig {
            ap_size: 32,
            min_contrast: 0.01,
            ..MultipointConfig::default()
        };
        let grid = ApGrid::build(&r, &cfg).expect("grid");
        let mut sum = vec![0.0f32; r.width * r.height];
        for ap in &grid.aps {
            let win = grid.window(ap);
            for y in ap.rect.y0..ap.rect.y1 {
                for x in ap.rect.x0..ap.rect.x1 {
                    sum[y * r.width + x] +=
                        win[(y - ap.rect.y0) * ap.rect.width() + (x - ap.rect.x0)];
                }
            }
        }
        for y in 0..r.height {
            for x in 0..r.width {
                assert!(
                    (sum[y * r.width + x] - 1.0).abs() < 1e-4,
                    "({x},{y}) {}",
                    sum[y * r.width + x]
                );
            }
        }
        let corner = grid
            .aps
            .iter()
            .find(|a| a.cx == 0 && a.cy == 0)
            .expect("corner point");
        assert!(!corner.active, "corner is empty sky");
        assert!(grid.active_count() > 10 && grid.active_count() < grid.aps.len());
        assert!(ApGrid::build(&r, &MultipointConfig { ap_size: 4, ..cfg }).is_err());
    }

    #[test]
    fn measures_known_local_shifts_to_a_tenth_of_a_pixel() {
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let reference = sun.render_clean(Shift::default(), None);
        let cfg = MultipointConfig {
            ap_size: 32,
            min_contrast: 0.01,
            ..MultipointConfig::default()
        };
        let grid = ApGrid::build(&reference, &cfg).expect("grid");
        let templates = grid.templates(&reference);
        let truth = Shift::new(1.3, -0.7);
        let frame = crate::filter::gaussian_blur(&sun.render_clean(truth, None), 1.0);
        let prep = grid.prepare(&frame);
        // Global estimate is off by a pixel; the search must absorb it.
        let guess = Shift::new(0.4, 0.2);
        let (mut n, mut worst) = (0, 0.0f32);
        for t in templates.iter().flatten() {
            if let Some(m) = measure(t, &prep, guess, &cfg) {
                n += 1;
                worst = worst.max((m.shift - truth).length());
            }
        }
        assert!(n >= 8, "only {n} points measured");
        assert!(worst < 0.1, "worst local error {worst}");
        eprintln!("multipoint: {n} points, worst error {worst:.4} px");
    }

    /// The original one-offset-at-a-time evaluation.
    fn naive_surface(t: &ApTemplate, frame: &Frame, gx: i64, gy: i64, r: i64) -> Vec<f32> {
        let side = (2 * r + 1) as usize;
        let (fw, fh) = (frame.width as i64, frame.height as i64);
        let (rw, rh) = (t.rect.width(), t.rect.height());
        let n = (rw * rh) as f64;
        let mut surface = vec![f32::NAN; side * side];
        for oy in -r..=r {
            for ox in -r..=r {
                let x0 = t.rect.x0 as i64 + gx + ox;
                let y0 = t.rect.y0 as i64 + gy + oy;
                if x0 < 0 || y0 < 0 || x0 + rw as i64 > fw || y0 + rh as i64 > fh {
                    continue;
                }
                let (mut sp, mut spp, mut stp) = (0.0f64, 0.0f64, 0.0f64);
                for row in 0..rh {
                    let base = (y0 as usize + row) * frame.width + x0 as usize;
                    for (&pv, &tv) in frame.data[base..base + rw]
                        .iter()
                        .zip(&t.data[row * rw..(row + 1) * rw])
                    {
                        sp += f64::from(pv);
                        spp += f64::from(pv) * f64::from(pv);
                        stp += f64::from(pv) * f64::from(tv);
                    }
                }
                let var = spp - sp * sp / n;
                if var > 1e-12 {
                    surface[((oy + r) as usize) * side + (ox + r) as usize] =
                        (stp / (f64::from(t.norm) * var.sqrt())) as f32;
                }
            }
        }
        surface
    }

    #[test]
    fn fast_correlation_surface_matches_the_naive_one_including_borders() {
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let reference = sun.render_clean(Shift::default(), None);
        let cfg = MultipointConfig {
            ap_size: 32,
            min_contrast: 0.01,
            ..MultipointConfig::default()
        };
        let grid = ApGrid::build(&reference, &cfg).expect("grid");
        let templates = grid.templates(&reference);
        let frame = grid
            .prepare(&sun.render_clean(Shift::new(0.6, -0.3), None))
            .correlate;
        let (mut compared, mut partial) = (0, 0);
        for t in templates.iter().flatten() {
            // Guesses that push the search window off every edge of the frame.
            for (gx, gy) in [(0, 0), (3, -2), (-40, 0), (0, 70), (90, 90), (-200, 5)] {
                let fast = correlation_surface(t, &frame, gx, gy, 4);
                let slow = naive_surface(t, &frame, gx, gy, 4);
                partial += usize::from(
                    slow.iter().any(|v| v.is_nan()) && !slow.iter().all(|v| v.is_nan()),
                );
                for (a, b) in fast.iter().zip(&slow) {
                    if b.is_nan() {
                        assert!(a.is_nan());
                    } else {
                        assert!((a - b).abs() < 2e-5, "{a} vs {b}");
                        compared += 1;
                    }
                }
            }
        }
        assert!(compared > 1000, "{compared} offsets compared");
        assert!(partial > 0, "no window straddled the frame edge");
    }

    #[test]
    fn best_frames_orders_by_quality_and_skips_failures() {
        let mut t = LocalTable::new(4, 1);
        let m = |q| {
            Some(ApMeasure {
                shift: Shift::default(),
                quality: q,
                correlation: 1.0,
                interpolated: false,
            })
        };
        for (f, q) in [(0, 1.0), (2, 3.0), (3, 2.0)] {
            t.set_row(f, &[m(q)]);
        }
        assert_eq!(t.best_frames(4, 0, 2), vec![2, 3]);
        assert_eq!(t.best_frames(4, 0, 10), vec![2, 3, 0]);
    }

    #[test]
    fn missing_points_take_neighbouring_shifts() {
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let reference = sun.render_clean(Shift::default(), None);
        let cfg = MultipointConfig {
            ap_size: 32,
            min_contrast: 0.01,
            ..MultipointConfig::default()
        };
        let grid = ApGrid::build(&reference, &cfg).expect("grid");
        let templates = grid.templates(&reference);
        let truth = Shift::new(1.0, 2.0);
        let frame = sun.render_clean(truth, None);
        let prep = grid.prepare(&frame);
        let mut row: Vec<Option<ApMeasure>> = templates
            .iter()
            .map(|t| t.as_ref().and_then(|t| measure(t, &prep, truth, &cfg)))
            .collect();
        // Knock out one well-supported point and check it is rebuilt from its neighbours.
        let victim = (0..row.len())
            .find(|&a| row[a].is_some())
            .expect("a measured point");
        let own = row[victim].expect("measured").shift;
        row[victim] = None;
        fill_missing(&grid, &templates, &mut row, &prep, Shift::default());
        let filled = row[victim].expect("filled");
        assert!(filled.interpolated && filled.correlation == 0.0);
        assert!(
            (filled.shift - own).length() < 0.3,
            "{:?} vs {:?}",
            filled.shift,
            own
        );
    }

    #[test]
    fn mask_keeps_points_inside_the_disc_and_off_exclusions() {
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let reference = sun.render_clean(Shift::default(), None);
        let cfg = MultipointConfig {
            ap_size: 32,
            min_contrast: 0.0,
            ..MultipointConfig::default()
        };
        let (cx, cy) = sun.centre;
        let disc = Disc {
            cx,
            cy,
            r: sun.radius,
        };
        let all = ApGrid::build(&reference, &cfg)
            .expect("grid")
            .active_count();
        let on_disc = |band: f32, exclude: Vec<Rect>| {
            let mask = ApMask {
                disc: Some(disc),
                band,
                exclude,
            };
            ApGrid::build_masked(&reference, &cfg, &mask).expect("grid")
        };
        let tight = on_disc(0.0, Vec::new());
        let banded = on_disc(16.0, Vec::new());
        assert!(tight.active_count() < banded.active_count());
        assert!(banded.active_count() < all);
        for ap in tight.aps.iter().filter(|a| a.active) {
            assert!((ap.cx as f32 - cx).hypot(ap.cy as f32 - cy) <= sun.radius);
        }
        let hole = Rect::new(cx as usize - 10, cy as usize - 10, 20, 20);
        let holed = on_disc(0.0, vec![hole]);
        assert!(holed.active_count() < tight.active_count());
        assert!(
            holed
                .aps
                .iter()
                .all(|a| !(a.active && overlaps(a.rect, hole)))
        );
    }

    #[test]
    fn blend_uses_fallback_where_no_tiles() {
        let f = Frame {
            width: 4,
            height: 4,
            data: vec![0.7; 16],
            bayer: None,
        };
        let grid = ApGrid {
            width: 4,
            height: 4,
            size: 8,
            aps: Vec::new(),
            smooth: 1.0,
            highpass: 4.0,
        };
        let out = blend(&grid, &[], &f);
        assert!(out.data.iter().all(|&v| (v - 0.7).abs() < 1e-6));
    }
}
