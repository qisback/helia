// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The global placement of the panels by weighted least squares.
//!
//! Every pair of overlapping panels gives one measurement of where the second
//! lies in the first (see [`super::pair`]). With more panels than a chain needs
//! the measurements disagree a little, and the panels are placed so that they
//! disagree as little as possible, rather than one after another with the errors
//! piling up.
//!
//! Panel `i` goes to the mosaic as `W_i(p) = R(theta_i) (p - c_i) + c_i + t_i`.
//! Panel 0 is fixed (`theta = 0`, `t = 0`).
//!
//! The rotations come first: each pair says `theta_b - theta_a = theta_ab`, a
//! linear system. With them known, a pair's transform sends the centre of `b` to
//! `c_b + t_ab` in `a`, and the two panels must agree there, which is linear in
//! the shifts: `t_b - t_a = R(theta_a) (c_b + t_ab - c_a) + c_a - c_b`.
//!
//! A few rounds of iteratively reweighted least squares (a Huber weight on how
//! far each pair is from the solution) stop one false match from bending the
//! mosaic, and a pair that is still far off at the end is put aside and named.
//!
//! # References
//!
//! - Huber, P. J. and Ronchetti, E. M. (2009), Robust Statistics, 2nd ed., Wiley,
//!   doi:10.1002/9780470434697 (the Huber weight in iteratively reweighted least squares).

#![allow(
    clippy::many_single_char_names,
    clippy::similar_names,
    reason = "n, k, m, x, a, b and the matrix entries are the natural names in least squares"
)]

use super::pair::Rigid;
use crate::error::{Error, Result};

/// One measured pair: panel `b` in panel `a`.
#[derive(Clone, Copy, Debug)]
pub struct Measure {
    /// The panel measured against.
    pub a: usize,
    /// The panel that was placed in it.
    pub b: usize,
    /// Where `b` lies in `a`.
    pub rigid: Rigid,
    /// How much to trust it (a share of the overlap and the correlation).
    pub weight: f64,
}

/// The placement.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// Rotation of each panel, radians.
    pub theta: Vec<f64>,
    /// Shift of each panel.
    pub shift: Vec<(f64, f64)>,
    /// How far each measure is from the solution, pixels at the panel's edge.
    pub residual: Vec<f64>,
    /// Whether each measure was used in the end.
    pub used: Vec<bool>,
}

/// Solve the dense symmetric system `m x = b` by Gaussian elimination.
#[allow(
    clippy::needless_range_loop,
    reason = "the elimination reads two rows by column"
)]
pub(super) fn solve_dense(mut m: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
    let n = b.len();
    for col in 0..n {
        let piv = (col..n).max_by(|&i, &j| m[i][col].abs().total_cmp(&m[j][col].abs()))?;
        if m[piv][col].abs() < 1e-12 {
            return None;
        }
        m.swap(col, piv);
        b.swap(col, piv);
        for r in col + 1..n {
            let f = m[r][col] / m[col][col];
            if f != 0.0 {
                for k in col..n {
                    m[r][k] -= f * m[col][k];
                }
                b[r] -= f * b[col];
            }
        }
    }
    let mut x = vec![0.0; n];
    for r in (0..n).rev() {
        let s: f64 = (r + 1..n).map(|k| m[r][k] * x[k]).sum();
        x[r] = (b[r] - s) / m[r][r];
    }
    Some(x)
}

/// Least squares of `x_b - x_a = d` over the equations, `x_0 = 0`. `eq` are
/// `(a, b, d, w)`.
fn lsq(n: usize, eq: &[(usize, usize, f64, f64)]) -> Option<Vec<f64>> {
    let k = n - 1;
    if k == 0 {
        return Some(vec![0.0]);
    }
    let mut m = vec![vec![0.0; k]; k];
    let mut v = vec![0.0; k];
    for &(a, b, d, w) in eq {
        // Unknown index: panel i is i - 1; panel 0 is the fixed zero.
        if a > 0 {
            m[a - 1][a - 1] += w;
            v[a - 1] -= w * d;
        }
        if b > 0 {
            m[b - 1][b - 1] += w;
            v[b - 1] += w * d;
        }
        if a > 0 && b > 0 {
            m[a - 1][b - 1] -= w;
            m[b - 1][a - 1] -= w;
        }
    }
    let x = solve_dense(m, v)?;
    let mut out = vec![0.0];
    out.extend(x);
    Some(out)
}

/// Panels reachable from panel 0 through the pairs.
fn reachable(n: usize, pairs: &[(usize, usize)]) -> Vec<bool> {
    let mut seen = vec![false; n];
    seen[0] = true;
    let mut grew = true;
    while grew {
        grew = false;
        for &(a, b) in pairs {
            if seen[a] != seen[b] {
                seen[a] = true;
                seen[b] = true;
                grew = true;
            }
        }
    }
    seen
}

/// Place the panels. `centres` are the panel centres; `radius` is the typical
/// distance from a centre to the edge, pixels, to turn a rotation into a
/// distance.
///
/// # Errors
/// [`Error::Invalid`] naming the panels no pair connects to panel 0.
pub fn solve(centres: &[(f64, f64)], measures: &[Measure], radius: f64) -> Result<Layout> {
    let n = centres.len();
    let live: Vec<(usize, usize)> = measures.iter().map(|m| (m.a, m.b)).collect();
    let seen = reachable(n, &live);
    if let Some(first) = seen.iter().position(|s| !s) {
        let lost: Vec<String> = seen
            .iter()
            .enumerate()
            .filter(|(_, s)| !**s)
            .map(|(i, _)| (i + 1).to_string())
            .collect();
        return Err(Error::Invalid(format!(
            "panel(s) {} do not overlap the rest enough to be placed (panel {} has no matching neighbour)",
            lost.join(", "),
            first + 1
        )));
    }
    let mut weights: Vec<f64> = measures.iter().map(|m| m.weight).collect();
    let mut used = vec![true; measures.len()];
    let mut layout = Layout {
        theta: vec![0.0; n],
        shift: vec![(0.0, 0.0); n],
        residual: vec![0.0; measures.len()],
        used: used.clone(),
    };
    for round in 0..6 {
        let theta = {
            let eq: Vec<_> = measures
                .iter()
                .enumerate()
                .filter(|(i, _)| used[*i])
                .map(|(i, m)| (m.a, m.b, m.rigid.theta, weights[i]))
                .collect();
            lsq(n, &eq).ok_or_else(|| Error::Invalid("the panels cannot be placed".into()))?
        };
        let rhs = |m: &Measure| {
            let (ca, cb) = (centres[m.a], centres[m.b]);
            let (s, c) = theta[m.a].sin_cos();
            let (u, v) = (cb.0 + m.rigid.tx - ca.0, cb.1 + m.rigid.ty - ca.1);
            (c * u - s * v + ca.0 - cb.0, s * u + c * v + ca.1 - cb.1)
        };
        let (mut ex, mut ey) = (Vec::new(), Vec::new());
        for (i, m) in measures.iter().enumerate().filter(|(i, _)| used[*i]) {
            let (dx, dy) = rhs(m);
            ex.push((m.a, m.b, dx, weights[i]));
            ey.push((m.a, m.b, dy, weights[i]));
        }
        let tx = lsq(n, &ex).ok_or_else(|| Error::Invalid("the panels cannot be placed".into()))?;
        let ty = lsq(n, &ey).ok_or_else(|| Error::Invalid("the panels cannot be placed".into()))?;
        let mut res = Vec::with_capacity(measures.len());
        for m in measures {
            let (dx, dy) = rhs(m);
            let (rx, ry) = (tx[m.b] - tx[m.a] - dx, ty[m.b] - ty[m.a] - dy);
            let rt = (theta[m.b] - theta[m.a] - m.rigid.theta) * radius;
            res.push((rx * rx + ry * ry + rt * rt).sqrt());
        }
        layout = Layout {
            theta: theta.clone(),
            shift: tx.iter().zip(&ty).map(|(&x, &y)| (x, y)).collect(),
            residual: res.clone(),
            used: used.clone(),
        };
        if round == 5 {
            break;
        }
        // Reweight: Huber about one pixel, and put aside what is far off while
        // the rest still connects every panel.
        for (i, r) in res.iter().enumerate() {
            weights[i] = measures[i].weight * if *r < 1.0 { 1.0 } else { 1.0 / *r };
        }
        let mut sorted: Vec<f64> = res.iter().copied().filter(|r| r.is_finite()).collect();
        sorted.sort_by(f64::total_cmp);
        let med = sorted.get(sorted.len() / 2).copied().unwrap_or(0.0);
        let limit = (3.0f64).max(4.0 * med);
        let keep: Vec<bool> = res.iter().map(|r| *r <= limit).collect();
        let pairs: Vec<(usize, usize)> = measures
            .iter()
            .enumerate()
            .filter(|(i, _)| keep[*i])
            .map(|(_, m)| (m.a, m.b))
            .collect();
        if reachable(n, &pairs).iter().all(|s| *s) {
            used = keep;
        }
    }
    Ok(layout)
}

#[cfg(test)]
#[allow(
    clippy::needless_range_loop,
    reason = "the panels are checked one by one against their own truth"
)]
mod tests {
    use super::*;

    fn measure(
        a: usize,
        b: usize,
        dx: f64,
        dy: f64,
        deg: f64,
        centres: &[(f64, f64)],
        truth: &[(f64, f64, f64)],
    ) -> Measure {
        // The pair's transform from the true placements: W_a^-1 (W_b(c_b)) for the shift.
        let (ta, tb) = (truth[a], truth[b]);
        let (ca, cb) = (centres[a], centres[b]);
        // W_b(c_b) = c_b + t_b; back through W_a: R(-theta_a)(. - c_a - t_a) + c_a.
        let (w, z) = (cb.0 + tb.0 - ca.0 - ta.0, cb.1 + tb.1 - ca.1 - ta.1);
        let (s, c) = (-ta.2.to_radians()).sin_cos();
        let (qx, qy) = (c * w - s * z + ca.0, s * w + c * z + ca.1);
        Measure {
            a,
            b,
            rigid: Rigid {
                tx: qx - cb.0 + dx,
                ty: qy - cb.1 + dy,
                theta: (tb.2 - ta.2).to_radians() + deg.to_radians(),
            },
            weight: 1.0,
        }
    }

    #[test]
    fn exact_measures_give_the_placements_back() {
        let centres = vec![(100.0, 80.0); 4];
        let truth = [
            (0.0, 0.0, 0.0),
            (170.0, 3.0, 1.0),
            (-4.0, 140.0, -0.5),
            (166.0, 147.0, 0.8),
        ];
        let mut ms = Vec::new();
        for (a, b) in [(0, 1), (0, 2), (1, 3), (2, 3), (0, 3)] {
            ms.push(measure(a, b, 0.0, 0.0, 0.0, &centres, &truth));
        }
        let l = solve(&centres, &ms, 120.0).unwrap_or_else(|e| panic!("{e}"));
        for i in 0..4 {
            assert!((l.theta[i].to_degrees() - truth[i].2).abs() < 1e-6, "{i}");
            assert!(
                (l.shift[i].0 - truth[i].0).abs() < 1e-6,
                "{i} {:?}",
                l.shift[i]
            );
            assert!((l.shift[i].1 - truth[i].1).abs() < 1e-6, "{i}");
        }
        assert!(l.residual.iter().all(|r| *r < 1e-6));
    }

    #[test]
    fn a_false_pair_is_put_aside_and_the_rest_hold() {
        let centres = vec![(100.0, 80.0); 4];
        let truth = [
            (0.0, 0.0, 0.0),
            (170.0, 3.0, 0.0),
            (-4.0, 140.0, 0.0),
            (166.0, 147.0, 0.0),
        ];
        let mut ms = Vec::new();
        for (a, b) in [(0, 1), (0, 2), (1, 3), (2, 3)] {
            ms.push(measure(a, b, 0.1, -0.1, 0.0, &centres, &truth));
        }
        // The diagonal is measured wrongly by 25 pixels.
        ms.push(measure(0, 3, 25.0, -18.0, 0.0, &centres, &truth));
        let l = solve(&centres, &ms, 120.0).unwrap_or_else(|e| panic!("{e}"));
        assert!(!l.used[4], "the false pair was used: {:?}", l.residual);
        for i in 0..4 {
            assert!(
                (l.shift[i].0 - truth[i].0).abs() < 0.5,
                "{i} {:?}",
                l.shift[i]
            );
            assert!(
                (l.shift[i].1 - truth[i].1).abs() < 0.5,
                "{i} {:?}",
                l.shift[i]
            );
        }
    }

    #[test]
    fn a_panel_with_no_neighbour_is_named() {
        let centres = vec![(50.0, 50.0); 3];
        let truth = [(0.0, 0.0, 0.0), (80.0, 0.0, 0.0), (0.0, 500.0, 0.0)];
        let ms = vec![measure(0, 1, 0.0, 0.0, 0.0, &centres, &truth)];
        let e = solve(&centres, &ms, 70.0)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(e.contains("panel(s) 3"), "{e}");
    }

    #[test]
    fn errors_spread_instead_of_piling_up() {
        // A ring of four panels, each pair measured 0.4 pixel long in x: the
        // placement shares the discrepancy out.
        let centres = vec![(100.0, 80.0); 4];
        let truth = [
            (0.0, 0.0, 0.0),
            (170.0, 0.0, 0.0),
            (170.0, 140.0, 0.0),
            (0.0, 140.0, 0.0),
        ];
        let mut ms = Vec::new();
        for (a, b) in [(0, 1), (1, 2), (2, 3), (3, 0)] {
            ms.push(measure(a, b, 0.4, 0.0, 0.0, &centres, &truth));
        }
        let l = solve(&centres, &ms, 120.0).unwrap_or_else(|e| panic!("{e}"));
        assert!(l.residual.iter().all(|r| *r < 0.5), "{:?}", l.residual);
    }
}
