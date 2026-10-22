// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! A time-lapse from a series of stacked images of the Sun.
//!
//! Each frame is registered to a reference frame in two steps. The limb fit
//! ([`crate::align::limb`]) gives the shift of the disc centre to a tenth of a
//! pixel and does not care what is on the disc. A phase correlation of the
//! middle of the disc, after that shift, then removes whatever the limb cannot
//! tell (an edge distorted by seeing, a prominence pulling the circle): it
//! follows the granulation and the filaments themselves. The correlation is
//! only trusted when its peak is sharp and the correction it asks for is
//! small; otherwise the limb shift stands. A frame with no usable limb is
//! registered by phase correlation of the whole frame.
//!
//! The brightness can be made even too: each frame's mean over the inner disc
//! is brought to one level (`Match`) or to a running median of its neighbours
//! (`Smooth`), which takes out thin cloud and exposure flicker and keeps a slow
//! trend.

#![allow(
    clippy::many_single_char_names,
    reason = "x, y, w, h and r are the natural names in image maths"
)]

use std::sync::atomic::{AtomicBool, Ordering};

use rayon::prelude::*;

use crate::align::Shift;
use crate::align::global::{PhaseCorrelator, PhaseOptions};
use crate::align::limb::LimbReference;
use crate::derotate::{DiscGeometry, Orientation, RotationLaw, Spec, derotate};
use crate::error::{Error, Result};
use crate::frame::{Frame, Rect};
use crate::picture::{self, Picture};
use crate::resample::{Kernel, shifted};

/// Peak-to-sidelobe ratio from which a phase correlation of the disc centre is
/// trusted.
pub const MIN_PSR: f32 = 6.0;
/// Peak-to-sidelobe ratio from which a whole-frame phase correlation, the
/// fallback without a limb, is trusted.
pub const MIN_PSR_WHOLE: f32 = 4.0;
/// Half the side of the disc-centre patch, as a share of the radius.
const PATCH: f32 = 0.7;
/// Radius of the disc over which the brightness is measured, as a share of the
/// radius.
const LEVEL_RADIUS: f32 = 0.8;

/// Which frame the others are registered to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Reference {
    /// The first frame.
    First,
    /// The middle frame (the least shift, so the least cropping).
    #[default]
    Middle,
    /// A frame by position, counting from 0.
    Index(usize),
}

impl std::str::FromStr for Reference {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "first" => Ok(Self::First),
            "middle" | "mid" => Ok(Self::Middle),
            n => n.parse().map(Self::Index).map_err(|_| {
                Error::Invalid(format!(
                    "unknown reference {s:?}; use first, middle or a frame number"
                ))
            }),
        }
    }
}

/// How the brightness of the frames is evened out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Deflicker {
    /// Leave the brightness alone.
    #[default]
    Off,
    /// Bring every frame to the median brightness of the series.
    Match,
    /// Bring every frame to the median of the `n` frames around it (odd, at
    /// least 3): flicker goes, a slow change stays.
    Smooth(usize),
}

/// Settings of [`register`].
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// The frame the others are registered to.
    pub reference: Reference,
    /// Refine the limb shift by phase correlation of the disc centre.
    pub refine: bool,
    /// Largest correction the phase correlation may make to the limb shift,
    /// pixels. A larger one is taken to be a false lock and ignored.
    pub max_refine_px: f32,
    /// Even out the brightness.
    pub deflicker: Deflicker,
    /// Cut the frames to the area that every frame covers.
    pub crop: bool,
    /// Interpolation of the moved frames.
    pub kernel: Kernel,
    /// Bring every frame to one time by the Sun's differential rotation; needs
    /// the time of every frame.
    pub derotate: Option<Derotation>,
}

/// The time every frame is brought to.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum DerotateTarget {
    /// The time of the reference frame, which stays as it is.
    #[default]
    Reference,
    /// A time, unix seconds UTC.
    Unix(f64),
}

/// Settings of the derotation of a series (see [`crate::derotate`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Derotation {
    /// The rotation law.
    pub law: RotationLaw,
    /// How the images sit against the sky.
    pub orient: Orientation,
    /// The time to bring every frame to.
    pub target: DerotateTarget,
    /// Carry the contrast against the limb darkening with each feature.
    pub preserve_contrast: bool,
}

impl Default for Derotation {
    fn default() -> Self {
        Self {
            law: RotationLaw::Howard,
            orient: Orientation::default(),
            target: DerotateTarget::Reference,
            preserve_contrast: true,
        }
    }
}

impl Default for Options {
    fn default() -> Self {
        Self {
            reference: Reference::default(),
            refine: true,
            max_refine_px: 3.0,
            deflicker: Deflicker::Off,
            crop: false,
            kernel: Kernel::Lanczos3,
            derotate: None,
        }
    }
}

/// How a frame was registered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// This is the reference frame.
    Reference,
    /// By the limb fit alone.
    Limb,
    /// By the limb fit, refined by phase correlation of the disc centre.
    LimbPhase,
    /// By phase correlation of the whole frame (no usable limb).
    Phase,
    /// No registration was possible; the frame is unmoved.
    Failed,
}

impl Method {
    /// Short name for tables.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Reference => "reference",
            Self::Limb => "limb",
            Self::LimbPhase => "limb+phase",
            Self::Phase => "phase",
            Self::Failed => "failed",
        }
    }
}

/// What was measured and done for one frame.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameReport {
    /// Position in the series, from 0.
    pub index: usize,
    /// Capture time, unix seconds UTC, when known.
    pub time: Option<f64>,
    /// Shift applied (see [`crate::resample`]): the frame's content was at
    /// the reference plus this.
    pub shift: Shift,
    /// How the shift was found.
    pub method: Method,
    /// Disc centre and radius of the frame's own limb fit, and how far to
    /// trust it (0 to 1).
    pub limb: Option<(f64, f64, f64, f32)>,
    /// Correction made by the phase correlation, and its peak-to-sidelobe ratio.
    pub phase: Option<(Shift, f32)>,
    /// Mean brightness of the inner disc before the brightness was evened out.
    pub level: f32,
    /// Gain applied for the brightness (1 when off).
    pub gain: f32,
    /// How far the derotation moved the surface at most, pixels, when it ran.
    pub derotated_px: Option<f32>,
}

/// A registered series.
#[derive(Clone, Debug)]
pub struct Sequence {
    /// The moved (and brightness-matched, and cropped) frames, in order.
    pub frames: Vec<Picture>,
    /// One report per frame.
    pub report: Vec<FrameReport>,
    /// The reference frame's position.
    pub reference: usize,
    /// The area of the original frames that is kept, when cropped.
    pub crop: Option<Rect>,
    /// The disc of the reference frame, in the coordinates of `frames`: centre
    /// and radius, when it has a limb.
    pub disc: Option<(f64, f64, f64)>,
    /// The time every frame was brought to by the derotation, unix seconds.
    pub derotated_to: Option<f64>,
}

impl Sequence {
    /// Frames that could not be registered.
    #[must_use]
    pub fn failed(&self) -> Vec<usize> {
        self.report
            .iter()
            .filter(|r| r.method == Method::Failed)
            .map(|r| r.index)
            .collect()
    }
}

fn reference_index(r: Reference, n: usize) -> Result<usize> {
    let i = match r {
        Reference::First => 0,
        Reference::Middle => n / 2,
        Reference::Index(i) => i,
    };
    if i >= n {
        return Err(Error::Invalid(format!(
            "reference frame {i} is not in a series of {n}"
        )));
    }
    Ok(i)
}

/// The square patch of the disc centre, in pixels, clipped to the frame.
fn patch(cx: f64, cy: f64, r: f64, w: usize, h: usize) -> Rect {
    let half = (f64::from(PATCH) * r).max(32.0);
    let x0 = (cx - half).floor().max(0.0) as usize;
    let y0 = (cy - half).floor().max(0.0) as usize;
    let x1 = ((cx + half).ceil() as usize).min(w);
    let y1 = ((cy + half).ceil() as usize).min(h);
    Rect { x0, y0, x1, y1 }
}

fn cut(f: &Frame, r: Rect) -> Frame {
    let mut data = Vec::with_capacity(r.area());
    for y in r.y0..r.y1 {
        data.extend_from_slice(&f.row(y)[r.x0..r.x1]);
    }
    Frame {
        width: r.width(),
        height: r.height(),
        data,
        bayer: None,
    }
}

fn median(v: &mut [f32]) -> f32 {
    v.sort_by(f32::total_cmp);
    let n = v.len();
    if n == 0 {
        1.0
    } else if n % 2 == 1 {
        v[n / 2]
    } else {
        f32::midpoint(v[n / 2 - 1], v[n / 2])
    }
}

/// The gain each frame needs for the brightness `levels` to be even.
#[must_use]
pub fn deflicker_gains(levels: &[f32], mode: Deflicker) -> Vec<f32> {
    let n = levels.len();
    let target: Vec<f32> = match mode {
        Deflicker::Off => return vec![1.0; n],
        Deflicker::Match => {
            let m = median(&mut levels.to_vec());
            vec![m; n]
        }
        Deflicker::Smooth(window) => {
            let half = window.max(3) / 2;
            (0..n)
                .map(|i| {
                    let lo = i.saturating_sub(half);
                    let hi = (i + half + 1).min(n);
                    median(&mut levels[lo..hi].to_vec())
                })
                .collect()
        }
    };
    levels
        .iter()
        .zip(&target)
        .map(|(&l, &t)| if l > 1e-6 && t > 1e-6 { t / l } else { 1.0 })
        .collect()
}

/// The area that every frame, moved by its shift, still has data for.
#[must_use]
pub fn common_area(shifts: &[Shift], w: usize, h: usize) -> Option<Rect> {
    // The result at x reads the source at x + dx, which must lie inside the
    // frame with a pixel of margin for the interpolation.
    let m = 2.0f32;
    let (mut x0, mut y0, mut x1, mut y1) = (0.0f32, 0.0f32, w as f32, h as f32);
    for s in shifts {
        x0 = x0.max(m - s.dx);
        y0 = y0.max(m - s.dy);
        x1 = x1.min(w as f32 - 1.0 - m - s.dx);
        y1 = y1.min(h as f32 - 1.0 - m - s.dy);
    }
    let r = Rect {
        x0: x0.ceil().max(0.0) as usize,
        y0: y0.ceil().max(0.0) as usize,
        x1: (x1.floor().max(0.0) as usize + 1).min(w),
        y1: (y1.floor().max(0.0) as usize + 1).min(h),
    };
    (r.width() >= 16 && r.height() >= 16).then_some(r)
}

/// Register a series to one of its frames, and even out its brightness.
///
/// `times` are the capture times (any length; missing ones are `None`).
/// `progress` is told how far the work has got, 0 to 1. The cancel flag is
/// checked between frames.
///
/// # Errors
/// An empty series, frames of unequal size, a reference outside the series, or
/// cancellation.
#[allow(
    clippy::too_many_lines,
    reason = "one sequence: measure, move, even out the brightness, crop"
)]
pub fn register(
    inputs: &[Picture],
    times: &[Option<f64>],
    opts: &Options,
    progress: &(dyn Fn(f32) + Sync),
    cancel: &AtomicBool,
) -> Result<Sequence> {
    let n = inputs.len();
    if n == 0 {
        return Err(Error::Invalid(
            "a time-lapse needs at least one frame".into(),
        ));
    }
    let (w, h) = inputs[0].size();
    if inputs.iter().any(|p| p.size() != (w, h)) {
        return Err(Error::Invalid(
            "the frames differ in size; stack every capture with the same crop and resize".into(),
        ));
    }
    if let Deflicker::Smooth(win) = opts.deflicker
        && win < 3
    {
        return Err(Error::Invalid(
            "the smoothing window must be at least 3 frames".into(),
        ));
    }
    let r = reference_index(opts.reference, n)?;
    let lumas: Vec<Frame> = inputs.iter().map(picture::luma).collect();
    let limb_ref = LimbReference::new(&lumas[r]);
    let disc = limb_ref.as_ref().map(|l| (l.fit.cx, l.fit.cy, l.fit.r));
    // Phase correlation of the disc centre, or of the whole frame without a limb.
    let (rect, whole) = match disc {
        Some((cx, cy, rad)) => (patch(cx, cy, rad, w, h), false),
        None => (Rect::new(0, 0, w, h), true),
    };
    // Derotation: one spec for each frame, from its time to the target.
    let derot: Option<(DiscGeometry, Vec<Spec>)> = match (&opts.derotate, disc) {
        (None, _) => None,
        (Some(_), None) => {
            return Err(Error::Invalid(
                "derotation needs a clear solar limb in the reference frame".into(),
            ));
        }
        (Some(d), Some((cx, cy, rad))) => {
            let t: Vec<f64> = (0..n)
                .map(|i| times.get(i).copied().flatten())
                .collect::<Option<_>>()
                .ok_or_else(|| {
                    Error::Invalid(
                        "derotation needs the time of every frame (see --date for names that hold only a time)"
                            .into(),
                    )
                })?;
            let target = match d.target {
                DerotateTarget::Reference => t[r],
                DerotateTarget::Unix(u) => u,
            };
            let specs = t
                .iter()
                .map(|&ti| Spec {
                    law: d.law,
                    orient: d.orient,
                    from_unix: ti,
                    to_unix: target,
                    preserve_contrast: d.preserve_contrast,
                    kernel: opts.kernel,
                })
                .collect();
            Some((DiscGeometry { cx, cy, r: rad }, specs))
        }
    };
    let ref_luma = match &derot {
        Some((g, specs)) if !specs[r].is_identity() => {
            match derotate(&Picture::Mono(lumas[r].clone()), *g, &specs[r]) {
                Ok((Picture::Mono(f), _)) => f,
                _ => lumas[r].clone(),
            }
        }
        _ => lumas[r].clone(),
    };
    let correlator = PhaseCorrelator::new(&cut(&ref_luma, rect), PhaseOptions::default())?;
    let done = std::sync::atomic::AtomicUsize::new(0);

    let measured: Vec<Result<Measured>> = (0..n)
        .into_par_iter()
        .map(|i| {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let out = if i == r {
                let limb = disc.map(|(cx, cy, rad)| (cx, cy, rad, 1.0));
                (Shift::default(), Method::Reference, limb, None)
            } else {
                let dr = derot.as_ref().map(|(g, specs)| (g, &specs[i]));
                measure_one(
                    &lumas[i],
                    limb_ref.as_ref(),
                    &correlator,
                    rect,
                    whole,
                    opts,
                    dr,
                )
            };
            let d = done.fetch_add(1, Ordering::Relaxed) + 1;
            progress(0.6 * d as f32 / n as f32);
            Ok(out)
        })
        .collect();
    let mut shifts = Vec::with_capacity(n);
    let mut methods = Vec::with_capacity(n);
    let mut limbs = Vec::with_capacity(n);
    let mut phases = Vec::with_capacity(n);
    for m in measured {
        let (s, method, limb, phase) = m?;
        shifts.push(s);
        methods.push(method);
        limbs.push(limb);
        phases.push(phase);
    }

    // Move the frames, then bring them to one time.
    let moved_all: Vec<(Picture, Option<f32>)> = (0..n)
        .into_par_iter()
        .map(|i| {
            let p = if methods[i] == Method::Reference || methods[i] == Method::Failed {
                inputs[i].clone()
            } else {
                picture::shift(&inputs[i], shifts[i], opts.kernel)
            };
            let out = match &derot {
                Some((g, specs)) if methods[i] != Method::Failed => derotate(&p, *g, &specs[i])
                    .map_or((p, None), |(q, rep)| (q, Some(rep.max_shift_px))),
                _ => (p, None),
            };
            let d = done.fetch_add(1, Ordering::Relaxed) + 1;
            progress((0.6 + 0.3 * (d - n) as f32 / n as f32).min(0.9));
            out
        })
        .collect();
    let (moved, derotated): (Vec<Picture>, Vec<Option<f32>>) = moved_all.into_iter().unzip();

    // Brightness over the inner disc of the reference, in the moved frames.
    let levels: Vec<f32> = moved
        .iter()
        .map(|p| {
            let f = picture::luma(p);
            disc.and_then(|(cx, cy, rad)| {
                picture::mean_in_disc(&f, cx as f32, cy as f32, LEVEL_RADIUS * rad as f32)
            })
            .unwrap_or_else(|| f.mean())
        })
        .collect();
    let gains = deflicker_gains(&levels, opts.deflicker);
    let mut frames: Vec<Picture> = moved
        .into_iter()
        .zip(&gains)
        .map(|(p, &g)| {
            if (g - 1.0).abs() > f32::EPSILON {
                picture::gain_offset(&p, g, 0.0)
            } else {
                p
            }
        })
        .collect();

    let crop = if opts.crop {
        let used: Vec<Shift> = (0..n)
            .filter(|&i| methods[i] != Method::Failed)
            .map(|i| shifts[i])
            .collect();
        let rect = common_area(&used, w, h);
        if let Some(rc) = rect {
            frames = frames
                .iter()
                .map(|p| picture::crop(p, rc))
                .collect::<Result<_>>()?;
        }
        rect
    } else {
        None
    };
    progress(1.0);
    let report = (0..n)
        .map(|i| FrameReport {
            index: i,
            time: times.get(i).copied().flatten(),
            shift: shifts[i],
            method: methods[i],
            limb: limbs[i],
            phase: phases[i],
            level: levels[i],
            gain: gains[i],
            derotated_px: derotated[i],
        })
        .collect();
    let disc = disc.map(|(cx, cy, rad)| match crop {
        Some(c) => (cx - c.x0 as f64, cy - c.y0 as f64, rad),
        None => (cx, cy, rad),
    });
    Ok(Sequence {
        frames,
        report,
        reference: r,
        crop,
        disc,
        derotated_to: derot.as_ref().map(|(_, specs)| specs[r].to_unix),
    })
}

type Measured = (
    Shift,
    Method,
    Option<(f64, f64, f64, f32)>,
    Option<(Shift, f32)>,
);

#[allow(
    clippy::too_many_arguments,
    reason = "everything one frame is measured against"
)]
fn measure_one(
    luma: &Frame,
    limb_ref: Option<&LimbReference>,
    correlator: &PhaseCorrelator,
    rect: Rect,
    whole: bool,
    opts: &Options,
    derot: Option<(&DiscGeometry, &Spec)>,
) -> Measured {
    if whole {
        // No limb in the reference: correlate the whole frame.
        return match correlator.estimate(luma) {
            Ok(e) if e.psr >= MIN_PSR_WHOLE => {
                (e.shift, Method::Phase, None, Some((e.shift, e.psr)))
            }
            _ => (Shift::default(), Method::Failed, None, None),
        };
    }
    let limb = limb_ref.and_then(|l| l.estimate(luma));
    let Some((s_limb, fit)) = limb else {
        // The reference has a limb but this frame does not: nothing to anchor on.
        return (Shift::default(), Method::Failed, None, None);
    };
    let info = Some((fit.cx, fit.cy, fit.r, fit.confidence));
    if !opts.refine {
        return (s_limb, Method::Limb, info, None);
    }
    let mut aligned = shifted(luma, s_limb, Kernel::Lanczos3);
    // The surface has turned since the reference: undo it first, so that the
    // correlation tracks the pointing and not the rotation.
    if let Some((g, spec)) = derot
        && let Ok((Picture::Mono(f), _)) = derotate(&Picture::Mono(aligned.clone()), *g, spec)
    {
        aligned = f;
    }
    match correlator.estimate(&cut(&aligned, rect)) {
        Ok(e) if e.psr >= MIN_PSR && e.shift.length() <= opts.max_refine_px => {
            let total = Shift::new(s_limb.dx + e.shift.dx, s_limb.dy + e.shift.dy);
            (total, Method::LimbPhase, info, Some((e.shift, e.psr)))
        }
        Ok(e) => (s_limb, Method::Limb, info, Some((e.shift, e.psr))),
        Err(_) => (s_limb, Method::Limb, info, None),
    }
}

/// Format of the numbered frames.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FrameFormat {
    /// 8-bit PNG, for sharing.
    #[default]
    Png,
    /// 16-bit TIFF, for further work.
    Tiff,
}

impl FrameFormat {
    /// File extension.
    #[must_use]
    pub fn ext(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Tiff => "tif",
        }
    }
}

impl std::str::FromStr for FrameFormat {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "png" => Ok(Self::Png),
            "tif" | "tiff" => Ok(Self::Tiff),
            other => Err(Error::Invalid(format!(
                "unknown frame format {other:?}; use png or tiff"
            ))),
        }
    }
}

/// One line of notes recorded in a written frame.
#[must_use]
pub fn frame_note(r: &FrameReport) -> String {
    let when = r.time.map_or(String::new(), |t| {
        format!(" at {} UTC", crate::io::solarnet::iso_from_unix(t))
    });
    format!(
        "Helia time-lapse frame {}{when}: registered by {}, shift ({:.2}, {:.2}) px, gain {:.3}",
        r.index + 1,
        r.method.label(),
        r.shift.dx,
        r.shift.dy,
        r.gain
    )
}

/// Write the frames of `seq` into `dir` as `frame-0001.png`, `frame-0002.png`
/// and so on. Returns the paths.
///
/// # Errors
/// I/O or encoding failures.
pub fn write_frames(
    dir: &std::path::Path,
    seq: &Sequence,
    format: FrameFormat,
) -> Result<Vec<std::path::PathBuf>> {
    std::fs::create_dir_all(dir)?;
    seq.frames
        .iter()
        .zip(&seq.report)
        .map(|(p, r)| {
            let path = dir.join(frame_name(r.index, format.ext()));
            let notes = crate::io::ImageNotes::line(frame_note(r));
            picture::save(&path, p, &notes)?;
            Ok(path)
        })
        .collect()
}

/// The registration as CSV: one row per frame, with the file it was written to.
#[must_use]
pub fn report_csv(seq: &Sequence, files: &[String], sources: &[String]) -> String {
    use std::fmt::Write as _;
    let mut s = String::from(
        "frame,file,source,time_utc,shift_x_px,shift_y_px,method,limb_x,limb_y,limb_radius,limb_confidence,phase_dx,phase_dy,phase_psr,level,gain,derotated_px
",
    );
    for r in &seq.report {
        let i = r.index;
        let num = |v: Option<f64>| v.map_or(String::new(), |x| format!("{x:.3}"));
        let (lx, ly, lr, lc) = r.limb.map_or((None, None, None, None), |l| {
            (Some(l.0), Some(l.1), Some(l.2), Some(f64::from(l.3)))
        });
        let (pdx, pdy, ppsr) = r.phase.map_or((None, None, None), |(d, p)| {
            (
                Some(f64::from(d.dx)),
                Some(f64::from(d.dy)),
                Some(f64::from(p)),
            )
        });
        let _ = writeln!(
            s,
            "{},{},{},{},{:.3},{:.3},{},{},{},{},{},{},{},{},{:.5},{:.4},{}",
            i + 1,
            files.get(i).map_or("", String::as_str),
            sources.get(i).map_or("", String::as_str),
            r.time
                .map_or(String::new(), crate::io::solarnet::iso_from_unix),
            r.shift.dx,
            r.shift.dy,
            r.method.label(),
            num(lx),
            num(ly),
            num(lr),
            num(lc),
            num(pdx),
            num(pdy),
            num(ppsr),
            r.level,
            r.gain,
            num(r.derotated_px.map(f64::from))
        );
    }
    s
}

/// A few lines saying what registration did, for the terminal and the logs.
#[must_use]
pub fn summary_lines(seq: &Sequence) -> Vec<String> {
    let n = seq.report.len();
    let count = |m: Method| seq.report.iter().filter(|r| r.method == m).count();
    let max = seq
        .report
        .iter()
        .map(|r| r.shift.length())
        .fold(0.0f32, f32::max);
    let mut v = vec![format!(
        "{n} frames registered to frame {} ({} limb+phase, {} limb, {} phase, {} failed); largest shift {max:.1} px",
        seq.reference + 1,
        count(Method::LimbPhase),
        count(Method::Limb),
        count(Method::Phase),
        count(Method::Failed)
    )];
    if let Some(c) = seq.crop {
        v.push(format!(
            "cropped to the common area: {} x {} at ({}, {})",
            c.width(),
            c.height(),
            c.x0,
            c.y0
        ));
    }
    if let Some(t) = seq.derotated_to {
        let most = seq
            .report
            .iter()
            .filter_map(|r| r.derotated_px)
            .fold(0.0f32, f32::max);
        v.push(format!(
            "derotated to {} UTC by the Sun's differential rotation; the surface moved by up to {most:.1} px",
            crate::io::solarnet::iso_from_unix(t).replace('T', " ")[..19].to_owned()
        ));
    }
    let gains: Vec<f32> = seq.report.iter().map(|r| r.gain).collect();
    let (lo, hi) = gains
        .iter()
        .fold((f32::MAX, f32::MIN), |(l, h), &g| (l.min(g), h.max(g)));
    if (hi - lo).abs() > 1e-6 || (hi - 1.0).abs() > 1e-6 {
        v.push(format!("brightness gains {lo:.3} to {hi:.3}"));
    }
    v
}

/// Where and how a series is written.
#[derive(Clone, Debug)]
pub struct Output {
    /// Directory for the numbered frames, the movie and the table.
    pub dir: std::path::PathBuf,
    /// Format of the numbered frames.
    pub format: FrameFormat,
    /// The movie to make.
    pub movie: crate::animate::MovieChoice,
    /// Frames a second of the movie.
    pub fps: f32,
}

/// What [`run`] made.
#[derive(Clone, Debug)]
pub struct Outcome {
    /// The registered series.
    pub sequence: Sequence,
    /// The numbered frames written.
    pub files: Vec<std::path::PathBuf>,
    /// The movie, when one was made.
    pub movie: Option<crate::animate::Movie>,
    /// The registration table (`timelapse.csv`).
    pub table: std::path::PathBuf,
}

/// Read stacked images, register them, and write the numbered frames, the
/// table and the movie into `out.dir`. `progress` runs from 0 to 1 over the
/// whole job.
///
/// # Errors
/// A file that cannot be read, anything [`register`] refuses, or I/O failures.
pub fn run(
    inputs: &[std::path::PathBuf],
    times: &[Option<f64>],
    opts: &Options,
    out: &Output,
    progress: &(dyn Fn(f32) + Sync),
    cancel: &AtomicBool,
) -> Result<Outcome> {
    let pictures: Vec<Picture> = inputs
        .iter()
        .map(|p| {
            crate::io::any::read_any(p)
                .map(|(l, _)| l)
                .map_err(|e| Error::Invalid(format!("cannot read {}: {e}", p.display())))
        })
        .collect::<Result<_>>()?;
    progress(0.05);
    let seq = register(
        &pictures,
        times,
        opts,
        &|f| progress(0.05 + 0.65 * f),
        cancel,
    )?;
    let sources: Vec<String> = inputs
        .iter()
        .map(|p| {
            p.file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
        })
        .collect();
    let (files, movie, table) = write_all(&seq, &sources, out)?;
    progress(1.0);
    Ok(Outcome {
        sequence: seq,
        files,
        movie,
        table,
    })
}

/// Write a registered series: the numbered frames, the table and the movie.
/// `sources` name the input of each frame, for the table.
///
/// # Errors
/// I/O or encoding failures, or an MP4 asked for without `ffmpeg`.
pub fn write_all(
    seq: &Sequence,
    sources: &[String],
    out: &Output,
) -> Result<(
    Vec<std::path::PathBuf>,
    Option<crate::animate::Movie>,
    std::path::PathBuf,
)> {
    let files = write_frames(&out.dir, seq, out.format)?;
    let written: Vec<String> = files
        .iter()
        .map(|p| {
            p.file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
        })
        .collect();
    let table = out.dir.join("timelapse.csv");
    std::fs::write(&table, report_csv(seq, &written, sources))?;
    let movie = crate::animate::make_movie(&out.dir, "timelapse", &seq.frames, out.fps, out.movie)?;
    Ok((files, movie, table))
}

/// The order that puts frames in time order: the indices sorted by time when
/// every frame has one, else the order as given.
#[must_use]
pub fn order_by_time(times: &[Option<f64>]) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..times.len()).collect();
    if times.iter().all(Option::is_some) {
        idx.sort_by(|&a, &b| {
            let (ta, tb) = (times[a].unwrap_or(0.0), times[b].unwrap_or(0.0));
            ta.total_cmp(&tb)
        });
    }
    idx
}

/// File name of frame `index` (counting from 0) of a numbered sequence:
/// `frame-0001.png` for the first.
#[must_use]
pub fn frame_name(index: usize, ext: &str) -> String {
    format!("frame-{:04}.{ext}", index + 1)
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation, reason = "test data generation")]
mod tests {
    use super::*;
    use crate::testutil::{SyntheticConfig, SyntheticSun};

    fn sun() -> SyntheticSun {
        SyntheticSun::new(&SyntheticConfig {
            width: 256,
            height: 224,
            radius_frac: 0.36,
            ..SyntheticConfig::default()
        })
    }

    fn series(shifts: &[(f32, f32)], gains: &[f32]) -> Vec<Picture> {
        let s = sun();
        shifts
            .iter()
            .zip(gains)
            .map(|(&(dx, dy), &g)| {
                let f = s.render_clean(Shift::new(dx, dy), None);
                picture::gain_offset(&Picture::Mono(f), g, 0.0)
            })
            .collect()
    }

    fn run(p: &[Picture], opts: &Options) -> Sequence {
        register(p, &[], opts, &|_| {}, &AtomicBool::new(false)).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn known_shifts_are_recovered_to_a_tenth_of_a_pixel() {
        let truth = [(0.0, 0.0), (3.4, -2.2), (-5.7, 4.1), (1.3, 6.6)];
        let p = series(&truth, &[1.0; 4]);
        let seq = run(
            &p,
            &Options {
                reference: Reference::First,
                ..Options::default()
            },
        );
        for (r, &(dx, dy)) in seq.report.iter().zip(&truth) {
            assert!(
                (r.shift.dx - dx).abs() < 0.1 && (r.shift.dy - dy).abs() < 0.1,
                "frame {} shift {:?} want ({dx}, {dy}) by {}",
                r.index,
                r.shift,
                r.method.label()
            );
        }
        assert_eq!(seq.report[0].method, Method::Reference);
        assert!(
            seq.report[1..]
                .iter()
                .all(|r| r.method == Method::LimbPhase)
        );
    }

    #[test]
    fn registered_frames_agree_with_the_reference() {
        let truth = [(0.0, 0.0), (4.5, -3.3), (-2.8, 5.1)];
        let p = series(&truth, &[1.0; 3]);
        let seq = run(
            &p,
            &Options {
                reference: Reference::First,
                ..Options::default()
            },
        );
        let a = picture::luma(&seq.frames[0]);
        for f in &seq.frames[1..] {
            let b = picture::luma(f);
            let m = 24;
            let mut e = 0.0f64;
            let mut n = 0.0f64;
            for y in m..a.height - m {
                for x in m..a.width - m {
                    e += f64::from(a.get(x, y) - b.get(x, y)).powi(2);
                    n += 1.0;
                }
            }
            assert!((e / n).sqrt() < 0.01, "rms {}", (e / n).sqrt());
        }
    }

    #[test]
    fn the_phase_refinement_can_be_switched_off() {
        let p = series(&[(0.0, 0.0), (2.0, 1.0)], &[1.0; 2]);
        let seq = run(
            &p,
            &Options {
                reference: Reference::First,
                refine: false,
                ..Options::default()
            },
        );
        assert_eq!(seq.report[1].method, Method::Limb);
    }

    #[test]
    fn flicker_is_taken_out() {
        let g = [1.0, 0.8, 1.2, 0.9, 1.1];
        let p = series(&[(0.0, 0.0); 5], &g);
        let seq = run(
            &p,
            &Options {
                deflicker: Deflicker::Match,
                ..Options::default()
            },
        );
        let lv: Vec<f32> = seq
            .frames
            .iter()
            .map(|f| {
                let l = picture::luma(f);
                let (cx, cy, r) = seq.disc.unwrap_or_else(|| panic!("disc"));
                picture::mean_in_disc(&l, cx as f32, cy as f32, 0.8 * r as f32)
                    .unwrap_or_else(|| panic!("level"))
            })
            .collect();
        let spread = lv.iter().copied().fold(f32::MIN, f32::max)
            / lv.iter().copied().fold(f32::MAX, f32::min);
        assert!(spread < 1.005, "spread {spread}");
    }

    #[test]
    fn smoothing_keeps_a_slow_trend() {
        let levels = [1.0, 1.02, 1.04, 1.30, 1.08, 1.10, 1.12];
        let gains = deflicker_gains(&levels, Deflicker::Smooth(3));
        let out: Vec<f32> = levels.iter().zip(&gains).map(|(l, g)| l * g).collect();
        // The spike is pulled down; the ramp is still a ramp.
        assert!((out[3] - 1.06).abs() < 0.03, "{}", out[3]);
        assert!(out[6] > out[0] + 0.08);
    }

    #[test]
    fn the_common_area_excludes_every_edge_that_moved() {
        let s = [
            Shift::new(0.0, 0.0),
            Shift::new(10.0, -6.0),
            Shift::new(-4.0, 3.0),
        ];
        let r = common_area(&s, 200, 100).unwrap_or_else(|| panic!("area"));
        // Frame two reads x + 10 and y - 6.
        assert!(r.x1 <= 200 - 10 - 2 && r.x0 >= 4 + 2);
        assert!(r.y0 >= 6 + 2 && r.y1 <= 100 - 3 - 2);
    }

    #[test]
    fn a_crop_makes_every_frame_the_same_smaller_size() {
        let p = series(&[(0.0, 0.0), (6.0, -4.0), (-5.0, 3.0)], &[1.0; 3]);
        let seq = run(
            &p,
            &Options {
                crop: true,
                ..Options::default()
            },
        );
        let rect = seq.crop.unwrap_or_else(|| panic!("crop"));
        assert!(rect.width() < 256 && rect.height() < 224);
        assert!(
            seq.frames
                .iter()
                .all(|f| f.size() == (rect.width(), rect.height()))
        );
    }

    #[test]
    fn mismatched_frames_are_refused() {
        let mut p = series(&[(0.0, 0.0)], &[1.0]);
        p.push(Picture::Mono(Frame::zeros(10, 10)));
        assert!(
            register(
                &p,
                &[],
                &Options::default(),
                &|_| {},
                &AtomicBool::new(false)
            )
            .is_err()
        );
    }

    #[test]
    fn a_frame_without_a_disc_is_reported_not_guessed() {
        let mut p = series(&[(0.0, 0.0), (2.0, 2.0)], &[1.0; 2]);
        p.push(Picture::Mono(Frame::zeros(256, 224)));
        let seq = run(
            &p,
            &Options {
                reference: Reference::First,
                ..Options::default()
            },
        );
        assert_eq!(seq.failed(), vec![2]);
    }

    #[test]
    fn frames_are_ordered_by_time_only_when_all_have_one() {
        assert_eq!(
            order_by_time(&[Some(5.0), Some(1.0), Some(3.0)]),
            vec![1, 2, 0]
        );
        assert_eq!(order_by_time(&[Some(5.0), None, Some(3.0)]), vec![0, 1, 2]);
    }

    #[test]
    fn derotation_brings_the_surface_to_one_time_and_leaves_the_pointing_alone() {
        use crate::derotate::synth::render_sun;
        use crate::io::solarnet::unix_from_iso;
        let disc = DiscGeometry {
            cx: 320.0,
            cy: 260.0,
            r: 240.0,
        };
        let feats = [
            (0.0, -5.0),
            (2.0, -30.0),
            (35.0, -12.0),
            (-35.0, -8.0),
            (20.0, -35.0),
            (-20.0, 10.0),
            (50.0, -18.0),
        ];
        let t0 = unix_from_iso("2026-10-20T09:00:00").unwrap_or(0.0);
        let hours = [0.0, 3.0, 6.0];
        let pointing = [(0.0f32, 0.0f32), (4.5, -2.5), (-3.5, 3.0)];
        let law = RotationLaw::Howard;
        let frames: Vec<Picture> = hours
            .iter()
            .zip(&pointing)
            .map(|(&h, &(dx, dy))| {
                let f = render_sun(
                    640,
                    520,
                    disc,
                    t0 + h * 3600.0,
                    t0,
                    law,
                    Orientation::default(),
                    &feats,
                );
                Picture::Mono(shifted(&f, Shift::new(-dx, -dy), Kernel::Lanczos3))
            })
            .collect();
        let times: Vec<Option<f64>> = hours.iter().map(|h| Some(t0 + h * 3600.0)).collect();
        let base = Options {
            reference: Reference::First,
            ..Options::default()
        };
        let with = Options {
            derotate: Some(Derotation::default()),
            ..base
        };
        let run = |o: &Options| {
            register(&frames, &times, o, &|_| {}, &AtomicBool::new(false))
                .unwrap_or_else(|e| panic!("{e}"))
        };
        let (plain, turned) = (run(&base), run(&with));
        for (r, &(dx, dy)) in turned.report.iter().zip(&pointing) {
            assert!(
                (r.shift.dx - dx).abs() < 0.3 && (r.shift.dy - dy).abs() < 0.3,
                "frame {} shift {:?} want ({dx}, {dy})",
                r.index,
                r.shift
            );
        }
        assert!(turned.report[1].derotated_px.is_some_and(|d| d > 5.0));
        assert_eq!(turned.derotated_to, times[0]);
        // The surface agrees with the reference's after the derotation, and not before.
        let fit = |s: &Sequence| {
            let a = picture::luma(&s.frames[0]);
            let b = picture::luma(&s.frames[2]);
            let (mut e, mut n) = (0.0f64, 0.0f64);
            for y in 0..a.height {
                for x in 0..a.width {
                    if (x as f64 - 320.0).hypot(y as f64 - 260.0) < 0.85 * 240.0 {
                        e += f64::from(a.get(x, y) - b.get(x, y)).powi(2);
                        n += 1.0;
                    }
                }
            }
            (e / n).sqrt()
        };
        let (bad, good) = (fit(&plain), fit(&turned));
        assert!(good < 0.4 * bad, "rms {bad} -> {good}");
        // A series without times cannot be derotated.
        assert!(register(&frames, &[], &with, &|_| {}, &AtomicBool::new(false)).is_err());
    }

    #[test]
    fn reference_names_parse() {
        assert_eq!("first".parse::<Reference>().ok(), Some(Reference::First));
        assert_eq!("3".parse::<Reference>().ok(), Some(Reference::Index(3)));
        assert!("x".parse::<Reference>().is_err());
        assert_eq!(frame_name(0, "png"), "frame-0001.png");
    }
}
