// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Refining the alignment point shifts (issue #4).
//!
//! Two independent steps, both on the [`LocalTable`] the registration fills:
//!
//! * [`smooth_table`] suppresses outliers in each frame's shift field. A
//!   point whose shift is further from the median of its neighbours than the
//!   neighbours disagree among themselves is a mismatch (a window that locked
//!   on a neighbouring granule, or on noise), and takes the neighbours' median.
//!   A genuine strong local distortion varies smoothly, so its neighbours
//!   spread and nothing is replaced.
//! * The second pass (in the pipeline) re-measures the table against the
//!   first stack instead of the five-frame reference. The stack has far less
//!   noise and more detail, so its windows lock more reliably.

use rayon::prelude::*;

use crate::align::Shift;
use crate::align::multipoint::{ApGrid, LocalTable};

/// Outlier rejection in the shift field, see [`smooth_table`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SmoothConfig {
    /// A shift is an outlier when it is further from the neighbours' median
    /// than this many robust standard deviations of the neighbours.
    pub k: f32,
    /// ... and further than this many pixels (so a very quiet field does not
    /// reject real sub-pixel differences).
    pub floor: f32,
    /// Neighbours lie within this many grid steps.
    pub reach: f32,
}

impl Default for SmoothConfig {
    fn default() -> Self {
        Self {
            k: 3.0,
            floor: 0.3,
            reach: 1.5,
        }
    }
}

/// Fewest usable neighbours a point needs to be judged.
const MIN_NEIGHBOURS: usize = 4;

/// Grid spacing: the smallest positive distance between two point centres.
fn spacing(grid: &ApGrid) -> f32 {
    let mut best = usize::MAX;
    for pair in grid.aps.windows(2) {
        let d = pair[0].cx.abs_diff(pair[1].cx);
        if d > 0 {
            best = best.min(d);
        }
    }
    if best == usize::MAX {
        grid.size as f32 / 2.0
    } else {
        best as f32
    }
}

/// For each point the indices of the other points within `cfg.reach` steps.
#[must_use]
pub fn neighbours(grid: &ApGrid, cfg: &SmoothConfig) -> Vec<Vec<usize>> {
    let reach2 = (cfg.reach * spacing(grid)).powi(2) + 0.5;
    grid.aps
        .iter()
        .enumerate()
        .map(|(i, a)| {
            grid.aps
                .iter()
                .enumerate()
                .filter(|&(j, b)| {
                    let (dx, dy) = (a.cx as f32 - b.cx as f32, a.cy as f32 - b.cy as f32);
                    j != i && dx * dx + dy * dy <= reach2
                })
                .map(|(j, _)| j)
                .collect()
        })
        .collect()
}

fn median(v: &mut [f32]) -> f32 {
    v.sort_by(f32::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        v[n / 2 - 1].midpoint(v[n / 2])
    }
}

/// Replace the outliers of one frame's field. `shifts` and `valid` are that
/// frame's row of the table. Returns the number replaced. Judged against the
/// row as it came in, so the order of the points does not matter.
pub fn smooth_row(
    near: &[Vec<usize>],
    shifts: &mut [Shift],
    valid: &[bool],
    cfg: &SmoothConfig,
) -> usize {
    let before = shifts.to_vec();
    let mut replaced = 0;
    for (a, nb) in near.iter().enumerate() {
        if !valid[a] {
            continue;
        }
        let ok: Vec<Shift> = nb
            .iter()
            .filter(|&&b| valid[b])
            .map(|&b| before[b])
            .collect();
        if ok.len() < MIN_NEIGHBOURS {
            continue;
        }
        let mx = median(&mut ok.iter().map(|s| s.dx).collect::<Vec<_>>());
        let my = median(&mut ok.iter().map(|s| s.dy).collect::<Vec<_>>());
        let centre = Shift::new(mx, my);
        let mut dev: Vec<f32> = ok.iter().map(|s| (*s - centre).length()).collect();
        let scale = 1.4826 * median(&mut dev);
        let off = (before[a] - centre).length();
        if off > cfg.floor && off > cfg.k * scale {
            shifts[a] = centre;
            replaced += 1;
        }
    }
    replaced
}

/// [`smooth_row`] over every frame of `table`; returns the number of shifts
/// replaced. Points with no measurement (`NaN` quality) are left alone.
pub fn smooth_table(grid: &ApGrid, table: &mut LocalTable, cfg: &SmoothConfig) -> usize {
    let near = neighbours(grid, cfg);
    let n = table.n_aps;
    if n == 0 {
        return 0;
    }
    let quality = &table.quality;
    table
        .shifts
        .par_chunks_mut(n)
        .zip(quality.par_chunks(n))
        .map(|(row, q)| {
            let valid: Vec<bool> = q.iter().map(|v| v.is_finite()).collect();
            smooth_row(&near, row, &valid, cfg)
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::align::multipoint::{ApGrid, MultipointConfig};
    use crate::frame::Frame;
    use crate::testutil::Rng;

    fn grid() -> ApGrid {
        let mut f = Frame::zeros(256, 256);
        for (i, v) in f.data.iter_mut().enumerate() {
            *v = 0.5 + 0.2 * ((i % 256) as f32 * 0.3).sin() * ((i / 256) as f32 * 0.2).cos();
        }
        let cfg = MultipointConfig {
            ap_size: 32,
            min_contrast: 0.0,
            ..MultipointConfig::default()
        };
        ApGrid::build(&f, &cfg).expect("grid")
    }

    /// A smooth warp with noise, as one row of a table.
    fn field(grid: &ApGrid, rng: &mut Rng, noise: f32) -> Vec<Shift> {
        grid.aps
            .iter()
            .map(|a| {
                let (x, y) = (a.cx as f32, a.cy as f32);
                Shift::new(
                    1.5 * (x / 90.0).sin() + noise * rng.gauss(),
                    1.2 * (y / 70.0).cos() + noise * rng.gauss(),
                )
            })
            .collect()
    }

    #[test]
    fn a_wild_point_takes_its_neighbours_median_and_the_rest_stay() {
        let g = grid();
        let near = neighbours(&g, &SmoothConfig::default());
        let mut rng = Rng::new(7);
        let clean = field(&g, &mut rng, 0.03);
        let mut row = clean.clone();
        let bad = [40usize, 77, 123];
        for &b in &bad {
            row[b] = row[b] + Shift::new(2.5, -3.0);
        }
        let valid = vec![true; row.len()];
        let n = smooth_row(&near, &mut row, &valid, &SmoothConfig::default());
        assert!(n >= bad.len(), "replaced {n}");
        for &b in &bad {
            assert!((row[b] - clean[b]).length() < 0.4, "{b}: {:?}", row[b]);
        }
        let changed = row
            .iter()
            .zip(&clean)
            .filter(|(a, b)| (**a - **b).length() > 1e-6)
            .count();
        assert!(changed <= bad.len() + 4, "{changed} points changed");
    }

    #[test]
    fn a_strong_smooth_distortion_is_left_alone() {
        let g = grid();
        let near = neighbours(&g, &SmoothConfig::default());
        let mut row: Vec<Shift> = g
            .aps
            .iter()
            .map(|a| Shift::new(a.cx as f32 * 0.02, a.cy as f32 * -0.015))
            .collect();
        let want = row.clone();
        let valid = vec![true; row.len()];
        assert_eq!(
            smooth_row(&near, &mut row, &valid, &SmoothConfig::default()),
            0
        );
        assert_eq!(row, want);
    }

    #[test]
    fn unmeasured_points_are_neither_judged_nor_used() {
        let g = grid();
        let near = neighbours(&g, &SmoothConfig::default());
        let mut rng = Rng::new(3);
        let mut row = field(&g, &mut rng, 0.02);
        let mut valid = vec![true; row.len()];
        valid[10] = false;
        row[10] = Shift::new(99.0, 99.0);
        smooth_row(&near, &mut row, &valid, &SmoothConfig::default());
        assert_eq!(row[10], Shift::new(99.0, 99.0));
        assert!(
            row.iter()
                .enumerate()
                .all(|(i, s)| i == 10 || s.length() < 3.0)
        );
    }

    #[test]
    fn the_table_is_smoothed_frame_by_frame() {
        let g = grid();
        let mut rng = Rng::new(11);
        let n = g.aps.len();
        let mut t = LocalTable::new(3, n);
        for f in 0..3 {
            let mut row = field(&g, &mut rng, 0.02);
            row[50 + f] = row[50 + f] + Shift::new(3.0, 3.0);
            t.shifts[f * n..(f + 1) * n].copy_from_slice(&row);
            t.quality[f * n..(f + 1) * n].fill(1.0);
        }
        let replaced = smooth_table(&g, &mut t, &SmoothConfig::default());
        assert!(replaced >= 3, "{replaced}");
        for f in 0..3 {
            assert!(t.shifts[f * n + 50 + f].length() < 3.0);
        }
    }
}
