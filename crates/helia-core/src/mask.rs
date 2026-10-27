// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Places the alignment must not look: burned-in text, user exclusions and the
//! sky outside the disc.
//!
//! Three pieces:
//!
//! * [`detect_overlay`] finds burned-in text (a `SharpCap` timestamp) from a
//!   few frames spread over the capture: pixels that flip between near-white
//!   and dark from frame to frame, in a small region at a frame edge.
//! * [`fit_disc`] fits a circle to the limb of the reference image, robust to
//!   prominences, flares and a limb cut by the frame edge.
//! * [`MaskedSource`] wraps a capture and overwrites the masked rectangles with
//!   the plane that fits the sky around them ([`fill_planes`]) in every frame it serves, so scoring, registration,
//!   alignment points and the stack all see the same thing, on the CPU and on
//!   the GPU (which decode from the raw bytes this wrapper edits).
//!
//! A masked rectangle is therefore smooth and continuous with its surroundings
//! in the result: a timestamp is removed rather than smeared, and leaves no bar.
#![allow(
    clippy::many_single_char_names,
    reason = "x, y, w, h, r are the natural names in image maths"
)]

use crate::error::Result;
use crate::filter::gaussian_blur;
use crate::frame::{BayerPattern, Frame, Rect};
use crate::io::raw::{Planes, RowLayout, Sample, SharedFrames};
use crate::io::source::FrameSource;

/// What to keep the alignment away from.
#[derive(Clone, Debug, PartialEq)]
pub struct MaskConfig {
    /// Fit the limb and place alignment points on the disc only.
    pub disc: bool,
    /// Pixels outside the fitted limb where alignment points are still
    /// allowed (for prominences). 0 keeps them on the disc.
    pub prominence_band: usize,
    /// Detect burned-in text and mask it.
    pub overlay: bool,
    /// Rectangles the user excludes, in pixels.
    pub exclude: Vec<Rect>,
}

impl Default for MaskConfig {
    fn default() -> Self {
        Self {
            disc: true,
            prominence_band: 0,
            overlay: true,
            exclude: Vec::new(),
        }
    }
}

impl MaskConfig {
    /// No masking at all.
    #[must_use]
    pub fn off() -> Self {
        Self {
            disc: false,
            prominence_band: 0,
            overlay: false,
            exclude: Vec::new(),
        }
    }
}

/// Parse `x,y,w,h` (pixels).
///
/// # Errors
/// [`crate::Error::Invalid`] when it is not four non-negative integers with a
/// positive width and height.
pub fn parse_region(s: &str) -> Result<Rect> {
    let bad = || {
        crate::Error::Invalid(format!(
            "exclusion region {s:?}: expected x,y,w,h in pixels (for example 0,0,200,20)"
        ))
    };
    let v: Vec<usize> = s
        .split(',')
        .map(|p| p.trim().parse::<usize>().map_err(|_| bad()))
        .collect::<Result<_>>()?;
    let [x, y, w, h] = v[..] else {
        return Err(bad());
    };
    if w == 0 || h == 0 {
        return Err(bad());
    }
    Ok(Rect::new(x, y, w, h))
}

/// True when the rectangles share at least one pixel.
#[must_use]
pub fn overlaps(a: Rect, b: Rect) -> bool {
    a.x0 < b.x1 && b.x0 < a.x1 && a.y0 < b.y1 && b.y0 < a.y1
}

/// Frames wanted by [`detect_overlay`].
pub const OVERLAY_SAMPLES: usize = 16;

/// `want` indices spread evenly over `n` frames (all of them when `n <= want`).
#[must_use]
pub fn sample_indices(n: usize, want: usize) -> Vec<usize> {
    let want = want.min(n);
    (0..want).map(|i| i * (n - 1) / (want - 1).max(1)).collect()
}

/// Find burned-in text in `frames` (mono, `0..=1`, a handful spread over the
/// capture). Returns padded rectangles, empty when there is none.
///
/// A pixel counts as text when it reaches white (0.95 or more) in some frame
/// yet drops by more than 0.6 in another: a changing digit. The disc's limb is also a
/// large swing under drift, so a region is accepted only when it is small
/// (a line of text) and lies in the top or bottom strip of the frame (a
/// sixteenth of its height, at least 32 rows). The static part of the line (the
/// date) is added by growing the region sideways along near-white columns.
#[must_use]
pub fn detect_overlay(frames: &[Frame]) -> Vec<Rect> {
    let Some(first) = frames.first() else {
        return Vec::new();
    };
    let (w, h) = (first.width, first.height);
    if frames.len() < 3 || frames.iter().any(|f| f.width != w || f.height != h) || w < 32 || h < 32
    {
        return Vec::new();
    }
    let n = w * h;
    let mut hi = vec![f32::NEG_INFINITY; n];
    let mut lo = vec![f32::INFINITY; n];
    for f in frames {
        for (i, &v) in f.data.iter().enumerate() {
            hi[i] = hi[i].max(v);
            lo[i] = lo[i].min(v);
        }
    }
    let raw: Vec<bool> = (0..n)
        .map(|i| hi[i] >= 0.95 && hi[i] - lo[i] >= 0.6)
        .collect();
    if !raw.iter().any(|&d| d) {
        return Vec::new();
    }
    // Join the glyphs of a line: grow 8 px sideways and 2 px vertically.
    let dynamic = dilate(&raw, w, h, 8, 2);
    let max_h = (h / 20).max(24);
    let max_w = w / 2;
    // Burned-in text sits in the first or last few rows. A wider band let a
    // drifting limb, a sunspot or a bright filament pass for text (a 315 x 128
    // patch of a white-light disc was once flattened), so the strip is narrow.
    let strip = (h / 16).max(32);
    let mut out: Vec<Rect> = Vec::new();
    for comp in components(&dynamic, w, h) {
        let rect = comp.rect;
        let near_edge = rect.y1 <= strip || rect.y0 >= h - strip;
        if rect.height() > max_h || rect.width() > max_w || !near_edge {
            continue;
        }
        // Evidence of text: enough changing pixels (the dilation does not
        // count: a handful of hot or flickering pixels grows into a
        // text-sized blob), and pixels that flip often between white and
        // dark, as changing digits do, rather than once as a drifting edge.
        let (mut pixels, mut flips) = (0usize, 0usize);
        for y in rect.y0..rect.y1 {
            for x in rect.x0..rect.x1 {
                if raw[y * w + x] {
                    pixels += 1;
                    flips += flips_of(frames, y * w + x);
                }
            }
        }
        if pixels < MIN_TEXT_PIXELS || (flips as f32) < MIN_TEXT_FLIPS * pixels as f32 {
            continue;
        }
        out.push(rect);
    }
    // Static characters of the same line: near-white columns just beside it.
    let median_white = |x: usize, y0: usize, y1: usize| -> bool {
        (y0..y1).any(|y| {
            let mut vals: Vec<f32> = frames.iter().map(|f| f.data[y * w + x]).collect();
            vals.sort_by(f32::total_cmp);
            vals[vals.len() / 2] >= 0.9
        })
    };
    let mut grown = Vec::new();
    for r in out {
        let (mut x0, mut x1) = (r.x0, r.x1);
        let mut gap = 0;
        while x0 > 0 && gap <= 12 && r.x1 - x0 < max_w {
            x0 -= 1;
            gap = if median_white(x0, r.y0, r.y1) {
                0
            } else {
                gap + 1
            };
        }
        x0 += gap;
        let mut gap2 = 0;
        while x1 < w && gap2 <= 12 && x1 - r.x0 < max_w {
            gap2 = if median_white(x1, r.y0, r.y1) {
                0
            } else {
                gap2 + 1
            };
            x1 += 1;
        }
        x1 = x1.saturating_sub(gap2);
        grown.push(Rect {
            x0: x0.saturating_sub(4),
            y0: r.y0.saturating_sub(4),
            x1: (x1 + 4).min(w),
            y1: (r.y1 + 4).min(h),
        });
    }
    merge(grown)
}

/// Fewest changing pixels a region needs to pass for text. A line of changing
/// digits has hundreds (330 on the `SharpCap` captures); a limb or sensor
/// flicker that once passed for text had under a dozen.
const MIN_TEXT_PIXELS: usize = 40;

/// Fewest white/dark flips per changing pixel over the sampled frames (taken
/// in capture order). Digits flip about five times in 16 frames; a drifting
/// limb crosses a pixel about once.
const MIN_TEXT_FLIPS: f32 = 2.0;

/// Times pixel `i` flips between near-white (0.8 or more) and dark (0.4 or
/// less) from one sampled frame to the next; in-between values are skipped.
fn flips_of(frames: &[Frame], i: usize) -> usize {
    let mut prev = None;
    let mut flips = 0;
    for f in frames {
        let v = f.data[i];
        let state = if v >= 0.8 {
            true
        } else if v <= 0.4 {
            false
        } else {
            continue;
        };
        if prev.is_some_and(|p| p != state) {
            flips += 1;
        }
        prev = Some(state);
    }
    flips
}

/// Merge rectangles that overlap.
fn merge(mut rects: Vec<Rect>) -> Vec<Rect> {
    let mut i = 0;
    while i < rects.len() {
        let mut j = i + 1;
        let mut merged = false;
        while j < rects.len() {
            if overlaps(rects[i], rects[j]) {
                let b = rects.swap_remove(j);
                let a = &mut rects[i];
                a.x0 = a.x0.min(b.x0);
                a.y0 = a.y0.min(b.y0);
                a.x1 = a.x1.max(b.x1);
                a.y1 = a.y1.max(b.y1);
                merged = true;
            } else {
                j += 1;
            }
        }
        if !merged {
            i += 1;
        }
    }
    rects
}

fn dilate(mask: &[bool], w: usize, h: usize, rx: usize, ry: usize) -> Vec<bool> {
    let mut horiz = vec![false; mask.len()];
    for y in 0..h {
        let row = &mask[y * w..(y + 1) * w];
        let mut last: Option<usize> = None;
        for x in 0..w {
            if row[x] {
                last = Some(x);
            }
            horiz[y * w + x] = last.is_some_and(|l| x - l <= rx);
        }
        let mut next: Option<usize> = None;
        for x in (0..w).rev() {
            if row[x] {
                next = Some(x);
            }
            if next.is_some_and(|l| l - x <= rx) {
                horiz[y * w + x] = true;
            }
        }
    }
    let mut out = vec![false; mask.len()];
    for x in 0..w {
        let mut last: Option<usize> = None;
        for y in 0..h {
            if horiz[y * w + x] {
                last = Some(y);
            }
            out[y * w + x] = last.is_some_and(|l| y - l <= ry);
        }
        let mut next: Option<usize> = None;
        for y in (0..h).rev() {
            if horiz[y * w + x] {
                next = Some(y);
            }
            if next.is_some_and(|l| l - y <= ry) {
                out[y * w + x] = true;
            }
        }
    }
    out
}

struct Component {
    rect: Rect,
}

/// 4-connected components of `mask`.
fn components(mask: &[bool], w: usize, h: usize) -> Vec<Component> {
    let mut seen = vec![false; mask.len()];
    let mut out = Vec::new();
    let mut stack = Vec::new();
    for start in 0..mask.len() {
        if !mask[start] || seen[start] {
            continue;
        }
        let mut rect = Rect {
            x0: start % w,
            y0: start / w,
            x1: start % w + 1,
            y1: start / w + 1,
        };
        seen[start] = true;
        stack.push(start);
        while let Some(p) = stack.pop() {
            let (x, y) = (p % w, p / w);
            rect.x0 = rect.x0.min(x);
            rect.y0 = rect.y0.min(y);
            rect.x1 = rect.x1.max(x + 1);
            rect.y1 = rect.y1.max(y + 1);
            let mut push = |q: usize| {
                if mask[q] && !seen[q] {
                    seen[q] = true;
                    stack.push(q);
                }
            };
            if x > 0 {
                push(p - 1);
            }
            if x + 1 < w {
                push(p + 1);
            }
            if y > 0 {
                push(p - w);
            }
            if y + 1 < h {
                push(p + w);
            }
        }
        out.push(Component { rect });
    }
    out
}

/// A circle: the fitted solar disc.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Disc {
    /// Centre, x.
    pub cx: f32,
    /// Centre, y.
    pub cy: f32,
    /// Radius in pixels.
    pub r: f32,
}

impl Disc {
    /// True when pixel `(x, y)` is inside the limb plus `band` pixels.
    #[must_use]
    pub fn contains(&self, x: usize, y: usize, band: f32) -> bool {
        (x as f32 - self.cx).hypot(y as f32 - self.cy) <= self.r + band
    }
}

/// Fit the limb of `image`: the circle through the edge between sky and disc.
///
/// Edge points come from scanning every second row and column for the first
/// and last crossing of the half-way level between sky and disc; crossings on
/// the frame border are not limb. The circle is a least-squares fit refitted
/// without the points that sit far from it (prominences, flares, noise).
/// `None` when there is no clear disc: too little contrast, too few edge
/// points or a fit that most points disagree with.
#[must_use]
pub fn fit_disc(image: &Frame) -> Option<Disc> {
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
    let (lo_level, mid_level) = (sky + 0.1 * (disc - sky), f32::midpoint(sky, disc));
    let mut pts: Vec<(f64, f64)> = Vec::new();
    for y in (0..h).step_by(2) {
        let (a, b) = edges(&blurred.data[y * w..(y + 1) * w], lo_level, mid_level);
        pts.extend(a.into_iter().chain(b).map(|x| (x, y as f64)));
    }
    let mut col = vec![0.0f32; h];
    for x in (0..w).step_by(2) {
        for (y, c) in col.iter_mut().enumerate() {
            *c = blurred.data[y * w + x];
        }
        let (a, b) = edges(&col, lo_level, mid_level);
        pts.extend(a.into_iter().chain(b).map(|y| (x as f64, y)));
    }
    if pts.len() < 24 {
        return None;
    }
    let mut inliers = pts.clone();
    let mut circle = kasa(&inliers)?;
    for _ in 0..8 {
        let mut res: Vec<f64> = pts
            .iter()
            .map(|&(x, y)| ((x - circle.0).hypot(y - circle.1) - circle.2).abs())
            .collect();
        let mut sorted = res.clone();
        sorted.sort_by(f64::total_cmp);
        let mad = sorted[sorted.len() / 2];
        let tol = (3.0 * 1.4826 * mad).max(1.5);
        inliers = pts
            .iter()
            .zip(&res)
            .filter(|(_, r)| **r <= tol)
            .map(|(p, _)| *p)
            .collect();
        res.clear();
        if inliers.len() < 24 {
            return None;
        }
        circle = kasa(&inliers)?;
    }
    let min_side = w.min(h) as f64;
    // Most edge points must agree with the circle, and it must be a plausible
    // disc rather than a fit to noise.
    if (inliers.len() as f64) < 0.6 * pts.len() as f64
        || circle.2 < 0.15 * min_side
        || circle.2 > 2.0 * w.max(h) as f64
    {
        return None;
    }
    Some(Disc {
        cx: circle.0 as f32,
        cy: circle.1 as f32,
        r: circle.2 as f32,
    })
}

/// Both limb edges along one scan line (start side, end side); each is
/// `None` when the disc runs off the frame on that side.
fn edges(line: &[f32], lo: f32, mid: f32) -> (Option<f64>, Option<f64>) {
    let rev: Vec<f32> = line.iter().rev().copied().collect();
    (
        limb_edge(line, lo, mid),
        limb_edge(&rev, lo, mid).map(|p| (line.len() - 1) as f64 - p),
    )
}

/// Position of the steepest rise within the first stretch of `vals` that
/// climbs from `lo` past `mid`, to sub-pixel accuracy. `None` when the line
/// starts above `lo` (the disc continues past the frame edge) or never rises.
///
/// The steepest point, not a level crossing, is the limb: limb darkening puts
/// the half-way level a few pixels inside it.
fn limb_edge(vals: &[f32], lo: f32, mid: f32) -> Option<f64> {
    let n = vals.len();
    if n < 8 || vals[0] > lo {
        return None;
    }
    let c_lo = vals.iter().position(|&v| v > lo)?;
    let c_mid = vals.iter().position(|&v| v > mid)?;
    let from = c_lo.saturating_sub(4).max(1);
    let to = (c_mid + 4).min(n - 2);
    if from >= to || c_mid + 1 >= n {
        return None;
    }
    let g = |i: usize| f64::from(vals[i + 1] - vals[i - 1]) / 2.0;
    let best = (from..=to).max_by(|&a, &b| g(a).total_cmp(&g(b)))?;
    if g(best) <= 0.0 {
        return None;
    }
    // Parabola through the gradient samples around the peak.
    let (gm, g0, gp) = (
        if best > 1 { g(best - 1) } else { g(best) },
        g(best),
        if best + 2 < n { g(best + 1) } else { g(best) },
    );
    let denom = gm - 2.0 * g0 + gp;
    let off = if denom.abs() > 1e-12 {
        (0.5 * (gm - gp) / denom).clamp(-0.5, 0.5)
    } else {
        0.0
    };
    Some(best as f64 + off)
}

/// Algebraic least-squares circle (Kasa), on centred coordinates.
fn kasa(pts: &[(f64, f64)]) -> Option<(f64, f64, f64)> {
    let n = pts.len() as f64;
    let (mx, my) = pts.iter().fold((0.0, 0.0), |a, p| (a.0 + p.0, a.1 + p.1));
    let (mx, my) = (mx / n, my / n);
    // Normal equations of the algebraic fit, on coordinates centred on the mean:
    // [m0 m1; m1 m2] [uc vc] = [(m3 + m5) / 2, (m4 + m6) / 2].
    let mut m = [0.0f64; 7];
    for &(x, y) in pts {
        let (u, v) = (x - mx, y - my);
        m[0] += u * u;
        m[1] += u * v;
        m[2] += v * v;
        m[3] += u * u * u;
        m[4] += v * v * v;
        m[5] += u * v * v;
        m[6] += v * u * u;
    }
    let det = m[0] * m[2] - m[1] * m[1];
    if det.abs() < 1e-9 {
        return None;
    }
    let (b1, b2) = (f64::midpoint(m[3], m[5]), f64::midpoint(m[4], m[6]));
    let uc = (b1 * m[2] - b2 * m[1]) / det;
    let vc = (m[0] * b2 - m[1] * b1) / det;
    let r = (uc * uc + vc * vc + (m[0] + m[2]) / n).sqrt();
    Some((uc + mx, vc + my, r))
}

/// Width of the ring around a masked rectangle that the fill is fitted to.
const FILL_RING: usize = 12;

/// What a masked rectangle is filled with: the plane that best fits the
/// pixels in a ring just outside it (the sky, normally), so a removed
/// timestamp leaves the local background, gradient included, rather than a
/// flat bar against the scattered-light halo.
///
/// The plane is fitted once, to the mean of the sample frames, and is the
/// same in every frame, so the alignment still sees a static region.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fill {
    cx: f32,
    cy: f32,
    a: f32,
    b: f32,
    c: f32,
    lo: f32,
    hi: f32,
}

impl Fill {
    /// One flat level.
    #[must_use]
    pub fn flat(level: f32) -> Self {
        Self {
            cx: 0.0,
            cy: 0.0,
            a: level,
            b: 0.0,
            c: 0.0,
            lo: level,
            hi: level,
        }
    }

    /// Value at pixel `(x, y)`, held within the range of the pixels it was
    /// fitted to so a steep ring cannot extrapolate past it.
    #[must_use]
    pub fn at(&self, x: usize, y: usize) -> f32 {
        let v = self.a + self.b * (x as f32 - self.cx) + self.c * (y as f32 - self.cy);
        v.clamp(self.lo, self.hi)
    }
}

/// One [`Fill`] per rectangle, from the ring of pixels around it in the
/// sample `frames` (averaged). Pixels of other masked rectangles do not
/// count. A plane is fitted by least squares, refitted twice without the
/// pixels more than three robust sigma from it (a bright limb, a stray
/// star); a ring with too few pixels gives a flat fill at its median.
#[must_use]
pub fn fill_planes(frames: &[Frame], rects: &[Rect]) -> Vec<Fill> {
    let Some(first) = frames.first() else {
        return rects.iter().map(|_| Fill::flat(0.0)).collect();
    };
    let (w, h) = (first.width, first.height);
    let inv = 1.0 / frames.len() as f32;
    let mean = |i: usize| frames.iter().map(|f| f.data[i]).sum::<f32>() * inv;
    rects
        .iter()
        .map(|r| {
            let ring = Rect {
                x0: r.x0.saturating_sub(FILL_RING),
                y0: r.y0.saturating_sub(FILL_RING),
                x1: (r.x1 + FILL_RING).min(w),
                y1: (r.y1 + FILL_RING).min(h),
            };
            let (cx, cy) = ((r.x0 + r.x1) as f32 / 2.0, (r.y0 + r.y1) as f32 / 2.0);
            let mut pts: Vec<(f32, f32, f32)> = Vec::new();
            for y in ring.y0..ring.y1 {
                for x in ring.x0..ring.x1 {
                    let inside = rects
                        .iter()
                        .any(|q| x >= q.x0 && x < q.x1 && y >= q.y0 && y < q.y1);
                    if !inside {
                        pts.push((x as f32 - cx, y as f32 - cy, mean(y * w + x)));
                    }
                }
            }
            plane_fit(cx, cy, &pts)
        })
        .collect()
}

/// Robust least-squares plane `v = a + b x + c y` through `pts` (`x`, `y`
/// relative to `(cx, cy)`).
fn plane_fit(cx: f32, cy: f32, pts: &[(f32, f32, f32)]) -> Fill {
    if pts.is_empty() {
        return Fill::flat(0.0);
    }
    let mut vals: Vec<f32> = pts.iter().map(|p| p.2).collect();
    vals.sort_by(f32::total_cmp);
    let median = vals[vals.len() / 2];
    let (lo, hi) = (
        vals[vals.len() / 20],
        vals[vals.len() - 1 - vals.len() / 20],
    );
    let mut best = Fill {
        cx,
        cy,
        a: median,
        b: 0.0,
        c: 0.0,
        lo,
        hi,
    };
    if pts.len() < 24 {
        return best;
    }
    let mut keep: Vec<bool> = vec![true; pts.len()];
    for round in 0..3 {
        // Normal equations over the kept points.
        let (mut s, mut sx, mut sy, mut sxx, mut sxy, mut syy) = (0.0f64, 0.0, 0.0, 0.0, 0.0, 0.0);
        let (mut sv, mut sxv, mut syv) = (0.0f64, 0.0, 0.0);
        for (&(x, y, v), _) in pts.iter().zip(&keep).filter(|(_, k)| **k) {
            let (x, y, v) = (f64::from(x), f64::from(y), f64::from(v));
            s += 1.0;
            sx += x;
            sy += y;
            sxx += x * x;
            sxy += x * y;
            syy += y * y;
            sv += v;
            sxv += x * v;
            syv += y * v;
        }
        let m = [[s, sx, sy], [sx, sxx, sxy], [sy, sxy, syy]];
        let Some([a, b, c]) = solve3(m, [sv, sxv, syv]) else {
            return best;
        };
        best = Fill {
            cx,
            cy,
            a: a as f32,
            b: b as f32,
            c: c as f32,
            lo,
            hi,
        };
        if round == 2 {
            break;
        }
        let res: Vec<f32> = pts
            .iter()
            .map(|&(x, y, v)| (v - (best.a + best.b * x + best.c * y)).abs())
            .collect();
        let mut sorted = res.clone();
        sorted.sort_by(f32::total_cmp);
        let tol = (3.0 * 1.4826 * sorted[sorted.len() / 2]).max(0.01);
        for (k, r) in keep.iter_mut().zip(&res) {
            *k = *r <= tol;
        }
        if keep.iter().filter(|&&k| k).count() < 24 {
            break;
        }
    }
    best
}

/// Solve the 3 x 3 system `m x = v` by Cramer's rule; `None` when singular.
fn solve3(m: [[f64; 3]; 3], v: [f64; 3]) -> Option<[f64; 3]> {
    let det = |a: [[f64; 3]; 3]| {
        a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
            - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
            + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0])
    };
    let d = det(m);
    if d.abs() < 1e-9 {
        return None;
    }
    let mut out = [0.0; 3];
    for (k, o) in out.iter_mut().enumerate() {
        let mut t = m;
        for (row, tv) in t.iter_mut().zip(v) {
            row[k] = tv;
        }
        *o = det(t) / d;
    }
    Some(out)
}

/// Write each rectangle's fill (`0..=1`) into every plane of the masked
/// pixels of the stored frame `raw`.
fn fill_raw(layout: &RowLayout, raw: &mut [u8], rects: &[Rect], fills: &[Fill]) {
    let planes = match layout.planes {
        Planes::One => 1,
        _ => 3,
    };
    let sample = layout.sample;
    let bytes = |level: f32| -> Vec<u8> {
        match sample {
            Sample::U8 => vec![(level * 255.0).round().clamp(0.0, 255.0) as u8],
            Sample::U16Le => ((level * 65535.0).round().clamp(0.0, 65535.0) as u16)
                .to_le_bytes()
                .to_vec(),
            Sample::U16Be => ((level * 65535.0).round().clamp(0.0, 65535.0) as u16)
                .to_be_bytes()
                .to_vec(),
            Sample::F32 => level.to_le_bytes().to_vec(),
        }
    };
    let step = sample.bytes();
    for (r, fill) in rects.iter().zip(fills) {
        for y in r.y0..r.y1.min(layout.height) {
            let sy = if layout.bottom_up {
                layout.height - 1 - y
            } else {
                y
            };
            let row = sy * layout.stride;
            for x in r.x0..r.x1.min(layout.width) {
                let b = bytes(fill.at(x, y));
                for p in 0..planes {
                    let o = row + (x * planes + p) * step;
                    if let Some(dst) = raw.get_mut(o..o + step) {
                        dst.copy_from_slice(&b);
                    }
                }
            }
        }
    }
}

/// A capture with rectangles overwritten by one flat level, in everything it
/// serves: [`FrameSource::read`], and the raw bytes that [`SharedFrames`]
/// hands to the CPU and GPU decoders.
pub struct MaskedSource<'a> {
    inner: &'a mut dyn FrameSource,
    rects: Vec<Rect>,
    fills: Vec<Fill>,
    cache: crate::io::raw::DeviceCache,
}

impl<'a> MaskedSource<'a> {
    /// Wrap `inner`, flattening `rects` to `level`.
    pub fn new(inner: &'a mut dyn FrameSource, rects: Vec<Rect>, level: f32) -> Self {
        let fills = rects.iter().map(|_| Fill::flat(level)).collect();
        Self::with_fills(inner, rects, fills)
    }

    /// Wrap `inner`, filling each of `rects` with the matching entry of
    /// `fills` (see [`fill_planes`]).
    pub fn with_fills(inner: &'a mut dyn FrameSource, rects: Vec<Rect>, fills: Vec<Fill>) -> Self {
        debug_assert_eq!(rects.len(), fills.len());
        Self {
            inner,
            rects,
            fills,
            cache: crate::io::raw::DeviceCache::default(),
        }
    }

    fn shared_inner(&self) -> Result<&dyn SharedFrames> {
        self.inner
            .shared()
            .ok_or_else(|| crate::Error::Unsupported("source cannot be read concurrently".into()))
    }
}

impl FrameSource for MaskedSource<'_> {
    fn dimensions(&self) -> (usize, usize) {
        self.inner.dimensions()
    }
    fn frame_count(&self) -> usize {
        self.inner.frame_count()
    }
    fn read(&mut self, index: usize) -> Result<Frame> {
        let mut f = self.inner.read(index)?;
        for (r, fill) in self.rects.iter().zip(&self.fills) {
            for y in r.y0..r.y1.min(f.height) {
                for x in r.x0..r.x1.min(f.width) {
                    f.data[y * f.width + x] = fill.at(x, y);
                }
            }
        }
        Ok(f)
    }
    fn describe(&self) -> String {
        format!(
            "{} (masked: {} region{})",
            self.inner.describe(),
            self.rects.len(),
            if self.rects.len() == 1 { "" } else { "s" }
        )
    }
    fn shared(&self) -> Option<&dyn SharedFrames> {
        self.inner.shared().map(|_| self as &dyn SharedFrames)
    }
}

impl SharedFrames for MaskedSource<'_> {
    fn frame_count(&self) -> usize {
        self.inner.frame_count()
    }
    fn layout(&self) -> RowLayout {
        self.shared_inner().map_or(
            RowLayout {
                width: 0,
                height: 0,
                sample: Sample::U8,
                planes: Planes::One,
                stride: 0,
                bottom_up: false,
            },
            SharedFrames::layout,
        )
    }
    fn bayer(&self) -> Option<BayerPattern> {
        self.inner.shared().and_then(SharedFrames::bayer)
    }
    fn device_cache(&self) -> Option<&crate::io::raw::DeviceCache> {
        Some(&self.cache)
    }
    fn read_raw(&self, index: usize, buf: &mut Vec<u8>) -> Result<()> {
        let inner = self.shared_inner()?;
        inner.read_raw(index, buf)?;
        fill_raw(&inner.layout(), buf, &self.rects, &self.fills);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use super::*;

    #[test]
    fn region_parses_and_rejects() {
        assert_eq!(parse_region("1,2,30,40").unwrap(), Rect::new(1, 2, 30, 40));
        assert_eq!(parse_region(" 0, 0, 5, 5 ").unwrap(), Rect::new(0, 0, 5, 5));
        for bad in ["", "1,2,3", "1,2,3,4,5", "a,b,c,d", "1,2,0,4", "-1,0,4,4"] {
            assert!(parse_region(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn overlap_is_strict() {
        let a = Rect::new(0, 0, 10, 10);
        assert!(overlaps(a, Rect::new(9, 9, 5, 5)));
        assert!(!overlaps(a, Rect::new(10, 0, 5, 5)));
    }

    use crate::align::Shift;
    use crate::testutil::{
        CaptureConfig, SyntheticCapture, SyntheticConfig, SyntheticSun, burn_timestamp,
    };

    fn sun(w: usize, h: usize, radius_frac: f32) -> SyntheticSun {
        SyntheticSun::new(&SyntheticConfig {
            width: w,
            height: h,
            radius_frac,
            ..SyntheticConfig::default()
        })
    }

    #[test]
    fn limb_fit_finds_the_disc_to_subpixel_accuracy() {
        let s = sun(320, 240, 0.35);
        let d = fit_disc(&s.render_clean(Shift::new(3.3, -2.1), None)).unwrap();
        let (cx, cy) = (s.centre.0 + 3.3, s.centre.1 - 2.1);
        assert!((d.cx - cx).abs() < 0.7 && (d.cy - cy).abs() < 0.7, "{d:?}");
        assert!((d.r - s.radius).abs() < 1.0, "{d:?} vs {}", s.radius);
    }

    #[test]
    fn limb_fit_survives_a_prominence_and_a_disc_cut_by_the_frame() {
        let s = sun(320, 240, 0.35);
        let mut f = s.render_clean(Shift::default(), None);
        // A bright spike off the limb, wider than the 2 px tolerance.
        let (cx, cy, r) = (s.centre.0 as usize, s.centre.1 as usize, s.radius as usize);
        for y in cy - 8..cy + 8 {
            for x in cx + r..cx + r + 18 {
                f.data[y * f.width + x] = 0.7;
            }
        }
        let d = fit_disc(&f).unwrap();
        assert!((d.r - s.radius).abs() < 1.5, "{d:?}");
        // Same sun in a frame that cuts its left side off.
        let big = sun(320, 240, 0.6);
        let shifted = big.render_clean(Shift::new(60.0, 0.0), None);
        let d = fit_disc(&shifted).unwrap();
        assert!((d.r - big.radius).abs() < 2.0, "{d:?} vs {}", big.radius);
        assert!((d.cx - (big.centre.0 + 60.0)).abs() < 2.0, "{d:?}");
    }

    #[test]
    fn no_disc_means_no_fit() {
        let flat = Frame::new(200, 150, vec![0.2; 200 * 150]).unwrap();
        assert!(fit_disc(&flat).is_none());
    }

    #[test]
    fn overlay_detection_finds_a_changing_timestamp_only() {
        let s = sun(320, 240, 0.35);
        let cap = SyntheticCapture::generate(&s, &CaptureConfig::default());
        // Clean captures, even with the sun drifting, have no overlay.
        let clean: Vec<Frame> = cap.frames.iter().step_by(2).cloned().collect();
        assert_eq!(detect_overlay(&clean), Vec::<Rect>::new());
        let mut stamped = clean;
        let mut text = Rect::new(0, 0, 0, 0);
        for (i, f) in stamped.iter_mut().enumerate() {
            text = burn_timestamp(f, i);
        }
        let found = detect_overlay(&stamped);
        assert_eq!(found.len(), 1, "{found:?}");
        let r = found[0];
        assert!(r.x0 <= text.x0 && r.y0 <= text.y0 && r.x1 >= text.x1 && r.y1 >= text.y1);
        assert!(r.area() < 3 * text.area(), "{r:?} vs {text:?}");
    }

    #[test]
    fn a_changing_patch_away_from_the_frame_edge_is_not_text() {
        // A bright patch that flips between frames, 50 rows down in a 240-row
        // frame: inside the old quarter-height band, outside the edge strip.
        let (w, h) = (320, 240);
        let frames: Vec<Frame> = (0..4)
            .map(|i| {
                let mut f = Frame::zeros(w, h);
                for y in 50..64 {
                    for x in 100..180 {
                        f.data[y * w + x] = if i % 2 == 0 { 1.0 } else { 0.0 };
                    }
                }
                f
            })
            .collect();
        assert_eq!(detect_overlay(&frames), Vec::<Rect>::new());
        // The same patch in the top strip is text.
        let top: Vec<Frame> = (0..4)
            .map(|i| {
                let mut f = Frame::zeros(w, h);
                for y in 4..18 {
                    for x in 100..180 {
                        f.data[y * w + x] = if i % 2 == 0 { 1.0 } else { 0.0 };
                    }
                }
                f
            })
            .collect();
        assert_eq!(detect_overlay(&top).len(), 1);
    }

    #[test]
    fn a_few_flickering_pixels_or_a_drifting_edge_are_not_text() {
        let (w, h) = (320, 240);
        // Three pixels flipping in the bottom strip (a hot pixel pair and a
        // star), which the dilation grows to text size.
        let flicker: Vec<Frame> = (0..8)
            .map(|i| {
                let mut f = Frame::zeros(w, h);
                for x in [100, 103, 106] {
                    f.data[(h - 10) * w + x] = if i % 2 == 0 { 1.0 } else { 0.0 };
                }
                f
            })
            .collect();
        assert_eq!(detect_overlay(&flicker), Vec::<Rect>::new());
        // A bright block whose top edge climbs 30 rows over the capture: many
        // pixels change, but each crosses once, as a limb does.
        let drift: Vec<Frame> = (0..8)
            .map(|i| {
                let mut f = Frame::zeros(w, h);
                for y in (h - 4 * i - 2)..h {
                    for x in 100..160 {
                        f.data[y * w + x] = 1.0;
                    }
                }
                f
            })
            .collect();
        assert_eq!(detect_overlay(&drift), Vec::<Rect>::new());
    }

    #[test]
    fn the_fill_follows_the_gradient_round_the_hole() {
        // A sky that brightens to the right, a hole in it, a bright limb in
        // one corner of the ring that the robust fit must ignore.
        let (w, h) = (200, 100);
        let sky = |x: usize| 0.1 + 0.002 * x as f32;
        let mut data = vec![0.0; w * h];
        for y in 0..h {
            for x in 0..w {
                data[y * w + x] = sky(x);
            }
        }
        for y in 40..60 {
            for x in 170..200 {
                data[y * w + x] = 0.9;
            }
        }
        let frames = vec![Frame::new(w, h, data).unwrap(); 3];
        let hole = Rect::new(60, 40, 80, 20);
        let fills = fill_planes(&frames, &[hole]);
        for x in (60..140).step_by(10) {
            let got = fills[0].at(x, 50);
            assert!((got - sky(x)).abs() < 0.01, "x {x}: {got} vs {}", sky(x));
        }
        // Flat fill still serves a flat level.
        assert!((Fill::flat(0.3).at(5, 5) - 0.3).abs() < 1e-9);
    }

    #[test]
    fn masked_source_flattens_reads_and_raw_bytes_alike() {
        use crate::io::source::MemorySource;
        let frames: Vec<Frame> = (0..3)
            .map(|i| Frame::new(16, 12, vec![0.1 * (i + 1) as f32; 16 * 12]).unwrap())
            .collect();
        let mut inner = MemorySource(frames);
        let hole = Rect::new(2, 3, 5, 4);
        let mut m = MaskedSource::new(&mut inner, vec![hole], 0.9);
        let read = m.read(1).unwrap();
        let shared = m.shared().expect("shared");
        let mut raw = Vec::new();
        shared.read_raw(1, &mut raw).unwrap();
        let decoded = shared.layout().decode(&raw);
        for y in 0..12 {
            for x in 0..16 {
                let inside = (2..7).contains(&x) && (3..7).contains(&y);
                let want = if inside { 0.9 } else { 0.2 };
                assert!((read.data[y * 16 + x] - want).abs() < 1e-6);
                assert!((decoded[y * 16 + x] - want).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn kasa_recovers_a_circle() {
        let pts: Vec<(f64, f64)> = (0..90)
            .map(|i| {
                let t = f64::from(i) * 0.07;
                (120.5 + 80.25 * t.cos(), 60.0 + 80.25 * t.sin())
            })
            .collect();
        let (x, y, r) = kasa(&pts).unwrap();
        assert!((x - 120.5).abs() < 1e-6 && (y - 60.0).abs() < 1e-6 && (r - 80.25).abs() < 1e-6);
    }
}
