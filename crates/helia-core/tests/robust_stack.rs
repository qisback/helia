// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Known-answer study of the robust combinations (issue #53).
//!
//! Synthetic samples of a known scene, combined per pixel by the weighted
//! mean, a hard sigma clip, a Huber step and a winsorised mean, each through
//! the streaming two-pass [`TileStack`]. The tables are printed with
//! `cargo test -p helia-core --test robust_stack -- --nocapture`; the numbers in
//! the user guide come from this output. The assertions are the acceptance
//! criteria: no measurable cost on clean data, a smaller error than the mean
//! under contamination, and the streaming result equal to the in-memory one.

use helia_core::frame::Rect;
use helia_core::stack::{RobustKind, TileStack, robust_location};
use helia_core::testutil::Rng;

const W: usize = 48;
const H: usize = 48;
const N: usize = 200;
const SIGMA: f32 = 0.02;

/// Mean, then the three robust kinds at their default thresholds.
const KINDS: [(&str, Option<RobustKind>); 4] = [
    ("mean", None),
    ("sigma 2.5", Some(RobustKind::Sigma)),
    ("huber 1.345", Some(RobustKind::Huber)),
    ("winsor 2", Some(RobustKind::Winsor)),
];

/// The scene: a bright disc with a sharp limb and a patch of fine "granulation"
/// (3 per cent contrast, 4 px period), on a dim background.
fn truth(x: f32, y: f32) -> f32 {
    let (dx, dy) = (x - 24.0, y - 24.0);
    let r = (dx * dx + dy * dy).sqrt();
    if r < 18.0 {
        0.8 + 0.03
            * (x * std::f32::consts::FRAC_PI_2).sin()
            * (y * std::f32::consts::FRAC_PI_2).cos()
    } else {
        0.2
    }
}

fn truth_image() -> Vec<f32> {
    (0..H)
        .flat_map(|y| (0..W).map(move |x| truth(x as f32, y as f32)))
        .collect()
}

/// `N` frames of the scene plus Gaussian noise, with quality weights.
fn clean_frames(rng: &mut Rng) -> (Vec<Vec<f32>>, Vec<f32>) {
    let t = truth_image();
    let frames = (0..N)
        .map(|_| t.iter().map(|&v| v + SIGMA * rng.gauss()).collect())
        .collect();
    let weights = (0..N).map(|i| 0.5 + (i % 7) as f32 / 6.0).collect();
    (frames, weights)
}

/// Combine `frames` per pixel with the streaming two-pass stack.
fn stack(frames: &[Vec<f32>], weights: &[f32], kind: Option<RobustKind>) -> Vec<f32> {
    let mut t = TileStack::new(Rect::new(0, 0, W, H), vec![0.5; W * H], kind.is_some());
    for (f, &w) in frames.iter().zip(weights) {
        t.add(f, w);
    }
    if let Some(kind) = kind {
        t.begin_robust(kind, kind.default_k());
        for (f, &w) in frames.iter().zip(weights) {
            t.add_clipped(f, w);
        }
    }
    t.finish()
}

fn rmse_over(got: &[f32], want: &[f32], inside: impl Fn(usize, usize) -> bool) -> f32 {
    let (mut sum, mut count) = (0.0f64, 0.0f64);
    for y in 0..H {
        for x in 0..W {
            if inside(x, y) {
                let diff = f64::from(got[y * W + x] - want[y * W + x]);
                sum += diff * diff;
                count += 1.0;
            }
        }
    }
    (sum / count).sqrt() as f32
}

fn everywhere(_: usize, _: usize) -> bool {
    true
}

#[test]
fn streaming_two_pass_equals_in_memory_irls() {
    let mut rng = Rng::new(7);
    let (mut frames, weights) = clean_frames(&mut rng);
    // Some outliers so the robust branch is exercised.
    for (i, f) in frames.iter_mut().enumerate().take(12) {
        for x in 0..W {
            f[(i * 3 % H) * W + x] += 0.7;
        }
    }
    for kind in [RobustKind::Sigma, RobustKind::Huber, RobustKind::Winsor] {
        let got = stack(&frames, &weights, Some(kind));
        let mut worst = 0.0f32;
        for (i, &g) in got.iter().enumerate() {
            let col: Vec<f32> = frames.iter().map(|f| f[i]).collect();
            let want = robust_location(&col, &weights, kind, kind.default_k());
            worst = worst.max((g - want).abs());
        }
        eprintln!("streaming vs in-memory {kind:?}: worst abs diff {worst:.2e}");
        assert!(worst < 2e-6, "{kind:?}: {worst}");
    }
}

#[test]
fn clean_data_costs_no_contrast_or_bias() {
    let t = truth_image();
    let rms_contrast = |img: &[f32]| {
        // rms of the fine texture inside the disc, around the disc mean.
        let inside = |x: usize, y: usize| {
            let (dx, dy) = (x as f32 - 24.0, y as f32 - 24.0);
            dx * dx + dy * dy < 14.0 * 14.0
        };
        let (mut s, mut s2, mut n) = (0.0f64, 0.0f64, 0.0f64);
        for y in 0..H {
            for x in 0..W {
                if inside(x, y) {
                    let v = f64::from(img[y * W + x]);
                    s += v;
                    s2 += v * v;
                    n += 1.0;
                }
            }
        }
        ((s2 / n) - (s / n) * (s / n)).sqrt()
    };
    let mut rng = Rng::new(11);
    let (frames, weights) = clean_frames(&mut rng);
    let base = stack(&frames, &weights, None);
    let (c0, mean0) = (
        rms_contrast(&base),
        base.iter().map(|&v| f64::from(v)).sum::<f64>() / base.len() as f64,
    );
    eprintln!("clean Gaussian data, N={N}, noise {SIGMA}: change relative to the weighted mean");
    eprintln!(
        "{:<12} {:>12} {:>14} {:>12}",
        "estimator", "bias (%)", "contrast (%)", "rmse"
    );
    for (name, kind) in KINDS {
        let img = stack(&frames, &weights, kind);
        let mean = img.iter().map(|&v| f64::from(v)).sum::<f64>() / img.len() as f64;
        let bias = 100.0 * (mean - mean0) / mean0;
        let contrast = 100.0 * (rms_contrast(&img) - c0) / c0;
        eprintln!(
            "{name:<12} {bias:>12.4} {contrast:>14.4} {:>12.5}",
            rmse_over(&img, &t, everywhere)
        );
        assert!(bias.abs() < 1.0, "{name}: bias {bias}");
        assert!(contrast.abs() < 1.0, "{name}: contrast change {contrast}");
    }
}

/// How the samples are damaged.
#[derive(Clone, Copy, Debug)]
enum Damage {
    /// Isolated hot pixels, 1.2 above the truth.
    Hot,
    /// Satellite or plane trails: 2 px wide diagonal lines, +0.5, in a few frames.
    Trail,
    /// A run of consecutive frames dimmed to 40 per cent (cloud).
    Cloud,
}

fn damaged(damage: Damage, fraction: f64, rng: &mut Rng) -> (Vec<Vec<f32>>, Vec<f32>, f64) {
    let (mut frames, weights) = clean_frames(rng);
    let total = (N * W * H) as f64;
    let quota = (fraction * total).round() as usize;
    let mut hit = 0usize;
    match damage {
        Damage::Hot => {
            while hit < quota {
                let f = (rng.next_u64() % N as u64) as usize;
                let i = (rng.next_u64() % (W * H) as u64) as usize;
                frames[f][i] += 1.2;
                hit += 1;
            }
        }
        Damage::Trail => {
            while hit < quota {
                let f = (rng.next_u64() % N as u64) as usize;
                let off = (rng.next_u64() % W as u64) as usize;
                for x in 0..W {
                    for t in 0..2 {
                        let y = (x + off + t) % H;
                        frames[f][y * W + x] += 0.5;
                        hit += 1;
                    }
                }
            }
        }
        Damage::Cloud => {
            let run = ((fraction * N as f64).round() as usize).max(1);
            let start = N / 3;
            for f in &mut frames[start..start + run] {
                for v in f.iter_mut() {
                    *v *= 0.4;
                }
            }
            hit = run * W * H;
        }
    }
    (frames, weights, hit as f64 / total)
}

#[test]
fn contaminated_data_rmse_against_truth() {
    let t = truth_image();
    for damage in [Damage::Hot, Damage::Trail, Damage::Cloud] {
        eprintln!("{damage:?}: rmse against the truth (x1e-3), N={N}");
        eprintln!(
            "{:>9} {:>10} {:>10} {:>10} {:>10} {:>10}",
            "target %", "actual %", "mean", "sigma 2.5", "huber", "winsor"
        );
        for fraction in [0.001, 0.01, 0.05] {
            let mut rng = Rng::new(31);
            let (frames, weights, actual) = damaged(damage, fraction, &mut rng);
            let errs: Vec<f32> = KINDS
                .iter()
                .map(|&(_, kind)| rmse_over(&stack(&frames, &weights, kind), &t, everywhere))
                .collect();
            eprintln!(
                "{:>9.1} {:>10.2} {:>10.2} {:>10.2} {:>10.2} {:>10.2}",
                100.0 * fraction,
                100.0 * actual,
                1e3 * errs[0],
                1e3 * errs[1],
                1e3 * errs[2],
                1e3 * errs[3],
            );
            // Cloud dimming is a whole-frame gain error: a handful of frames
            // among 200 move the mean a little and the robust forms must not
            // make it worse. Hot pixels and trails must be clearly improved.
            for (e, (name, _)) in errs.iter().zip(KINDS).skip(1) {
                let slack = if matches!(damage, Damage::Cloud) {
                    1.05
                } else {
                    1.0
                };
                assert!(
                    *e <= errs[0] * slack,
                    "{damage:?} {fraction}: {name} {e} worse than mean {}",
                    errs[0]
                );
            }
        }
    }
}

#[test]
fn a_misregistered_patch_is_limited() {
    // 5 per cent of the frames sample a 16x16 patch 2.5 px away.
    let t = truth_image();
    let mut rng = Rng::new(5);
    let (mut frames, weights) = clean_frames(&mut rng);
    let (px0, py0) = (16usize, 16usize);
    for f in frames.iter_mut().take(N / 20) {
        for y in py0..py0 + 16 {
            for x in px0..px0 + 16 {
                f[y * W + x] = truth(x as f32 + 2.5, y as f32) + SIGMA * rng.gauss();
            }
        }
    }
    let patch = |x: usize, y: usize| (px0..px0 + 16).contains(&x) && (py0..py0 + 16).contains(&y);
    eprintln!("mis-registered 16x16 patch in 5% of frames: rmse in the patch (x1e-3)");
    let mut errs = Vec::new();
    for (name, kind) in KINDS {
        let e = rmse_over(&stack(&frames, &weights, kind), &t, patch);
        eprintln!("{name:<12} {:>8.2}", 1e3 * e);
        errs.push(e);
    }
    // The patch crosses the limb, where a 2.5 px shift is a large step. The
    // soft forms must at least not be worse than the mean.
    assert!(errs[2] <= errs[0] && errs[3] <= errs[0], "{errs:?}");
}

#[test]
fn skewed_clean_data_bias_relative_to_the_mean() {
    // No outliers, but a real asymmetric spread: one sample in five is pulled
    // down by 0.05 (a smeared frame). The weighted mean is the right answer
    // for such data, so this measures what each robust form takes away.
    let mut rng = Rng::new(21);
    let t = truth_image();
    let frames: Vec<Vec<f32>> = (0..N)
        .map(|_| {
            t.iter()
                .map(|&v| {
                    let dip = if rng.uniform() < 0.2 { -0.05 } else { 0.0 };
                    v + dip + SIGMA * rng.gauss()
                })
                .collect()
        })
        .collect();
    let weights = vec![1.0; N];
    let base = stack(&frames, &weights, None);
    eprintln!("skewed clean data (20% of samples -0.05): shift from the weighted mean (x1e-3)");
    let mut shifts = Vec::new();
    for (name, kind) in KINDS {
        let img = stack(&frames, &weights, kind);
        let e = rmse_over(&img, &base, everywhere);
        let signed = img
            .iter()
            .zip(&base)
            .map(|(a, b)| f64::from(a - b))
            .sum::<f64>()
            / img.len() as f64;
        eprintln!(
            "{name:<12} rmse {:>7.3}  mean shift {:>8.3}",
            1e3 * e,
            1e3 * signed
        );
        shifts.push(signed.abs());
    }
    // Clamping keeps the tail's weight where a hard clip discards it. Huber at
    // its customary 1.345 down-weights harder than either; that is reported,
    // not hidden.
    assert!(shifts[3] <= shifts[1], "{shifts:?}");
}
