// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! A full-disc mosaic from panel stacks.
//!
//! A telescope and camera that see only part of the Sun take it in panels. The
//! panels are stacked one by one, and this module puts them together:
//!
//! 1. every pair of panels is matched on its overlap ([`pair`]): a search over
//!    shift and a few degrees of rotation by masked normalised cross-correlation,
//!    then a Lucas-Kanade refinement to a small fraction of a pixel;
//! 2. all the pairs are placed at once by weighted least squares ([`solve`]), with
//!    a robust weight so a false match is put aside and named;
//! 3. optionally, each panel is **derotated** to one time (see
//!    [`crate::derotate`]) using the disc fitted to the whole mosaic, and the
//!    matching is repeated on the derotated panels;
//! 4. the panels are resampled onto a canvas, matched in gain and offset on their
//!    overlaps, joined along seams of least difference and feathered ([`blend`]).
//!
//! The result says where each panel went, how well every pair agrees, the gains,
//! and how well the panels agree along the seams.

#![allow(
    clippy::many_single_char_names,
    reason = "n, s, c, u, v and the panel indices are the natural names in this geometry"
)]

pub mod blend;
pub mod pair;
pub mod solve;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use rayon::prelude::*;

use crate::align::limb::LimbReference;
use crate::derotate::{DiscGeometry, Orientation, RotationLaw, Spec, derotate_with};
use crate::error::{Error, Result};
use crate::limbdark::{self, Law, Source};
use crate::picture::{self, Picture};
use crate::resample::Kernel;

/// The time the panels are brought to.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Target {
    /// The middle time of the panels.
    #[default]
    Middle,
    /// The earliest panel.
    First,
    /// The latest panel.
    Last,
    /// A time, unix seconds UTC.
    Unix(f64),
}

/// Settings of the derotation of the panels to one time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Derotation {
    /// The rotation law.
    pub law: RotationLaw,
    /// How the panels sit against the Sun (the orientation of the mosaic).
    pub orient: Orientation,
    /// The time to bring every panel to.
    pub target: Target,
    /// Carry the contrast against the limb darkening with each feature.
    pub preserve_contrast: bool,
}

impl Default for Derotation {
    fn default() -> Self {
        Self {
            law: RotationLaw::Howard,
            orient: Orientation::default(),
            target: Target::Middle,
            preserve_contrast: true,
        }
    }
}

/// Settings of [`build`].
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// The most the panels may be turned against each other, degrees. 0 holds
    /// them all square to the first.
    pub max_rotation_deg: f64,
    /// Least overlap that counts, as a share of the smaller panel.
    pub min_overlap: f32,
    /// Least normalised correlation of the detail on an overlap for a pair to
    /// be believed.
    pub min_ncc: f32,
    /// Width of the blend across a seam and at a panel's edge, pixels.
    pub feather: f32,
    /// Match the panels' gain and offset on the overlaps.
    pub match_levels: bool,
    /// Join along seams of least difference (off: blend by distance from each
    /// panel's edge).
    pub seams: bool,
    /// Bring the panels to one time first.
    pub derotate: Option<Derotation>,
    /// Interpolation.
    pub kernel: Kernel,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_rotation_deg: 4.0,
            min_overlap: 0.05,
            min_ncc: 0.45,
            feather: 12.0,
            match_levels: true,
            seams: true,
            derotate: None,
            kernel: Kernel::Lanczos3,
        }
    }
}

/// Where a panel went.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PanelReport {
    /// Position in the list, from 0.
    pub index: usize,
    /// Capture time, unix seconds UTC, when known.
    pub time: Option<f64>,
    /// Shift in the mosaic's coordinates, pixels (the first panel is at 0).
    pub x: f64,
    /// Shift in y.
    pub y: f64,
    /// Rotation against the first panel, degrees (clockwise on screen).
    pub angle_deg: f64,
    /// Gain applied for the match.
    pub gain: f32,
    /// Offset applied for the match.
    pub offset: f32,
    /// The most the derotation moved the surface of this panel, pixels.
    pub derotated_px: Option<f32>,
}

/// How a pair of panels matched.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PairReport {
    /// The first panel.
    pub a: usize,
    /// The second.
    pub b: usize,
    /// Normalised correlation of the detail on the overlap.
    pub ncc: f32,
    /// Pixels shared.
    pub overlap_px: usize,
    /// Where the pair was measured to lie against where the placement put it,
    /// pixels.
    pub residual_px: f64,
    /// Whether the pair was used in the placement.
    pub used: bool,
}

/// A finished mosaic.
#[derive(Clone, Debug)]
pub struct Mosaic {
    /// The picture.
    pub image: Picture,
    /// Where each panel went.
    pub panels: Vec<PanelReport>,
    /// How each matched pair agreed.
    pub pairs: Vec<PairReport>,
    /// Mosaic coordinates of the picture's top-left pixel.
    pub origin: (f64, f64),
    /// RMS difference of the panels beside a seam, as a share of the mean level
    /// there.
    pub seam_rms: f32,
    /// RMS difference of the panels over the whole overlap, the same measure.
    pub overlap_rms: f32,
    /// The disc fitted to the finished mosaic, in its coordinates.
    pub disc: Option<DiscGeometry>,
    /// The time the panels were brought to, unix seconds.
    pub derotated_to: Option<f64>,
    /// Plain-language lines about what happened.
    pub notes: Vec<String>,
}

fn target_time(t: Target, times: &[f64]) -> f64 {
    let mut s = times.to_vec();
    s.sort_by(f64::total_cmp);
    match t {
        Target::Middle => s[s.len() / 2],
        Target::First => s[0],
        Target::Last => s[s.len() - 1],
        Target::Unix(u) => u,
    }
}

/// Measure every pair; `init` gives the transform to start from for a pair.
fn measure_pairs(
    prep: &[pair::Prepared],
    opts: &Options,
    init: Option<&(dyn Fn(usize, usize) -> Option<pair::Rigid> + Sync)>,
    progress: &(dyn Fn(f32) + Sync),
    cancel: &AtomicBool,
    span: (f32, f32),
    min_ncc: f32,
) -> Result<Vec<(usize, usize, pair::Fit)>> {
    let n = prep.len();
    let jobs: Vec<(usize, usize)> = (0..n)
        .flat_map(|a| (a + 1..n).map(move |b| (a, b)))
        .collect();
    let done = AtomicUsize::new(0);
    let out: Vec<Result<Option<(usize, usize, pair::Fit)>>> = jobs
        .par_iter()
        .map(|&(a, b)| {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let start = init.and_then(|f| f(a, b));
            let fit = if init.is_some() && start.is_none() {
                None
            } else {
                pair::match_pair(
                    &prep[a],
                    &prep[b],
                    opts.max_rotation_deg,
                    opts.min_overlap,
                    start,
                )
            };
            let d = done.fetch_add(1, Ordering::Relaxed) + 1;
            progress(span.0 + (span.1 - span.0) * d as f32 / jobs.len().max(1) as f32);
            Ok(fit
                .filter(|f| f.ncc >= min_ncc && f.overlap >= 400)
                .map(|f| (a, b, f)))
        })
        .collect();
    let mut fits = Vec::new();
    for o in out {
        if let Some(f) = o? {
            fits.push(f);
        }
    }
    Ok(fits)
}

struct Placement {
    layout: solve::Layout,
    fits: Vec<(usize, usize, pair::Fit)>,
}

fn place_panels(
    prep: &[pair::Prepared],
    fits: Vec<(usize, usize, pair::Fit)>,
) -> Result<Placement> {
    let centres: Vec<(f64, f64)> = prep.iter().map(pair::Prepared::centre).collect();
    let radius = prep
        .iter()
        .map(|p| (p.width as f64).hypot(p.height as f64) / 2.0)
        .fold(0.0f64, f64::max);
    let measures: Vec<solve::Measure> = fits
        .iter()
        .map(|(a, b, f)| solve::Measure {
            a: *a,
            b: *b,
            rigid: f.rigid,
            weight: f64::from(f.ncc.max(0.0)).powi(2) * (f.overlap as f64).sqrt(),
        })
        .collect();
    let layout = solve::solve(&centres, &measures, radius)?;
    Ok(Placement { layout, fits })
}

/// A plain feathered composite of `panels` for fitting the disc.
fn composite(
    panels: &[Picture],
    layout: &solve::Layout,
    opts: &Options,
) -> (Picture, blend::Canvas) {
    let sizes: Vec<(usize, usize)> = panels.iter().map(Picture::size).collect();
    let canvas = blend::canvas_for(&sizes, &layout.theta, &layout.shift);
    let placed: Vec<blend::Placed> = panels
        .iter()
        .enumerate()
        .map(|(i, p)| {
            blend::place(
                p,
                layout.theta[i],
                layout.shift[i],
                &canvas,
                Kernel::Bilinear,
            )
        })
        .collect();
    let owner = blend::assign_owners(&placed, &canvas, false);
    (
        blend::blend(&placed, &owner, &canvas, opts.feather.min(6.0)),
        canvas,
    )
}

/// Put the panels together.
///
/// `times` are the capture times (needed only for derotation). `progress` is
/// told how far the work has got, 0 to 1.
///
/// # Errors
/// Fewer than two panels, panels that cannot be placed against the rest
/// (named), derotation without times or a limb, or cancellation.
#[allow(
    clippy::too_many_lines,
    reason = "one sequence: match, place, derotate, blend"
)]
pub fn build(
    inputs: &[Picture],
    times: &[Option<f64>],
    opts: &Options,
    progress: &(dyn Fn(f32) + Sync),
    cancel: &AtomicBool,
) -> Result<Mosaic> {
    let n = inputs.len();
    if n < 2 {
        return Err(Error::Invalid("a mosaic needs at least two panels".into()));
    }
    let derot_times: Option<Vec<f64>> = if opts.derotate.is_some() {
        Some(
            (0..n)
                .map(|i| times.get(i).copied().flatten())
                .collect::<Option<_>>()
                .ok_or_else(|| {
                    Error::Invalid(
                        "derotation needs the time of every panel (see --date for names that hold only a time)"
                            .into(),
                    )
                })?,
        )
    } else {
        None
    };
    let mut notes = Vec::new();
    let mut panels: Vec<Picture> = inputs.to_vec();
    let mut prep: Vec<pair::Prepared> = panels
        .par_iter()
        .map(|p| pair::prepare(&picture::luma(p)))
        .collect();
    let span1 = if opts.derotate.is_some() {
        (0.02, 0.35)
    } else {
        (0.02, 0.6)
    };
    // Panels taken hours apart differ where the surface has turned, so the first
    // match, which only has to be good enough to fit the disc, is allowed a lower
    // correlation when the panels are going to be derotated.
    let first_ncc = if opts.derotate.is_some() {
        0.4 * opts.min_ncc
    } else {
        opts.min_ncc
    };
    let fits = measure_pairs(&prep, opts, None, progress, cancel, span1, first_ncc)?;
    if fits.is_empty() {
        return Err(Error::Invalid(
            "no two panels overlap enough to be matched; each needs a strip of the Sun in common with a neighbour"
                .into(),
        ));
    }
    let mut placement = place_panels(&prep, fits)?;

    // Bring the panels to one time, then match again.
    let mut derotated_px: Vec<Option<f32>> = vec![None; n];
    let mut derotated_to = None;
    if let (Some(d), Some(t)) = (&opts.derotate, &derot_times) {
        let target = target_time(d.target, t);
        let (rough, canvas) = composite(&panels, &placement.layout, opts);
        let luma = picture::luma(&rough);
        let Some(fit) = LimbReference::new(&luma).map(|l| l.fit) else {
            return Err(Error::Invalid(
                "derotation needs the whole disc, or enough of its limb, in the mosaic to fit it"
                    .into(),
            ));
        };
        let plan = limbdark::plan(&luma, Source::Model, Law::Quadratic).map(|(p, _)| p);
        notes.push(format!(
            "disc fitted to the mosaic: centre ({:.1}, {:.1}), radius {:.1} px",
            fit.cx + canvas.origin.0,
            fit.cy + canvas.origin.1,
            fit.r
        ));
        let mut moved = Vec::with_capacity(n);
        for i in 0..n {
            let (pw, ph) = panels[i].size();
            let (cx, cy) = ((pw as f64 - 1.0) / 2.0, (ph as f64 - 1.0) / 2.0);
            let th = placement.layout.theta[i];
            let (tx, ty) = placement.layout.shift[i];
            // The disc centre in the panel's own pixels: p = R(-theta)(W - c - t) + c.
            let (wx, wy) = (fit.cx + canvas.origin.0, fit.cy + canvas.origin.1);
            let (u, v) = (wx - cx - tx, wy - cy - ty);
            let (s, c) = th.sin_cos();
            let geom = DiscGeometry {
                cx: c * u + s * v + cx,
                cy: -s * u + c * v + cy,
                r: fit.r,
            };
            // The panel is turned by theta (clockwise) to reach the mosaic, so
            // solar north in the panel is theta counter-clockwise further round.
            let mut orient = d.orient;
            orient.rotate_deg += th.to_degrees() as f32;
            let spec = Spec {
                law: d.law,
                orient,
                from_unix: t[i],
                to_unix: target,
                preserve_contrast: d.preserve_contrast,
                kernel: opts.kernel,
            };
            let (p, rep) = derotate_with(&panels[i], geom, &spec, plan.as_ref())?;
            derotated_px[i] = Some(rep.max_shift_px);
            moved.push(p);
        }
        panels = moved;
        prep = panels
            .par_iter()
            .map(|p| pair::prepare(&picture::luma(p)))
            .collect();
        let first = placement.fits.clone();
        let init = move |a: usize, b: usize| {
            first
                .iter()
                .find(|(x, y, _)| *x == a && *y == b)
                .map(|(_, _, f)| f.rigid)
        };
        let fits2 = measure_pairs(
            &prep,
            opts,
            Some(&init),
            progress,
            cancel,
            (0.35, 0.6),
            opts.min_ncc,
        )?;
        if fits2.is_empty() {
            return Err(Error::Invalid(
                "the panels no longer match after the derotation".into(),
            ));
        }
        placement = place_panels(&prep, fits2)?;
        derotated_to = Some(target);
    }

    // Canvas, levels, seams, blend.
    progress(0.62);
    let layout = &placement.layout;
    let sizes: Vec<(usize, usize)> = panels.iter().map(Picture::size).collect();
    let canvas = blend::canvas_for(&sizes, &layout.theta, &layout.shift);
    let mut placed: Vec<blend::Placed> = panels
        .par_iter()
        .enumerate()
        .map(|(i, p)| blend::place(p, layout.theta[i], layout.shift[i], &canvas, opts.kernel))
        .collect();
    progress(0.75);
    let levels = if opts.match_levels {
        let (l, samples) = blend::match_levels(&placed);
        if samples < 200 {
            notes.push(
                "too little overlap to match the brightness of the panels; left as they are".into(),
            );
        }
        l
    } else {
        vec![(1.0, 0.0); n]
    };
    for (p, &(g, o)) in placed.iter_mut().zip(&levels) {
        if (g - 1.0).abs() > f32::EPSILON || o.abs() > f32::EPSILON {
            blend::apply_levels(p, g, o);
        }
    }
    let owner = blend::assign_owners(&placed, &canvas, opts.seams);
    progress(0.88);
    let image = blend::blend(&placed, &owner, &canvas, opts.feather);
    let (seam, overlap, level) = blend::seam_statistics(&placed, &owner, &canvas);
    let rel = |v: f32| if level > 1e-9 { v / level } else { v };
    let disc = {
        let l = picture::luma(&image);
        LimbReference::new(&l).map(|r| DiscGeometry {
            cx: r.fit.cx,
            cy: r.fit.cy,
            r: r.fit.r,
        })
    };
    let pairs: Vec<PairReport> = placement
        .fits
        .iter()
        .enumerate()
        .map(|(i, (a, b, f))| PairReport {
            a: *a,
            b: *b,
            ncc: f.ncc,
            overlap_px: f.overlap,
            residual_px: layout.residual[i],
            used: layout.used[i],
        })
        .collect();
    for p in pairs.iter().filter(|p| !p.used) {
        notes.push(format!(
            "panels {} and {} matched {:.1} px away from where the rest put them and were not used",
            p.a + 1,
            p.b + 1,
            p.residual_px
        ));
    }
    let panel_reports = (0..n)
        .map(|i| PanelReport {
            index: i,
            time: times.get(i).copied().flatten(),
            x: layout.shift[i].0,
            y: layout.shift[i].1,
            angle_deg: layout.theta[i].to_degrees(),
            gain: levels[i].0,
            offset: levels[i].1,
            derotated_px: derotated_px[i],
        })
        .collect();
    progress(1.0);
    Ok(Mosaic {
        image,
        panels: panel_reports,
        pairs,
        origin: canvas.origin,
        seam_rms: rel(seam),
        overlap_rms: rel(overlap),
        disc,
        derotated_to,
        notes,
    })
}

/// A few lines saying what the mosaic did.
#[must_use]
pub fn summary_lines(m: &Mosaic) -> Vec<String> {
    let (w, h) = m.image.size();
    let used = m.pairs.iter().filter(|p| p.used).count();
    let worst = m
        .pairs
        .iter()
        .filter(|p| p.used)
        .map(|p| p.residual_px)
        .fold(0.0f64, f64::max);
    let mut v = vec![format!(
        "{} panels joined into {w} x {h} px: {used} of {} overlapping pairs used, worst disagreement {worst:.2} px",
        m.panels.len(),
        m.pairs.len()
    )];
    v.push(format!(
        "panels differ by {:.2}% (RMS) over their overlaps and {:.2}% along the seams",
        100.0 * m.overlap_rms,
        100.0 * m.seam_rms
    ));
    if let Some(t) = m.derotated_to {
        v.push(format!(
            "derotated to {} UTC by the Sun's differential rotation",
            crate::io::solarnet::iso_from_unix(t).replace('T', " ")[..19].to_owned()
        ));
    }
    if let Some(d) = m.disc {
        v.push(format!(
            "disc: centre ({:.1}, {:.1}), radius {:.1} px",
            d.cx, d.cy, d.r
        ));
    }
    v.extend(m.notes.iter().cloned());
    v
}

/// The placements as CSV, one row per panel.
#[must_use]
pub fn panels_csv(m: &Mosaic, names: &[String]) -> String {
    use std::fmt::Write as _;
    let mut s = String::from("panel,file,time_utc,x_px,y_px,angle_deg,gain,offset,derotated_px\n");
    for p in &m.panels {
        let _ = writeln!(
            s,
            "{},{},{},{:.3},{:.3},{:.4},{:.4},{:.5},{}",
            p.index + 1,
            names.get(p.index).map_or("", String::as_str),
            p.time
                .map_or(String::new(), crate::io::solarnet::iso_from_unix),
            p.x,
            p.y,
            p.angle_deg,
            p.gain,
            p.offset,
            p.derotated_px.map_or(String::new(), |d| format!("{d:.3}"))
        );
    }
    s
}

/// The pairs as CSV, one row per matched pair.
#[must_use]
pub fn pairs_csv(m: &Mosaic) -> String {
    use std::fmt::Write as _;
    let mut s = String::from("panel_a,panel_b,ncc,overlap_px,residual_px,used\n");
    for p in &m.pairs {
        let _ = writeln!(
            s,
            "{},{},{:.4},{},{:.3},{}",
            p.a + 1,
            p.b + 1,
            p.ncc,
            p.overlap_px,
            p.residual_px,
            p.used
        );
    }
    s
}

#[cfg(test)]
#[allow(
    clippy::many_single_char_names,
    reason = "x, y, w, h are the natural names in image maths"
)]
mod tests {
    use super::pair::Rigid;
    use super::*;
    use crate::derotate::synth::render_sun;
    use crate::frame::Frame;
    use crate::io::solarnet::unix_from_iso;
    use crate::resample::sample;
    use crate::testutil::{Rng, SyntheticConfig, SyntheticSun};

    /// A `w` x `h` view of `truth`: panel pixel `p` is the truth at
    /// `R(theta)(p - c) + c + (x0, y0)`, then `gain * v + offset` and noise.
    fn panel(
        truth: &Frame,
        (x0, y0): (f64, f64),
        (w, h): (usize, usize),
        deg: f64,
        (gain, offset): (f32, f32),
        noise: f32,
        seed: u64,
    ) -> Picture {
        let r = Rigid {
            tx: x0,
            ty: y0,
            theta: deg.to_radians(),
        };
        let c = ((w as f64 - 1.0) / 2.0, (h as f64 - 1.0) / 2.0);
        let mut rng = Rng::new(seed);
        let mut f = Frame::zeros(w, h);
        for y in 0..h {
            for x in 0..w {
                let (qx, qy) = r.apply(x as f64, y as f64, c);
                let v = sample(truth, qx as f32, qy as f32, Kernel::Lanczos3);
                f.data[y * w + x] = v * gain + offset + noise * rng.gauss();
            }
        }
        Picture::Mono(f)
    }

    fn run(panels: &[Picture], opts: &Options) -> Mosaic {
        build(panels, &[], opts, &|_| {}, &AtomicBool::new(false)).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn four_panels_with_known_offsets_rotations_and_gains_are_joined() {
        let sun = SyntheticSun::new(&SyntheticConfig {
            width: 560,
            height: 460,
            radius_frac: 0.43,
            granulation: 0.15,
            spots: 5,
            ..SyntheticConfig::default()
        });
        let truth = sun.truth();
        let size = (320, 270);
        let spec = [
            ((10.0, 12.0), 0.0, (1.00, 0.00)),
            ((230.0, 18.0), 1.2, (0.92, 0.02)),
            ((14.0, 186.0), -0.8, (1.08, -0.02)),
            ((236.0, 192.0), 0.5, (0.97, 0.01)),
        ];
        let panels: Vec<Picture> = spec
            .iter()
            .enumerate()
            .map(|(i, &(o, d, g))| panel(truth, o, size, d, g, 0.006, 11 + i as u64))
            .collect();
        let m = run(&panels, &Options::default());
        for (p, &(o, d, _)) in m.panels.iter().zip(&spec) {
            let (ex, ey) = (o.0 - spec[0].0.0, o.1 - spec[0].0.1);
            assert!(
                (p.x - ex).abs() < 0.25 && (p.y - ey).abs() < 0.25,
                "panel {} at ({:.2}, {:.2}) want ({ex}, {ey})",
                p.index + 1,
                p.x,
                p.y
            );
            assert!(
                (p.angle_deg - d).abs() < 0.06,
                "panel {} angle {}",
                p.index + 1,
                p.angle_deg
            );
        }
        // Panel 0's brightness is the reference: a panel read at gain g is brought back by g0 / g.
        for (p, &(_, _, g)) in m.panels.iter().zip(&spec).skip(1) {
            let want = spec[0].2.0 / g.0;
            assert!(
                (p.gain - want).abs() < 0.02,
                "panel {} gain {} want {want}",
                p.index + 1,
                p.gain
            );
        }
        assert!(m.pairs.iter().filter(|p| p.used).count() >= 4);
        assert!(m.seam_rms < 0.02, "seam {}", m.seam_rms);
        // The mosaic against the truth it was cut from, in the middle of the disc.
        let Picture::Mono(img) = &m.image else {
            panic!("mono")
        };
        let (ox, oy) = (m.origin.0 + spec[0].0.0, m.origin.1 + spec[0].0.1);
        let (mut e, mut n) = (0.0f64, 0.0f64);
        for y in 0..img.height {
            for x in 0..img.width {
                let (tx, ty) = (x as f64 + ox, y as f64 + oy);
                if (tx - 280.0).hypot(ty - 230.0) < 0.85 * 0.43 * 460.0
                    && img.data[y * img.width + x] != 0.0
                {
                    let t = f64::from(sample(truth, tx as f32, ty as f32, Kernel::Lanczos3));
                    e += (f64::from(img.data[y * img.width + x]) - t).powi(2);
                    n += 1.0;
                }
            }
        }
        let rms = (e / n).sqrt();
        assert!(n > 50_000.0 && rms < 0.02, "mosaic rms {rms} over {n} px");
    }

    #[test]
    fn matching_the_levels_helps() {
        let sun = SyntheticSun::new(&SyntheticConfig {
            width: 480,
            height: 360,
            radius_frac: 0.45,
            ..SyntheticConfig::default()
        });
        let a = panel(
            sun.truth(),
            (0.0, 0.0),
            (300, 360),
            0.0,
            (1.0, 0.0),
            0.004,
            1,
        );
        let b = panel(
            sun.truth(),
            (180.0, 0.0),
            (300, 360),
            0.0,
            (0.85, 0.03),
            0.004,
            2,
        );
        let with = run(&[a.clone(), b.clone()], &Options::default());
        let without = run(
            &[a, b],
            &Options {
                match_levels: false,
                ..Options::default()
            },
        );
        assert!(
            with.seam_rms < 0.3 * without.seam_rms,
            "{} vs {}",
            with.seam_rms,
            without.seam_rms
        );
    }

    #[test]
    fn fewer_than_two_or_unrelated_panels_are_refused_clearly() {
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let a = panel(sun.truth(), (0.0, 0.0), (90, 80), 0.0, (1.0, 0.0), 0.0, 1);
        assert!(
            build(
                std::slice::from_ref(&a),
                &[],
                &Options::default(),
                &|_| {},
                &AtomicBool::new(false)
            )
            .is_err()
        );
        let noise = {
            let mut rng = Rng::new(5);
            let mut f = Frame::zeros(90, 80);
            f.data.iter_mut().for_each(|v| *v = 0.5 + 0.2 * rng.gauss());
            Picture::Mono(f)
        };
        let e = build(
            &[a, noise],
            &[],
            &Options::default(),
            &|_| {},
            &AtomicBool::new(false),
        )
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
        assert!(e.contains("overlap") || e.contains("panel"), "{e}");
    }

    #[test]
    fn panels_taken_hours_apart_line_up_after_derotation() {
        use crate::derotate::{DiscGeometry, RotationLaw};
        let disc = DiscGeometry {
            cx: 300.0,
            cy: 240.0,
            r: 200.0,
        };
        // Sixty-odd spots scattered over the disc give the panels something to match.
        let mut rng = Rng::new(21);
        let feats: Vec<(f64, f64)> = (0..70)
            .map(|_| {
                let lat = (f64::from(rng.uniform()) * 2.0 - 1.0).asin().to_degrees() * 0.9;
                (lat, (f64::from(rng.uniform()) - 0.5) * 140.0)
            })
            .collect();
        let t0 = unix_from_iso("2026-10-20T09:00:00").unwrap_or(0.0);
        let t1 = t0 + 4.0 * 3600.0;
        let law = RotationLaw::Howard;
        let view = |t: f64| render_sun(600, 480, disc, t, t0, law, Orientation::default(), &feats);
        let (full0, full1) = (view(t0), view(t1));
        // The left panel at t0, the right one (turned 1.2 degrees) four hours later.
        let left = panel(&full0, (0.0, 0.0), (350, 480), 0.0, (1.0, 0.0), 0.0, 1);
        let right = panel(&full1, (250.0, 0.0), (350, 480), 1.2, (1.0, 0.0), 0.0, 2);
        let times = [Some(t0), Some(t1)];
        let score = |m: &Mosaic| {
            let Picture::Mono(img) = &m.image else {
                panic!("mono")
            };
            // The truth at t0, where the left panel sits.
            let (ox, oy) = (m.origin.0, m.origin.1);
            let (mut e, mut n) = (0.0f64, 0.0f64);
            for y in 0..img.height {
                for x in 0..img.width {
                    let (tx, ty) = (x as f64 + ox, y as f64 + oy);
                    if (tx - 300.0).hypot(ty - 240.0) < 0.9 * 200.0
                        && img.data[y * img.width + x] != 0.0
                    {
                        let t = f64::from(sample(&full0, tx as f32, ty as f32, Kernel::Lanczos3));
                        e += (f64::from(img.data[y * img.width + x]) - t).powi(2);
                        n += 1.0;
                    }
                }
            }
            (e / n).sqrt()
        };
        let go = |opts: &Options| {
            build(
                &[left.clone(), right.clone()],
                &times,
                opts,
                &|_| {},
                &AtomicBool::new(false),
            )
            .unwrap_or_else(|e| panic!("{e}"))
        };
        let plain = go(&Options {
            min_ncc: 0.1,
            ..Options::default()
        });
        let turned = go(&Options {
            derotate: Some(Derotation {
                target: Target::First,
                ..Derotation::default()
            }),
            ..Options::default()
        });
        let (bad, good) = (score(&plain), score(&turned));
        assert!(good < 0.15 * bad, "rms {bad} -> {good}");
        assert!(turned.panels[1].derotated_px.is_some_and(|d| d > 5.0));
        assert_eq!(turned.derotated_to, Some(t0));
        // Without times it is refused.
        let e = build(
            &[left, right],
            &[],
            &Options {
                derotate: Some(Derotation::default()),
                ..Options::default()
            },
            &|_| {},
            &AtomicBool::new(false),
        )
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
        assert!(e.contains("time of every panel"), "{e}");
    }
}
