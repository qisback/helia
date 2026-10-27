// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Putting the panels on one canvas: gain and offset, seams, feathering.
//!
//! Every panel is resampled onto the canvas with its placement
//! (`W(p) = R(theta) (p - c) + c + t`). Where two panels overlap they are first
//! matched in brightness, then one is chosen to show on each side of a seam, and
//! the seam is blurred a little so that what is left of a difference does not
//! show as an edge.
//!
//! **Gain and offset.** Panel `i` is scaled and shifted, `a_i x + b_i`, so that
//! on the overlaps the panels agree. All the overlap pixels of all the pairs go
//! into one least squares fit for every `a_i` and `b_i` at once (panel 0 is held
//! at `a = 1, b = 0`), so three panels around a corner agree with each other and
//! not only in pairs.
//!
//! **Seams.** The panels are added in order. Where the next one meets what is
//! there, the seam is the path across the overlap, one pixel to a row, that
//! passes through the smallest differences between the two (dynamic
//! programming, as in seam carving): it goes round a feature that does not
//! match and follows a smooth patch of the disc. Everything on the new panel's
//! side belongs to it.
//!
//! **Feathering.** The panels' ownership masks are blurred with a Gaussian
//! (`feather` pixels), so the blend across a seam is gradual and the weights of
//! all the panels at a pixel sum to one. A panel's own border fades out too, so
//! that an edge with no seam on it (the outside of the mosaic) does not leave a
//! step.
//!
//! # References
//!
//! - Avidan, S. and Shamir, A. (2007), Seam carving for content-aware image resizing, ACM Trans. Graph.
//!   26(3), 10, doi:10.1145/1276377.1276390 (the least-difference seam by dynamic programming).

#![allow(
    clippy::many_single_char_names,
    clippy::similar_names,
    reason = "x, y, w, h, a, b and the per-axis names are the natural names in image maths"
)]

use rayon::prelude::*;

use super::pair::Rigid;
use crate::filter::gaussian_blur;
use crate::frame::Frame;
use crate::picture::{self, Picture};
use crate::resample::{Kernel, sample};

/// Pixels along a panel's own edge that are given little weight (resampling
/// reads past the edge there).
const EDGE_MARGIN: f64 = 3.0;

/// A panel on the canvas.
#[derive(Clone, Debug)]
pub struct Placed {
    /// The panel's planes resampled onto the canvas.
    pub planes: Vec<Frame>,
    /// How far each canvas pixel is inside the panel's border, pixels (0 or
    /// less outside).
    pub depth: Vec<f32>,
}

/// The canvas: the bounding box of the placed panels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Canvas {
    /// Width.
    pub width: usize,
    /// Height.
    pub height: usize,
    /// Mosaic coordinates of the canvas's top-left pixel.
    pub origin: (f64, f64),
}

/// The canvas that holds every panel.
#[must_use]
pub fn canvas_for(sizes: &[(usize, usize)], theta: &[f64], shift: &[(f64, f64)]) -> Canvas {
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for ((&(w, h), &th), &(tx, ty)) in sizes.iter().zip(theta).zip(shift) {
        let r = Rigid { tx, ty, theta: th };
        let c = ((w as f64 - 1.0) / 2.0, (h as f64 - 1.0) / 2.0);
        for (x, y) in [
            (0.0, 0.0),
            (w as f64 - 1.0, 0.0),
            (0.0, h as f64 - 1.0),
            (w as f64 - 1.0, h as f64 - 1.0),
        ] {
            let (qx, qy) = r.apply(x, y, c);
            x0 = x0.min(qx);
            y0 = y0.min(qy);
            x1 = x1.max(qx);
            y1 = y1.max(qy);
        }
    }
    let (ox, oy) = (x0.floor(), y0.floor());
    Canvas {
        width: ((x1 - ox).ceil() as usize + 1).max(1),
        height: ((y1 - oy).ceil() as usize + 1).max(1),
        origin: (ox, oy),
    }
}

/// Resample a panel onto the canvas.
#[must_use]
pub fn place(
    panel: &Picture,
    theta: f64,
    shift: (f64, f64),
    canvas: &Canvas,
    kernel: Kernel,
) -> Placed {
    let (pw, ph) = panel.size();
    let (cx, cy) = ((pw as f64 - 1.0) / 2.0, (ph as f64 - 1.0) / 2.0);
    let (s, c) = theta.sin_cos();
    let (w, h) = (canvas.width, canvas.height);
    // Canvas pixel -> panel pixel: p = R(-theta)(W - c - t) + c.
    let to_panel = |x: usize, y: usize| {
        let (wx, wy) = (x as f64 + canvas.origin.0, y as f64 + canvas.origin.1);
        let (u, v) = (wx - cx - shift.0, wy - cy - shift.1);
        (c * u + s * v + cx, -s * u + c * v + cy)
    };
    let planes_in = picture::planes(panel);
    let depth: Vec<f32> = (0..w * h)
        .into_par_iter()
        .map(|i| {
            let (px, py) = to_panel(i % w, i / w);
            let d = px
                .min(pw as f64 - 1.0 - px)
                .min(py)
                .min(ph as f64 - 1.0 - py);
            d as f32
        })
        .collect();
    let planes = planes_in
        .iter()
        .map(|f| {
            let mut data = vec![0.0f32; w * h];
            data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
                for (x, v) in row.iter_mut().enumerate() {
                    if depth[y * w + x] >= 0.0 {
                        let (px, py) = to_panel(x, y);
                        *v = sample(f, px as f32, py as f32, kernel);
                    }
                }
            });
            Frame {
                width: w,
                height: h,
                data,
                bayer: None,
            }
        })
        .collect();
    Placed { planes, depth }
}

/// Whether a placed panel has usable data at a canvas pixel.
fn inside(p: &Placed, i: usize) -> bool {
    f64::from(p.depth[i]) >= EDGE_MARGIN
}

/// Mean of a placed panel's planes at a pixel.
fn value(p: &Placed, i: usize) -> f32 {
    let n = p.planes.len() as f32;
    p.planes.iter().map(|f| f.data[i]).sum::<f32>() / n
}

/// The gain and offset of every panel: `a_i x + b_i` makes them agree on the
/// overlaps. Returns `(a, b)` for each, and the number of overlap samples used.
#[must_use]
pub fn match_levels(placed: &[Placed]) -> (Vec<(f32, f32)>, usize) {
    let n = placed.len();
    let mut ata = vec![vec![0.0f64; 2 * n]; 2 * n];
    let mut samples = 0usize;
    if n < 2 {
        return (vec![(1.0, 0.0); n], 0);
    }
    let len = placed[0].depth.len();
    for i in 0..n {
        for j in i + 1..n {
            for k in (0..len).step_by(5) {
                if inside(&placed[i], k) && inside(&placed[j], k) {
                    let (xi, xj) = (
                        f64::from(value(&placed[i], k)),
                        f64::from(value(&placed[j], k)),
                    );
                    // The row: a_i xi + b_i - a_j xj - b_j.
                    let row = [
                        (2 * i, xi),
                        (2 * i + 1, 1.0),
                        (2 * j, -xj),
                        (2 * j + 1, -1.0),
                    ];
                    for &(p, vp) in &row {
                        for &(q, vq) in &row {
                            ata[p][q] += vp * vq;
                        }
                    }
                    samples += 1;
                }
            }
        }
    }
    if samples < 200 {
        return (vec![(1.0, 0.0); n], samples);
    }
    // Hold panel 0 (a = 1, b = 0) and lean a little on gains near one and
    // offsets near zero, so a panel with a poor overlap is not thrown about.
    let (mut mean, mut cnt) = (0.0f64, 0usize);
    for k in (0..len).step_by(97) {
        if inside(&placed[0], k) {
            mean += f64::from(value(&placed[0], k));
            cnt += 1;
        }
    }
    let mean = (mean / cnt.max(1) as f64).max(1e-6);
    let lam_gain = 1e-5 * samples as f64 * mean * mean;
    let lam_offset = 1e-5 * samples as f64;
    let mut m = vec![vec![0.0f64; 2 * n - 2]; 2 * n - 2];
    let mut rhs = vec![0.0f64; 2 * n - 2];
    for p in 2..2 * n {
        for q in 2..2 * n {
            m[p - 2][q - 2] = ata[p][q];
        }
        rhs[p - 2] = -ata[p][0];
        if p % 2 == 0 {
            m[p - 2][p - 2] += lam_gain;
            rhs[p - 2] += lam_gain;
        } else {
            m[p - 2][p - 2] += lam_offset;
        }
    }
    let Some(x) = super::solve::solve_dense(m, rhs) else {
        return (vec![(1.0, 0.0); n], samples);
    };
    let mut out = vec![(1.0f32, 0.0f32)];
    for i in 1..n {
        out.push((x[2 * i - 2] as f32, x[2 * i - 1] as f32));
    }
    (out, samples)
}

/// Apply a gain and offset to every plane of a placed panel.
pub fn apply_levels(p: &mut Placed, gain: f32, offset: f32) {
    for f in &mut p.planes {
        for v in &mut f.data {
            *v = *v * gain + offset;
        }
    }
}

/// The cheapest path across an overlap: for each `u` (a row for a vertical
/// seam, a column for a horizontal one) the `v` where the seam crosses it.
/// `cost(u, v)` is the cost of the pixel; `span(u)` gives the first and last
/// `v` of the overlap on that line, `None` when it has none.
fn seam_path(
    n_u: usize,
    span: &dyn Fn(usize) -> Option<(usize, usize)>,
    cost: &dyn Fn(usize, usize) -> f32,
) -> Vec<Option<usize>> {
    let mut best: Vec<Vec<f32>> = Vec::with_capacity(n_u);
    let mut from: Vec<Vec<usize>> = Vec::with_capacity(n_u);
    let mut spans: Vec<Option<(usize, usize)>> = Vec::with_capacity(n_u);
    let mut last: Option<usize> = None;
    for u in 0..n_u {
        let s = span(u);
        spans.push(s);
        let Some((lo, hi)) = s else {
            best.push(Vec::new());
            from.push(Vec::new());
            continue;
        };
        let mut row = vec![f32::INFINITY; hi - lo + 1];
        let mut back = vec![usize::MAX; hi - lo + 1];
        match last {
            None => {
                for (k, v) in (lo..=hi).enumerate() {
                    row[k] = cost(u, v);
                }
            }
            Some(pu) => {
                let (plo, phi) = spans[pu].unwrap_or((lo, hi));
                for (k, v) in (lo..=hi).enumerate() {
                    let mut b = f32::INFINITY;
                    let mut arg = usize::MAX;
                    for pv in v.saturating_sub(1).max(plo)..=(v + 1).min(phi) {
                        let c = best[pu][pv - plo];
                        if c < b {
                            b = c;
                            arg = pv;
                        }
                    }
                    if arg == usize::MAX {
                        // The lines do not line up: take the nearest end.
                        let pv = v.clamp(plo, phi);
                        b = best[pu][pv - plo];
                        arg = pv;
                    }
                    row[k] = b + cost(u, v);
                    back[k] = arg;
                }
            }
        }
        best.push(row);
        from.push(back);
        last = Some(u);
    }
    // Trace back from the cheapest end.
    let mut path = vec![None; n_u];
    let Some(mut u) = last else { return path };
    let (lo, _) = spans[u].unwrap_or((0, 0));
    let mut v = best[u]
        .iter()
        .enumerate()
        .min_by(|a, b| a.1.total_cmp(b.1))
        .map_or(lo, |(k, _)| lo + k);
    loop {
        path[u] = Some(v);
        let (lo, _) = spans[u].unwrap_or((0, 0));
        let b = from[u][v - lo];
        // Step to the previous line that had a span.
        let Some(pu) = (0..u).rev().find(|&q| spans[q].is_some()) else {
            break;
        };
        if b == usize::MAX {
            break;
        }
        v = b;
        u = pu;
    }
    path
}

/// Who shows where: for each canvas pixel the index of the panel that owns it
/// (`usize::MAX` for none).
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "one sequence: for each panel, find the overlap, the seam and who owns what"
)]
pub fn assign_owners(placed: &[Placed], canvas: &Canvas, use_seams: bool) -> Vec<usize> {
    let (w, h) = (canvas.width, canvas.height);
    let len = w * h;
    let none = usize::MAX;
    let mut owner = vec![none; len];
    if !use_seams {
        // The panel the pixel is deepest inside.
        for (i, o) in owner.iter_mut().enumerate() {
            let mut bestd = EDGE_MARGIN as f32;
            for (k, p) in placed.iter().enumerate() {
                if p.depth[i] >= bestd {
                    bestd = p.depth[i];
                    *o = k;
                }
            }
        }
        return owner;
    }
    // Panel centres on the canvas, to know which side a panel is on.
    let centre_of = |p: &Placed| {
        let (mut sx, mut sy, mut n) = (0.0f64, 0.0f64, 0.0f64);
        for i in (0..len).step_by(11) {
            if inside(p, i) {
                sx += (i % w) as f64;
                sy += (i / w) as f64;
                n += 1.0;
            }
        }
        (sx / n.max(1.0), sy / n.max(1.0))
    };
    let centres: Vec<(f64, f64)> = placed.iter().map(centre_of).collect();
    for (k, p) in placed.iter().enumerate() {
        let mut overlap = vec![false; len];
        let (mut bx0, mut by0, mut bx1, mut by1) = (w, h, 0usize, 0usize);
        let mut other_cx = 0.0f64;
        let mut other_cy = 0.0f64;
        let mut count = 0.0f64;
        for i in 0..len {
            if !inside(p, i) {
                continue;
            }
            if owner[i] == none {
                owner[i] = k;
            } else {
                overlap[i] = true;
                let (x, y) = (i % w, i / w);
                bx0 = bx0.min(x);
                bx1 = bx1.max(x);
                by0 = by0.min(y);
                by1 = by1.max(y);
                other_cx += centres[owner[i]].0;
                other_cy += centres[owner[i]].1;
                count += 1.0;
            }
        }
        if count < 1.0 {
            continue;
        }
        let (ocx, ocy) = (other_cx / count, other_cy / count);
        let vertical = (bx1 - bx0) <= (by1 - by0);
        // The cost of a pixel: how much the new panel and the owner differ.
        let cost_at = |x: usize, y: usize| -> f32 {
            let i = y * w + x;
            if !overlap[i] {
                return 0.0;
            }
            let o = owner[i];
            let mut d = 0.0f32;
            for (fp, fo) in p.planes.iter().zip(&placed[o].planes) {
                let e = fp.data[i] - fo.data[i];
                d += e * e;
            }
            d
        };
        let new_is_after = if vertical {
            centres[k].0 > ocx
        } else {
            centres[k].1 > ocy
        };
        let path = if vertical {
            let span = |y: usize| {
                if y < by0 || y > by1 {
                    return None;
                }
                let row = &overlap[y * w..(y + 1) * w];
                let lo = row.iter().position(|&b| b)?;
                let hi = row.iter().rposition(|&b| b)?;
                Some((lo, hi))
            };
            seam_path(h, &span, &|y, x| cost_at(x, y))
        } else {
            let span = |x: usize| {
                if x < bx0 || x > bx1 {
                    return None;
                }
                let lo = (0..h).find(|&y| overlap[y * w + x])?;
                let hi = (0..h).rev().find(|&y| overlap[y * w + x])?;
                Some((lo, hi))
            };
            seam_path(w, &span, &|x, y| cost_at(x, y))
        };
        for i in 0..len {
            if !overlap[i] {
                continue;
            }
            let (x, y) = (i % w, i / w);
            let (u, v) = if vertical { (y, x) } else { (x, y) };
            let Some(sv) = path[u] else { continue };
            let new_side = if new_is_after { v >= sv } else { v <= sv };
            if new_side {
                owner[i] = k;
            }
        }
    }
    owner
}

/// Blend the placed panels into one picture: each panel's ownership blurred
/// by `feather` pixels, multiplied by how far inside its border the pixel is.
#[must_use]
pub fn blend(placed: &[Placed], owner: &[usize], canvas: &Canvas, feather: f32) -> Picture {
    let (w, h) = (canvas.width, canvas.height);
    let len = w * h;
    let planes_n = placed[0].planes.len();
    let weights: Vec<Vec<f32>> = placed
        .iter()
        .enumerate()
        .map(|(k, p)| {
            let mask = Frame {
                width: w,
                height: h,
                data: owner
                    .iter()
                    .map(|&o| if o == k { 1.0 } else { 0.0 })
                    .collect(),
                bayer: None,
            };
            let soft = if feather > 0.0 {
                gaussian_blur(&mask, feather)
            } else {
                mask
            };
            soft.data
                .iter()
                .zip(&p.depth)
                .map(|(&m, &d)| {
                    let edge = ((f64::from(d) - EDGE_MARGIN) / f64::from(feather.max(1.0)))
                        .clamp(0.0, 1.0) as f32;
                    m * edge
                })
                .collect()
        })
        .collect();
    let mut out: Vec<Frame> = (0..planes_n).map(|_| Frame::zeros(w, h)).collect();
    for i in 0..len {
        let total: f32 = weights.iter().map(|wk| wk[i]).sum();
        if total <= 1e-6 {
            continue;
        }
        for (c, plane) in out.iter_mut().enumerate() {
            let mut s = 0.0f32;
            for (k, p) in placed.iter().enumerate() {
                let wk = weights[k][i];
                if wk > 0.0 {
                    s += wk * p.planes[c].data[i];
                }
            }
            plane.data[i] = s / total;
        }
    }
    picture::from_planes(&out).unwrap_or_else(|_| Picture::Mono(Frame::zeros(w, h)))
}

/// How well the panels agree where they meet, after the levels are matched:
/// the RMS difference, of the mean of the planes, over the overlap pixels next
/// to a seam (owner changes) and over the whole overlap, with the mean level of
/// the overlap.
#[must_use]
pub fn seam_statistics(placed: &[Placed], owner: &[usize], canvas: &Canvas) -> (f32, f32, f32) {
    let (w, h) = (canvas.width, canvas.height);
    let (mut s_seam, mut n_seam, mut s_all, mut n_all, mut level) =
        (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let (mut count, mut lo, mut hi, mut sum) = (0usize, f32::MAX, f32::MIN, 0.0f32);
            for p in placed {
                if inside(p, i) {
                    let v = value(p, i);
                    count += 1;
                    lo = lo.min(v);
                    hi = hi.max(v);
                    sum += v;
                }
            }
            if count < 2 {
                continue;
            }
            let d = f64::from(hi - lo);
            s_all += d * d;
            n_all += 1.0;
            level += f64::from(sum / count as f32);
            let near_seam = [(1usize, 0usize), (0, 1)].iter().any(|&(dx, dy)| {
                let (nx, ny) = (x + dx, y + dy);
                nx < w && ny < h && owner[ny * w + nx] != owner[i]
            });
            if near_seam {
                s_seam += d * d;
                n_seam += 1.0;
            }
        }
    }
    let rms = |s: f64, n: f64| if n > 0.0 { (s / n).sqrt() as f32 } else { 0.0 };
    (
        rms(s_seam, n_seam),
        rms(s_all, n_all),
        if n_all > 0.0 {
            (level / n_all) as f32
        } else {
            0.0
        },
    )
}
