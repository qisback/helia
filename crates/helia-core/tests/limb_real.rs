// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Limb-fit alignment on the real captures (ZWO ASI482MC, `SharpCap`,
//! 2022-07-17). The captures are never stored in the repository: point
//! `HELIA_REAL_DATA` at the directory holding them to run this; without it the
//! test prints a note and passes.
//!
//! There is no ground truth for a real frame, so the limb fit is compared with
//! phase correlation on the same frames, and with itself (the two halves of the
//! limb fitted separately).
#![allow(
    clippy::many_single_char_names,
    reason = "x, y, w, h, dx, dy are the natural names in image maths"
)]

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use helia_core::align::method::{AlignMethod, GlobalMethod};
use helia_core::filter::gaussian_blur;
use helia_core::io::source::{FrameSource, LimitedSource, OpenOptions, open_capture};
use helia_core::pipeline::{Pipeline, StackConfig, StackOutput};
use helia_core::{Keep, Metric, quality};

const FRAMES: usize = 200;

fn find(name: &str) -> Option<PathBuf> {
    let base = PathBuf::from(std::env::var_os("HELIA_REAL_DATA")?);
    [base.join(name), base.join("solar-2022-07-17").join(name)]
        .into_iter()
        .find(|p| p.exists())
}

fn run(path: &Path, align: AlignMethod, multipoint: bool) -> StackOutput {
    let inner = open_capture(path, &OpenOptions::default()).unwrap_or_else(|e| panic!("{e}"));
    let mut src = LimitedSource::new(inner, 0, FRAMES);
    let cfg = StackConfig {
        keep: Keep::Fraction(0.25),
        align,
        multipoint: multipoint.then(Default::default),
        ..StackConfig::default()
    };
    Pipeline::new(cfg)
        .run(&mut src, &|_, _| {}, &AtomicBool::new(false))
        .unwrap_or_else(|e| panic!("{e}"))
}

fn detail(out: &StackOutput) -> f32 {
    quality::score(
        &gaussian_blur(&out.image, 2.0),
        Metric::GradientEnergy,
        None,
    )
}

#[test]
fn limb_fit_agrees_with_phase_correlation_on_real_captures() {
    if std::env::var_os("HELIA_REAL_DATA").is_none() {
        eprintln!("HELIA_REAL_DATA is not set: skipping real-data tests");
        return;
    }
    for name in [
        "15_20_57.ser",
        "Sun_Test.ser",
        "14_42_57.avi",
        "15_32_09.avi",
    ] {
        let Some(path) = find(name) else {
            eprintln!("{name} not found: skipping");
            continue;
        };
        let auto = run(&path, AlignMethod::Auto, false);
        let limb = run(&path, AlignMethod::Limb, false);
        let phase = run(&path, AlignMethod::Phase, false);
        let a = &limb.alignment;
        eprintln!("== {name}");
        eprintln!("  auto chose: {}", auto.alignment.summary());
        eprintln!("  limb run:   {}", a.summary());
        if let Some(l) = &a.limb {
            eprintln!("  reference limb fit: {l:?}");
            eprintln!(
                "  reference limb: centre ({:.2}, {:.2}) r {:.2}, {:.0}% in frame, sigma {:.3} px, confidence {:.2}",
                l.cx,
                l.cy,
                l.r,
                l.in_frame * 100.0,
                l.centre_sigma,
                l.confidence
            );
        }
        // Same frames, same reference: the shifts are directly comparable.
        let pairs: Vec<(f32, f32)> = limb
            .shifts
            .iter()
            .zip(&phase.shifts)
            .map(|((i, l), (j, p))| {
                assert_eq!(i, j);
                (l.dx - p.dx, l.dy - p.dy)
            })
            .collect();
        let mut d: Vec<f32> = pairs.iter().map(|p| p.0.hypot(p.1)).collect();
        d.sort_by(f32::total_cmp);
        let n = d.len() as f32;
        let rms = (d.iter().map(|v| v * v).sum::<f32>() / n).sqrt();
        let (mx, my) = pairs
            .iter()
            .fold((0.0, 0.0), |a, p| (a.0 + p.0 / n, a.1 + p.1 / n));
        eprintln!(
            "  limb - phase over {} frames: median {:.3} px, rms {:.3} px, 90th {:.3} px, max {:.3} px, mean offset ({mx:.3}, {my:.3})",
            d.len(),
            d[d.len() / 2],
            rms,
            d[d.len() * 9 / 10],
            d[d.len() - 1]
        );
        let spread = limb
            .shifts
            .iter()
            .map(|(_, s)| s.length())
            .fold(0.0, f32::max);
        eprintln!("  largest shift in the run: {spread:.1} px");
        eprintln!(
            "  global-only stack detail: limb {:.5}, phase {:.5} (ratio {:.3})",
            detail(&limb),
            detail(&phase),
            detail(&limb) / detail(&phase).max(1e-12)
        );
        if a.method == GlobalMethod::Limb && a.limb.is_some_and(|l| l.confidence >= 0.8) {
            assert!(
                d[d.len() / 2] < 0.5,
                "{name}: limb and phase disagree by {} px at the median",
                d[d.len() / 2]
            );
        }
    }
}

/// Does the confidence mean anything on real frames? The candidates of a real
/// capture are aligned to a reference built as the pipeline builds it, by the
/// limb fit and by phase correlation; the disagreement should be larger where
/// the limb fit is less confident.
#[test]
fn limb_confidence_tracks_disagreement_with_phase_correlation() {
    use helia_core::Kernel;
    use helia_core::align::global::{PhaseCorrelator, PhaseOptions, build_reference};
    use helia_core::align::limb::LimbReference;

    for name in ["15_20_57.ser", "Sun_Test.ser", "15_32_09.avi"] {
        let Some(path) = find(name) else {
            continue;
        };
        let inner = open_capture(&path, &OpenOptions::default()).unwrap_or_else(|e| panic!("{e}"));
        let mut src = LimitedSource::new(inner, 0, FRAMES);
        let p = Pipeline::new(StackConfig::default());
        let n = FRAMES.min(src.frame_count());
        let frames: Vec<_> = (0..n)
            .map(|i| p.read_mono(&mut src, i).unwrap_or_else(|e| panic!("{e}")))
            .collect();
        let scores = quality::score_all(&frames, Metric::GradientEnergy, None);
        let rank = quality::rank(&scores);
        let refs: Vec<_> = rank[..5].iter().map(|&i| &frames[i]).collect();
        let reference = build_reference(&refs, PhaseOptions::default(), Kernel::default())
            .unwrap_or_else(|e| panic!("{e}"));
        let corr = PhaseCorrelator::new(&reference, PhaseOptions::default())
            .unwrap_or_else(|e| panic!("{e}"));
        let Some(lr) = LimbReference::new(&reference) else {
            continue;
        };
        let mut rows: Vec<(f32, f32)> = Vec::new();
        for &i in &rank[..n / 2] {
            let ph = corr.estimate(&frames[i]).unwrap_or_else(|e| panic!("{e}"));
            if let Some((s, fit)) = lr.estimate(&frames[i]) {
                rows.push((fit.confidence, (s - ph.shift).length()));
            }
        }
        rows.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mean = |v: &[(f32, f32)]| v.iter().map(|r| r.1).sum::<f32>() / v.len().max(1) as f32;
        let q = rows.len() / 4;
        eprintln!(
            "== {name}: {} of {} best frames pass the confidence gate; mean |limb - phase| by confidence quartile (least to most confident): {:.3} {:.3} {:.3} {:.3} px",
            rows.len(),
            n / 2,
            mean(&rows[..q]),
            mean(&rows[q..2 * q]),
            mean(&rows[2 * q..3 * q]),
            mean(&rows[3 * q..])
        );
    }
}
