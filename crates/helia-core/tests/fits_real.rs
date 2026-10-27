// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! FITS frames against the real SER they were cut from.
//!
//! A real Sun capture (ZWO ASI482MC, 2022-07-17, 16-bit RGGB) has its first
//! frames written out as a folder of 16-bit FITS subs (`BZERO = 32768`,
//! `BAYERPAT` as a camera program writes it) and as one FITS cube. Stacking
//! the subs, the cube and the SER must give the same picture, because the
//! samples are the same numbers. The measured differences are printed.
//!
//! Needs `HELIA_REAL_DATA` (see `real_data.rs`); without it the test prints a
//! note and passes. Set `HELIA_FITS_KEEP` to a directory to keep the files it
//! writes (`subs/`, `cube.fits`, `first.ser`) for running the `helia` command
//! on them.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use helia_core::Keep;
use helia_core::io::fits::{self, FitsFormat, HeaderCard};
use helia_core::io::ser::{ColorId, SerHeader, SerWriter};
use helia_core::io::source::{FrameSource, LimitedSource, OpenOptions, open_capture};
use helia_core::pipeline::{Pipeline, StackConfig, StackOutput};
use helia_core::{BayerPattern, Frame};

/// Frames cut from the capture.
const FRAMES: usize = 60;

fn find(name: &str) -> Option<PathBuf> {
    let Some(base) = std::env::var_os("HELIA_REAL_DATA").map(PathBuf::from) else {
        eprintln!("HELIA_REAL_DATA is not set: skipping the FITS real-data test");
        return None;
    };
    [base.join(name), base.join("solar-2022-07-17").join(name)]
        .into_iter()
        .find(|p| p.exists())
}

fn stack(src: &mut dyn FrameSource) -> StackOutput {
    let cfg = StackConfig {
        keep: Keep::Fraction(0.25),
        ..StackConfig::default()
    };
    Pipeline::new(cfg)
        .run(src, &|_, _| {}, &AtomicBool::new(false))
        .expect("pipeline")
}

fn compare(label: &str, a: &StackOutput, b: &StackOutput) {
    assert_eq!(
        (a.image.width, a.image.height),
        (b.image.width, b.image.height)
    );
    let (mut max, mut sum) = (0.0f32, 0.0f64);
    for (x, y) in a.image.data.iter().zip(&b.image.data) {
        let d = (x - y).abs();
        max = max.max(d);
        sum += f64::from(d);
    }
    let mean = sum / a.image.data.len() as f64;
    eprintln!(
        "{label}: {} frames kept in both: {}; stack differs from the SER stack by max {max:.2e}, mean {mean:.2e} (of 1.0 full scale)",
        a.kept.len(),
        a.kept == b.kept
    );
    assert_eq!(
        a.kept, b.kept,
        "{label}: a different set of frames was kept"
    );
    assert!(max < 2e-4, "{label}: the stack differs by {max}");
}

/// The pattern as a camera program writes it for a bottom-up file: the one
/// the first stored row starts, which for an even height is the picture's
/// pattern flipped.
fn stored(p: BayerPattern, height: usize) -> &'static str {
    let name = |p: BayerPattern| match p {
        BayerPattern::Rggb => "RGGB",
        BayerPattern::Gbrg => "GBRG",
        BayerPattern::Grbg => "GRBG",
        BayerPattern::Bggr => "BGGR",
    };
    if !height.is_multiple_of(2) {
        return name(p);
    }
    name(match p {
        BayerPattern::Rggb => BayerPattern::Gbrg,
        BayerPattern::Gbrg => BayerPattern::Rggb,
        BayerPattern::Grbg => BayerPattern::Bggr,
        BayerPattern::Bggr => BayerPattern::Grbg,
    })
}

#[test]
fn fits_subs_and_a_fits_cube_stack_like_the_ser_they_came_from() {
    let Some(path) = find("15_20_57.ser") else {
        return;
    };
    let keep = std::env::var_os("HELIA_FITS_KEEP").map(PathBuf::from);
    let dir = keep.clone().unwrap_or_else(|| {
        std::env::temp_dir().join(format!("helia-fits-real-{}", std::process::id()))
    });
    let subs = dir.join("subs");
    std::fs::create_dir_all(&subs).expect("subs dir");

    let mut ser = open_capture(&path, &OpenOptions::default()).expect("open the SER");
    let (w, h) = ser.dimensions();
    let frames: Vec<Frame> = (0..FRAMES.min(ser.frame_count()))
        .map(|i| ser.read(i).expect("frame"))
        .collect();
    let pattern = frames[0].bayer.expect("the capture is a Bayer mosaic");
    let card = HeaderCard::string("BAYERPAT", stored(pattern, h), "Bayer pattern as stored");

    // The same frames as a SER of their own (the original is 100 frames).
    let hdr = SerHeader::new(w as u32, h as u32, ColorId::Bayer(pattern), 16);
    let mut writer = SerWriter::new(std::io::Cursor::new(Vec::new()), hdr).expect("SER header");
    for f in &frames {
        writer.write_frame(f, None).expect("SER frame");
    }
    let first_ser = dir.join("first.ser");
    std::fs::write(
        &first_ser,
        writer.finish().expect("SER finish").into_inner(),
    )
    .expect("write the SER");

    for (i, f) in frames.iter().enumerate() {
        // Times are made up (the file name order is the capture order).
        let cards = [
            card.clone(),
            HeaderCard::string(
                "DATE-OBS",
                format!("2022-07-17T14:20:{:06.3}", 10.0 + i as f64 * 0.0625),
                "[UTC] made up for the test",
            ),
            HeaderCard::float("EXPTIME", 0.0104, "[s]"),
        ];
        let mut bytes = Vec::new();
        fits::write_fits_cards(&mut bytes, f, FitsFormat::Int16, &cards, &[]).expect("encode");
        std::fs::write(subs.join(format!("Light_{i:04}.fit")), bytes).expect("write a sub");
    }
    let planes: Vec<&Frame> = frames.iter().collect();
    let mut cube = Vec::new();
    fits::write_fits_planes(&mut cube, &planes, FitsFormat::Int16, &[card], &[]).expect("cube");
    let cube_path = dir.join("cube.fits");
    std::fs::write(&cube_path, cube).expect("write the cube");

    let reference = stack(&mut LimitedSource::new(
        open_capture(&path, &OpenOptions::default()).expect("open"),
        0,
        FRAMES,
    ));
    for (label, input) in [
        ("folder of subs", subs.as_path()),
        ("cube", cube_path.as_path()),
    ] {
        let mut src = open_capture(input, &OpenOptions::default()).expect("open the FITS capture");
        assert_eq!(src.frame_count(), FRAMES.min(frames.len()));
        assert_eq!(src.read(0).expect("frame").bayer, Some(pattern));
        compare(label, &reference, &stack(&mut *src));
    }
    if keep.is_none() {
        let _ = std::fs::remove_dir_all(&dir);
    }
}
