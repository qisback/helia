// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Deconvolution on a real H-alpha capture: the PSF measured from the limb,
//! and the check that says when deconvolution mostly amplified the noise.
//!
//! Needs the owner's capture; set `HELIA_REAL_DATA` to the folder that holds
//! `owner-2026-10-20/15_32_09.avi` (the test is skipped without it). On a 24-core build server with an NVIDIA A100
//! (25% of 1,000 frames kept, the H-alpha limb) the limb gives a Gaussian
//! sigma of 1.68 px against the 0.70 px the image scale suggests; plain
//! Richardson-Lucy with the scale's PSF raises the pixel-scale band 1.5 times
//! and the 1 to 4 pixel band 1.2 times, which the check flags.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use helia_core::Preset;
use helia_core::align::multipoint::MultipointConfig;
use helia_core::edit::diagnose::{Verdict, check};
use helia_core::edit::{Op, Psf, SharpenMethod, apply_cpu, limbpsf, sharpen_edits};
use helia_core::io::source::{OpenOptions, open_capture};
use helia_core::pipeline::{Pipeline, StackConfig};
use helia_core::recommend::recommend;

fn capture() -> Option<PathBuf> {
    let Some(base) = std::env::var_os("HELIA_REAL_DATA").map(PathBuf::from) else {
        eprintln!("HELIA_REAL_DATA is not set: skipping the deconvolution real-data test");
        return None;
    };
    let p = base.join("owner-2026-10-20").join("15_32_09.avi");
    if p.exists() {
        Some(p)
    } else {
        eprintln!("{} not found: skipping", p.display());
        None
    }
}

#[test]
fn the_limb_psf_is_measured_and_noisy_deconvolution_is_flagged() {
    let Some(path) = capture() else { return };
    let mut src = open_capture(&path, &OpenOptions::default()).expect("open capture");
    let (w, h) = src.dimensions();
    let rec = recommend(w, h, true, &[], Some(Preset::Ha));
    let cfg = StackConfig {
        keep: rec.keep,
        clip_fallback: true,
        multipoint: Some(MultipointConfig {
            ap_size: rec.ap_size,
            ..MultipointConfig::default()
        }),
        debayer: helia_core::debayer::Debayer {
            channel: rec.channel,
            ..helia_core::debayer::Debayer::default()
        },
        ..StackConfig::default()
    };
    let out = Pipeline::new(cfg)
        .run(&mut src, &|_, _| {}, &AtomicBool::new(false))
        .expect("stack");
    let limb = limbpsf::measure(&out.image).expect("a limb in a full-disc stack");
    eprintln!(
        "15_32_09.avi limb: sigma {:.2} px, edge radius {:.1} px, misfit {:.3}",
        limb.sigma, limb.radius, limb.misfit
    );
    assert!(
        (0.8..=3.5).contains(&limb.sigma) && limb.misfit < 0.1,
        "{limb:?}"
    );
    // The recommended deconvolution measures the PSF from the limb: the detail
    // rises as much as the noise or more, so it is not flagged.
    let list = sharpen_edits(
        SharpenMethod::RichardsonLucy,
        Some(Preset::Ha),
        out.image.width,
        out.image.height,
    )
    .resolve(&out.image);
    let psf = match &list.ops[0] {
        Op::Deconvolve(d) => d.psf,
        other => panic!("{other:?}"),
    };
    let after = apply_cpu(&out.image, &list).expect("deconvolve");
    let c = check(&out.image, &after, Some(psf)).expect("check");
    eprintln!("limb PSF {}: {}", psf.describe(), c.summary());
    assert_eq!(c.verdict, Verdict::Fine, "{c:?}");
    assert!(c.detail_gain > 1.5, "{c:?}");
    // The PSF the image scale suggests is too small for this stack: the
    // pixel-scale band rises more than the 1 to 4 pixel band, which is mostly
    // noise (or the iterations fit the noise), and the check says so.
    let mut scale_list = sharpen_edits(
        SharpenMethod::RichardsonLucy,
        Some(Preset::Ha),
        out.image.width,
        out.image.height,
    );
    match &mut scale_list.ops[0] {
        Op::Deconvolve(d) => d.psf = Psf::Auto,
        other => panic!("{other:?}"),
    }
    let scale_list = scale_list.resolve(&out.image);
    let psf = match &scale_list.ops[0] {
        Op::Deconvolve(d) => d.psf,
        other => panic!("{other:?}"),
    };
    let after = apply_cpu(&out.image, &scale_list).expect("deconvolve");
    let c = check(&out.image, &after, Some(psf)).expect("check");
    eprintln!("scale PSF {}: {}", psf.describe(), c.summary());
    assert!(
        matches!(c.verdict, Verdict::NoiseDominated | Verdict::Overfit),
        "{c:?}"
    );
    assert!(c.noise_gain > 1.4 && c.detail_gain < 1.3, "{c:?}");
}
