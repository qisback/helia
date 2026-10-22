// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The alignment point motion index: a relative seeing index from the shifts
//! the alignment points already measure.
//!
//! The shift of a point is the image motion of its patch of the Sun. Tip and
//! tilt of the whole frame (and tracking and wind shake) move every patch
//! together, so they cancel in the difference of two neighbouring points; what
//! is left is the differential motion between patches, which turbulence causes.
//! The index is the RMS of that difference per axis, over many frames and
//! neighbouring pairs, in pixels (and arcseconds with the scale).
//!
//! It is **not** an absolute `r0`. The patches look through the atmosphere
//! at different field angles, so the differential motion depends on how high
//! the turbulence is (the S-DIMM+ regime, Scharmer and van Werkhoven 2010):
//! for the same `r0`, turbulence at 5 km gives more differential motion than
//! turbulence at the ground. Use it to compare runs through the same optics at
//! the same grid spacing, and the spectral ratio for an absolute figure.
//!
//! # References
//!
//! - Scharmer, G. B. and van Werkhoven, T. I. M. (2010), Astron. Astrophys. 513, A25,
//!   doi:10.1051/0004-6361/200913791.

use crate::align::Shift;

/// The index and what it was made from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MotionIndex {
    /// RMS of the differential shift of neighbouring points, per axis, pixels.
    pub rms_px: f32,
    /// Median separation of the pairs, pixels.
    pub separation_px: f32,
    /// Pairs of points that counted.
    pub pairs: usize,
    /// Frames that counted (the most any pair had).
    pub frames: usize,
}

/// Fewest frames a pair needs to count.
pub const MIN_FRAMES: usize = 8;

/// The index over `rows`, one row per frame holding the shift of each point
/// (`None` where it did not lock); `centres` are the points' positions. Pairs
/// are the points within one and a half grid spacings of each other. `None`
/// when no pair has [`MIN_FRAMES`] frames.
#[must_use]
pub fn motion_index(centres: &[(f32, f32)], rows: &[Vec<Option<Shift>>]) -> Option<MotionIndex> {
    let n = centres.len();
    if n < 2 {
        return None;
    }
    let dist =
        |a: usize, b: usize| (centres[a].0 - centres[b].0).hypot(centres[a].1 - centres[b].1);
    let spacing = (0..n)
        .flat_map(|a| (a + 1..n).map(move |b| (a, b)))
        .map(|(a, b)| dist(a, b))
        .filter(|&d| d > 0.0)
        .fold(f32::INFINITY, f32::min);
    if !spacing.is_finite() {
        return None;
    }
    let (mut var_sum, mut pairs, mut frames) = (0.0f64, 0usize, 0usize);
    let mut seps = Vec::new();
    for a in 0..n {
        for b in a + 1..n {
            let d = dist(a, b);
            if d > 1.5 * spacing {
                continue;
            }
            let diffs: Vec<(f32, f32)> = rows
                .iter()
                .filter_map(|r| match (r[a], r[b]) {
                    (Some(p), Some(q)) => Some((p.dx - q.dx, p.dy - q.dy)),
                    _ => None,
                })
                .collect();
            if diffs.len() < MIN_FRAMES {
                continue;
            }
            // The mean difference is the static offset of the two patches
            // between the reference and the scene; only the scatter is seeing.
            let m = diffs.len() as f32;
            let (mx, my) = (
                diffs.iter().map(|d| d.0).sum::<f32>() / m,
                diffs.iter().map(|d| d.1).sum::<f32>() / m,
            );
            let var = diffs
                .iter()
                .map(|d| f64::from((d.0 - mx).powi(2) + (d.1 - my).powi(2)))
                .sum::<f64>()
                / f64::from(m - 1.0);
            var_sum += var;
            pairs += 1;
            frames = frames.max(diffs.len());
            seps.push(d);
        }
    }
    if pairs == 0 {
        return None;
    }
    seps.sort_by(f32::total_cmp);
    Some(MotionIndex {
        rms_px: ((var_sum / pairs as f64) / 2.0).sqrt() as f32,
        separation_px: seps[seps.len() / 2],
        pairs,
        frames,
    })
}

/// Differential motion of each frame, in pixels per axis, on the scale of
/// [`MotionIndex::rms_px`]: for every neighbouring pair of points the frame's
/// distance from the pair's mean offset, squared, averaged over the pairs
/// that locked in the frame, halved, and rooted. `None` for a frame in which no
/// pair locked. The RMS of the values over the frames is close to the index.
/// Same pairs and same static offsets as [`motion_index`], so the two agree.
#[must_use]
pub fn motion_series(centres: &[(f32, f32)], rows: &[Vec<Option<Shift>>]) -> Vec<Option<f32>> {
    let none = vec![None; rows.len()];
    let n = centres.len();
    if n < 2 {
        return none;
    }
    let dist =
        |a: usize, b: usize| (centres[a].0 - centres[b].0).hypot(centres[a].1 - centres[b].1);
    let spacing = (0..n)
        .flat_map(|a| (a + 1..n).map(move |b| (a, b)))
        .map(|(a, b)| dist(a, b))
        .filter(|&d| d > 0.0)
        .fold(f32::INFINITY, f32::min);
    if !spacing.is_finite() {
        return none;
    }
    let mut sum = vec![0.0f64; rows.len()];
    let mut count = vec![0u32; rows.len()];
    for a in 0..n {
        for b in a + 1..n {
            if dist(a, b) > 1.5 * spacing {
                continue;
            }
            let diffs: Vec<(usize, f32, f32)> = rows
                .iter()
                .enumerate()
                .filter_map(|(i, r)| match (r[a], r[b]) {
                    (Some(p), Some(q)) => Some((i, p.dx - q.dx, p.dy - q.dy)),
                    _ => None,
                })
                .collect();
            if diffs.len() < MIN_FRAMES {
                continue;
            }
            let m = diffs.len() as f32;
            let (mx, my) = (
                diffs.iter().map(|d| d.1).sum::<f32>() / m,
                diffs.iter().map(|d| d.2).sum::<f32>() / m,
            );
            for &(i, dx, dy) in &diffs {
                sum[i] += f64::from((dx - mx).powi(2) + (dy - my).powi(2));
                count[i] += 1;
            }
        }
    }
    sum.iter()
        .zip(&count)
        .map(|(&s, &c)| (c > 0).then(|| ((s / f64::from(c)) / 2.0).sqrt() as f32))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seeing::screen::{GRID, PUPIL, Rng, kolmogorov_screen, pupil_pixels};

    /// Phase gradient (radians per pupil pixel) over the pupil window at `(ox, oy)`.
    fn tilt(screen: &[f32], ox: usize, oy: usize, pix: &[(usize, usize)]) -> (f32, f32) {
        let c = (PUPIL as f32 - 1.0) / 2.0;
        let at = |x: usize, y: usize| screen[(oy + y) * GRID + ox + x];
        let mean = pix.iter().map(|&(x, y)| at(x, y)).sum::<f32>() / pix.len() as f32;
        let (mut sx, mut sy, mut sxx, mut syy) = (0.0, 0.0, 0.0, 0.0);
        for &(x, y) in pix {
            let (dx, dy) = (x as f32 - c, y as f32 - c);
            let v = at(x, y) - mean;
            sx += v * dx;
            sy += v * dy;
            sxx += dx * dx;
            syy += dy * dy;
        }
        (sx / sxx, sy / syy)
    }

    type Rows = Vec<Vec<Option<Shift>>>;

    /// Shifts of a `side` x `side` grid of points over `frames` frames of
    /// turbulence of `rho = D / r0` in layers: `(share of the turbulence,
    /// footprint offset per grid step in pupil pixels)`. Every point of a frame
    /// shares the common tilt and tip, as a real frame does.
    fn simulate(
        rho: f32,
        layers: &[(f32, usize)],
        frames: usize,
        side: usize,
        seed: u64,
    ) -> (Vec<(f32, f32)>, Rows) {
        let pix = pupil_pixels();
        let mut rng = Rng::new(seed);
        let centres: Vec<(f32, f32)> = (0..side * side)
            .map(|i| ((i % side) as f32 * 64.0, (i / side) as f32 * 64.0))
            .collect();
        let mut rows = Vec::new();
        for _ in 0..frames {
            let mut shifts = vec![(0.0f32, 0.0f32); side * side];
            for &(share, step) in layers {
                // A layer carrying `share` of the turbulence has r0 larger by share^(-3/5).
                let r0 = (PUPIL as f32 / rho) * share.powf(-0.6);
                let screen = kolmogorov_screen(&mut rng, r0);
                for (i, s) in shifts.iter_mut().enumerate() {
                    let (gx, gy) = ((i % side) * step, (i / side) * step);
                    let (tx, ty) = tilt(&screen, gx, gy, &pix);
                    s.0 += tx;
                    s.1 += ty;
                }
            }
            rows.push(
                shifts
                    .iter()
                    .map(|&(x, y)| Some(Shift::new(x, y)))
                    .collect(),
            );
        }
        (centres, rows)
    }

    fn index(rho: f32, layers: &[(f32, usize)], seed: u64) -> f32 {
        let (c, r) = simulate(rho, layers, 120, 3, seed);
        motion_index(&c, &r).expect("index").rms_px
    }

    #[test]
    fn a_common_shift_and_a_fixed_offset_cancel() {
        let centres = [(0.0, 0.0), (64.0, 0.0), (0.0, 64.0), (64.0, 64.0)];
        let mut rng = Rng::new(1);
        let rows: Vec<Vec<Option<Shift>>> = (0..40)
            .map(|_| {
                let common = Shift::new(3.0 * rng.gauss(), 3.0 * rng.gauss());
                centres
                    .iter()
                    .enumerate()
                    .map(|(i, _)| Some(common + Shift::new(i as f32 * 0.7, -(i as f32) * 0.4)))
                    .collect()
            })
            .collect();
        let m = motion_index(&centres, &rows).expect("index");
        assert!(m.rms_px < 1e-4, "{m:?}");
        assert_eq!((m.pairs, m.frames), (6, 40));
        assert!((m.separation_px - 64.0).abs() < 1e-3);
    }

    #[test]
    fn points_that_did_not_lock_are_left_out_and_few_frames_give_nothing() {
        let centres = [(0.0, 0.0), (64.0, 0.0)];
        let rows: Vec<Vec<Option<Shift>>> = (0..30)
            .map(|k| {
                vec![
                    Some(Shift::new(k as f32 * 0.1, 0.0)),
                    (k % 2 == 0).then(|| Shift::new(0.0, 0.0)),
                ]
            })
            .collect();
        assert_eq!(motion_index(&centres, &rows).expect("index").frames, 15);
        assert!(motion_index(&centres, &rows[..6]).is_none());
        assert!(motion_index(&centres[..1], &rows).is_none());
    }

    #[test]
    fn the_series_rms_matches_the_index() {
        let (c, r) = simulate(6.0, &[(1.0, 16)], 60, 3, 9);
        let idx = motion_index(&c, &r).expect("index").rms_px;
        let ser = motion_series(&c, &r);
        assert_eq!(ser.len(), 60);
        let v: Vec<f32> = ser.iter().flatten().copied().collect();
        let rms = (v.iter().map(|x| x * x).sum::<f32>() / v.len() as f32).sqrt();
        assert!((rms - idx).abs() < 0.1 * idx, "{rms} vs {idx}");
    }

    #[test]
    fn the_index_ranks_the_true_r0_and_follows_the_five_sixths_law() {
        // One layer 5 km up: neighbouring points look through footprints 16 pupil pixels apart.
        let layers = [(1.0f32, 16usize)];
        let sweep = [3.0f32, 4.5, 6.0, 8.0, 10.0, 12.0];
        let idx: Vec<f32> = sweep.iter().map(|&r| index(r, &layers, 3)).collect();
        eprintln!("index against D/r0 {sweep:?}: {idx:?}");
        assert!(
            idx.windows(2).all(|w| w[1] > w[0]),
            "not monotonic: {idx:?}"
        );
        // Differential tilt variance goes as r0^(-5/3), so the RMS as (D/r0)^(5/6).
        let slope = (idx[5] / idx[0]).ln() / (sweep[5] / sweep[0]).ln();
        eprintln!("log-log slope {slope:.2} (theory 0.83)");
        assert!((slope - 5.0 / 6.0).abs() < 0.15, "{slope}");
    }

    #[test]
    fn it_depends_on_how_high_the_turbulence_is() {
        // The same total r0: all at the ground (no differential motion between
        // footprints), half and half, all at height.
        let rho = 6.0;
        let ground = index(rho, &[(1.0, 0)], 5);
        let mixed = index(rho, &[(0.5, 0), (0.5, 16)], 5);
        let high = index(rho, &[(1.0, 16)], 5);
        eprintln!("ground {ground:.4}, half and half {mixed:.4}, high {high:.4}");
        assert!(ground < mixed && mixed < high);
    }
}
