// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Small statistics shared by the figures and their JSON.

use serde::{Deserialize, Serialize};

/// Summary of a list of numbers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    /// How many values.
    pub n: usize,
    /// Smallest.
    pub min: f32,
    /// 5th percentile.
    pub p05: f32,
    /// Median.
    pub median: f32,
    /// 95th percentile.
    pub p95: f32,
    /// Largest.
    pub max: f32,
    /// Mean.
    pub mean: f32,
    /// Standard deviation (sample).
    pub std: f32,
}

/// Replace non-finite values with 0 so the data round-trips through JSON.
#[must_use]
pub fn finite(v: f32) -> f32 {
    if v.is_finite() { v } else { 0.0 }
}

/// Percentile `q` (0 to 1) of sorted data, by linear interpolation.
#[must_use]
pub fn percentile_sorted(sorted: &[f32], q: f32) -> f32 {
    match sorted.len() {
        0 => 0.0,
        1 => sorted[0],
        n => {
            let pos = q.clamp(0.0, 1.0) * (n - 1) as f32;
            let i = pos.floor() as usize;
            let f = pos - i as f32;
            sorted[i] * (1.0 - f) + sorted[(i + 1).min(n - 1)] * f
        }
    }
}

/// Summarise `values` (non-finite ones are skipped).
#[must_use]
pub fn summarise(values: &[f32]) -> Summary {
    let mut v: Vec<f32> = values.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return Summary::default();
    }
    v.sort_by(f32::total_cmp);
    let n = v.len();
    let mean = v.iter().sum::<f32>() / n as f32;
    let var = if n > 1 {
        v.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / (n - 1) as f32
    } else {
        0.0
    };
    Summary {
        n,
        min: v[0],
        p05: percentile_sorted(&v, 0.05),
        median: percentile_sorted(&v, 0.5),
        p95: percentile_sorted(&v, 0.95),
        max: v[n - 1],
        mean,
        std: var.sqrt(),
    }
}

/// Least-squares line `y = a + b x`; `None` for fewer than two distinct x.
#[must_use]
pub fn linear_fit(xs: &[f32], ys: &[f32]) -> Option<(f32, f32)> {
    let n = xs.len().min(ys.len());
    if n < 2 {
        return None;
    }
    let nf = n as f64;
    let mx = xs[..n].iter().map(|&x| f64::from(x)).sum::<f64>() / nf;
    let my = ys[..n].iter().map(|&y| f64::from(y)).sum::<f64>() / nf;
    let (mut sxx, mut sxy) = (0.0, 0.0);
    for i in 0..n {
        let dx = f64::from(xs[i]) - mx;
        sxx += dx * dx;
        sxy += dx * (f64::from(ys[i]) - my);
    }
    if sxx <= 0.0 {
        return None;
    }
    let b = sxy / sxx;
    Some(((my - b * mx) as f32, b as f32))
}

/// Median of a slice (0 when empty).
#[must_use]
pub fn median(values: &[f32]) -> f32 {
    let mut v: Vec<f32> = values.iter().copied().filter(|x| x.is_finite()).collect();
    v.sort_by(f32::total_cmp);
    percentile_sorted(&v, 0.5)
}

/// Centred running median of odd `window` (shrinks at the ends).
#[must_use]
pub fn running_median(values: &[f32], window: usize) -> Vec<f32> {
    let half = window / 2;
    (0..values.len())
        .map(|i| {
            let lo = i.saturating_sub(half);
            let hi = (i + half + 1).min(values.len());
            median(&values[lo..hi])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_of_one_to_five() {
        let s = summarise(&[5.0, 1.0, 3.0, 2.0, 4.0]);
        assert_eq!((s.n, s.min, s.median, s.max), (5, 1.0, 3.0, 5.0));
        assert!((s.mean - 3.0).abs() < 1e-6);
        assert!((s.std - 1.581_139).abs() < 1e-4);
    }

    #[test]
    fn the_fit_recovers_a_line() {
        let xs: Vec<f32> = (0..20).map(|i| i as f32).collect();
        let ys: Vec<f32> = xs.iter().map(|x| 2.0 + 0.5 * x).collect();
        let (a, b) = linear_fit(&xs, &ys).expect("fit");
        assert!((a - 2.0).abs() < 1e-4 && (b - 0.5).abs() < 1e-5);
        assert!(linear_fit(&[1.0], &[1.0]).is_none());
    }

    #[test]
    fn the_running_median_rejects_a_spike() {
        let v = [1.0, 1.0, 50.0, 1.0, 1.0];
        assert_eq!(running_median(&v, 3), vec![1.0, 1.0, 1.0, 1.0, 1.0]);
    }
}
