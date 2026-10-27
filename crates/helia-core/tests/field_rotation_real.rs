// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Field rotation on the real capture `15_20_57.ser` (set `HELIA_REAL_DATA` to
//! the folder holding it). Its frames carry real timestamps over ten seconds,
//! which is far too short to rotate: that is reported. Then its frames are
//! given the time stamps of an hour-long session and turned by the field
//! rotation of an altazimuth mount at those times, and the stack of that
//! capture with and without the correction is compared with the stack of the
//! frames as they were.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use helia_core::debayer::Debayer;
use helia_core::field_rotation::{FieldRotation, Reference, rotate_ccw};
use helia_core::filter::gaussian_blur;
use helia_core::io::solarnet::{CaptureInfo, Site};
use helia_core::io::source::{LimitedSource, MemorySource, OpenOptions, open_capture};
use helia_core::measure::{centre_crop, granulation_contrast_in};
use helia_core::pipeline::{Pipeline, StackConfig};
use helia_core::quality::{Metric, score};
use helia_core::{Frame, Keep, Kernel};

const FRAMES: usize = 100;

/// A site for the demonstration: a mid-northern latitude, not anybody's own.
const SITE: Site = Site {
    lat_deg: 51.5,
    lon_deg: -0.1,
    alt_m: 30.0,
};

fn find(name: &str) -> Option<PathBuf> {
    let base = PathBuf::from(std::env::var_os("HELIA_REAL_DATA")?);
    [base.join(name), base.join("solar-2022-07-17").join(name)]
        .into_iter()
        .find(|p| p.exists())
}

fn sharp(f: &Frame) -> f32 {
    score(&gaussian_blur(f, 2.0), Metric::GradientEnergy, None)
}

fn stack(frames: Vec<Frame>, multipoint: bool, rotation: Option<FieldRotation>) -> Frame {
    let cfg = StackConfig {
        keep: Keep::Fraction(0.5),
        multipoint: multipoint.then(Default::default),
        field_rotation: rotation,
        ..StackConfig::default()
    };
    Pipeline::new(cfg)
        .run(
            &mut MemorySource(frames),
            &|_, _| {},
            &AtomicBool::new(false),
        )
        .expect("stack")
        .image
}

#[test]
fn field_rotation_on_the_real_capture() {
    let Some(path) = find("15_20_57.ser") else {
        eprintln!("HELIA_REAL_DATA is not set or holds no 15_20_57.ser: skipped");
        return;
    };
    // What the capture's own stamps say.
    let info = CaptureInfo::read(&path, None);
    let own = FieldRotation {
        site: SITE,
        mirror: false,
        reference: Reference::Middle,
        target: None,
        auto_mirror: false,
        frame_unix: info.frame_unix.clone(),
    }
    .plan()
    .expect("the capture has timestamps");
    eprintln!(
        "own stamps: {:.4} degrees over {:.1} s, {:.3} degrees per minute, {:.2} px at a limb of 445 px",
        own.span_deg,
        info.frame_unix.last().copied().flatten().unwrap_or(0.0)
            - info.frame_unix.first().copied().flatten().unwrap_or(0.0),
        own.rate_deg_per_min,
        own.limb_shift_px(445.0)
    );
    assert!(own.limb_shift_px(445.0) < 1.0);

    // The frames, demosaiced to luminance.
    let inner = open_capture(&path, &OpenOptions::default()).expect("open");
    let mut src = LimitedSource::new(inner, 0, FRAMES);
    let debayer = Debayer::default();
    let frames: Vec<Frame> = (0..FRAMES)
        .map(|i| {
            use helia_core::io::source::FrameSource;
            debayer.apply_owned(src.read(i).expect("frame"))
        })
        .collect();

    // The same frames as an hour of an altazimuth session, one every 36 s.
    let t0 = info.start_unix.expect("start time");
    let times: Vec<Option<f64>> = (0..FRAMES).map(|i| Some(t0 + i as f64 * 36.0)).collect();
    let rotation = FieldRotation {
        site: SITE,
        mirror: false,
        reference: Reference::Middle,
        target: None,
        auto_mirror: false,
        frame_unix: times,
    };
    let plan = rotation.plan().expect("plan");
    eprintln!(
        "an hour: {:.2} degrees ({:.3} per minute, Sun at {:.0} degrees), turns up to {:.2} degrees, {:.1} px at a limb of 445 px",
        plan.span_deg,
        plan.rate_deg_per_min,
        plan.altitude_deg,
        plan.max_turn_deg,
        plan.limb_shift_px(445.0)
    );
    let turned: Vec<Frame> = frames
        .iter()
        .zip(&plan.q_deg)
        .map(|(f, &q)| rotate_ccw(f, -(q - plan.reference_q_deg), Kernel::Lanczos3))
        .collect();

    for multipoint in [false, true] {
        let truth = stack(frames.clone(), multipoint, None);
        let rotating = stack(turned.clone(), multipoint, None);
        let fixed = stack(turned.clone(), multipoint, Some(rotation.clone()));
        let (t, r, f) = (sharp(&truth), sharp(&rotating), sharp(&fixed));
        // Granulation in a crop of the disc centre (the 1 to 4 pixel band).
        let band = |img: &Frame| {
            let area = centre_crop(img, 400);
            granulation_contrast_in(img, area).granulation
        };
        let (band_truth, band_plain, band_fixed) = (band(&truth), band(&rotating), band(&fixed));
        eprintln!(
            "multipoint {multipoint}: granulation band (1 to 4 px) at the disc centre: frames as they were {:.3}%, rotating capture {:.3}%, corrected {:.3}%",
            band_truth * 100.0,
            band_plain * 100.0,
            band_fixed * 100.0
        );
        assert!(band_fixed > band_plain, "{band_fixed} against {band_plain}");
        eprintln!(
            "multipoint {multipoint}: sharpness of the stack of the frames as they were {t:.5}, of the rotating capture {r:.5}, corrected {f:.5} ({:.0}% and {:.0}% of the first)",
            100.0 * r / t,
            100.0 * f / t
        );
        assert!(f > r, "{f} against {r}");
        assert!(f > 0.9 * t, "{f} against {t}");
    }
}
