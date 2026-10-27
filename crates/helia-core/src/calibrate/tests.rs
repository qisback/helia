// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

#![allow(clippy::unwrap_used, reason = "tests")]

use std::sync::atomic::AtomicBool;

use super::badpix::{self, DetectOptions};
use super::diag;
use super::master::{
    BuildOptions, CalInput, Combine, Master, MasterKind, build_dark, build_flat, combine_values,
    median,
};
use super::*;
use crate::frame::BayerPattern;
use crate::io::source::{FrameSource, MemorySource};
use crate::pipeline::{Pipeline, StackConfig, StackOutput};
use crate::quality::Keep;
use crate::testutil::{
    CaptureConfig, DefectConfig, SensorDefects, SyntheticCapture, SyntheticConfig, SyntheticSun,
};

fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

/// Master of `kind` from in-memory frames, through the same path files take.
fn dark_master(frames: Vec<Frame>, combine: Combine) -> Master {
    let mut input = in_memory(frames);
    build_dark(
        MasterKind::Dark,
        &mut input,
        &BuildOptions {
            combine,
            ..BuildOptions::default()
        },
        &|_| {},
        &no_cancel(),
    )
    .unwrap()
}

fn in_memory(frames: Vec<Frame>) -> CalInput {
    CalInput::from_frames(frames, None, "synthetic").unwrap()
}

fn flat_master(frames: Vec<Frame>, dark: Option<&Master>) -> super::master::FlatBuild {
    let mut input = in_memory(frames);
    build_flat(
        &mut input,
        dark,
        &BuildOptions::default(),
        &|_| {},
        &no_cancel(),
    )
    .unwrap()
}

fn rms(v: &[f32]) -> f32 {
    (v.iter().map(|x| f64::from(*x) * f64::from(*x)).sum::<f64>() / v.len() as f64).sqrt() as f32
}

#[test]
fn median_and_sigma_clip_ignore_an_outlier() {
    let mut v = vec![0.5, 0.51, 0.49, 0.5, 0.52, 0.48, 0.9];
    assert!((median(&mut v.clone()) - 0.5).abs() < 1e-6);
    let c = combine_values(&mut v, Combine::SigmaClip { kappa: 2.0 });
    assert!((c - 0.5).abs() < 0.01, "{c}");
    // A plain mean would be 0.563.
    assert!((median(&mut [1.0, 2.0, 3.0, 4.0]) - 2.5).abs() < 1e-6);
}

#[test]
fn combine_methods_parse() {
    assert_eq!("median".parse::<Combine>().unwrap(), Combine::Median);
    assert_eq!(
        "sigma".parse::<Combine>().unwrap(),
        Combine::SigmaClip { kappa: 3.0 }
    );
    assert_eq!(
        "sigma:2.5".parse::<Combine>().unwrap(),
        Combine::SigmaClip { kappa: 2.5 }
    );
    assert!("sigma:0".parse::<Combine>().is_err());
    assert!("mean".parse::<Combine>().is_err());
}

#[test]
fn master_dark_recovers_the_offset_and_beats_the_noise() {
    let s = SensorDefects::new(96, 80, 3, &DefectConfig::default());
    let noise = 0.01;
    let one = &s.dark_frames(1, noise, 1)[0];
    let m = dark_master(s.dark_frames(40, noise, 2), Combine::default());
    let err = |f: &[f32]| {
        let d: Vec<f32> = f.iter().zip(&s.offset).map(|(a, b)| a - b).collect();
        rms(&d)
    };
    let (e1, e40) = (err(&one.data), err(&m.frame.data));
    eprintln!("dark: single frame error {e1:.5}, master of 40 {e40:.5}");
    assert!(e40 < e1 / 4.5, "{e40} vs {e1}");
    assert_eq!(m.info.frames_used, 40);
    assert!(m.describe().contains("master dark of 40"));
}

#[test]
fn a_hot_frame_does_not_spoil_a_sigma_clipped_master() {
    let s = SensorDefects::new(64, 48, 3, &DefectConfig::default());
    let mut frames = s.dark_frames(15, 0.005, 2);
    // A cosmic ray in one frame.
    frames[4].data[1000] = 1.0;
    let m = dark_master(frames, Combine::default());
    assert!((m.frame.data[1000] - s.offset[1000]).abs() < 0.01);
}

#[test]
fn master_flat_is_normalised_and_recovers_the_response() {
    let cfg = DefectConfig {
        hot: 0,
        cold: 0,
        ..DefectConfig::default()
    };
    let s = SensorDefects::new(96, 80, 5, &cfg);
    let dark = dark_master(s.dark_frames(30, 0.004, 3), Combine::default());
    let built = flat_master(s.flat_frames(20, 0.6, 0.006, 4), Some(&dark));
    let f = &built.master.frame;
    let mean = f.data.iter().sum::<f32>() / f.data.len() as f32;
    assert!((mean - 1.0).abs() < 1e-4, "{mean}");
    let gm = s.gain.iter().sum::<f32>() / s.gain.len() as f32;
    let diff: Vec<f32> = f
        .data
        .iter()
        .zip(&s.gain)
        .map(|(a, g)| a - g / gm)
        .collect();
    eprintln!(
        "flat: rms error against the true response {:.5}",
        rms(&diff)
    );
    assert!(rms(&diff) < 0.004, "{}", rms(&diff));
}

#[test]
fn a_flat_without_a_dark_flat_says_so_and_is_off_by_the_offset() {
    let s = SensorDefects::new(64, 48, 5, &DefectConfig::default());
    let with = flat_master(
        s.flat_frames(10, 0.5, 0.004, 4),
        Some(&dark_master(
            s.dark_frames(20, 0.004, 3),
            Combine::default(),
        )),
    );
    let without = flat_master(s.flat_frames(10, 0.5, 0.004, 4), None);
    assert!(without.warnings.iter().any(|w| w.contains("no dark flat")));
    assert!(with.warnings.iter().all(|w| !w.contains("no dark flat")));
}

#[test]
fn exposure_mismatch_between_flat_and_dark_flat_is_reported() {
    let s = SensorDefects::new(64, 48, 5, &DefectConfig::default());
    let mut dark = dark_master(s.dark_frames(5, 0.004, 3), Combine::default());
    dark.info.exposure_s = Some(1.0);
    let mut input = in_memory(s.flat_frames(5, 0.5, 0.004, 4));
    input.exposure_s = Some(2.0);
    let built = build_flat(
        &mut input,
        Some(&dark),
        &BuildOptions::default(),
        &|_| {},
        &no_cancel(),
    )
    .unwrap();
    assert!(
        built.warnings.iter().any(|w| w.contains("same exposure")),
        "{:?}",
        built.warnings
    );
}

#[test]
fn rings_are_reported_and_a_clean_flat_is_not() {
    let rings = DefectConfig {
        rings: 0.02,
        dust: 0,
        hot: 0,
        cold: 0,
        ..DefectConfig::default()
    };
    let s = SensorDefects::new(160, 140, 8, &rings);
    let built = flat_master(s.flat_frames(10, 0.6, 0.004, 4), None);
    let d = &built.diagnostics;
    eprintln!(
        "rings: amplitude {:.4}, crossings {}",
        d.ring_amplitude, d.ring_crossings
    );
    assert!(
        d.warnings.iter().any(|w| w.contains("rings")),
        "{:?} {} {}",
        d.warnings,
        d.ring_amplitude,
        d.ring_crossings
    );
    let clean = DefectConfig {
        dust: 0,
        hot: 0,
        cold: 0,
        prnu: 0.002,
        ..DefectConfig::default()
    };
    let s = SensorDefects::new(160, 140, 8, &clean);
    let built = flat_master(s.flat_frames(10, 0.6, 0.004, 4), None);
    assert!(
        built
            .diagnostics
            .warnings
            .iter()
            .all(|w| !w.contains("rings")),
        "{:?}",
        built.diagnostics
    );
}

#[test]
fn strong_vignetting_and_dust_are_reported() {
    let cfg = DefectConfig {
        vignette: 0.9,
        dust: 12,
        dust_depth: 0.3,
        hot: 0,
        cold: 0,
        ..DefectConfig::default()
    };
    let s = SensorDefects::new(200, 160, 9, &cfg);
    let built = flat_master(s.flat_frames(10, 0.6, 0.004, 4), None);
    let d = &built.diagnostics;
    assert!(d.edge_to_centre < diag::STRONG_VIGNETTING, "{d:?}");
    assert!(d.warnings.iter().any(|w| w.contains("sweet spot")));
    assert!(d.dust_fraction > 0.0, "{d:?}");
}

#[test]
fn hot_and_cold_pixels_are_found_in_a_dark_and_a_flat() {
    let s = SensorDefects::new(128, 96, 11, &DefectConfig::default());
    let dark = dark_master(s.dark_frames(30, 0.004, 3), Combine::default());
    let built = flat_master(s.flat_frames(20, 0.6, 0.004, 4), Some(&dark));
    let map = badpix::detect(
        Some(&dark.frame),
        Some(&built.master.frame),
        false,
        &DetectOptions::default(),
        "test",
    );
    let truth: std::collections::HashSet<u32> = s
        .hot
        .iter()
        .chain(&s.cold)
        .map(|&i| u32::try_from(i).unwrap())
        .collect();
    let found: std::collections::HashSet<u32> = map.indices.iter().copied().collect();
    let missed = truth.difference(&found).count();
    let false_pos = found.difference(&truth).count();
    eprintln!(
        "bad pixels: {} true, {} found, {missed} missed, {false_pos} false",
        truth.len(),
        found.len()
    );
    assert_eq!(missed, 0);
    assert!(false_pos <= 2, "{false_pos}");
    assert!(map.hot >= s.hot.len() && map.cold >= s.cold.len());
}

#[test]
fn a_clean_sensor_has_no_bad_pixels() {
    let cfg = DefectConfig {
        hot: 0,
        cold: 0,
        ..DefectConfig::default()
    };
    let s = SensorDefects::new(128, 96, 11, &cfg);
    let dark = dark_master(s.dark_frames(30, 0.004, 3), Combine::default());
    let map = badpix::detect(
        Some(&dark.frame),
        None,
        false,
        &DetectOptions::default(),
        "",
    );
    assert!(map.indices.len() <= 1, "{}", map.indices.len());
}

#[test]
fn cosmetic_correction_keeps_the_colour_of_a_mosaic() {
    // Red/blue pixels read 0.8 and green 0.2; a hot red pixel must be replaced
    // from red neighbours, not from the green ones next to it.
    let (w, h) = (16, 16);
    let pattern = BayerPattern::Rggb;
    let mut data: Vec<f32> = (0..w * h)
        .map(|i| {
            if pattern.channel_at(i % w, i / w) == 1 {
                0.2
            } else {
                0.8
            }
        })
        .collect();
    let bad = 6 * w + 6; // even/even: a red pixel in RGGB
    data[bad] = 1.0;
    let c = badpix::Cosmetic::new(w, h, true, &[bad as u32]);
    c.apply(&mut data);
    assert!((data[bad] - 0.8).abs() < 1e-6, "{}", data[bad]);
    // As a mono frame the same pixel takes the median of its 8 neighbours.
    let mut mono: Vec<f32> = (0..w * h).map(|_| 0.5).collect();
    mono[bad] = 1.0;
    badpix::Cosmetic::new(w, h, false, &[bad as u32]).apply(&mut mono);
    assert!((mono[bad] - 0.5).abs() < 1e-6);
}

#[test]
fn a_cluster_of_bad_pixels_is_filled_from_good_ones() {
    let (w, h) = (16, 16);
    let mut data = vec![0.4f32; w * h];
    let bad: Vec<u32> = [(5, 5), (6, 5), (5, 6), (6, 6)]
        .iter()
        .map(|&(x, y)| (y * w + x) as u32)
        .collect();
    for &i in &bad {
        data[i as usize] = 1.0;
    }
    let c = badpix::Cosmetic::new(w, h, false, &bad);
    assert_eq!(c.len(), 4);
    c.apply(&mut data);
    assert!(data.iter().all(|&v| (v - 0.4).abs() < 1e-6));
}

#[test]
fn bad_pixel_text_lists_parse_and_reject() {
    let dir = std::env::temp_dir().join(format!("helia-bpl-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("bad.txt");
    std::fs::write(&p, "# hot\n3 4\n10, 2 # another\n\n").unwrap();
    let m = BadPixelMap::read_list(&p, 20, 10).unwrap();
    let mut want: Vec<u32> = vec![4 * 20 + 3, 2 * 20 + 10];
    want.sort_unstable();
    assert_eq!(m.indices, want);
    std::fs::write(&p, "3 40\n").unwrap();
    assert!(BadPixelMap::read_list(&p, 20, 10).is_err());
    std::fs::write(&p, "three four\n").unwrap();
    assert!(BadPixelMap::read_list(&p, 20, 10).is_err());
    // FITS round trip.
    let m = BadPixelMap {
        indices: vec![5, 77, 120],
        hot: 2,
        cold: 1,
        ..BadPixelMap::new(20, 10, "t")
    };
    let f = dir.join("bad.fits");
    m.write(&f).unwrap();
    assert_eq!(BadPixelMap::load(&f, 20, 10).unwrap().indices, m.indices);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn masters_round_trip_through_fits_and_catch_damage() {
    let s = SensorDefects::new(40, 30, 3, &DefectConfig::default());
    let m = dark_master(s.dark_frames(6, 0.004, 2), Combine::Median);
    let dir = std::env::temp_dir().join(format!("helia-master-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("dark.fits");
    m.write(&p).unwrap();
    let back = Master::read(&p, MasterKind::Dark).unwrap();
    assert_eq!(back.frame.data, m.frame.data);
    assert_eq!(back.info.frames_used, 6);
    assert_eq!(back.info.fingerprint, m.info.fingerprint);
    assert!(Master::read(&p, MasterKind::Flat).is_err());
    // A flipped byte in the data is caught by the fingerprint.
    let mut bytes = std::fs::read(&p).unwrap();
    let n = bytes.len();
    bytes[n - 1500] ^= 0x40;
    std::fs::write(&p, bytes).unwrap();
    assert!(Master::read(&p, MasterKind::Dark).is_err());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_foreign_flat_is_normalised_on_reading() {
    let dir = std::env::temp_dir().join(format!("helia-foreign-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("flat.fits");
    let f = Frame::new(8, 8, vec![0.5; 64]).unwrap();
    let mut out = std::fs::File::create(&p).unwrap();
    crate::io::fits::write_fits(&mut out, &f, crate::io::fits::FitsFormat::Float32).unwrap();
    drop(out);
    let m = Master::read(&p, MasterKind::Flat).unwrap();
    assert!(m.frame.data.iter().all(|&v| (v - 1.0).abs() < 1e-6));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn plan_rejects_masters_of_the_wrong_size_or_mosaic() {
    let s = SensorDefects::new(32, 24, 3, &DefectConfig::default());
    let dark = dark_master(s.dark_frames(3, 0.004, 2), Combine::Median);
    let cal = Calibration {
        dark: Some(dark.clone()),
        ..Calibration::default()
    };
    assert!(cal.plan(32, 24, None).is_ok());
    assert!(cal.plan(40, 24, None).is_err());
    let mut mosaic = dark;
    mosaic.info.bayer = Some(BayerPattern::Rggb);
    let cal = Calibration {
        dark: Some(mosaic),
        ..Calibration::default()
    };
    assert!(cal.plan(32, 24, Some(BayerPattern::Bggr)).is_err());
    assert!(cal.plan(32, 24, Some(BayerPattern::Rggb)).is_ok());
}

#[test]
fn dark_scaling_uses_the_bias_and_says_when_it_cannot() {
    let (w, h) = (8, 8);
    let frame = |v: f32| Frame::new(w, h, vec![v; w * h]).unwrap();
    let mk = |kind, v: f32, exp: Option<f64>| {
        Master::new(
            kind,
            frame(v),
            super::master::MasterInfo {
                kind,
                frames_used: 1,
                frames_total: 1,
                combine: String::new(),
                exposure_s: exp,
                bayer: None,
                source: String::new(),
                fingerprint: 0,
            },
        )
    };
    // bias 0.10, dark 0.14 at 1 s: thermal 0.04 per second; lights of 3 s.
    let cal = Calibration {
        dark: Some(mk(MasterKind::Dark, 0.14, Some(1.0))),
        bias: Some(mk(MasterKind::Bias, 0.10, Some(0.0))),
        options: ApplyOptions {
            scale_dark: true,
            light_exposure_s: Some(3.0),
            ..ApplyOptions::default()
        },
        ..Calibration::default()
    };
    let plan = cal.plan(w, h, None).unwrap();
    let mut d = vec![0.22f32; w * h]; // 0.10 + 3 * 0.04
    plan.apply(&mut d);
    assert!(d.iter().all(|v| v.abs() < 1e-6), "{}", d[0]);
    // Without a bias the whole dark is scaled, and the plan says so.
    let cal = Calibration { bias: None, ..cal };
    let plan = cal.plan(w, h, None).unwrap();
    assert!(plan.notes.iter().any(|n| n.contains("no master bias")));
    // Unknown exposure: not scaled, and says so.
    let cal = Calibration {
        options: ApplyOptions {
            scale_dark: true,
            light_exposure_s: None,
            ..ApplyOptions::default()
        },
        ..cal
    };
    assert!(
        cal.plan(w, h, None)
            .unwrap()
            .notes
            .iter()
            .any(|n| n.contains("not scaled"))
    );
}

#[test]
fn provenance_names_every_step() {
    let s = SensorDefects::new(32, 24, 3, &DefectConfig::default());
    let dark = dark_master(s.dark_frames(3, 0.004, 2), Combine::Median);
    let cal = Calibration {
        dark: Some(dark),
        bad: Some(BadPixelMap::new(32, 24, "x")),
        ..Calibration::default()
    };
    let steps: Vec<String> = cal.provenance().into_iter().map(|p| p.step).collect();
    assert_eq!(steps, ["DARK-SUBTRACTION", "BAD-PIXEL-CORRECTION"]);
    assert!(cal.note_lines()[0].starts_with("Calibration: dark subtracted"));
}

// ---------------------------------------------------------------- end to end

struct Rig {
    clean: Vec<Frame>,
    defective: Vec<Frame>,
    sensor: SensorDefects,
}

fn rig(cfg: &DefectConfig, frames: usize) -> Rig {
    let sun = SyntheticSun::new(&SyntheticConfig::default());
    let cap = SyntheticCapture::generate(
        &sun,
        &CaptureConfig {
            frames,
            max_shift: 5.0,
            ..CaptureConfig::default()
        },
    );
    let sensor = SensorDefects::new(192, 160, 21, cfg);
    let clean: Vec<Frame> = cap
        .frames
        .iter()
        .map(|f| Frame {
            data: f.data.iter().map(|v| v * 0.75).collect(),
            ..f.clone()
        })
        .collect();
    let defective = clean
        .iter()
        .map(|f| {
            let mut d = f.clone();
            sensor.apply(&mut d);
            d
        })
        .collect();
    Rig {
        clean,
        defective,
        sensor,
    }
}

fn stack(frames: &[Frame], calibration: Option<Calibration>) -> StackOutput {
    let cfg = StackConfig {
        keep: Keep::Fraction(0.5),
        normalise: false,
        multipoint: None,
        calibration: calibration.map(Calibration::shared),
        ..StackConfig::default()
    };
    let mut src = MemorySource(frames.to_vec());
    Pipeline::new(cfg)
        .run(&mut src, &|_, _| {}, &no_cancel())
        .unwrap()
}

/// RMS of the stack minus the stack of the clean frames, after the one scale
/// factor a flat is allowed to leave (it normalises to a mean of 1, so the
/// level is the mean response, not 1).
fn residual(out: &Frame, clean: &Frame) -> f32 {
    let border = 14;
    let (mut num, mut den) = (0.0f64, 0.0f64);
    let mut pairs = Vec::new();
    for y in border..out.height - border {
        for x in border..out.width - border {
            let (a, b) = (f64::from(out.get(x, y)), f64::from(clean.get(x, y)));
            num += a * b;
            den += a * a;
            pairs.push((a, b));
        }
    }
    let k = num / den;
    let d: Vec<f32> = pairs.iter().map(|&(a, b)| (a * k - b) as f32).collect();
    rms(&d)
}

fn full_calibration(r: &Rig) -> Calibration {
    let dark = dark_master(r.sensor.dark_frames(40, 0.004, 31), Combine::default());
    let flat = flat_master(r.sensor.flat_frames(30, 0.6, 0.004, 32), Some(&dark));
    let bad = badpix::detect(
        Some(&dark.frame),
        Some(&flat.master.frame),
        false,
        &DetectOptions::default(),
        "synthetic",
    );
    Calibration {
        dark: Some(dark),
        flat: Some(flat.master),
        bad: Some(bad),
        ..Calibration::default()
    }
}

/// Residual pattern, in dB below the uncalibrated stack's, must reach this.
const REQUIRED_DB: f32 = 20.0;

#[test]
fn calibration_removes_the_fixed_pattern_from_the_stack() {
    let r = rig(&DefectConfig::default(), 40);
    let reference = stack(&r.clean, None).image;
    let raw = residual(&stack(&r.defective, None).image, &reference);
    let cal = full_calibration(&r);
    let dark_only = residual(
        &stack(
            &r.defective,
            Some(Calibration {
                dark: cal.dark.clone(),
                ..Calibration::default()
            }),
        )
        .image,
        &reference,
    );
    let flat_only = residual(
        &stack(
            &r.defective,
            Some(Calibration {
                flat: cal.flat.clone(),
                ..Calibration::default()
            }),
        )
        .image,
        &reference,
    );
    let all = residual(&stack(&r.defective, Some(cal)).image, &reference);
    let db = |x: f32| 20.0 * (raw / x).log10();
    eprintln!(
        "residual pattern rms: none {raw:.5}, dark only {dark_only:.5} ({:.1} dB), flat only {flat_only:.5} ({:.1} dB), all {all:.5} ({:.1} dB)",
        db(dark_only),
        db(flat_only),
        db(all)
    );
    assert!(db(all) >= REQUIRED_DB, "{} dB", db(all));
    assert!(all < dark_only && all < flat_only);
}

#[test]
fn calibrated_frames_decode_the_same_on_every_path() {
    // `read`, the raw bytes the CPU and GPU decoders share, and the Bayer-less
    // decode must all agree to the last bit.
    let r = rig(&DefectConfig::default(), 6);
    let cal = full_calibration(&r);
    let mut inner = MemorySource(r.defective.clone());
    let plan = cal.plan(192, 160, None).unwrap();
    let mut src = CalibratedSource::new(&mut inner, plan).unwrap();
    let read = src.read(3).unwrap();
    let shared = src.shared().unwrap();
    let mut raw = Vec::new();
    shared.read_raw(3, &mut raw).unwrap();
    assert_eq!(shared.layout().decode(&raw), read.data);
    let via = shared
        .read_mono(
            3,
            &crate::debayer::Debayer::default(),
            &crate::profile::Profile::new(),
        )
        .unwrap();
    assert_eq!(via.data, read.data);
    assert!(src.describe().contains("calibrated"));
}

#[test]
fn calibration_does_not_change_a_clean_capture() {
    // Masters of a perfect sensor (gain 1, offset 0) leave the frames alone.
    let r = rig(
        &DefectConfig {
            vignette: 0.0,
            dust: 0,
            prnu: 0.0,
            fpn: 0.0,
            column: 0.0,
            hot: 0,
            cold: 0,
            ..DefectConfig::default()
        },
        6,
    );
    let mut s = r.sensor.clone();
    s.offset.fill(0.0);
    let dark = dark_master(s.dark_frames(5, 0.0, 1), Combine::Median);
    let flat = flat_master(s.flat_frames(5, 0.6, 0.0, 2), Some(&dark));
    let cal = Calibration {
        dark: Some(dark),
        flat: Some(flat.master),
        ..Calibration::default()
    };
    let mut inner = MemorySource(r.clean.clone());
    let mut src = CalibratedSource::new(&mut inner, cal.plan(192, 160, None).unwrap()).unwrap();
    let f = src.read(0).unwrap();
    let diff: Vec<f32> = f
        .data
        .iter()
        .zip(&r.clean[0].data)
        .map(|(a, b)| a - b)
        .collect();
    assert!(rms(&diff) < 1e-3, "{}", rms(&diff));
}

#[test]
fn a_capture_of_the_wrong_size_is_refused() {
    let r = rig(&DefectConfig::default(), 4);
    let cal = full_calibration(&r);
    let cfg = StackConfig {
        calibration: Some(cal.shared()),
        ..StackConfig::default()
    };
    let small: Vec<Frame> = (0..4)
        .map(|_| Frame::new(100, 80, vec![0.3; 8000]).unwrap())
        .collect();
    let mut src = MemorySource(small);
    let e = Pipeline::new(cfg)
        .run(&mut src, &|_, _| {}, &no_cancel())
        .unwrap_err();
    assert!(e.to_string().contains("same camera"), "{e}");
}

#[test]
fn the_stack_records_what_was_applied() {
    let r = rig(&DefectConfig::default(), 12);
    let out = stack(&r.defective, Some(full_calibration(&r)));
    let steps: Vec<&str> = out.provenance.iter().map(|p| p.step.as_str()).collect();
    assert_eq!(
        steps[..3],
        ["DARK-SUBTRACTION", "FLAT-FIELDING", "BAD-PIXEL-CORRECTION"]
    );
    // The three masters, and the dead pixels the flat revealed.
    assert_eq!(out.calibration_notes.len(), 4);
    assert!(out.calibration_notes[3].contains("corrected as bad"));
    assert!(out.calibration_notes[..2].iter().all(|n| n.contains("fp:")));
}

#[test]
fn deconvolution_amplifies_an_uncalibrated_pattern_and_calibration_prevents_it() {
    // Richardson-Lucy sharpens whatever is in the stack, including the dust and
    // fixed pattern that stacking did not remove. Compare the residual (against
    // the stack of the same capture from a perfect sensor) before and after
    // sharpening, with and without calibration.
    use crate::edit::{Deconvolve, EditList, Op, Psf, apply_cpu};
    let r = rig(&DefectConfig::default(), 40);
    let reference = stack(&r.clean, None).image;
    let raw = stack(&r.defective, None).image;
    let cal = stack(&r.defective, Some(full_calibration(&r))).image;
    let rl = |f: &Frame| {
        apply_cpu(
            f,
            &EditList::new(vec![Op::Deconvolve(Deconvolve {
                psf: Psf::Gaussian { sigma: 1.5 },
                iterations: 20,
                ..Deconvolve::default()
            })]),
        )
        .unwrap()
    };
    let (ref_rl, raw_rl, cal_rl) = (rl(&reference), rl(&raw), rl(&cal));
    let before = (residual(&raw, &reference), residual(&cal, &reference));
    let after = (residual(&raw_rl, &ref_rl), residual(&cal_rl, &ref_rl));
    eprintln!(
        "residual pattern before RL: uncalibrated {:.5}, calibrated {:.5}; after RL: uncalibrated {:.5}, calibrated {:.5}",
        before.0, before.1, after.0, after.1
    );
    assert!(after.0 > before.0, "RL should make the raw pattern worse");
    assert!(
        after.1 < after.0 / 10.0,
        "calibration must hold at 20 dB after RL"
    );
}

#[test]
fn stuck_pixels_are_found_in_the_lights_and_the_limb_is_not_one() {
    // Light frames only, no darks: hot and cold pixels that read the same wrong
    // value while the Sun moves, and a sharp limb whose pixels sit on a slope.
    let cfg = DefectConfig {
        dust: 0,
        vignette: 0.0,
        fpn: 0.0,
        column: 0.0,
        ..DefectConfig::default()
    };
    let r = rig(&cfg, 60);
    let mut src = MemorySource(r.defective.clone());
    let stats = badpix::sample_lights(&mut src, 60).unwrap();
    let map = badpix::detect_stuck(&stats, false, &DetectOptions::default(), "lights", &[]);
    let truth: std::collections::HashSet<u32> = r
        .sensor
        .hot
        .iter()
        .chain(&r.sensor.cold)
        .map(|&i| u32::try_from(i).unwrap())
        .collect();
    let found: std::collections::HashSet<u32> = map.indices.iter().copied().collect();
    // The ones on the disc are found; those in the sky are invisible without a dark.
    let on_disc = |i: u32| {
        stats.mean[i as usize] > 0.3
            || truth.contains(&i) && {
                let (x, y) = (i as usize % 192, i as usize / 192);
                let (dx, dy) = (x as f32 - 96.0, y as f32 - 80.0);
                dx * dx + dy * dy < 40.0 * 40.0
            }
    };
    let wanted: Vec<u32> = truth.iter().copied().filter(|&i| on_disc(i)).collect();
    let missed = wanted.iter().filter(|i| !found.contains(i)).count();
    let false_pos = found.difference(&truth).count();
    eprintln!(
        "stuck pixels from the lights: {} on the disc, {missed} missed, {false_pos} false of {} reported",
        wanted.len(),
        found.len()
    );
    assert_ne!(wanted.len(), 0);
    assert_eq!(missed, 0);
    assert!(false_pos <= 3, "{false_pos}");
}

#[test]
fn a_burned_in_timestamp_is_not_reported_as_stuck_pixels() {
    // A timestamp has changing digits (so it is found as text) and fixed marks
    // that, to the detector, are pixels that stay white while everything around
    // them is dark: stuck pixels. They must be left out of the map.
    let cfg = DefectConfig {
        hot: 0,
        cold: 0,
        dust: 0,
        vignette: 0.0,
        fpn: 0.0,
        column: 0.0,
        ..DefectConfig::default()
    };
    let r = rig(&cfg, 40);
    let mut text = crate::frame::Rect::new(0, 0, 0, 0);
    let marks = |t: &crate::frame::Rect| -> Vec<usize> {
        (0..4)
            .map(|k| (t.y1 + 2) * 192 + t.x0 + 20 + 10 * k)
            .collect()
    };
    let frames: Vec<Frame> = r
        .defective
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let mut f = f.clone();
            text = crate::testutil::burn_timestamp(&mut f, i);
            for m in marks(&text) {
                f.data[m] = 1.0;
            }
            f
        })
        .collect();
    let mark_set: std::collections::HashSet<u32> = marks(&text)
        .into_iter()
        .map(|m| u32::try_from(m).unwrap())
        .collect();
    let mut src = MemorySource(frames);
    let stats = badpix::sample_lights(&mut src, 40).unwrap();
    let naive = badpix::detect_stuck(&stats, false, &DetectOptions::default(), "", &[]);
    let naive_marks = naive
        .indices
        .iter()
        .filter(|i| mark_set.contains(i))
        .count();
    let found =
        badpix::find_stuck(&mut src, 40, false, &DetectOptions::default(), "lights").unwrap();
    eprintln!(
        "timestamp: {naive_marks} of {} fixed marks reported without the exclusion, {} with it",
        mark_set.len(),
        found
            .indices
            .iter()
            .filter(|i| mark_set.contains(i))
            .count()
    );
    assert_eq!(
        naive_marks,
        mark_set.len(),
        "the marks are stuck pixels to the detector"
    );
    assert!(found.indices.iter().all(|i| !mark_set.contains(i)));
}

#[test]
fn calibration_frames_come_from_ser_avi_fits_files_and_folders() {
    use crate::io::avi::write_test_avi;
    use crate::io::fits::{FitsFormat, write_fits};
    let s = SensorDefects::new(32, 24, 7, &DefectConfig::default());
    let frames = s.dark_frames(6, 0.004, 5);
    let dir = std::env::temp_dir().join(format!("helia-calin-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("fits")).unwrap();
    // A folder of FITS stills, a SER and an AVI of the same sensor.
    for (i, f) in frames.iter().enumerate() {
        let mut out = std::fs::File::create(dir.join("fits").join(format!("d{i}.fits"))).unwrap();
        write_fits(&mut out, f, FitsFormat::Float32).unwrap();
    }
    std::fs::write(dir.join("d.ser"), crate::testutil::ser_bytes(&frames)).unwrap();
    std::fs::write(
        dir.join("d.avi"),
        write_test_avi(&frames, b"DIB ", 8, false, None),
    )
    .unwrap();
    std::fs::write(dir.join("notes.txt"), "not a frame").unwrap();
    let opts = crate::io::source::OpenOptions {
        bayer: crate::io::source::BayerSetting::Mono,
        ..Default::default()
    };
    let build = |paths: &[std::path::PathBuf]| {
        let mut input = CalInput::open(paths, &opts).unwrap();
        let n = input.frame_count();
        let m = build_dark(
            MasterKind::Dark,
            &mut input,
            // The median moves continuously with the data; a clipped mean jumps
            // when a quantised value crosses the rejection limit.
            &BuildOptions {
                combine: Combine::Median,
                ..BuildOptions::default()
            },
            &|_| {},
            &no_cancel(),
        )
        .unwrap();
        (n, m)
    };
    let (n_fits, fits_master) = build(&[dir.join("fits")]);
    let (n_ser, ser_master) = build(&[dir.join("d.ser")]);
    let (n_avi, avi_master) = build(&[dir.join("d.avi")]);
    assert_eq!((n_fits, n_ser, n_avi), (6, 6, 6));
    // The same sensor read three ways agrees to the precision of the container.
    let diff = |a: &Master, b: &Master, tol: f32| {
        a.frame
            .data
            .iter()
            .zip(&b.frame.data)
            .all(|(x, y)| (x - y).abs() <= tol)
    };
    assert!(diff(&fits_master, &ser_master, 2.0 / 65535.0));
    assert!(diff(&fits_master, &avi_master, 1.0 / 255.0));
    // Several inputs at once, and a folder that holds all three (the text file is ignored).
    let (n_all, _) = build(&[dir.clone(), dir.join("fits")]);
    assert_eq!(n_all, 6 + 6 + 6);
    // Different sizes are refused, nothing at all is refused.
    let other = dir.join("o.ser");
    std::fs::write(&other, crate::testutil::ser_bytes(&[Frame::zeros(16, 16)])).unwrap();
    assert!(CalInput::open(&[dir.join("d.ser"), other], &opts).is_err());
    assert!(CalInput::open(&[], &opts).is_err());
    std::fs::remove_dir_all(&dir).ok();
}
