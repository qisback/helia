// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! What each alignment point measured over the capture, summarised per point:
//! how often it locked, how sharp its window was, how far it moved from the
//! global shift, and (with the residual pass) how far it is from the finished
//! stack. These are the numbers behind the alignment point heatmaps and the
//! shift-field plots of the science report.

use super::Shift;
use super::multipoint::{ApGrid, LocalTable};

/// One alignment point over the registered frames.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ApStat {
    /// Index into the grid.
    pub index: usize,
    /// Window centre, x.
    pub cx: usize,
    /// Window centre, y.
    pub cy: usize,
    /// False for a window skipped as featureless (it measured nothing).
    pub active: bool,
    /// Contrast of the reference window.
    pub contrast: f32,
    /// Frames in which the point was measured.
    pub measured: usize,
    /// Mean sharpness of the window over the frames it was measured in.
    pub quality_mean: f32,
    /// Mean sharpness over the frames this point stacks (its best ones).
    pub quality_kept: f32,
    /// Mean of the point's shift less the frame's global shift, over the
    /// frames it was measured in: the static displacement of the patch.
    pub warp_mean: Shift,
    /// RMS over those frames of the distance of that difference from its
    /// mean, in pixels: the part that changes from frame to frame (seeing).
    pub warp_rms: f32,
    /// Mean of the shift used for the stack less the shift measured against
    /// the finished stack; only with the residual pass.
    pub residual_mean: Option<Shift>,
    /// RMS of that difference over the frames, in pixels.
    pub residual_rms: Option<f32>,
}

/// The statistics of every point of a grid.
#[derive(Clone, Debug, PartialEq)]
pub struct ApStats {
    /// Registered frames (rows of the table).
    pub frames: usize,
    /// Frames each point stacks.
    pub keep: usize,
    /// One entry per grid point, in grid order.
    pub aps: Vec<ApStat>,
}

/// Summarise `table` (one row per registered frame, `global` the global shift
/// of each) for every point of `grid`; `keep` is how many frames each point
/// stacks.
#[must_use]
pub fn ap_stats(grid: &ApGrid, table: &LocalTable, global: &[Shift], keep: usize) -> ApStats {
    let frames = global.len();
    let n = table.n_aps;
    let aps = grid
        .aps
        .iter()
        .enumerate()
        .map(|(a, ap)| {
            let mut q_sum = 0.0f64;
            let mut count = 0usize;
            let (mut sx, mut sy) = (0.0f64, 0.0f64);
            let mut diffs: Vec<Shift> = Vec::new();
            for (f, g) in global.iter().enumerate() {
                let q = table.quality[f * n + a];
                if !q.is_finite() {
                    continue;
                }
                let d = table.shifts[f * n + a] - *g;
                q_sum += f64::from(q);
                sx += f64::from(d.dx);
                sy += f64::from(d.dy);
                diffs.push(d);
                count += 1;
            }
            let (mean, rms) = if count > 0 {
                let m = Shift::new((sx / count as f64) as f32, (sy / count as f64) as f32);
                let var =
                    diffs.iter().map(|d| (*d - m).length().powi(2)).sum::<f32>() / count as f32;
                (m, var.sqrt())
            } else {
                (Shift::default(), 0.0)
            };
            let best = table.best_frames(frames, a, keep);
            let quality_kept = if best.is_empty() {
                0.0
            } else {
                best.iter().map(|&f| table.quality[f * n + a]).sum::<f32>() / best.len() as f32
            };
            ApStat {
                index: a,
                cx: ap.cx,
                cy: ap.cy,
                active: ap.active,
                contrast: ap.contrast,
                measured: count,
                quality_mean: if count > 0 {
                    (q_sum / count as f64) as f32
                } else {
                    0.0
                },
                quality_kept,
                warp_mean: mean,
                warp_rms: rms,
                residual_mean: None,
                residual_rms: None,
            }
        })
        .collect();
    ApStats { frames, keep, aps }
}

impl ApStats {
    /// Add the residual of the shifts in `used` against `against`, a table
    /// measured on the finished stack: per point, the mean and RMS over the
    /// frames both measured. Returns the RMS over all points and frames.
    pub fn add_residual(&mut self, used: &LocalTable, against: &LocalTable) -> Option<f32> {
        let n = used.n_aps.min(against.n_aps);
        let (mut all, mut total) = (0.0f64, 0u64);
        for (a, stat) in self.aps.iter_mut().enumerate().take(n) {
            let (mut sx, mut sy, mut sq, mut count) = (0.0f64, 0.0f64, 0.0f64, 0u64);
            for f in 0..self.frames {
                let i = f * used.n_aps + a;
                let j = f * against.n_aps + a;
                if i >= used.quality.len() || j >= against.quality.len() {
                    continue;
                }
                if used.quality[i].is_finite() && against.quality[j].is_finite() {
                    let d = used.shifts[i] - against.shifts[j];
                    sx += f64::from(d.dx);
                    sy += f64::from(d.dy);
                    sq += f64::from(d.length()).powi(2);
                    count += 1;
                }
            }
            if count > 0 {
                let c = count as f64;
                stat.residual_mean = Some(Shift::new((sx / c) as f32, (sy / c) as f32));
                stat.residual_rms = Some((sq / c).sqrt() as f32);
                all += sq;
                total += count;
            }
        }
        (total > 0).then(|| (all / total as f64).sqrt() as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::align::multipoint::{ApMeasure, MultipointConfig};
    use crate::testutil::{SyntheticConfig, SyntheticSun};

    fn grid() -> ApGrid {
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let r = sun.render_clean(Shift::default(), None);
        let cfg = MultipointConfig {
            ap_size: 32,
            ..MultipointConfig::default()
        };
        ApGrid::build(&r, &cfg).expect("grid")
    }

    fn measure(dx: f32, dy: f32, q: f32) -> ApMeasure {
        ApMeasure {
            shift: Shift::new(dx, dy),
            quality: q,
            correlation: 1.0,
            interpolated: false,
        }
    }

    #[test]
    fn a_static_offset_is_the_mean_and_the_scatter_is_the_rms() {
        let g = grid();
        let n = g.aps.len();
        let global: Vec<Shift> = (0..6).map(|f| Shift::new(f as f32, 0.0)).collect();
        let mut t = LocalTable::new(6, n);
        for f in 0..6 {
            // Point 0 sits 2 px right of the global shift and wobbles by +-0.5 in y.
            let wobble = if f % 2 == 0 { 0.5 } else { -0.5 };
            let mut row = vec![None; n];
            row[0] = Some(measure(f as f32 + 2.0, wobble, 1.0 + f as f32));
            t.set_row(f, &row);
        }
        let s = ap_stats(&g, &t, &global, 2);
        let first = &s.aps[0];
        assert_eq!(first.measured, 6);
        assert!((first.warp_mean.dx - 2.0).abs() < 1e-5 && first.warp_mean.dy.abs() < 1e-5);
        assert!((first.warp_rms - 0.5).abs() < 1e-5, "{}", first.warp_rms);
        assert!((first.quality_mean - 3.5).abs() < 1e-5);
        assert!(
            (first.quality_kept - 5.5).abs() < 1e-5,
            "the best two of 1..6 are 6 and 5"
        );
        assert_eq!(s.aps[1].measured, 0);
    }

    #[test]
    fn the_residual_is_the_difference_to_the_second_table() {
        let g = grid();
        let n = g.aps.len();
        let global = vec![Shift::default(); 4];
        let (mut used, mut against) = (LocalTable::new(4, n), LocalTable::new(4, n));
        for f in 0..4 {
            let mut row_used = vec![None; n];
            let mut row_against = vec![None; n];
            row_used[0] = Some(measure(1.0, 0.0, 1.0));
            row_against[0] = Some(measure(0.4, 0.0, 1.0));
            used.set_row(f, &row_used);
            against.set_row(f, &row_against);
        }
        let mut s = ap_stats(&g, &used, &global, 2);
        let all = s.add_residual(&used, &against).expect("residual");
        assert!((all - 0.6).abs() < 1e-5);
        let r = s.aps[0].residual_mean.expect("mean");
        assert!((r.dx - 0.6).abs() < 1e-5 && r.dy.abs() < 1e-6);
        assert!(s.aps[1].residual_rms.is_none());
    }
}
