// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Self-calibrated flats from displaced-disc frames (issue #52).
//!
//! Capture the Sun at many positions on the sensor (the mount off tracking, or
//! nudged between frames). A frame is the Sun `O` seen through the sensor's
//! gain `g`, displaced by `s_k`:
//!
//! ```text
//! frame_k(x) = g(x) * O(x - s_k)
//! ```
//!
//! The gain stays where it is on the sensor while the Sun moves, so the two can
//! be told apart from the data alone (Kuhn, Lin and Loranz 1991): alternately
//! solve for the scene given the gain (shift the frames back, divide by the
//! gain and average, weighting by the gain squared) and for the gain given the
//! scene (the least-squares ratio over the frames), and repeat. The result is a
//! flat that includes dust, fringes and the column pattern of the sensor
//! without a diffuser or a sky flat, from the same optical train at the
//! observing moment.
//!
//! The scene is assumed not to change over the capture; the displacements
//! come from the limb fit of each frame, refined on the flattened frames.
//!
//! **What cannot be recovered.** A smooth gradient of the gain is nearly
//! indistinguishable from a smooth part of the scene: shifting by `s` changes a
//! mode of wavelength `L` by only `s / L` of itself, so the smooth modes
//! converge slowly and are the noisiest part of the solution. The iteration
//! starts from a flat gain and stops when the change is small, which leaves the
//! largest scales close to flat. [`SelfFlatReport::smooth_scale_px`] says
//! the wavelength below which the solution is trustworthy. A mosaic is solved
//! one colour plane at a time (each plane is an image of its own).
//!
//! # References
//!
//! - Kuhn, J. R., Lin, H. and Loranz, D. (1991), Gain calibrating nonuniform image-array data using only the
//!   image data, Publ. Astron. Soc. Pac. 103, 1097-1108, doi:10.1086/132932.
//! - Huber, P. J. (1964), Ann. Math. Statist. 35, 73-101, doi:10.1214/aoms/1177703732 (the misfit weight).

#![allow(
    clippy::many_single_char_names,
    reason = "x, y, w, h, n, m and the images g, o are the natural names in image maths"
)]

use std::sync::atomic::{AtomicBool, Ordering};

use rayon::prelude::*;

use crate::align::Shift;
use crate::calibrate::master::{
    BuildOptions, CalInput, Master, MasterInfo, MasterKind, Progress, load, normalise_per_colour,
};
use crate::error::{Error, Result};
use crate::filter::gaussian_blur;
use crate::frame::{Frame, Rect};
use crate::mask::fit_disc;
use crate::resample::{Kernel, resample_region};

/// Fewest frames the solution is made from.
pub const MIN_FRAMES: usize = 6;

/// Least displacement, in pixels between the two furthest frames, the solver
/// accepts.
pub const MIN_SPAN_PX: f32 = 30.0;

/// Frames used when the caller does not say: each one costs time in every
/// alternation, and more than this adds little (see the user guide).
pub const DEFAULT_FRAMES: usize = 60;

/// Settings of the solver.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SelfFlatOptions {
    /// Alternations of the scene and the gain.
    pub iterations: usize,
    /// Resampling kernel for the shifts.
    pub kernel: Kernel,
    /// What to do with the smooth scales of the solution.
    pub smooth: Smooth,
}

/// The smooth part of a self-calibrated gain, which the data cannot separate
/// from the smooth part of the scene (limb darkening, a gradient across the
/// disc): see the module documentation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Smooth {
    /// Keep every scale. Only right when the Sun was moved by much more than
    /// the scale of anything smooth in the scene.
    Keep,
    /// Divide out the solution blurred by a Gaussian of this many pixels, keeping
    /// the structure finer than that (dust, fringes, the column pattern).
    HighPass(f32),
    /// A quarter of the span of the displacements, between 6 and 120 pixels.
    Auto,
}

impl Default for SelfFlatOptions {
    fn default() -> Self {
        Self {
            iterations: 30,
            kernel: Kernel::Lanczos3,
            smooth: Smooth::Auto,
        }
    }
}

/// Divide `gain` by its own Gaussian blur of `sigma` pixels, taken over the
/// `covered` pixels only (normalised convolution), and leave the rest at 1:
/// the structure finer than `sigma` stays, the smooth scales go.
pub fn high_pass(gain: &mut [f32], covered: &[bool], w: usize, h: usize, sigma: f32) {
    let frame = |data: Vec<f32>| Frame {
        width: w,
        height: h,
        data,
        bayer: None,
    };
    let num = gaussian_blur(
        &frame(
            gain.iter()
                .zip(covered)
                .map(|(&g, &c)| if c { g } else { 0.0 })
                .collect(),
        ),
        sigma,
    );
    let den = gaussian_blur(
        &frame(covered.iter().map(|&c| f32::from(u8::from(c))).collect()),
        sigma,
    );
    for i in 0..w * h {
        gain[i] = if covered[i] && den.data[i] > 1e-3 {
            gain[i] / (num.data[i] / den.data[i])
        } else {
            1.0
        };
    }
}

/// What the solution looks like, for the user.
#[derive(Clone, Debug, PartialEq)]
pub struct SelfFlatReport {
    /// Frames used.
    pub frames: usize,
    /// Colour planes solved (1 for mono, 4 for a mosaic).
    pub planes: usize,
    /// Alternations made.
    pub iterations: usize,
    /// RMS change of the gain in the last alternation.
    pub last_change: f32,
    /// RMS of the frames' misfit to `gain * scene`, relative to the signal, over
    /// the well-lit pixels.
    pub residual: f32,
    /// Share of the pixels that the scene covered in enough frames for the gain
    /// to be known; the rest are left at 1.
    pub coverage: f32,
    /// Largest distance between two frames' displacements, in pixels.
    pub span_px: f32,
    /// Displacement of each frame from the first, in pixels of the sensor.
    pub shifts: Vec<Shift>,
    /// Wavelength, in pixels, above which the gain is not recovered well: eight
    /// times the span of the displacements.
    pub smooth_scale_px: f32,
    /// The Gaussian, in pixels, whose blur was divided out to remove the smooth
    /// scales; `None` when they were kept.
    pub high_pass_px: Option<f32>,
}

/// The flat and how it was made.
pub struct SelfFlat {
    /// The master flat, normalised to a mean of 1 within each colour.
    pub master: Master,
    /// The report.
    pub report: SelfFlatReport,
    /// Things the user should know.
    pub warnings: Vec<String>,
}

/// A plane of a frame: every second pixel of every second row of a mosaic, or
/// the whole of a mono frame.
fn plane(f: &Frame, px: usize, py: usize, step: usize) -> Frame {
    let (w, h) = (f.width.div_ceil(step), f.height.div_ceil(step));
    let mut data = Vec::with_capacity(w * h);
    for y in 0..h {
        let sy = (y * step + py).min(f.height - 1);
        for x in 0..w {
            data.push(f.data[sy * f.width + (x * step + px).min(f.width - 1)]);
        }
    }
    Frame {
        width: w,
        height: h,
        data,
        bayer: None,
    }
}

/// Displacement of each frame from the first, from the limb fit of the mean of
/// the planes (in plane pixels). `None` for a frame with no clear disc.
fn disc_centres(frames: &[Frame]) -> Vec<Option<(f32, f32)>> {
    frames
        .par_iter()
        .map(|f| fit_disc(f).map(|d| (d.cx, d.cy)))
        .collect()
}

/// The mean of the planes of a mosaic frame (or the frame itself), for the limb fit.
fn super_pixel(f: &Frame) -> Frame {
    if f.bayer.is_none() {
        return Frame {
            bayer: None,
            ..f.clone()
        };
    }
    let (w, h) = (f.width / 2, f.height / 2);
    let mut data = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let i = 2 * y * f.width + 2 * x;
            data.push(
                0.25 * (f.data[i] + f.data[i + 1] + f.data[i + f.width] + f.data[i + f.width + 1]),
            );
        }
    }
    Frame {
        width: w,
        height: h,
        data,
        bayer: None,
    }
}

/// `g * pad` placed on a canvas of `cw` x `ch` pixels with the frame at `(m, m)`.
fn pad(src: &[f32], w: usize, h: usize, m: usize) -> Frame {
    let (cw, ch) = (w + 2 * m, h + 2 * m);
    let mut data = vec![0.0f32; cw * ch];
    for y in 0..h {
        data[(y + m) * cw + m..(y + m) * cw + m + w].copy_from_slice(&src[y * w..(y + 1) * w]);
    }
    Frame {
        width: cw,
        height: ch,
        data,
        bayer: None,
    }
}

/// Pixels of the scene dimmer than this share of its brightness are not used
/// for the gain.
const BRIGHT: f32 = 0.3;

/// The brightness of the scene: its 99.5th percentile of positive values.
fn scene_level(scene: &[f32]) -> f32 {
    let mut v: Vec<f32> = scene.iter().copied().filter(|&x| x > 0.0).collect();
    if v.is_empty() {
        return 0.0;
    }
    let k = ((v.len() as f32) * 0.995) as usize;
    let k = k.min(v.len() - 1);
    *v.select_nth_unstable_by(k, f32::total_cmp).1
}

/// The scene as frame `shift` sees it: `scene(x - s)` for every pixel `x` of the
/// frame (`NaN` outside the canvas).
fn model_frame(scene: &Frame, s: Shift, m: f32, own: Rect, kernel: Kernel) -> Vec<f32> {
    let mut o = vec![0.0f32; own.area()];
    // The canvas holds the scene at (m, m): sample it at x + (m - s).
    resample_region(scene, Shift::new(m - s.dx, m - s.dy), own, kernel, &mut o);
    o
}

/// Weights for the gain step: zero where the model scene is dim or undefined,
/// else 1, or (after the first alternation) a Huber weight on the misfit
/// of the frame to `gain * model`, so that a misregistered patch does not
/// pull the gain.
fn huber_weights(frame: &[f32], model: &[f32], gain: &[f32], bright: f32, first: bool) -> Vec<f32> {
    let mut weight: Vec<f32> = model
        .iter()
        .map(|&o| {
            if o.is_finite() && o > bright {
                1.0
            } else {
                0.0
            }
        })
        .collect();
    if first {
        return weight;
    }
    let mut r: Vec<f32> = (0..frame.len())
        .filter(|&i| weight[i] > 0.0)
        .map(|i| (frame[i] - gain[i] * model[i]).abs())
        .collect();
    if r.len() < 16 {
        return weight;
    }
    let mid = r.len() / 2;
    let scale = 1.4826 * *r.select_nth_unstable_by(mid, f32::total_cmp).1;
    let limit = 2.5 * scale.max(1e-6);
    for i in 0..frame.len() {
        if weight[i] > 0.0 {
            let e = (frame[i] - gain[i] * model[i]).abs();
            if e > limit {
                weight[i] = limit / e;
            }
        }
    }
    weight
}

/// What one plane's solution gives.
struct PlaneSolution {
    gain: Vec<f32>,
    coverage: Vec<f32>,
    last_change: f32,
    residual: f32,
}

#[allow(
    clippy::too_many_lines,
    reason = "the two alternating steps and the misfit are one flat sequence"
)]
/// Alternating least squares for one plane. `frames` are the plane images
/// (dark removed), `shifts` their displacements in plane pixels.
fn solve_plane(
    frames: &[Frame],
    shifts: &[Shift],
    margin: usize,
    opts: &SelfFlatOptions,
    cancel: &AtomicBool,
    progress: &dyn Fn(f32),
) -> Result<PlaneSolution> {
    let (w, h) = (frames[0].width, frames[0].height);
    let (cw, ch) = (w + 2 * margin, h + 2 * margin);
    let canvas = Rect::new(0, 0, cw, ch);
    let own = Rect::new(0, 0, w, h);
    let m = margin as f32;
    let n = frames.len();
    let mut gain = vec![1.0f32; w * h];
    let mut scene = vec![0.0f32; cw * ch];
    let mut last_change = 0.0;
    for it in 0..opts.iterations {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        // The scene given the gain: shift each frame back, weight by the gain squared.
        let (num, den) = frames
            .par_iter()
            .zip(shifts)
            .fold(
                || (vec![0.0f32; cw * ch], vec![0.0f32; cw * ch]),
                |(mut num, mut den), (f, &s)| {
                    let weighted: Vec<f32> =
                        f.data.iter().zip(&gain).map(|(&v, &g)| v * g).collect();
                    let g2: Vec<f32> = gain.iter().map(|&g| g * g).collect();
                    // The scene at canvas pixel c is the padded frame at c + s.
                    let mut a = vec![0.0f32; cw * ch];
                    let mut b = vec![0.0f32; cw * ch];
                    let (pw, pd) = (pad(&weighted, w, h, margin), pad(&g2, w, h, margin));
                    resample_region(&pw, s, canvas, opts.kernel, &mut a);
                    resample_region(&pd, s, canvas, opts.kernel, &mut b);
                    for i in 0..cw * ch {
                        if a[i].is_finite() && b[i].is_finite() {
                            num[i] += a[i];
                            den[i] += b[i];
                        }
                    }
                    (num, den)
                },
            )
            .reduce(
                || (vec![0.0f32; cw * ch], vec![0.0f32; cw * ch]),
                |(mut an, mut ad), (bn, bd)| {
                    an.iter_mut().zip(&bn).for_each(|(a, b)| *a += b);
                    ad.iter_mut().zip(&bd).for_each(|(a, b)| *a += b);
                    (an, ad)
                },
            );
        for (o, (&a, &b)) in scene.iter_mut().zip(num.iter().zip(&den)) {
            // Where less than half a frame covers the point the scene is unknown.
            *o = if b > 0.5 { a / b } else { 0.0 };
        }
        // The gain given the scene: the least-squares ratio over the frames,
        // using only where the scene is bright (the sky and the limb, where a
        // pixel of misregistration changes the scene by its own size, say
        // nothing about the gain) and down-weighting what does not fit.
        let scene_frame = Frame {
            width: cw,
            height: ch,
            data: scene.clone(),
            bayer: None,
        };
        let lit = scene_level(&scene);
        let bright = BRIGHT * lit;
        let first = it == 0;
        let (gn, gd, cnt) = frames
            .par_iter()
            .zip(shifts)
            .fold(
                || {
                    (
                        vec![0.0f32; w * h],
                        vec![0.0f32; w * h],
                        vec![0.0f32; w * h],
                    )
                },
                |(mut gn, mut gd, mut cnt), (f, &s)| {
                    let o = model_frame(&scene_frame, s, m, own, opts.kernel);
                    let weight = huber_weights(&f.data, &o, &gain, bright, first);
                    for i in 0..w * h {
                        let wt = weight[i];
                        if wt > 0.0 {
                            gn[i] += wt * f.data[i] * o[i];
                            gd[i] += wt * o[i] * o[i];
                            cnt[i] += wt;
                        }
                    }
                    (gn, gd, cnt)
                },
            )
            .reduce(
                || {
                    (
                        vec![0.0f32; w * h],
                        vec![0.0f32; w * h],
                        vec![0.0f32; w * h],
                    )
                },
                |(mut an, mut ad, mut ac), (bn, bd, bc)| {
                    an.iter_mut().zip(&bn).for_each(|(a, b)| *a += b);
                    ad.iter_mut().zip(&bd).for_each(|(a, b)| *a += b);
                    ac.iter_mut().zip(&bc).for_each(|(a, b)| *a += b);
                    (an, ad, ac)
                },
            );
        let need = (n as f32 / 10.0).max(3.0);
        let mut sum = 0.0f64;
        let mut count = 0u64;
        let mut new = vec![1.0f32; w * h];
        for i in 0..w * h {
            if cnt[i] >= need && gd[i] > 0.0 {
                new[i] = gn[i] / gd[i];
                sum += f64::from(new[i]);
                count += 1;
            }
        }
        let mean = if count > 0 {
            (sum / count as f64) as f32
        } else {
            1.0
        };
        let mut change = 0.0f64;
        for i in 0..w * h {
            if cnt[i] >= need && gd[i] > 0.0 {
                new[i] /= mean;
            }
            change += f64::from((new[i] - gain[i]).powi(2));
        }
        last_change = (change / (w * h) as f64).sqrt() as f32;
        gain = new;
        progress((it + 1) as f32 / opts.iterations as f32);
    }
    // Coverage and the misfit.
    let scene_frame = Frame {
        width: cw,
        height: ch,
        data: scene,
        bayer: None,
    };
    let lit = scene_level(&scene_frame.data);
    let (cov, fit) = frames
        .par_iter()
        .zip(shifts)
        .fold(
            || (vec![0.0f32; w * h], (0.0f64, 0.0f64)),
            |(mut cov, mut fit), (f, &s)| {
                let o = model_frame(&scene_frame, s, m, own, opts.kernel);
                for i in 0..w * h {
                    if o[i].is_finite() && o[i] > BRIGHT * lit {
                        cov[i] += 1.0;
                        let e = f.data[i] - gain[i] * o[i];
                        fit.0 += f64::from(e * e);
                        fit.1 += f64::from(f.data[i] * f.data[i]);
                    }
                }
                (cov, fit)
            },
        )
        .reduce(
            || (vec![0.0f32; w * h], (0.0, 0.0)),
            |(mut a, fa), (b, fb)| {
                a.iter_mut().zip(&b).for_each(|(x, y)| *x += y);
                (a, (fa.0 + fb.0, fa.1 + fb.1))
            },
        );
    Ok(PlaneSolution {
        gain,
        coverage: cov.iter().map(|&c| c / n as f32).collect(),
        last_change,
        residual: if fit.1 > 0.0 {
            (fit.0 / fit.1).sqrt() as f32
        } else {
            0.0
        },
    })
}

/// Solve for the gain of frames that show the same scene displaced by
/// `shifts` (in pixels of the frames; `None` finds them from the limb). The
/// frames are the raw planes with the dark taken off and are not normalised.
///
/// # Errors
/// Fewer than [`MIN_FRAMES`] frames, frames of different sizes, no usable
/// limb in enough frames, or cancellation.
#[allow(
    clippy::too_many_lines,
    reason = "displacements, planes, then the report: one flat sequence"
)]
pub fn solve(
    frames: &[Frame],
    shifts: Option<&[Shift]>,
    opts: &SelfFlatOptions,
    cancel: &AtomicBool,
    progress: Progress<'_>,
) -> Result<SelfFlat> {
    let n = frames.len();
    if n < MIN_FRAMES {
        return Err(Error::Invalid(format!(
            "a self-calibrated flat needs at least {MIN_FRAMES} frames with the Sun in different places, got {n}"
        )));
    }
    let (w, h) = (frames[0].width, frames[0].height);
    if frames.iter().any(|f| (f.width, f.height) != (w, h)) {
        return Err(Error::Invalid("the frames differ in size".into()));
    }
    let bayer = frames[0].bayer;
    let step = if bayer.is_some() { 2 } else { 1 };
    let mut warnings = Vec::new();

    // Displacements in sensor pixels, from the limb of the plane means.
    let fit_frames: Vec<Frame> = frames.iter().map(super_pixel).collect();
    let (keep, shifts_px): (Vec<usize>, Vec<Shift>) = if let Some(s) = shifts {
        ((0..n).collect(), s.to_vec())
    } else {
        let centres = disc_centres(&fit_frames);
        let keep: Vec<usize> = (0..n).filter(|&i| centres[i].is_some()).collect();
        if keep.len() < MIN_FRAMES {
            return Err(Error::Invalid(format!(
                "the limb was found in only {} of {n} frames: the Sun must show clearly, at different places on the sensor, and at least {MIN_FRAMES} frames are needed",
                keep.len()
            )));
        }
        if keep.len() < n {
            warnings.push(format!(
                "the limb was not found in {} frame(s); they were left out",
                n - keep.len()
            ));
        }
        let (cx0, cy0) = centres[keep[0]].unwrap_or_default();
        let scale = step as f32;
        let s = keep
            .iter()
            .map(|&i| {
                let (cx, cy) = centres[i].unwrap_or_default();
                // The disc is at c: the content is displaced by c - c0.
                Shift::new((cx - cx0) * scale, (cy - cy0) * scale)
            })
            .collect();
        (keep, s)
    };
    let used: Vec<&Frame> = keep.iter().map(|&i| &frames[i]).collect();
    let nu = used.len();
    if nu < MIN_FRAMES {
        return Err(Error::Invalid(format!(
            "only {nu} frames have a clear limb: need at least {MIN_FRAMES}"
        )));
    }
    let span = shifts_px
        .iter()
        .flat_map(|a| shifts_px.iter().map(move |b| (*a - *b).length()))
        .fold(0.0f32, f32::max);
    if span < MIN_SPAN_PX {
        return Err(Error::Invalid(format!(
            "the Sun moved only {span:.0} pixels over the capture: the gain and the scene cannot be told apart (on the project's untracked-drift captures the solution took the granulation with it). Move it by at least {MIN_SPAN_PX:.0} pixels, a tenth of the field is better, between frames"
        )));
    }
    let margin = (shifts_px
        .iter()
        .map(|s| s.dx.abs().max(s.dy.abs()))
        .fold(0.0f32, f32::max)
        / step as f32)
        .ceil() as usize
        + 4;

    let high_pass_px = match opts.smooth {
        Smooth::Keep => None,
        Smooth::HighPass(s) => Some(s),
        Smooth::Auto => Some((span / 4.0).clamp(6.0, 120.0)),
    };
    // One plane at a time.
    let planes: Vec<(usize, usize)> = if bayer.is_some() {
        vec![(0, 0), (1, 0), (0, 1), (1, 1)]
    } else {
        vec![(0, 0)]
    };
    let mut gain = vec![1.0f32; w * h];
    let mut cover = vec![0.0f32; w * h];
    let (mut change, mut resid) = (0.0f32, 0.0f32);
    // A colour the filter leaves empty (the blue pixels behind a sodium filter)
    // has no scene to solve against: its gain would be noise over noise.
    let all_planes: Vec<Vec<Frame>> = planes
        .iter()
        .map(|&(px, py)| used.iter().map(|f| plane(f, px, py, step)).collect())
        .collect();
    let level = |pf: &[Frame]| pf.iter().map(Frame::mean).sum::<f32>() / pf.len() as f32;
    let brightest = all_planes.iter().map(|p| level(p)).fold(0.0f32, f32::max);
    let mut solved = 0;
    for (pi, pf) in all_planes.iter().enumerate() {
        let (px, py) = planes[pi];
        if level(pf) < 0.1 * brightest {
            warnings.push(format!(
                "the colour at ({px}, {py}) of the mosaic carries under a tenth of the brightest colour's signal (an empty channel behind the filter?): its flat is left at 1"
            ));
            for y in 0..pf[0].height {
                for x in 0..pf[0].width {
                    cover[((y * step + py).min(h - 1)) * w + (x * step + px).min(w - 1)] = 1.0;
                }
            }
            continue;
        }
        solved += 1;
        let ps: Vec<Shift> = shifts_px
            .iter()
            .map(|s| Shift::new(s.dx / step as f32, s.dy / step as f32))
            .collect();
        let sub = |f: f32| progress((pi as f32 + f) / planes.len() as f32);
        let mut sol = solve_plane(pf, &ps, margin, opts, cancel, &sub)?;
        let (pw, ph) = (pf[0].width, pf[0].height);
        if let Some(sigma) = high_pass_px {
            let covered: Vec<bool> = sol.coverage.iter().map(|&c| c >= 0.25).collect();
            high_pass(&mut sol.gain, &covered, pw, ph, sigma / step as f32);
        }
        for y in 0..ph {
            for x in 0..pw {
                let (fx, fy) = ((x * step + px).min(w - 1), (y * step + py).min(h - 1));
                gain[fy * w + fx] = sol.gain[y * pw + x];
                cover[fy * w + fx] = sol.coverage[y * pw + x];
            }
        }
        change = change.max(sol.last_change);
        resid = resid.max(sol.residual);
    }
    let mut frame = Frame {
        width: w,
        height: h,
        data: gain,
        bayer,
    };
    normalise_per_colour(&mut frame);
    let covered = cover.iter().filter(|&&c| c >= 0.25).count() as f32 / (w * h) as f32;
    if covered < 0.5 {
        warnings.push(format!(
            "only {:.0}% of the sensor was covered by the Sun in enough frames; the rest of the flat is 1. Move the Sun over more of the field",
            covered * 100.0
        ));
    }
    if resid > 0.05 {
        warnings.push(format!(
            "the frames fit the model to only {:.1}% (RMS): the Sun changed during the capture, the displacements are wrong, or the dark is missing",
            resid * 100.0
        ));
    }
    let report = SelfFlatReport {
        frames: nu,
        planes: solved,
        iterations: opts.iterations,
        last_change: change,
        residual: resid,
        coverage: covered,
        span_px: span,
        shifts: shifts_px,
        smooth_scale_px: 8.0 * span,
        high_pass_px,
    };
    let input_info = MasterInfo {
        kind: MasterKind::Flat,
        frames_used: nu,
        frames_total: n,
        combine: format!("self-calibration, {} alternations", opts.iterations),
        exposure_s: None,
        bayer,
        source: "displaced frames".to_owned(),
        fingerprint: 0,
    };
    Ok(SelfFlat {
        master: Master::new(MasterKind::Flat, frame, input_info),
        report,
        warnings,
    })
}

/// Build a self-calibrated flat from displaced-disc captures: read `input`, take
/// `dark` off (a master dark at the same exposure; without one the offset
/// stays in the frames and biases the flat), and solve.
///
/// # Errors
/// Read failure, see [`solve`].
pub fn build_selfflat(
    input: &mut CalInput,
    dark: Option<&Master>,
    opts: &BuildOptions,
    so: &SelfFlatOptions,
    progress: Progress<'_>,
    cancel: &AtomicBool,
) -> Result<SelfFlat> {
    let (w, h) = input.size();
    let bayer = input.bayer;
    let mut warnings = Vec::new();
    if let Some(d) = dark {
        if (d.frame.width, d.frame.height) != (w, h) {
            return Err(Error::Invalid(format!(
                "the dark is {}x{} but the frames are {w}x{h}",
                d.frame.width, d.frame.height
            )));
        }
    } else {
        warnings.push(
            "no dark given: the offset stays in the frames, which biases the flat in the dim parts. Capture darks at the exposure of the Sun"
                .to_owned(),
        );
    }
    let (data, _) = load(input, opts, progress, cancel, |d| {
        if let Some(dk) = dark {
            for (v, &o) in d.iter_mut().zip(&dk.frame.data) {
                *v -= o;
            }
        }
    })?;
    let frames: Vec<Frame> = data
        .into_iter()
        .map(|d| Frame {
            width: w,
            height: h,
            data: d,
            bayer,
        })
        .collect();
    let mut built = solve(&frames, None, so, cancel, progress)?;
    built.master.info.source.clone_from(&input.label);
    built.master.info.exposure_s = input.exposure_s;
    built.warnings.splice(0..0, warnings);
    Ok(built)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Rng, SyntheticConfig, SyntheticSun};

    const W: usize = 520;
    const H: usize = 420;

    fn sun() -> SyntheticSun {
        SyntheticSun::new(&SyntheticConfig {
            width: W,
            height: H,
            radius_frac: 0.3,
            granulation: 0.1,
            granule_sigma: 3.0,
            ..SyntheticConfig::default()
        })
    }

    /// Smooth vignetting, dust donuts and a column pattern.
    fn gain(seed: u64, vignette: f32) -> Vec<f32> {
        let mut rng = Rng::new(seed);
        let (cx, cy) = (W as f32 / 2.0, H as f32 / 2.0);
        let rmax = cx.hypot(cy);
        let cols: Vec<f32> = (0..W).map(|_| 1.0 + 0.008 * rng.gauss()).collect();
        let dust: Vec<(f32, f32, f32, f32)> = (0..14)
            .map(|_| {
                (
                    rng.uniform() * W as f32,
                    rng.uniform() * H as f32,
                    6.0 + 8.0 * rng.uniform(),
                    0.04 + 0.08 * rng.uniform(),
                )
            })
            .collect();
        (0..W * H)
            .map(|i| {
                let (x, y) = ((i % W) as f32, (i / W) as f32);
                let r = (x - cx).hypot(y - cy) / rmax;
                let mut g = (1.0 - vignette * r * r) * cols[i % W];
                for &(dx, dy, rad, depth) in &dust {
                    let d = (x - dx).hypot(y - dy);
                    // A ring: dark on the edge of the shadow, lighter inside.
                    g *= 1.0 - depth * (-((d - rad) / 2.5).powi(2)).exp()
                        + 0.3 * depth * (-(d / (0.5 * rad)).powi(2)).exp();
                }
                g
            })
            .collect()
    }

    fn capture(
        n: usize,
        g: &[f32],
        spread: f32,
        noise: f32,
        seed: u64,
    ) -> (Vec<Frame>, Vec<Shift>) {
        let s = sun();
        let mut rng = Rng::new(seed);
        let mut frames = Vec::new();
        let mut shifts = Vec::new();
        for _ in 0..n {
            let sh = Shift::new(
                spread * (2.0 * rng.uniform() - 1.0),
                spread * (2.0 * rng.uniform() - 1.0),
            );
            let clean = s.render_clean(sh, None);
            let data = clean
                .data
                .iter()
                .zip(g)
                .map(|(&v, &g)| v * g + noise * rng.gauss())
                .collect();
            frames.push(Frame::new(W, H, data).unwrap_or_else(|e| panic!("{e}")));
            shifts.push(sh);
        }
        (frames, shifts)
    }

    /// RMS of `est / truth - 1` over the central region, after the means are matched.
    fn error(est: &[f32], truth: &[f32]) -> f32 {
        let area: Vec<usize> = (0..W * H)
            .filter(|&i| {
                let (x, y) = (i % W, i / W);
                (120..W - 120).contains(&x) && (100..H - 100).contains(&y)
            })
            .collect();
        let r: Vec<f64> = area.iter().map(|&i| f64::from(est[i] / truth[i])).collect();
        let m = r.iter().sum::<f64>() / r.len() as f64;
        (r.iter().map(|v| (v / m - 1.0).powi(2)).sum::<f64>() / r.len() as f64).sqrt() as f32
    }

    fn run(n: usize, vignette: f32, spread: f32, known: bool, noise: f32) -> (f32, SelfFlatReport) {
        let g = gain(5, vignette);
        let (frames, shifts) = capture(n, &g, spread, noise, 9);
        let built = solve(
            &frames,
            known.then_some(&shifts[..]),
            &keep(),
            &AtomicBool::new(false),
            &|_| {},
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let e = error(&built.master.frame.data, &g);
        (e, built.report)
    }

    #[test]
    fn the_gain_is_recovered_to_under_one_percent_from_ten_to_fifty_frames() {
        let mut errors = Vec::new();
        for n in [10usize, 25, 50] {
            let (e, r) = run(n, 0.25, 90.0, false, 0.002);
            eprintln!(
                "{n} frames: gain error {:.3}% RMS, misfit {:.2}%",
                e * 100.0,
                r.residual * 100.0
            );
            assert!(e < 0.01, "{n} frames: {e}");
            errors.push(e);
        }
        assert!(
            errors[2] < errors[0],
            "more frames must not be worse: {errors:?}"
        );
    }

    #[test]
    fn the_limb_gives_the_displacements_as_well_as_knowing_them() {
        let (known, _) = run(25, 0.25, 90.0, true, 0.002);
        let (found, r) = run(25, 0.25, 90.0, false, 0.002);
        eprintln!(
            "known shifts {:.3}%, from the limb {:.3}%",
            known * 100.0,
            found * 100.0
        );
        assert!(
            found < 0.01 && found < 1.5 * known + 0.002,
            "{known} {found}"
        );
        // The displacements it found are the ones applied, relative to the first frame.
        let g = gain(5, 0.25);
        let (_, truth) = capture(25, &g, 90.0, 0.002, 9);
        let mut worst = 0.0f32;
        for (got, want) in r.shifts.iter().zip(&truth) {
            let d = (*got - (*want - truth[0])).length();
            worst = worst.max(d);
        }
        eprintln!("worst displacement error {worst:.3} px");
        assert!(worst < 0.5, "{worst}");
    }

    fn keep() -> SelfFlatOptions {
        SelfFlatOptions {
            smooth: Smooth::Keep,
            ..SelfFlatOptions::default()
        }
    }

    fn quiet() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn noise_and_shift_errors_cost_what_they_should() {
        let g = gain(5, 0.25);
        let (frames, shifts) = capture(25, &g, 90.0, 0.002, 9);
        let mut by_shift = Vec::new();
        for sigma in [0.0f32, 0.1, 0.2, 0.4] {
            let mut rng = Rng::new(3);
            let bad: Vec<Shift> = shifts
                .iter()
                .map(|s| Shift::new(s.dx + sigma * rng.gauss(), s.dy + sigma * rng.gauss()))
                .collect();
            let built = solve(&frames, Some(&bad), &keep(), &quiet(), &|_| {})
                .unwrap_or_else(|e| panic!("{e}"));
            by_shift.push(error(&built.master.frame.data, &g));
        }
        eprintln!("shift error 0, 0.1, 0.2, 0.4 px: {by_shift:.4?}");
        assert!(by_shift.windows(2).all(|w| w[1] >= w[0]), "{by_shift:?}");
        assert!(by_shift[2] < 0.01, "0.2 px of shift error: {}", by_shift[2]);
        let mut by_noise = Vec::new();
        for noise in [0.005f32, 0.02, 0.05] {
            by_noise.push(run(25, 0.25, 90.0, false, noise).0);
        }
        eprintln!("noise 0.005, 0.02, 0.05: {by_noise:.4?}");
        assert!(by_noise.windows(2).all(|w| w[1] > w[0]) && by_noise[1] < 0.01);
    }

    #[test]
    fn a_small_displacement_cannot_separate_the_gain_from_the_scene() {
        let (small, r) = run(25, 0.25, 15.0, false, 0.002);
        let (large, _) = run(25, 0.25, 60.0, false, 0.002);
        eprintln!(
            "moved {:.0} px: {:.2}%; moved 139 px: {:.2}%",
            r.span_px,
            small * 100.0,
            large * 100.0
        );
        assert!(small > 3.0 * large, "{small} vs {large}");
        assert!(r.span_px < 50.0 && r.smooth_scale_px < 400.0);
        // Under the least span it is refused, with the reason.
        let g = gain(5, 0.25);
        let (frames, _) = capture(25, &g, 8.0, 0.002, 9);
        let refused = solve(&frames, None, &keep(), &quiet(), &|_| {});
        assert!(
            matches!(&refused, Err(Error::Invalid(m)) if m.contains("moved only")),
            "{:?}",
            refused.map(|_| ())
        );
    }

    #[test]
    fn the_default_keeps_the_fine_structure_and_drops_the_smooth_scales() {
        // Dust and the column pattern stay; the vignetting, which the data
        // cannot tell from the scene, goes.
        let g = gain(5, 0.25);
        let (frames, _) = capture(25, &g, 90.0, 0.002, 9);
        let built = solve(
            &frames,
            None,
            &SelfFlatOptions::default(),
            &quiet(),
            &|_| {},
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let sigma = built.report.high_pass_px.expect("high-passed by default");
        let all = vec![true; W * H];
        let mut want = g.clone();
        high_pass(&mut want, &all, W, H, sigma);
        let e = error(&built.master.frame.data, &want);
        let smooth = crate::filter::gaussian_blur(&built.master.frame, sigma);
        let m = smooth.data.iter().sum::<f32>() / smooth.data.len() as f32;
        let flat = (smooth.data.iter().map(|v| (v - m).powi(2)).sum::<f32>()
            / smooth.data.len() as f32)
            .sqrt();
        eprintln!(
            "high-pass {sigma:.0} px: error against the high-passed truth {:.3}%, smooth part RMS {:.3}%",
            e * 100.0,
            flat * 100.0
        );
        assert!(e < 0.01, "{e}");
        assert!(flat < 0.02, "{flat}");
        assert!((6.0..=120.0).contains(&sigma));
        // Keeping every scale reports none.
        let kept =
            solve(&frames, None, &keep(), &quiet(), &|_| {}).unwrap_or_else(|e| panic!("{e}"));
        assert!(kept.report.high_pass_px.is_none());
    }

    #[test]
    fn a_scene_that_changes_costs_accuracy() {
        // Granule evolution: each frame carries its own smooth random field of
        // the given amplitude, bound to the Sun and not to the sensor.
        let g = gain(5, 0.25);
        let mut errors = Vec::new();
        for amp in [0.0f32, 0.02, 0.06] {
            let s = sun();
            let mut rng = Rng::new(11);
            let mut frames = Vec::new();
            for _ in 0..25 {
                let sh = Shift::new(
                    90.0 * (2.0 * rng.uniform() - 1.0),
                    90.0 * (2.0 * rng.uniform() - 1.0),
                );
                let clean = s.render_clean(sh, None);
                let field: Vec<f32> = (0..W * H).map(|_| rng.gauss()).collect();
                let field = crate::filter::gaussian_blur(
                    &Frame::new(W, H, field).unwrap_or_else(|e| panic!("{e}")),
                    2.5,
                );
                let sd = (field.data.iter().map(|v| v * v).sum::<f32>() / field.data.len() as f32)
                    .sqrt();
                let data = clean
                    .data
                    .iter()
                    .zip(&field.data)
                    .zip(&g)
                    .map(|((&v, &f), &g)| (v * (1.0 + amp * f / sd) + 0.002 * rng.gauss()) * g)
                    .collect();
                frames.push(Frame::new(W, H, data).unwrap_or_else(|e| panic!("{e}")));
            }
            let built =
                solve(&frames, None, &keep(), &quiet(), &|_| {}).unwrap_or_else(|e| panic!("{e}"));
            errors.push(error(&built.master.frame.data, &g));
        }
        eprintln!("evolution 0%, 2%, 6%: gain error {errors:.4?}");
        assert!(errors[0] < errors[1] && errors[1] < errors[2], "{errors:?}");
        assert!(errors[1] < 0.02);
    }

    #[test]
    fn a_colour_mosaic_is_solved_one_plane_at_a_time() {
        use crate::frame::BayerPattern;
        let g = gain(5, 0.25);
        let colour = [1.0f32, 0.8, 0.55];
        let (frames, _) = capture(20, &g, 90.0, 0.002, 9);
        let mosaic: Vec<Frame> = frames
            .iter()
            .map(|f| Frame {
                bayer: Some(BayerPattern::Rggb),
                data: f
                    .data
                    .iter()
                    .enumerate()
                    .map(|(i, &v)| v * colour[BayerPattern::Rggb.channel_at(i % W, i / W)])
                    .collect(),
                ..f.clone()
            })
            .collect();
        let built =
            solve(&mosaic, None, &keep(), &quiet(), &|_| {}).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(built.report.planes, 4);
        assert_eq!(built.master.frame.bayer, Some(BayerPattern::Rggb));
        let e = error(&built.master.frame.data, &g);
        eprintln!("mosaic: gain error {:.3}%", e * 100.0);
        assert!(e < 0.02, "{e}");
        // The flat fixes the shape of the field per colour and leaves the balance alone.
        for c in 0..3 {
            let v: Vec<f32> = built
                .master
                .frame
                .data
                .iter()
                .enumerate()
                .filter(|(i, _)| BayerPattern::Rggb.channel_at(i % W, i / W) == c)
                .map(|(_, &v)| v)
                .collect();
            let mean = v.iter().sum::<f32>() / v.len() as f32;
            assert!((mean - 1.0).abs() < 1e-3, "colour {c}: {mean}");
        }
    }

    #[test]
    fn too_few_frames_or_no_sun_are_refused() {
        let g = gain(5, 0.25);
        let (frames, _) = capture(4, &g, 60.0, 0.002, 9);
        let few = solve(
            &frames,
            None,
            &SelfFlatOptions::default(),
            &quiet(),
            &|_| {},
        );
        assert!(matches!(few, Err(Error::Invalid(m)) if m.contains("at least")));
        let dark: Vec<Frame> = (0..8).map(|_| Frame::zeros(W, H)).collect();
        let none = solve(&dark, None, &SelfFlatOptions::default(), &quiet(), &|_| {});
        assert!(matches!(none, Err(Error::Invalid(m)) if m.contains("limb")));
        let cancel = AtomicBool::new(true);
        let (frames, _) = capture(8, &g, 60.0, 0.002, 9);
        assert!(matches!(
            solve(&frames, None, &SelfFlatOptions::default(), &cancel, &|_| {}),
            Err(Error::Cancelled)
        ));
    }

    #[test]
    fn the_dark_is_taken_off_before_the_solve() {
        use crate::calibrate::master::{Combine, build_dark};
        let g = gain(5, 0.25);
        let (mut frames, _) = capture(15, &g, 90.0, 0.002, 9);
        let offset = 0.15f32;
        for f in &mut frames {
            f.data.iter_mut().for_each(|v| *v += offset);
        }
        let dark = {
            let mut input = CalInput::from_frames(
                (0..8)
                    .map(|_| {
                        Frame::new(W, H, vec![offset; W * H]).unwrap_or_else(|e| panic!("{e}"))
                    })
                    .collect(),
                None,
                "dark",
            )
            .unwrap_or_else(|e| panic!("{e}"));
            build_dark(
                MasterKind::Dark,
                &mut input,
                &BuildOptions {
                    combine: Combine::Median,
                    ..BuildOptions::default()
                },
                &|_| {},
                &quiet(),
            )
            .unwrap_or_else(|e| panic!("{e}"))
        };
        let with = |d: Option<&Master>| {
            let mut input = CalInput::from_frames(frames.clone(), None, "displaced")
                .unwrap_or_else(|e| panic!("{e}"));
            build_selfflat(
                &mut input,
                d,
                &BuildOptions::default(),
                &keep(),
                &|_| {},
                &quiet(),
            )
            .unwrap_or_else(|e| panic!("{e}"))
        };
        let (good, bad) = (with(Some(&dark)), with(None));
        let (eg, eb) = (
            error(&good.master.frame.data, &g),
            error(&bad.master.frame.data, &g),
        );
        eprintln!(
            "with the dark {:.3}%, without {:.3}%",
            eg * 100.0,
            eb * 100.0
        );
        assert!(eg < 0.01 && eb > eg, "{eg} {eb}");
        assert!(bad.warnings.iter().any(|w| w.contains("no dark")));
        assert_eq!(good.master.info.kind, MasterKind::Flat);
        assert_eq!(good.master.info.frames_used, 15);
    }
}
