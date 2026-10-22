// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Sunspots on a white-light disc: umbra and penumbra, groups, the relative
//! Wolf number and areas corrected for foreshortening.
//!
//! The disc is flattened first (the fitted limb-darkening law divided out, see
//! [`crate::limbdark`]), so one threshold means the same thing at the centre
//! and near the limb. A spot is a connected region of the lightly smoothed disc
//! darker than the `penumbra` fraction of the quiet level; the pixels of it
//! darker than `umbra` are its umbra. Regions are grouped by their separation
//! on the sphere. Positions and areas use the disc centre and radius of the
//! limb fit and, when the capture time is known, `P`, `B0` and `L0` of the
//! Sun at that time ([`crate::ephemeris`]).
//!
//! The Wolf number is the relative `R = k (10 g + s)`: `g` groups, `s`
//! individual spots (every umbra counts, and a penumbral region with no umbra,
//! a pore, counts as one). `k` is the observer factor; 1 is the raw count.
//!
//! # References
//!
//! - Clette, F. et al. (2014), Revisiting the sunspot number, Space Sci. Rev. 186, 35-103,
//!   doi:10.1007/s11214-014-0074-2 (the relative sunspot number `R = k (10 g + s)`).
//! - Meeus, J. (1998), Astronomical Algorithms, 2nd ed., Willmann-Bell, chapter 29 (heliographic
//!   coordinates; see also [`crate::ephemeris`]).
//! - Thompson, W. T. (2006), Astron. Astrophys. 449, 791-803, doi:10.1051/0004-6361:20054262.

#![allow(
    clippy::many_single_char_names,
    reason = "image maths uses w, h, x, y and the like"
)]
#![allow(
    clippy::cast_possible_wrap,
    reason = "pixel coordinates are far below i32::MAX"
)]

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::colour::RgbImage;
use crate::edit::conv;
use crate::ephemeris::{self, Ephemeris};
use crate::frame::Frame;
use crate::limbdark::{self, Law, Source};

/// Spots closer to the limb than this share of the radius are not measured:
/// the limb fit and the flattening are least certain there, and a pixel is
/// foreshortened by more than 4 to 1.
pub const LIMB_MARGIN: f32 = 0.97;

/// Smallest cosine of the heliocentric angle used to correct an area.
const MIN_MU: f32 = 0.2;

/// Area of one pixel's worth of the hemisphere is `1 / (2 pi R^2)`.
const MSH: f64 = 1e6;

/// Tuning of the detection.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Params {
    /// Umbra threshold: smoothed intensity above the sky as a fraction of the
    /// quiet disc's, below which a pixel is umbra.
    pub umbra: f32,
    /// Penumbra threshold, the same measure: below it a pixel is spot.
    pub penumbra: f32,
    /// Smallest spot to keep, in millionths of the visible hemisphere
    /// (corrected for foreshortening). At least 4 pixels are always needed.
    pub min_msh: f32,
    /// Regions larger than this, millionths of the hemisphere corrected, are
    /// not spots (the largest groups on record are about 6000): a cloud, a
    /// vignette or an uneven flat. They are left out, with a note.
    pub max_msh: f32,
    /// Spots whose edges are closer than this on the sphere, degrees, belong
    /// to one group.
    pub group_gap_deg: f32,
    /// Observer factor `k` of the Wolf number.
    pub k: f32,
    /// Angle, degrees counter-clockwise, from the top of the image to solar
    /// north. 0 when north is up; the position angle `P` of the Sun when
    /// celestial north is up (see [`Params::celestial_north_up`]).
    pub rotate_deg: f32,
    /// The image is a mirror image (an odd number of reflections in the
    /// optics, a diagonal without a flip): west is then on the left.
    pub mirror: bool,
    /// Take `rotate_deg` as an extra rotation on top of celestial north up:
    /// the image is turned by `P` from the capture time.
    pub celestial_north_up: bool,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            umbra: 0.55,
            penumbra: 0.88,
            min_msh: 2.0,
            max_msh: 6000.0,
            group_gap_deg: 3.0,
            k: 1.0,
            rotate_deg: 0.0,
            mirror: false,
            celestial_north_up: false,
        }
    }
}

/// The disc the positions are measured on.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Geometry {
    /// Centre, x pixels.
    pub cx: f32,
    /// Centre, y pixels, top row 0.
    pub cy: f32,
    /// Radius in pixels.
    pub radius: f32,
}

/// One spot: a penumbral region with its umbra or umbrae.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Spot {
    /// Number, from 1, in the order of the table (by group, then by size).
    pub id: usize,
    /// The group it belongs to.
    pub group: usize,
    /// Centroid, x pixels.
    pub x: f32,
    /// Centroid, y pixels.
    pub y: f32,
    /// Heliographic latitude, degrees north.
    pub lat_deg: f32,
    /// Central meridian distance, degrees, west positive.
    pub cmd_deg: f32,
    /// Carrington longitude, degrees; absent without a capture time.
    pub lon_deg: Option<f32>,
    /// Cosine of the angle from disc centre at the centroid.
    pub mu: f32,
    /// Separate umbrae inside the region (at least 1; a pore counts as one).
    pub umbrae: usize,
    /// Pixels of umbra.
    pub umbra_px: usize,
    /// Pixels of penumbra (the region without its umbra).
    pub penumbra_px: usize,
    /// Whole spot as seen, millionths of the visible hemisphere.
    pub projected_msh: f32,
    /// Whole spot corrected for foreshortening, millionths of the hemisphere.
    pub area_msh: f32,
    /// Umbra only, corrected, millionths of the hemisphere.
    pub umbra_msh: f32,
    /// Darkest smoothed intensity inside, as a share of the quiet disc's.
    pub darkest: f32,
}

/// A group of spots.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Group {
    /// Number, from 1, largest first.
    pub id: usize,
    /// Spots in it.
    pub spots: usize,
    /// Umbrae in it (the group's share of the Wolf count `s`).
    pub umbrae: usize,
    /// Area-weighted latitude, degrees north.
    pub lat_deg: f32,
    /// Area-weighted central meridian distance, degrees, west positive.
    pub cmd_deg: f32,
    /// Area-weighted Carrington longitude, degrees.
    pub lon_deg: Option<f32>,
    /// Whole group as seen, millionths of the visible hemisphere.
    pub projected_msh: f32,
    /// Whole group corrected, millionths of the hemisphere.
    pub area_msh: f32,
    /// Umbrae corrected, millionths of the hemisphere.
    pub umbra_msh: f32,
}

/// What a detection found.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Report {
    /// Capture time, unix seconds UTC, when known.
    pub when_unix: Option<f64>,
    /// `P`, `B0` and `L0` at that time.
    pub ephemeris: Option<Ephemeris3>,
    /// The disc.
    pub disc: Geometry,
    /// Quiet-disc level the thresholds are fractions of (the flattened
    /// median above the sky), in the units of the image.
    pub quiet: f32,
    /// What it was run with.
    pub params: Params,
    /// Rotation actually applied from image up to solar north, degrees.
    pub north_angle_deg: f32,
    /// Groups, `g`.
    pub group_count: usize,
    /// Individual spots, `s` (umbrae and pores).
    pub spot_count: usize,
    /// Relative Wolf number `k (10 g + s)`.
    pub wolf: f32,
    /// Everything as seen, millionths of the visible hemisphere.
    pub projected_msh: f32,
    /// Everything corrected, millionths of the hemisphere.
    pub area_msh: f32,
    /// Umbrae corrected, millionths of the hemisphere.
    pub umbra_msh: f32,
    /// The spots.
    pub spots: Vec<Spot>,
    /// The groups.
    pub groups: Vec<Group>,
    /// What to be careful about.
    pub notes: Vec<String>,
}

/// `P`, `B0` and `L0` of the Sun, degrees.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ephemeris3 {
    /// Position angle of the axis east of celestial north.
    pub p_deg: f32,
    /// Heliographic latitude of the disc centre.
    pub b0_deg: f32,
    /// Carrington longitude of the disc centre.
    pub l0_deg: f32,
}

impl From<Ephemeris> for Ephemeris3 {
    fn from(e: Ephemeris) -> Self {
        Self {
            p_deg: e.p_deg as f32,
            b0_deg: e.b0_deg as f32,
            l0_deg: e.l0_deg as f32,
        }
    }
}

/// A finished detection: the numbers and a picture of them.
#[derive(Clone, Debug)]
pub struct Detection {
    /// The numbers.
    pub report: Report,
    /// The flattened disc with umbra, penumbra, limb and group numbers drawn
    /// on it.
    pub overlay: RgbImage,
}

/// Position on the sphere, degrees.
#[derive(Clone, Copy)]
struct Pos {
    lat: f64,
    cmd: f64,
}

/// Maps image pixels to the heliographic grid.
struct Grid {
    cx: f64,
    cy: f64,
    radius: f64,
    sin_a: f64,
    cos_a: f64,
    mirror: bool,
    sin_b0: f64,
    cos_b0: f64,
}

impl Grid {
    /// Normalised disc coordinates (west, north, towards observer) of a pixel.
    fn disc_xyz(&self, x: f64, y: f64) -> (f64, f64, f64) {
        let mut dx = x - self.cx;
        let dy = self.cy - y;
        if self.mirror {
            dx = -dx;
        }
        let xs = (dx * self.cos_a + dy * self.sin_a) / self.radius;
        let ys = (-dx * self.sin_a + dy * self.cos_a) / self.radius;
        let z = (1.0 - xs * xs - ys * ys).max(0.0).sqrt();
        (xs, ys, z)
    }

    fn pos(&self, x: f64, y: f64) -> Pos {
        let (xs, ys, z) = self.disc_xyz(x, y);
        let sin_lat = (ys * self.cos_b0 + z * self.sin_b0).clamp(-1.0, 1.0);
        let cmd = xs.atan2(z * self.cos_b0 - ys * self.sin_b0);
        Pos {
            lat: sin_lat.asin().to_degrees(),
            cmd: cmd.to_degrees(),
        }
    }
}

/// Great-circle distance between two positions, degrees.
fn separation(a: Pos, b: Pos) -> f64 {
    let (la, lb) = (a.lat.to_radians(), b.lat.to_radians());
    let dl = (a.cmd - b.cmd).to_radians();
    let c = la.sin() * lb.sin() + la.cos() * lb.cos() * dl.cos();
    c.clamp(-1.0, 1.0).acos().to_degrees()
}

/// Gaussian taps of standard deviation `sigma`, to three sigma.
fn gaussian(sigma: f32) -> Vec<f32> {
    let reach = (3.0 * sigma).ceil().max(1.0) as i32;
    let mut taps: Vec<f32> = (-reach..=reach)
        .map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp())
        .collect();
    let sum: f32 = taps.iter().sum();
    for t in &mut taps {
        *t /= sum;
    }
    taps
}

/// Mean of `values` over the box of half-width `half` around every pixel,
/// counting only the pixels where `valid` is set; the count is returned too.
fn box_mean(
    values: &[f32],
    valid: &[bool],
    w: usize,
    h: usize,
    half: usize,
) -> (Vec<f32>, Vec<f32>) {
    // Integral images with one row and column of zeros in front.
    let sw = w + 1;
    let mut sum = vec![0.0f64; sw * (h + 1)];
    let mut cnt = vec![0.0f64; sw * (h + 1)];
    for y in 0..h {
        let (mut rs, mut rc) = (0.0f64, 0.0f64);
        for x in 0..w {
            if valid[y * w + x] {
                rs += f64::from(values[y * w + x]);
                rc += 1.0;
            }
            sum[(y + 1) * sw + x + 1] = sum[y * sw + x + 1] + rs;
            cnt[(y + 1) * sw + x + 1] = cnt[y * sw + x + 1] + rc;
        }
    }
    let at = |t: &[f64], x: usize, y: usize| t[y * sw + x];
    let mut mean = vec![0.0f32; w * h];
    let mut count = vec![0.0f32; w * h];
    for y in 0..h {
        let (y0, y1) = (y.saturating_sub(half), (y + half + 1).min(h));
        for x in 0..w {
            let (x0, x1) = (x.saturating_sub(half), (x + half + 1).min(w));
            let box_of = |t: &[f64]| at(t, x1, y1) - at(t, x0, y1) - at(t, x1, y0) + at(t, x0, y0);
            let c = box_of(&cnt);
            count[y * w + x] = c as f32;
            mean[y * w + x] = if c > 0.0 {
                (box_of(&sum) / c) as f32
            } else {
                0.0
            };
        }
    }
    (mean, count)
}

/// The quiet-Sun level at every pixel: the mean of the surrounding disc with
/// the spots and the bright areas left out, so a gradient the flattening did
/// not remove (a vignette, uneven cloud, a one-sided filter) does not read as
/// spot. `rel` is the pixel's share of the radius; `global` is the level
/// used where the surroundings hold too little disc.
fn local_quiet(
    smooth: &[f32],
    w: usize,
    h: usize,
    rel: &[f32],
    background: f32,
    global: f32,
    half: usize,
) -> Vec<f32> {
    let in_disc: Vec<bool> = rel.iter().map(|&u| u < LIMB_MARGIN).collect();
    let mut level = vec![global + background; w * h];
    let min_count = ((2 * half + 1) * (2 * half + 1)) as f32 * 0.05;
    // Start tolerant, so a broad dimming still counts as quiet, then tighten.
    for lo in [0.6f32, 0.8, 0.9, 0.93, 0.93] {
        let valid: Vec<bool> = (0..w * h)
            .map(|i| {
                let c = (smooth[i] - background) / (level[i] - background).max(1e-9);
                in_disc[i] && c > lo && c < 1.12
            })
            .collect();
        let (mean, count) = box_mean(smooth, &valid, w, h, half);
        for i in 0..w * h {
            if count[i] >= min_count {
                level[i] = mean[i];
            }
        }
    }
    level
}

/// Label the 8-connected regions of `mask`: 0 is background, regions are
/// numbered from 1. Returns the labels and the region count.
fn label(mask: &[bool], w: usize, h: usize) -> (Vec<u32>, usize) {
    let mut labels = vec![0u32; w * h];
    let mut count = 0usize;
    let mut stack = Vec::new();
    for start in 0..w * h {
        if !mask[start] || labels[start] != 0 {
            continue;
        }
        count += 1;
        labels[start] = count as u32;
        stack.push(start);
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                    if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                        continue;
                    }
                    let j = ny as usize * w + nx as usize;
                    if mask[j] && labels[j] == 0 {
                        labels[j] = count as u32;
                        stack.push(j);
                    }
                }
            }
        }
    }
    (labels, count)
}

/// Working sums of one region.
#[derive(Default, Clone)]
struct Acc {
    px: usize,
    umbra_px: usize,
    w: f64,
    wx: f64,
    wy: f64,
    umbra_w: f64,
    darkest: f32,
    umbrae: Vec<u32>,
}

fn find(p: &mut [usize], mut i: usize) -> usize {
    while p[i] != i {
        p[i] = p[p[i]];
        i = p[i];
    }
    i
}

/// Find the sunspots in `image`, a white-light stack. `when` is the capture
/// time (unix seconds UTC); without it `B0` is taken as 0 and longitudes are
/// relative to the central meridian. `None` when there is no disc to fit.
#[must_use]
#[allow(clippy::too_many_lines, reason = "one pass from pixels to a report")]
pub fn detect(image: &Frame, when: Option<f64>, params: &Params) -> Option<Detection> {
    let (plan, _) = limbdark::plan(image, Source::Model, Law::Quadratic)?;
    let flat = plan.apply(image);
    let (w, h) = (flat.width, flat.height);
    let radius = plan.radius;
    let sigma = (radius / 400.0).clamp(1.0, 4.0);
    let taps = gaussian(sigma);
    let smooth = conv::separable(&flat.data, w, h, &taps, 1);

    let inv_r = 1.0 / radius;
    let rel = |i: usize| {
        let (x, y) = (i % w, i / w);
        (x as f32 - plan.cx).hypot(y as f32 - plan.cy) * inv_r
    };
    // The quiet level: the median of the flattened disc away from the limb.
    let mut inner: Vec<f32> = (0..w * h)
        .step_by(3)
        .filter(|&i| rel(i) < 0.9)
        .map(|i| smooth[i])
        .collect();
    if inner.is_empty() {
        return None;
    }
    let mid = inner.len() / 2;
    let quiet_raw = *inner.select_nth_unstable_by(mid, f32::total_cmp).1;
    let quiet = quiet_raw - plan.background;
    if quiet <= 1e-6 {
        return None;
    }
    let rels: Vec<f32> = (0..w * h).map(rel).collect();
    let half = ((radius * 0.2) as usize).max(8);
    let local = local_quiet(&smooth, w, h, &rels, plan.background, quiet, half);
    let contrast =
        |i: usize| (smooth[i] - plan.background) / (local[i] - plan.background).max(1e-9);

    let ephem = when.map(ephemeris::solar_ephemeris);
    let (p, b0) = ephem.map_or((0.0, 0.0), |e| (e.p_deg, e.b0_deg));
    let north = if params.celestial_north_up {
        params.rotate_deg + p as f32
    } else {
        params.rotate_deg
    };
    let a = f64::from(north).to_radians();
    let grid = Grid {
        cx: f64::from(plan.cx),
        cy: f64::from(plan.cy),
        radius: f64::from(radius),
        sin_a: a.sin(),
        cos_a: a.cos(),
        mirror: params.mirror,
        sin_b0: b0.to_radians().sin(),
        cos_b0: b0.to_radians().cos(),
    };

    let pen_mask: Vec<bool> = (0..w * h)
        .map(|i| rel(i) < LIMB_MARGIN && contrast(i) < params.penumbra)
        .collect();
    let umb_mask: Vec<bool> = (0..w * h)
        .map(|i| pen_mask[i] && contrast(i) < params.umbra)
        .collect();
    let (labels, n) = label(&pen_mask, w, h);
    let (ulabels, un) = label(&umb_mask, w, h);

    // Sum each region, weighting every pixel by 1/mu: its share of the
    // sphere, not of the picture.
    let mut accs = vec![Acc::default(); n];
    for a in &mut accs {
        a.darkest = f32::INFINITY;
    }
    let mut umbra_size = vec![0usize; un];
    for i in 0..w * h {
        let l = labels[i] as usize;
        if l == 0 {
            continue;
        }
        let (x, y) = (i % w, i / w);
        let (_, _, z) = grid.disc_xyz(x as f64, y as f64);
        let wt = 1.0 / f64::from((z as f32).max(MIN_MU));
        let acc = &mut accs[l - 1];
        acc.px += 1;
        acc.w += wt;
        acc.wx += wt * x as f64;
        acc.wy += wt * y as f64;
        acc.darkest = acc.darkest.min(contrast(i));
        if ulabels[i] != 0 {
            acc.umbra_px += 1;
            acc.umbra_w += wt;
            let u = ulabels[i];
            umbra_size[u as usize - 1] += 1;
            if !acc.umbrae.contains(&u) {
                acc.umbrae.push(u);
            }
        }
    }

    let hemisphere = 2.0 * std::f64::consts::PI * f64::from(radius).powi(2);
    let to_msh = |w: f64| (w * MSH / hemisphere) as f32;
    let min_px = 4usize;
    let mut kept: Vec<(usize, Acc)> = Vec::new();
    let mut too_big = 0usize;
    for (idx, acc) in accs.into_iter().enumerate() {
        if acc.px >= min_px && to_msh(acc.w) >= params.min_msh {
            if to_msh(acc.w) > params.max_msh {
                too_big += 1;
            } else {
                kept.push((idx + 1, acc));
            }
        }
    }

    // Group by separation on the sphere, nearest edges.
    let info: Vec<(Pos, f64)> = kept
        .iter()
        .map(|(_, a)| {
            let pos = grid.pos(a.wx / a.w, a.wy / a.w);
            // Radius, degrees, of the circle of the same corrected area.
            let rho = (a.w / std::f64::consts::PI).sqrt() / f64::from(radius);
            (pos, rho.to_degrees())
        })
        .collect();
    let m = kept.len();
    let mut parent: Vec<usize> = (0..m).collect();
    for i in 0..m {
        for j in i + 1..m {
            let d = separation(info[i].0, info[j].0) - info[i].1 - info[j].1;
            if d < f64::from(params.group_gap_deg) {
                let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                parent[ri] = rj;
            }
        }
    }
    // Number groups by total area, largest first.
    let mut roots: Vec<usize> = (0..m).map(|i| find(&mut parent, i)).collect();
    let mut area_of = std::collections::BTreeMap::<usize, f64>::new();
    for (i, &r) in roots.iter().enumerate() {
        *area_of.entry(r).or_default() += kept[i].1.w;
    }
    let mut order: Vec<(usize, f64)> = area_of.into_iter().collect();
    order.sort_by(|a, b| b.1.total_cmp(&a.1));
    let group_of = |root: usize| order.iter().position(|o| o.0 == root).map_or(0, |p| p + 1);
    for r in &mut roots {
        *r = group_of(*r);
    }

    let l0 = ephem.map(|e| e.l0_deg);
    let mut spots: Vec<Spot> = kept
        .iter()
        .enumerate()
        .map(|(i, (_, a))| {
            let (pos, _) = info[i];
            let (x, y) = (a.wx / a.w, a.wy / a.w);
            let (_, _, z) = grid.disc_xyz(x, y);
            let umbrae = a
                .umbrae
                .iter()
                .filter(|&&u| umbra_size[u as usize - 1] >= 2)
                .count()
                .max(1);
            Spot {
                id: 0,
                group: roots[i],
                x: x as f32,
                y: y as f32,
                lat_deg: pos.lat as f32,
                cmd_deg: pos.cmd as f32,
                lon_deg: l0.map(|l| (l + pos.cmd).rem_euclid(360.0) as f32),
                mu: z as f32,
                umbrae,
                umbra_px: a.umbra_px,
                penumbra_px: a.px - a.umbra_px,
                projected_msh: (a.px as f64 * MSH / hemisphere) as f32,
                area_msh: to_msh(a.w),
                umbra_msh: to_msh(a.umbra_w),
                darkest: a.darkest,
            }
        })
        .collect();
    let label_of: Vec<(u32, usize)> = kept
        .iter()
        .enumerate()
        .map(|(i, (l, _))| (*l as u32, roots[i]))
        .collect();
    let mut idx: Vec<usize> = (0..spots.len()).collect();
    idx.sort_by(|&a, &b| {
        spots[a]
            .group
            .cmp(&spots[b].group)
            .then(spots[b].area_msh.total_cmp(&spots[a].area_msh))
    });
    let mut sorted: Vec<Spot> = idx.iter().map(|&i| spots[i].clone()).collect();
    for (n, s) in sorted.iter_mut().enumerate() {
        s.id = n + 1;
    }
    spots = sorted;

    let group_count = order.len();
    let groups: Vec<Group> = (1..=group_count)
        .map(|g| {
            let members: Vec<&Spot> = spots.iter().filter(|s| s.group == g).collect();
            let area: f32 = members.iter().map(|s| s.area_msh).sum();
            let weighted = |f: &dyn Fn(&Spot) -> f32| -> f32 {
                members.iter().map(|s| f(s) * s.area_msh).sum::<f32>() / area.max(1e-9)
            };
            Group {
                id: g,
                spots: members.len(),
                umbrae: members.iter().map(|s| s.umbrae).sum(),
                lat_deg: weighted(&|s| s.lat_deg),
                cmd_deg: weighted(&|s| s.cmd_deg),
                lon_deg: l0
                    .map(|l| (l + f64::from(weighted(&|s| s.cmd_deg))).rem_euclid(360.0) as f32),
                projected_msh: members.iter().map(|s| s.projected_msh).sum(),
                area_msh: area,
                umbra_msh: members.iter().map(|s| s.umbra_msh).sum(),
            }
        })
        .collect();
    let spot_count: usize = spots.iter().map(|s| s.umbrae).sum();
    let wolf = params.k * (10.0 * group_count as f32 + spot_count as f32);

    let mut notes = Vec::new();
    if when.is_none() {
        notes.push(
            "No capture time: B0 is taken as 0 and longitudes are measured from the central meridian only"
                .to_owned(),
        );
    }
    if params.rotate_deg == 0.0 && !params.celestial_north_up {
        notes.push(
            "Solar north is assumed to be up; give the rotation of the image if it is not"
                .to_owned(),
        );
    }
    if too_big > 0 {
        notes.push(format!(
            "{too_big} dark region{} larger than {:.0} millionths of the hemisphere left out: a cloud, vignette or uneven flat, not a spot",
            if too_big == 1 { "" } else { "s" },
            params.max_msh
        ));
    }
    notes.push(format!(
        "Spots nearer the limb than {:.0}% of the radius are not measured",
        LIMB_MARGIN * 100.0
    ));

    let report = Report {
        when_unix: when,
        ephemeris: ephem.map(Ephemeris3::from),
        disc: Geometry {
            cx: plan.cx,
            cy: plan.cy,
            radius,
        },
        quiet,
        params: *params,
        north_angle_deg: north,
        group_count,
        spot_count,
        wolf,
        projected_msh: spots.iter().map(|s| s.projected_msh).sum(),
        area_msh: spots.iter().map(|s| s.area_msh).sum(),
        umbra_msh: spots.iter().map(|s| s.umbra_msh).sum(),
        spots,
        groups,
        notes,
    };
    let overlay = draw_overlay(
        &flat, &plan, quiet, &labels, &ulabels, &label_of, &report, &grid,
    );
    Some(Detection { report, overlay })
}

impl Report {
    /// The spot table as CSV: one row per spot, a header row first.
    #[must_use]
    pub fn csv(&self) -> String {
        let mut s = String::from(
            "spot,group,x_px,y_px,lat_deg,cmd_deg,lon_deg,mu,umbrae,umbra_px,penumbra_px,projected_msh,area_msh,umbra_msh,darkest\n",
        );
        for p in &self.spots {
            let lon = p.lon_deg.map_or(String::new(), |l| format!("{l:.2}"));
            let _ = writeln!(
                s,
                "{},{},{:.1},{:.1},{:.2},{:.2},{},{:.3},{},{},{},{:.1},{:.1},{:.1},{:.3}",
                p.id,
                p.group,
                p.x,
                p.y,
                p.lat_deg,
                p.cmd_deg,
                lon,
                p.mu,
                p.umbrae,
                p.umbra_px,
                p.penumbra_px,
                p.projected_msh,
                p.area_msh,
                p.umbra_msh,
                p.darkest
            );
        }
        s
    }

    /// The whole report as JSON text.
    #[must_use]
    pub fn json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// The report in lines, as the command line prints it.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut v = vec![format!(
            "{} group{}, {} spot{}; Wolf number {:.1} (k = {})",
            self.group_count,
            if self.group_count == 1 { "" } else { "s" },
            self.spot_count,
            if self.spot_count == 1 { "" } else { "s" },
            self.wolf,
            self.params.k
        )];
        v.push(format!(
            "area {:.0} millionths of the hemisphere corrected ({:.0} as seen), umbra {:.0}",
            self.area_msh, self.projected_msh, self.umbra_msh
        ));
        if let Some(e) = &self.ephemeris {
            v.push(format!(
                "P {:.2}, B0 {:.2}, L0 {:.2} degrees",
                e.p_deg, e.b0_deg, e.l0_deg
            ));
        }
        for g in &self.groups {
            let lon = g
                .lon_deg
                .map_or(format!("CMD {:+.0}", g.cmd_deg), |l| format!("L {l:.0}"));
            v.push(format!(
                "group {}: {} spot{}, {} umbra{}, lat {:+.0}, {lon}, {:.0} MSH ({:.0} as seen)",
                g.id,
                g.spots,
                if g.spots == 1 { "" } else { "s" },
                g.umbrae,
                if g.umbrae == 1 { "" } else { "e" },
                g.lat_deg,
                g.area_msh,
                g.projected_msh
            ));
        }
        v
    }
}

// A 3 by 5 bitmap of each digit, one row per three bits, top first.
const DIGITS: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111],
    [0b010, 0b110, 0b010, 0b010, 0b111],
    [0b111, 0b001, 0b111, 0b100, 0b111],
    [0b111, 0b001, 0b111, 0b001, 0b111],
    [0b101, 0b101, 0b111, 0b001, 0b001],
    [0b111, 0b100, 0b111, 0b001, 0b111],
    [0b111, 0b100, 0b111, 0b101, 0b111],
    [0b111, 0b001, 0b001, 0b001, 0b001],
    [0b111, 0b101, 0b111, 0b101, 0b111],
    [0b111, 0b101, 0b111, 0b001, 0b111],
];

fn put(img: &mut RgbImage, x: i64, y: i64, c: [f32; 3]) {
    if x >= 0 && y >= 0 && (x as usize) < img.width && (y as usize) < img.height {
        let i = 3 * (y as usize * img.width + x as usize);
        img.data[i..i + 3].copy_from_slice(&c);
    }
}

fn draw_number(img: &mut RgbImage, n: usize, x: i64, y: i64, scale: i64, c: [f32; 3]) {
    for (k, ch) in n.to_string().bytes().enumerate() {
        let glyph = DIGITS[usize::from(ch - b'0')];
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..3 {
                if bits >> (2 - col) & 1 == 1 {
                    for sy in 0..scale {
                        for sx in 0..scale {
                            put(
                                img,
                                x + (k as i64 * 4 + col) * scale + sx,
                                y + row as i64 * scale + sy,
                                c,
                            );
                        }
                    }
                }
            }
        }
    }
}

/// The flattened disc, stretched, with the limb, each spot's penumbra and
/// umbra outlined, its group number beside it and an arrow to solar north.
#[allow(
    clippy::too_many_arguments,
    reason = "everything the picture is made of"
)]
fn draw_overlay(
    flat: &Frame,
    plan: &limbdark::FlattenPlan,
    quiet: f32,
    labels: &[u32],
    ulabels: &[u32],
    label_of: &[(u32, usize)],
    report: &Report,
    grid: &Grid,
) -> RgbImage {
    let (w, h) = (flat.width, flat.height);
    let mut img = RgbImage {
        width: w,
        height: h,
        data: vec![0.0; 3 * w * h],
    };
    // Quiet disc at 0.7, so the umbra and the faculae both have room.
    let scale = 0.7 / quiet.max(1e-9);
    for (i, &v) in flat.data.iter().enumerate() {
        let g = ((v - plan.background) * scale).clamp(0.0, 1.0);
        img.data[3 * i..3 * i + 3].fill(g);
    }
    let kept = |l: u32| label_of.iter().any(|k| k.0 == l);
    let edge = |lab: &[u32], x: usize, y: usize| -> bool {
        let l = lab[y * w + x];
        l != 0
            && [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)]
                .iter()
                .any(|&(dx, dy)| {
                    let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                    nx < 0
                        || ny < 0
                        || nx >= w as i32
                        || ny >= h as i32
                        || lab[ny as usize * w + nx as usize] != l
                })
    };
    for y in 0..h {
        for x in 0..w {
            let l = labels[y * w + x];
            if l != 0 && kept(l) && edge(labels, x, y) {
                put(&mut img, x as i64, y as i64, [1.0, 0.85, 0.1]);
            }
        }
    }
    for y in 0..h {
        for x in 0..w {
            let l = ulabels[y * w + x];
            if l != 0 && labels[y * w + x] != 0 && kept(labels[y * w + x]) && edge(ulabels, x, y) {
                put(&mut img, x as i64, y as i64, [1.0, 0.15, 0.1]);
            }
        }
    }
    // The limb.
    let steps = (plan.radius * 8.0) as usize;
    for k in 0..steps {
        let t = k as f32 / steps as f32 * std::f32::consts::TAU;
        put(
            &mut img,
            (plan.cx + plan.radius * t.cos()).round() as i64,
            (plan.cy + plan.radius * t.sin()).round() as i64,
            [0.2, 0.8, 1.0],
        );
    }
    // Group numbers beside the largest spot of each group.
    let text = (plan.radius / 150.0).round().clamp(2.0, 8.0) as i64;
    for g in &report.groups {
        if let Some(s) = report.spots.iter().find(|s| s.group == g.id) {
            draw_number(
                &mut img,
                g.id,
                s.x.round() as i64 + 3 * text,
                s.y.round() as i64 - 3 * text,
                text,
                [0.3, 1.0, 0.4],
            );
        }
    }
    // An arrow from the disc centre towards solar north.
    let a = f64::from(report.north_angle_deg).to_radians();
    let (mut ux, uy) = (-a.sin(), -a.cos());
    if grid.mirror {
        ux = -ux;
    }
    let len = f64::from(plan.radius) * 0.18;
    for k in 0..=(len as i64) {
        let t = k as f64;
        put(
            &mut img,
            (f64::from(plan.cx) + ux * t).round() as i64,
            (f64::from(plan.cy) + uy * t).round() as i64,
            [1.0, 1.0, 1.0],
        );
    }
    img
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use super::*;
    use crate::testutil::Rng;

    /// One synthetic spot: heliographic position and the angular radii of its
    /// umbra and penumbra.
    struct Truth {
        lat: f64,
        cmd: f64,
        umbra: f64,
        pen: f64,
    }

    /// A limb-darkened disc of radius `r` with the spots, seen with B0 = 0 and
    /// solar north up, blurred and noisy.
    fn render(size: usize, r: f32, spots: &[Truth], noise: f32, seed: u64) -> Frame {
        let c = size as f32 / 2.0;
        let mut data = vec![0.02f32; size * size];
        for y in 0..size {
            for x in 0..size {
                let (dx, dy) = (x as f32 - c, c - y as f32);
                let u2 = (dx * dx + dy * dy) / (r * r);
                if u2 >= 1.0 {
                    continue;
                }
                let mu = (1.0 - u2).sqrt();
                let base = 1.0 - 0.6 * (1.0 - mu) - 0.2 * (1.0 - mu).powi(2);
                let mut f = 1.0f32;
                let (px, py, pz) = (f64::from(dx / r), f64::from(dy / r), f64::from(mu));
                for s in spots {
                    let (la, lo) = (s.lat.to_radians(), s.cmd.to_radians());
                    let (sx, sy, sz) = (la.cos() * lo.sin(), la.sin(), la.cos() * lo.cos());
                    let ang = (px * sx + py * sy + pz * sz).clamp(-1.0, 1.0).acos();
                    if ang < s.umbra.to_radians() {
                        f = f.min(0.3);
                    } else if ang < s.pen.to_radians() {
                        f = f.min(0.75);
                    }
                }
                data[y * size + x] = base * f;
            }
        }
        let taps = gaussian(1.2);
        let mut data = conv::separable(&data, size, size, &taps, 1);
        let mut rng = Rng::new(seed);
        for v in &mut data {
            *v += noise * rng.gauss();
        }
        Frame::new(size, size, data).unwrap()
    }

    fn cap_msh(deg: f64) -> f64 {
        1e6 * (1.0 - deg.to_radians().cos())
    }

    fn truth(lat: f64, cmd: f64, pen: f64) -> Truth {
        Truth {
            lat,
            cmd,
            umbra: pen * 0.4,
            pen,
        }
    }

    #[test]
    fn isolated_spots_are_counted_and_measured() {
        let spots = [
            truth(15.0, -30.0, 3.0),
            truth(-12.0, 25.0, 2.5),
            truth(20.0, 70.0, 3.5),
            truth(-18.0, -62.0, 3.0),
        ];
        let img = render(900, 400.0, &spots, 0.004, 3);
        let d = detect(&img, None, &Params::default()).expect("a disc");
        let r = &d.report;
        assert_eq!(r.group_count, 4, "{:?}", r.lines());
        assert_eq!(r.spot_count, 4);
        assert!((r.wolf - 44.0).abs() < 1e-3);
        for t in &spots {
            let near = r
                .spots
                .iter()
                .min_by(|a, b| {
                    let da = (f64::from(a.lat_deg) - t.lat).hypot(f64::from(a.cmd_deg) - t.cmd);
                    let db = (f64::from(b.lat_deg) - t.lat).hypot(f64::from(b.cmd_deg) - t.cmd);
                    da.total_cmp(&db)
                })
                .unwrap();
            assert!((f64::from(near.lat_deg) - t.lat).abs() < 0.5, "{near:?}");
            assert!((f64::from(near.cmd_deg) - t.cmd).abs() < 0.5, "{near:?}");
            let want = cap_msh(t.pen);
            let got = f64::from(near.area_msh);
            assert!((got / want - 1.0).abs() < 0.05, "{got} against {want}");
            let want_u = cap_msh(t.umbra);
            let got_u = f64::from(near.umbra_msh);
            assert!(
                (got_u / want_u - 1.0).abs() < 0.15,
                "{got_u} against {want_u}"
            );
        }
        // Near the limb the spot is foreshortened: seen smaller than it is.
        let limb = r.spots.iter().find(|s| s.cmd_deg > 60.0).unwrap();
        assert!(limb.projected_msh < 0.6 * limb.area_msh, "{limb:?}");
    }

    #[test]
    fn neighbours_form_a_group_and_distant_spots_do_not() {
        let spots = [
            truth(10.0, -20.0, 2.5),
            truth(11.0, -13.0, 2.0),
            truth(9.0, -8.0, 2.0),
            truth(-15.0, 30.0, 2.5),
        ];
        let img = render(900, 400.0, &spots, 0.004, 5);
        let d = detect(&img, None, &Params::default()).unwrap();
        assert_eq!(d.report.group_count, 2, "{:?}", d.report.lines());
        assert_eq!(d.report.groups[0].spots, 3);
        assert_eq!(d.report.groups[1].spots, 1);
        // A wider gap joins the lot; a zero gap keeps the touching ones only.
        let loose = Params {
            group_gap_deg: 60.0,
            ..Params::default()
        };
        assert_eq!(detect(&img, None, &loose).unwrap().report.group_count, 1);
    }

    #[test]
    fn a_clean_disc_has_no_spots() {
        let img = render(700, 300.0, &[], 0.004, 9);
        let d = detect(&img, None, &Params::default()).unwrap();
        assert_eq!(d.report.spot_count, 0, "{:?}", d.report.lines());
        assert!(d.report.wolf.abs() < 1e-6);
    }

    #[test]
    fn two_umbrae_in_one_penumbra_are_two_spots() {
        // Overlapping penumbrae make one region; the two cores stay apart.
        let spots = [
            Truth {
                lat: 5.0,
                cmd: -3.0,
                umbra: 1.0,
                pen: 4.0,
            },
            Truth {
                lat: 5.0,
                cmd: 3.0,
                umbra: 1.0,
                pen: 4.0,
            },
        ];
        let img = render(900, 400.0, &spots, 0.003, 4);
        let d = detect(&img, None, &Params::default()).unwrap();
        assert_eq!(d.report.group_count, 1, "{:?}", d.report.lines());
        assert_eq!(d.report.spots.len(), 1);
        assert_eq!(d.report.spot_count, 2, "{:?}", d.report.lines());
        assert!((d.report.wolf - 12.0).abs() < 1e-3);
    }

    #[test]
    fn the_date_gives_carrington_longitude_and_pole_tilt() {
        let img = render(800, 350.0, &[truth(10.0, 20.0, 3.0)], 0.003, 2);
        // 2022-07-17 15:30 UTC.
        let when = 1_658_071_800.0;
        let d = detect(&img, Some(when), &Params::default()).unwrap();
        let e = d.report.ephemeris.unwrap();
        let s = &d.report.spots[0];
        let lon = s.lon_deg.unwrap();
        let want = (f64::from(e.l0_deg) + 20.0).rem_euclid(360.0);
        // B0 is not zero on that date, so the position differs from the B0 = 0 render.
        assert!(e.b0_deg.abs() > 1.0);
        assert!((f64::from(lon) - want).abs() < 6.0, "{lon} {want}");
    }

    #[test]
    fn csv_and_json_have_a_row_per_spot() {
        let img = render(700, 300.0, &[truth(10.0, 10.0, 3.0)], 0.003, 7);
        let d = detect(&img, None, &Params::default()).unwrap();
        assert_eq!(d.report.csv().lines().count(), 2);
        let back: Report = serde_json::from_str(&d.report.json()).unwrap();
        assert_eq!(back.spots.len(), 1);
        assert_eq!(d.overlay.width, 700);
    }
}
