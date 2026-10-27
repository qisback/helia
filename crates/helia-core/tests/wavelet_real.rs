// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The wavelet settings on a real H-alpha capture: the recommended values and
//! a RegiStax-style set both lift the fine detail clearly more than they lift
//! the pixel noise.
//!
//! Needs the owner's capture; set `HELIA_REAL_DATA` to the folder that holds
//! `owner-2026-10-20/15_32_09.avi` (the test is skipped without it). It ran
//! on a 24-core build server with an NVIDIA A100 at 25% of 1,000 frames kept: the 1 to 4 px band 0.96% of the mean in
//! the stack, 2.17% with the recommended values and 2.05% with the
//! RegiStax-style set; the pixel-scale band 0.30%, 0.62% and 0.72%.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use helia_core::align::multipoint::MultipointConfig;
use helia_core::edit::{Op, Wavelet, apply_cpu};
use helia_core::io::source::{OpenOptions, open_capture};
use helia_core::measure::{DEFAULT_CROP, GranulationBands, centre_crop, granulation_contrast_in};
use helia_core::pipeline::{Pipeline, StackConfig};
use helia_core::recommend::recommend;
use helia_core::{EditList, Preset};

fn capture() -> Option<PathBuf> {
    let Some(base) = std::env::var_os("HELIA_REAL_DATA").map(PathBuf::from) else {
        eprintln!("HELIA_REAL_DATA is not set: skipping the wavelet real-data test");
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

/// The bands of `frame` edited, over the same crop (the plain stack's disc
/// centre) so the sharpening cannot move the area it is judged on.
fn bands(frame: &helia_core::Frame, edits: &EditList) -> GranulationBands {
    let area = centre_crop(frame, DEFAULT_CROP);
    granulation_contrast_in(&apply_cpu(frame, edits).expect("edit"), area)
}

#[test]
fn the_wavelets_lift_detail_more_than_noise_on_real_h_alpha() {
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
    let plain = bands(&out.image, &EditList::default());
    let recommended = bands(&out.image, &rec.sharpen_edits);
    let registax = Wavelet::from_registax_spec(
        "registax:l1=100/0.14/0.2,l2=100/0.13/0.15,l3=50/0.11/0.1,filter=gaussian",
    )
    .expect("spec");
    let tutorial = bands(&out.image, &EditList::new(vec![Op::Wavelet(registax)]));
    eprintln!(
        "15_32_09.avi bands (fine / 1-4 px / 4-12 px), per cent: stack {:.2} / {:.2} / {:.2}, recommended {:.2} / {:.2} / {:.2}, RegiStax-style {:.2} / {:.2} / {:.2}",
        plain.fine * 100.0,
        plain.granulation * 100.0,
        plain.meso * 100.0,
        recommended.fine * 100.0,
        recommended.granulation * 100.0,
        recommended.meso * 100.0,
        tutorial.fine * 100.0,
        tutorial.granulation * 100.0,
        tutorial.meso * 100.0,
    );
    for (name, b) in [("recommended", recommended), ("RegiStax-style", tutorial)] {
        let detail = b.granulation / plain.granulation;
        let noise = b.fine / plain.fine;
        assert!(
            detail >= 1.6,
            "{name}: the 1 to 4 px band rose only {detail:.2}x"
        );
        assert!(noise <= 3.0, "{name}: the pixel band rose {noise:.2}x");
        assert!(
            noise < 1.3 * detail,
            "{name}: noise {noise:.2}x against detail {detail:.2}x"
        );
    }
}
