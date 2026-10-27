// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Editing the tone curve's control points with the pointer or keyboard.
//!
//! The rule that keeps every edit valid: a point can never pass its
//! neighbours. Its input stays at least [`GAP`] away from the next point on
//! either side, and its output stays between theirs, so the curve never falls
//! and inputs stay distinct. A point at input 0 or 1 only moves up and down.
//! The ends `(0, 0)` and `(1, 1)` count as neighbours when no point sits there.

use helia_core::edit::tone::Curve;

/// Closest two inputs may be.
pub const GAP: f32 = 0.005;

/// Point indices in order of input.
pub fn order(points: &[[f32; 2]]) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..points.len()).collect();
    idx.sort_by(|&a, &b| points[a][0].total_cmp(&points[b][0]));
    idx
}

/// The nearest point within `tol` (in curve units, per axis) of `at`.
pub fn nearest(points: &[[f32; 2]], at: [f32; 2], tol: [f32; 2]) -> Option<usize> {
    let dist = |p: &[f32; 2]| ((p[0] - at[0]) / tol[0]).powi(2) + ((p[1] - at[1]) / tol[1]).powi(2);
    points
        .iter()
        .enumerate()
        .filter(|(_, p)| dist(p) <= 1.0)
        .min_by(|(_, a), (_, b)| dist(a).total_cmp(&dist(b)))
        .map(|(i, _)| i)
}

/// Lower and upper neighbours of an input `x`, ignoring point `skip`.
fn neighbours(points: &[[f32; 2]], x: f32, skip: Option<usize>) -> ([f32; 2], [f32; 2]) {
    let mut lo = [0.0, 0.0];
    let mut hi = [1.0, 1.0];
    for (j, p) in points.iter().enumerate() {
        if Some(j) == skip {
            continue;
        }
        if p[0] < x && p[0] >= lo[0] {
            lo = *p;
        }
        if p[0] > x && p[0] <= hi[0] {
            hi = *p;
        }
    }
    (lo, hi)
}

/// True when the point sits on an end of the input range and so only moves
/// vertically.
fn pinned(p: [f32; 2]) -> bool {
    p[0] <= 0.0 || p[0] >= 1.0
}

/// Move point `i` towards `want`, held inside its neighbours. Returns true
/// when it moved.
pub fn place(points: &mut [[f32; 2]], i: usize, want: [f32; 2]) -> bool {
    let cur = points[i];
    let (lo, hi) = neighbours(points, cur[0], Some(i));
    let x = if pinned(cur) {
        cur[0]
    } else {
        let (a, b) = (lo[0] + GAP, hi[0] - GAP);
        if a > b { cur[0] } else { want[0].clamp(a, b) }
    };
    let y = want[1].clamp(lo[1], hi[1].max(lo[1]));
    let new = [x, y];
    points[i] = new;
    new.iter().zip(&cur).any(|(a, b)| a.total_cmp(b).is_ne())
}

/// Add a point near `want`, keeping the list in order of input. Returns its
/// index, or `None` when the curve is full or another point is too close.
pub fn insert(points: &mut Vec<[f32; 2]>, want: [f32; 2]) -> Option<usize> {
    if points.len() >= Curve::MAX_POINTS {
        return None;
    }
    let x = want[0].clamp(GAP, 1.0 - GAP);
    if points.iter().any(|p| (p[0] - x).abs() < GAP) {
        return None;
    }
    let (lo, hi) = neighbours(points, x, None);
    let y = want[1].clamp(lo[1], hi[1]);
    let at = points.iter().position(|p| p[0] > x).unwrap_or(points.len());
    points.insert(at, [x, y]);
    Some(at)
}

/// Remove point `i` unless it is an end point. Returns true when removed.
pub fn remove(points: &mut Vec<[f32; 2]>, i: usize) -> bool {
    if i >= points.len() || pinned(points[i]) {
        return false;
    }
    points.remove(i);
    true
}

/// The point after (`forward`) or before `i` in order of input, wrapping.
pub fn step_selection(points: &[[f32; 2]], i: usize, forward: bool) -> usize {
    let ord = order(points);
    let n = ord.len();
    let at = ord.iter().position(|&j| j == i).unwrap_or(0);
    ord[if forward {
        (at + 1) % n
    } else {
        (at + n - 1) % n
    }]
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use super::*;

    fn valid(points: &[[f32; 2]]) -> bool {
        Curve {
            points: points.to_vec(),
        }
        .validate()
        .is_ok()
    }

    #[test]
    fn a_point_cannot_pass_its_neighbours() {
        let mut p = vec![[0.0, 0.0], [0.3, 0.2], [0.6, 0.5], [1.0, 1.0]];
        place(&mut p, 1, [0.9, 0.9]);
        assert!((p[1][0] - (0.6 - GAP)).abs() < 1e-6, "{p:?}");
        assert!((p[1][1] - 0.5).abs() < 1e-6, "{p:?}");
        place(&mut p, 1, [0.0, -1.0]);
        assert!((p[1][0] - GAP).abs() < 1e-6, "{p:?}");
        assert!(p[1][1].abs() < 1e-6);
        assert!(valid(&p));
    }

    #[test]
    fn end_points_only_move_up_and_down() {
        let mut p = vec![[0.0, 0.0], [0.5, 0.5], [1.0, 1.0]];
        place(&mut p, 0, [0.3, 0.2]);
        assert_eq!(p[0][0], 0.0);
        assert!((p[0][1] - 0.2).abs() < 1e-6);
        place(&mut p, 2, [0.4, 0.3]);
        assert_eq!(p[2][0], 1.0);
        assert!((p[2][1] - 0.5).abs() < 1e-6);
        assert!(!remove(&mut p, 0));
        assert!(!remove(&mut p, 2));
        assert!(remove(&mut p, 1));
    }

    #[test]
    fn a_lone_point_is_bounded_by_the_implicit_ends() {
        let mut p = vec![[0.5, 0.45]];
        place(&mut p, 0, [2.0, 2.0]);
        assert!((p[0][0] - (1.0 - GAP)).abs() < 1e-6);
        assert!((p[0][1] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn inserting_keeps_order_and_validity() {
        let mut p = vec![[0.0, 0.0], [1.0, 1.0]];
        assert_eq!(insert(&mut p, [0.5, 0.2]), Some(1));
        let b = insert(&mut p, [0.25, 0.9]).unwrap();
        assert_eq!(b, 1);
        assert!(p[b][1] <= 0.2 + 1e-6, "output held under the next point");
        assert!(valid(&p));
        assert!(insert(&mut p, [0.251, 0.2]).is_none(), "too close");
        for k in 0..40 {
            let x = 0.02 + 0.024 * k as f32;
            let _ = insert(&mut p, [x, 0.5]);
            assert!(valid(&p));
        }
        assert_eq!(p.len(), Curve::MAX_POINTS);
        assert!(insert(&mut p, [0.99, 1.0]).is_none(), "full");
    }

    #[test]
    fn selection_walks_in_input_order_and_wraps() {
        let p = vec![[0.6, 0.6], [0.0, 0.0], [0.3, 0.3]];
        assert_eq!(step_selection(&p, 1, true), 2);
        assert_eq!(step_selection(&p, 0, true), 1);
        assert_eq!(step_selection(&p, 1, false), 0);
    }

    #[test]
    fn nearest_prefers_the_closest_in_reach() {
        let p = vec![[0.5, 0.5], [0.52, 0.5]];
        assert_eq!(nearest(&p, [0.515, 0.5], [0.05, 0.05]), Some(1));
        assert_eq!(nearest(&p, [0.9, 0.9], [0.05, 0.05]), None);
    }
}
