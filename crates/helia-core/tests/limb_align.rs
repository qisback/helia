// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Global alignment by limb fit against known answers: a synthetic capture whose
//! frames are displaced by known shifts goes through the whole pipeline, with
//! the limb fit and with phase correlation, and the shifts that come out are
//! compared with the truth.
#![cfg(feature = "testutil")]
#![allow(
    clippy::many_single_char_names,
    reason = "x, y, w, h are the natural names in image maths"
)]

use std::sync::atomic::AtomicBool;

use helia_core::align::method::{AlignMethod, GlobalMethod};
use helia_core::io::source::MemorySource;
use helia_core::pipeline::{Pipeline, StackConfig, StackOutput};
use helia_core::testutil::{CaptureConfig, SyntheticCapture, SyntheticConfig, SyntheticSun};
use helia_core::{Frame, Keep};

fn capture(radius_frac: f32, frames: usize) -> SyntheticCapture {
    let sun = SyntheticSun::new(&SyntheticConfig {
        width: 256,
        height: 200,
        radius_frac,
        ..SyntheticConfig::default()
    });
    SyntheticCapture::generate(
        &sun,
        &CaptureConfig {
            frames,
            max_shift: 8.0,
            noise: 0.02,
            ..CaptureConfig::default()
        },
    )
}

fn run(frames: Vec<Frame>, align: AlignMethod) -> StackOutput {
    let cfg = StackConfig {
        keep: Keep::Count(12),
        multipoint: None,
        align,
        ..StackConfig::default()
    };
    Pipeline::new(cfg)
        .run(
            &mut MemorySource(frames),
            &|_, _| {},
            &AtomicBool::new(false),
        )
        .unwrap_or_else(|e| panic!("{e}"))
}

/// Largest deviation of the registered shifts from the truth, after removing
/// the constant offset that the choice of reference imposes.
fn worst_error(out: &StackOutput, cap: &SyntheticCapture) -> f32 {
    let err: Vec<(f32, f32)> = out
        .shifts
        .iter()
        .map(|(i, s)| (s.dx - cap.shifts[*i].dx, s.dy - cap.shifts[*i].dy))
        .collect();
    let n = err.len() as f32;
    let (mx, my) = err
        .iter()
        .fold((0.0, 0.0), |a, e| (a.0 + e.0 / n, a.1 + e.1 / n));
    err.iter()
        .map(|e| (e.0 - mx).hypot(e.1 - my))
        .fold(0.0, f32::max)
}

#[test]
fn limb_and_phase_agree_with_the_truth_on_a_full_disc() {
    let cap = capture(0.38, 40);
    let limb = run(cap.frames.clone(), AlignMethod::Limb);
    let phase = run(cap.frames.clone(), AlignMethod::Phase);
    let auto = run(cap.frames.clone(), AlignMethod::Auto);
    assert_eq!(limb.alignment.method, GlobalMethod::Limb);
    assert_eq!(phase.alignment.method, GlobalMethod::Phase);
    assert_eq!(auto.alignment.method, GlobalMethod::Limb, "full disc: limb");
    let (el, ep) = (worst_error(&limb, &cap), worst_error(&phase, &cap));
    eprintln!(
        "full disc, 40 frames, blur 0.6-3, noise 0.02: limb worst {el:.4} px, phase worst {ep:.4} px"
    );
    assert!(el < 0.1, "limb error {el}");
    assert!(ep < 0.1, "phase error {ep}");
    let a = &limb.alignment;
    assert_eq!(a.frames, limb.shifts.len());
    assert_eq!(a.limb_frames + a.phase_frames, a.frames);
    assert!(a.confidence_mean.is_some_and(|c| c > 0.8), "{a:?}");
    assert!(a.limb.is_some_and(|l| l.is_full_disc()));
    assert!(a.summary().starts_with("limb fit"), "{}", a.summary());
}

#[test]
fn a_cropped_region_uses_phase_correlation() {
    let cap = capture(0.38, 30);
    // A window that cuts the limb: no whole disc is left to fit.
    let crop = |f: &Frame| {
        let (x0, y0, w, h) = (60usize, 40usize, 120usize, 100usize);
        let mut data = Vec::with_capacity(w * h);
        for y in y0..y0 + h {
            data.extend_from_slice(&f.row(y)[x0..x0 + w]);
        }
        Frame::new(w, h, data).unwrap_or_else(|e| panic!("{e}"))
    };
    let frames: Vec<Frame> = cap.frames.iter().map(crop).collect();
    let out = run(frames.clone(), AlignMethod::Auto);
    assert_eq!(out.alignment.method, GlobalMethod::Phase);
    assert!(
        out.alignment.reason.contains("cuts the limb") || out.alignment.reason.contains("no solar"),
        "{}",
        out.alignment.reason
    );
    assert!(out.alignment.confidence_mean.is_some_and(|c| c > 0.5));
    assert_eq!(out.alignment.fallbacks, 0);
    let e = worst_error(&out, &cap);
    eprintln!("cropped region, phase correlation: worst {e:.4} px");
    assert!(e < 0.3, "{e}");
}

#[test]
fn a_disc_cut_by_the_frame_can_still_be_fitted_when_asked() {
    let cap = capture(0.55, 30);
    // The disc is larger than the frame is tall: the limb is cut top and bottom.
    let auto = run(cap.frames.clone(), AlignMethod::Auto);
    assert_eq!(auto.alignment.method, GlobalMethod::Phase);
    let limb = run(cap.frames.clone(), AlignMethod::Limb);
    assert_eq!(limb.alignment.method, GlobalMethod::Limb);
    let e = worst_error(&limb, &cap);
    eprintln!("disc cut by the frame, limb fit forced: worst {e:.4} px");
    assert!(e < 0.2, "{e}");
}

#[test]
fn phase_is_the_only_choice_without_a_disc() {
    let mut frames = Vec::new();
    for k in 0..6 {
        let mut f = Frame::zeros(96, 96);
        for (i, v) in f.data.iter_mut().enumerate() {
            *v = ((i * 37 + k) % 101) as f32 / 400.0;
        }
        frames.push(f);
    }
    let cfg = StackConfig {
        keep: Keep::Count(3),
        multipoint: None,
        align: AlignMethod::Limb,
        ..StackConfig::default()
    };
    let err = Pipeline::new(cfg)
        .run(
            &mut MemorySource(frames.clone()),
            &|_, _| {},
            &AtomicBool::new(false),
        )
        .map(|_| ());
    assert!(
        err.is_err(),
        "asking for a limb that is not there is an error"
    );
    let out = run(frames, AlignMethod::Auto);
    assert_eq!(out.alignment.method, GlobalMethod::Phase);
    assert!(out.alignment.reason.contains("no solar disc"));
}
