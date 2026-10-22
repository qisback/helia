// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Tests against real solar captures (ZWO ASI482MC, `SharpCap`, 2022-07-17, and the owner's 2026-10-20 captures).
//!
//! The captures are large and are never stored in the repository. Point
//! `HELIA_REAL_DATA` at the directory holding them to run these tests;
//! without it every test prints a note and passes.
#![allow(
    clippy::many_single_char_names,
    reason = "width, height and pixel coordinates are the natural names"
)]

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use helia_core::align::multipoint::ApGrid;
use helia_core::io::avi::{AviReader, PixelFormat};
use helia_core::io::ser::{ColorId, SerReader};
use helia_core::io::sidecar::CameraSettings;
use helia_core::io::source::{FrameSource, LimitedSource, OpenOptions, open_capture};
use helia_core::mask::{self, MaskConfig};
use helia_core::pipeline::{Pipeline, StackConfig, StackOutput};
use rayon::prelude::*;

use helia_core::{BayerPattern, EditList, Frame, Keep, Metric, Op, Preset, Rect, filter, quality};

fn data_dir() -> Option<PathBuf> {
    let Some(dir) = std::env::var_os("HELIA_REAL_DATA").map(PathBuf::from) else {
        eprintln!("HELIA_REAL_DATA is not set: skipping real-data tests");
        return None;
    };
    Some(dir)
}

/// Directory with the captures, accepting either the folder itself or its parent.
fn capture(name: &str) -> Option<PathBuf> {
    let base = data_dir()?;
    [
        base.join(name),
        base.join("solar-2022-07-17").join(name),
        base.join("owner-2026-10-20").join(name),
    ]
    .into_iter()
    .find(|p| p.exists())
    .or_else(|| {
        eprintln!("{name} not found under {}: skipping", base.display());
        None
    })
}

fn check_frame(f: &Frame, w: usize, h: usize) {
    assert_eq!((f.width, f.height), (w, h));
    assert!(
        f.data
            .iter()
            .all(|v| v.is_finite() && *v >= 0.0 && *v <= 1.0001)
    );
    let (_, hi) = f.min_max();
    assert!(hi > 0.05, "frame looks empty (max {hi})");
}

/// Run the whole pipeline on the first 100 frames and compare with the best single frame.
fn stack_subset(path: &Path, opts: OpenOptions) {
    let inner = open_capture(path, &opts).expect("open capture");
    let mut src = LimitedSource::new(inner, 0, 100);
    let cfg = StackConfig {
        keep: Keep::Fraction(0.25),
        ..StackConfig::default()
    };
    let t = Instant::now();
    let out = Pipeline::new(cfg.clone())
        .run(&mut src, &|_, _| {}, &AtomicBool::new(false))
        .expect("pipeline");
    let wall = t.elapsed();
    // Plain gradient energy rewards pixel noise, which stacking removes, so the
    // comparison blurs everything first (sigma 2 px) to measure detail rather
    // than noise. The unblurred figures are printed for reference.
    //
    // The pipeline flattens burned-in text (and any exclusion) in every frame it
    // reads (issue #34), so the stack has none, while the raw frames the
    // test reads still carry it. The text edges are the highest-contrast detail
    // in the frame: counted in one side only they made a correct stack look 28%
    // short (issue #62). Both sides are measured with the same rectangles
    // blanked.
    let masked = out.masked.clone();
    let robust = |f: &Frame| {
        quality::score(
            &filter::gaussian_blur(&blank(f, &masked), 2.0),
            Metric::GradientEnergy,
            None,
        )
    };
    let plain_best = out.scores.iter().copied().fold(0.0f32, f32::max);
    let plain_stack = quality::score(&out.image, Metric::GradientEnergy, None);
    let mut detail = Vec::new();
    let mut noise_best = 0.0;
    let all: Vec<usize> = (0..src.frame_count()).collect();
    for chunk in all.chunks(16) {
        let frames: Vec<Frame> = chunk
            .iter()
            .map(|&i| cfg.debayer.apply_owned(src.read(i).expect("frame")))
            .collect();
        let part: Vec<(f32, f32)> = frames
            .par_iter()
            .map(|f| (robust(f), relative_noise(f)))
            .collect();
        for (k, &(d, n)) in part.iter().enumerate() {
            if chunk[k] == out.ranking[0] {
                noise_best = n;
            }
            detail.push(d);
        }
    }
    let stacked = robust(&out.image);
    let (noise_stack, best_single) = (
        relative_noise(&out.image),
        detail.iter().copied().fold(0.0f32, f32::max),
    );
    let mut kept: Vec<f32> = out.kept.iter().map(|&i| detail[i]).collect();
    kept.sort_by(f32::total_cmp);
    let kept_median = kept[kept.len() / 2];
    let stages: Vec<String> = out
        .timings
        .iter()
        .map(|(s, d)| format!("{}={:.2}s", s.label(), d.as_secs_f32()))
        .collect();
    eprintln!(
        "{}: {} | backend {} | wall {:.2}s | blurred detail: stack {stacked:.5}, best single {best_single:.5} ({:.2}x), median kept frame {kept_median:.5} ({:.2}x); relative noise: stack {noise_stack:.4} vs best single {noise_best:.4}; plain gradient: {plain_stack:.5} vs {plain_best:.5} | {}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
        src.describe(),
        out.backend,
        wall.as_secs_f32(),
        stacked / best_single,
        stacked / kept_median,
        stages.join(" ")
    );
    // Global-only stack of the same frames, to show what multipoint buys.
    let mut global_src =
        LimitedSource::new(open_capture(path, &opts).expect("open capture"), 0, 100);
    let global_cfg = StackConfig {
        multipoint: None,
        ..cfg.clone()
    };
    let global = Pipeline::new(global_cfg)
        .run(&mut global_src, &|_, _| {}, &AtomicBool::new(false))
        .expect("global pipeline");
    let global_detail = robust(&global.image);
    eprintln!(
        "  multipoint detail {stacked:.5} vs global-only {global_detail:.5} ({:.2}x)",
        stacked / global_detail
    );
    // The stack is an average of 25 frames chosen from 100, while the best
    // single frame is the maximum of 100 noisy draws, so it is not expected to
    // be beaten on detail alone. What must hold: the stack keeps nearly all of
    // that frame's detail, multipoint alignment is no worse than global-only
    // (on 100-frame subsets the two are close; over whole captures multipoint
    // pulls clearly ahead because the seeing distorts the disc locally), and
    // pixel noise falls whenever the frame is bright enough for the estimate
    // to mean much.
    assert!(
        stacked > 0.85 * best_single,
        "stack detail {stacked} fell below 85% of best single frame {best_single}"
    );
    assert!(
        stacked > 0.97 * global_detail,
        "multipoint detail {stacked} fell behind global-only {global_detail}"
    );
    if noise_best < 0.2 {
        assert!(
            noise_stack < 0.9 * noise_best,
            "stack noise {noise_stack} must be below the best single frame's {noise_best}"
        );
    }
}

/// A copy of `f` with the rectangles set to zero.
fn blank(f: &Frame, rects: &[Rect]) -> Frame {
    let mut g = f.clone();
    for r in rects {
        for y in r.y0..r.y1.min(g.height) {
            for x in r.x0..r.x1.min(g.width) {
                g.data[y * g.width + x] = 0.0;
            }
        }
    }
    g
}

/// Pixel noise relative to the mean level, from the residual against the
/// 4-neighbour mean over the central quarter of the frame.
fn relative_noise(f: &Frame) -> f32 {
    let (x0, x1, y0, y1) = (f.width / 4, 3 * f.width / 4, f.height / 4, 3 * f.height / 4);
    let (mut s2, mut sum, mut n) = (0.0f64, 0.0f64, 0.0f64);
    for y in y0..y1 {
        for x in x0..x1 {
            let c = f64::from(f.get(x, y));
            let nb = 0.25
                * f64::from(f.get(x - 1, y) + f.get(x + 1, y) + f.get(x, y - 1) + f.get(x, y + 1));
            s2 += (c - nb) * (c - nb);
            sum += c;
            n += 1.0;
        }
    }
    ((s2 / n / 1.25).sqrt() / (sum / n).max(1e-9)) as f32
}

#[test]
fn colour_ser_header_frames_and_stack() {
    let Some(path) = capture("15_20_57.ser") else {
        return;
    };
    let mut r = SerReader::new(BufReader::new(File::open(&path).expect("open"))).expect("ser");
    let h = r.header().clone();
    assert_eq!(h.color, ColorId::Bayer(BayerPattern::Rggb));
    assert_eq!(
        (h.width, h.height, h.pixel_depth, h.frame_count),
        (1920, 1080, 16, 100)
    );
    assert!(!h.little_endian, "SharpCap writes the flag as 0");
    assert!(
        r.data_is_little_endian(),
        "but the pixels are little-endian"
    );
    for i in [0, 50, 99] {
        let f = r.read_frame(i).expect("frame");
        assert_eq!(f.bayer, Some(BayerPattern::Rggb));
        check_frame(&f, 1920, 1080);
    }
    stack_subset(&path, OpenOptions::default());
}

#[test]
fn mono_ser_header_frames_and_stack() {
    let Some(path) = capture("Sun_Test.ser") else {
        return;
    };
    let mut r = SerReader::new(BufReader::new(File::open(&path).expect("open"))).expect("ser");
    let h = r.header().clone();
    assert_eq!(h.color, ColorId::Mono);
    assert_eq!(
        (h.width, h.height, h.pixel_depth, h.frame_count),
        (1920, 1080, 8, 631)
    );
    for i in [0, 315, 630] {
        check_frame(&r.read_frame(i).expect("frame"), 1920, 1080);
    }
    stack_subset(&path, OpenOptions::default());
}

/// Means of the four pixel positions of the central 300 x 300 crop (even
/// origin) and their spread, (largest - smallest) / average.
fn position_spread(f: &Frame) -> ([f64; 4], f64) {
    let (x0, y0) = (((f.width - 300) / 2) & !1, ((f.height - 300) / 2) & !1);
    let mut sum = [0.0f64; 4];
    for y in y0..y0 + 300 {
        for x in x0..x0 + 300 {
            sum[(y & 1) * 2 + (x & 1)] += f64::from(f.get(x, y));
        }
    }
    let m = sum.map(|s| s / 22_500.0);
    let mean = m.iter().sum::<f64>() / 4.0;
    let hi = m.iter().copied().fold(f64::MIN, f64::max);
    let lo = m.iter().copied().fold(f64::MAX, f64::min);
    (m, (hi - lo) / mean)
}

/// `Sun_Test.ser` is a colour camera behind an H-alpha filter that `SharpCap`
/// labelled mono (`ColorID` 0) although the frames are raw RGGB. Trusting the
/// header kept the mosaic in the stack (a 2x2 spread of about a third); the
/// pixels are checked now, and the stack has none.
#[test]
fn mono_labelled_bayer_ser_stacks_without_a_grid() {
    use helia_core::debayer::{Channel, Debayer};
    use helia_core::io::capture::{Origin, open_reported};
    use helia_core::io::source::BayerSetting;

    let Some(path) = capture("Sun_Test.ser") else {
        return;
    };
    let (_, report) = open_reported(&path, &OpenOptions::default()).expect("open");
    assert_eq!(report.ser.as_ref().map(|h| h.colour_id), Some(0));
    assert_eq!(report.effective.bayer.value, Some(BayerPattern::Rggb));
    assert_eq!(report.effective.bayer.origin, Origin::Detection);
    let det = report.detection.expect("the mosaic test ran");
    eprintln!(
        "Sun_Test.ser: header ColorID 0 (MONO); detected {:?}; means {:?}, spread {:.3} over {} frames",
        report.effective.bayer.value, det.means, det.spread, det.frames_used
    );
    assert!(det.spread > 1.0, "{}", det.spread);

    let stack = |opts: OpenOptions, channel: Channel| {
        let mut src = LimitedSource::new(open_capture(&path, &opts).expect("open"), 0, 100);
        let cfg = StackConfig {
            keep: Keep::Fraction(0.25),
            debayer: Debayer {
                channel,
                ..Debayer::default()
            },
            ..StackConfig::default()
        };
        Pipeline::new(cfg)
            .run(&mut src, &|_, _| {}, &AtomicBool::new(false))
            .expect("pipeline")
    };
    // `--bayer none` is what Helia did before it looked at the pixels.
    let trusting = OpenOptions {
        bayer: BayerSetting::Mono,
        ..OpenOptions::default()
    };
    for channel in [Channel::Luminance, Channel::Red] {
        let (m_before, before) = position_spread(&stack(trusting, channel).image);
        let (m_after, after) = position_spread(&stack(OpenOptions::default(), channel).image);
        eprintln!(
            "Sun_Test.ser {channel:?}: 2x2 position means before {m_before:.4?} (spread {before:.4}), after {m_after:.4?} (spread {after:.4})"
        );
        assert!(
            before > 0.10,
            "trusting the header keeps the mosaic: {before}"
        );
        assert!(after < 0.02, "the stack still shows the grid: {after}");
    }
}

/// The other three captures say what they are, in their header or sidecar,
/// and are decoded as before: no pixel test, the same layout, the same pixels.
#[test]
fn captures_that_say_what_they_are_are_unchanged() {
    use helia_core::io::capture::{Level, Origin, open_reported};

    if let Some(path) = capture("15_20_57.ser") {
        let (mut src, report) = open_reported(&path, &OpenOptions::default()).expect("open");
        assert_eq!(report.effective.bayer.value, Some(BayerPattern::Rggb));
        assert_eq!(report.effective.bayer.origin, Origin::Header);
        assert!(report.detection.is_none());
        // SharpCap writes the byte-order flag wrongly; the one note says so.
        assert!(
            report.notices.iter().all(|n| n.level == Level::Info),
            "{:?}",
            report.notices
        );
        assert_eq!(report.effective.endian.origin, Origin::Detection);
        let mut r = SerReader::new(BufReader::new(File::open(&path).expect("open"))).expect("ser");
        for i in [0, 50, 99] {
            assert_eq!(src.read(i).expect("frame"), r.read_frame(i).expect("frame"));
        }
    }
    for name in ["15_32_09.avi", "14_42_57.avi"] {
        let Some(path) = capture(name) else { continue };
        let (mut src, report) = open_reported(&path, &OpenOptions::default()).expect("open");
        assert_eq!(
            report.effective.bayer.value,
            Some(BayerPattern::Rggb),
            "{name}"
        );
        assert_eq!(report.effective.bayer.origin, Origin::Sidecar, "{name}");
        assert!(report.detection.is_none(), "{name}");
        let mut r = AviReader::new(BufReader::new(File::open(&path).expect("open")))
            .expect("avi")
            .with_bayer(Some(BayerPattern::Rggb));
        for i in [0, r.frame_count() / 2] {
            assert_eq!(
                src.read(i).expect("frame"),
                r.read_frame(i).expect("frame"),
                "{name} {i}"
            );
        }
    }
}

fn avi_checks(name: &str, w: u32, h: u32) {
    let Some(path) = capture(name) else { return };
    let mut r = AviReader::new(BufReader::new(File::open(&path).expect("open"))).expect("avi");
    let info = r.info().clone();
    assert_eq!((info.width, info.height), (w, h));
    assert_eq!(
        (info.format, info.fourcc.as_str()),
        (PixelFormat::Gray8, "DIB ")
    );
    let n = r.frame_count();
    let settings = CameraSettings::for_capture(&path).expect("sidecar next to the capture");
    assert_eq!(settings.is_raw_colour(), Some(true));
    assert_eq!(settings.bayer_hint(), Some(BayerPattern::Rggb));
    if let Some(expected) = settings.frame_count() {
        // SharpCap's own count; a frame or two may differ if it drops the last one on stop.
        assert!(
            n.abs_diff(expected) <= 2,
            "{n} frames found, sidecar says {expected}"
        );
    }
    eprintln!("{name}: {n} frames, bottom_up {}", info.bottom_up);
    for i in [0, n / 2, n - 1] {
        let f = r.read_frame(i).expect("frame");
        check_frame(&f, w as usize, h as usize);
    }
    // Auto picks the sidecar pattern, so frames come back as an RGGB mosaic.
    let mut src = open_capture(&path, &OpenOptions::default()).expect("open");
    assert_eq!(src.read(0).expect("frame").bayer, Some(BayerPattern::Rggb));
    stack_subset(&path, OpenOptions::default());
}

#[test]
fn small_avi_header_frames_and_stack() {
    avi_checks("15_32_09.avi", 720, 540);
}

#[test]
fn large_opendml_avi_header_frames_and_stack() {
    avi_checks("14_42_57.avi", 1920, 1080);
}

/// `SharpCap` burns "2022 07 17 14:32:09:647" into the top-left corner of
/// `15_32_09.avi` (720 x 540). Without masking, alignment points sit on that text
/// and on the empty sky around the disc.
#[test]
fn timestamp_and_sky_are_masked_on_the_small_avi() {
    let Some(path) = capture("15_32_09.avi") else {
        return;
    };
    let opts = OpenOptions::default();
    let run = |mask: MaskConfig| {
        let mut src = LimitedSource::new(open_capture(&path, &opts).expect("open"), 0, 300);
        let cfg = StackConfig {
            mask,
            ..StackConfig::default()
        };
        Pipeline::new(cfg)
            .run(&mut src, &|_, _| {}, &AtomicBool::new(false))
            .expect("pipeline")
    };
    let count = |o: &StackOutput| o.grid.as_ref().map_or(0, ApGrid::active_count);
    let over_text = |o: &StackOutput| {
        let text = Rect::new(0, 0, 190, 16);
        let grid = o.grid.as_ref().expect("grid");
        grid.aps
            .iter()
            .filter(|a| a.active && mask::overlaps(a.rect, text))
            .count()
    };
    let before = run(MaskConfig::off());
    let after = run(MaskConfig::default());
    eprintln!(
        "15_32_09.avi: active points {} -> {}; over the timestamp rows {} -> {}; masked {:?}; disc {:?}",
        count(&before),
        count(&after),
        over_text(&before),
        over_text(&after),
        after.masked,
        after.disc
    );
    assert_eq!(over_text(&after), 0);
    assert!(count(&after) > 0 && count(&after) < count(&before));
    assert_eq!(after.masked.len(), 1, "{:?}", after.masked);
    let m = after.masked[0];
    assert!(m.x0 == 0 && m.y0 == 0 && (185..=260).contains(&m.x1) && (15..=40).contains(&m.y1));
    let disc = after.disc.expect("disc");
    assert!(disc.r > 180.0 && disc.r < 260.0, "{disc:?}");
    // Every remaining point has its centre on the disc.
    let grid = after.grid.as_ref().expect("grid");
    for a in grid.aps.iter().filter(|a| a.active) {
        assert!(disc.contains(a.cx, a.cy, 0.0));
    }
}

/// What Helia 0.2.0 did to a photospheric capture by default: all channels,
/// keep 25%, the general alignment point size, the general wavelets.
fn granulation_before() -> (helia_core::debayer::Channel, Keep, usize, EditList) {
    use helia_core::colour::recommended_ap_size;
    use helia_core::edit::{Layer, Wavelet};
    let gains = [1.5, 1.45, 1.25, 1.05, 1.0, 1.0];
    let thresholds = [1.8, 1.0, 0.5, 0.0, 0.0, 0.0];
    let layers = gains
        .iter()
        .zip(&thresholds)
        .map(|(&gain, &threshold)| Layer::from_gain(gain, threshold))
        .collect();
    (
        helia_core::debayer::Channel::Luminance,
        Keep::Fraction(0.25),
        recommended_ap_size(1920, 1080),
        EditList::new(vec![Op::Wavelet(Wavelet::from_layers(layers))]),
    )
}

/// Stack `path` with `keep` and `ap_size`, sharpen with `edits`, and measure
/// the granulation bands at the disc centre.
fn granulation_of(
    path: &Path,
    channel: helia_core::debayer::Channel,
    keep: Keep,
    ap_size: usize,
    edits: &EditList,
) -> helia_core::measure::GranulationBands {
    use helia_core::align::multipoint::MultipointConfig;
    use helia_core::debayer::Debayer;
    use helia_core::measure::granulation_contrast;
    let mut src = open_capture(path, &OpenOptions::default()).expect("open capture");
    let cfg = StackConfig {
        keep,
        multipoint: Some(MultipointConfig {
            ap_size,
            ..MultipointConfig::default()
        }),
        debayer: Debayer {
            channel,
            ..Debayer::default()
        },
        ..StackConfig::default()
    };
    let out = Pipeline::new(cfg)
        .run(&mut src, &|_, _| {}, &AtomicBool::new(false))
        .expect("pipeline");
    let sharp = helia_core::edit::apply_cpu(&out.image, edits).expect("sharpen");
    granulation_contrast(&sharp)
}

/// The recommended settings for a photospheric filter keep granulation that
/// the 0.2.0 defaults lost (#101): on `Sun_Test.ser` (H-alpha, read through the
/// green plane; it was first taken for sodium) the granulation band
/// (1 to 4 px) holds at least 1.3 times the contrast. Measured on a 24-core build server with an NVIDIA A100 at
/// 1.84 times (1.53% to 2.81%); the bar leaves room for noise.
#[test]
fn photospheric_defaults_keep_the_granulation() {
    use helia_core::recommend::recommend;
    let Some(path) = capture("Sun_Test.ser") else {
        return;
    };
    let (old_channel, old_keep, old_ap, old_edits) = granulation_before();
    let old = granulation_of(&path, old_channel, old_keep, old_ap, &old_edits);
    let rec = recommend(1920, 1080, true, &[], Some(Preset::Na));
    let new = granulation_of(
        &path,
        rec.channel,
        rec.keep,
        rec.ap_size,
        &rec.sharpen_edits,
    );
    eprintln!(
        "Sun_Test.ser granulation contrast (fine / granulation / meso): old {:.3}% / {:.3}% / {:.3}%, new {:.3}% / {:.3}% / {:.3}% ({:.2}x in the granulation band)",
        old.fine * 100.0,
        old.granulation * 100.0,
        old.meso * 100.0,
        new.fine * 100.0,
        new.granulation * 100.0,
        new.meso * 100.0,
        new.granulation / old.granulation
    );
    assert!(
        new.granulation >= 1.3 * old.granulation,
        "new {new:?} against old {old:?}"
    );
}

/// Automatic alignment points and the disc crop on the small H-alpha AVI
/// (2x binned, 4.3 arcsec per pixel): the limb gives the scale, the probe
/// finds the warp, the crop holds the whole disc and the granulation band is
/// no worse than with the fixed 32 pixel preset (#25).
#[test]
fn auto_alignment_points_and_the_crop_keep_the_detail() {
    use helia_core::autoap::AutoAp;
    use helia_core::debayer::{Channel, Debayer};
    use helia_core::measure::granulation_contrast;
    let Some(path) = capture("15_32_09.avi") else {
        return;
    };
    let run = |cfg: StackConfig| {
        let mut src = open_capture(&path, &OpenOptions::default()).expect("open capture");
        Pipeline::new(cfg)
            .run(&mut src, &|_, _| {}, &AtomicBool::new(false))
            .expect("pipeline")
    };
    let base = StackConfig {
        keep: Keep::Fraction(0.12),
        debayer: Debayer {
            channel: Channel::Green,
            ..Debayer::default()
        },
        ..StackConfig::default()
    };
    let fixed = run(StackConfig {
        multipoint: Some(helia_core::align::multipoint::MultipointConfig {
            ap_size: 32,
            ..Default::default()
        }),
        ..base.clone()
    });
    let auto = run(StackConfig {
        auto_ap: Some(AutoAp::photosphere(32)),
        autocrop: Some(0.1),
        ..base
    });
    let choice = auto.auto_ap.expect("choice");
    let scale = choice.scale.expect("scale from the limb");
    assert!((3.5..5.0).contains(&scale), "{scale} arcsec per pixel");
    assert_eq!(choice.size, 32);
    let crop = auto.crop.expect("cropped");
    let disc = fixed.disc.expect("disc");
    // The crop holds the whole disc with its margin.
    assert!(crop.rect.x0 as f32 <= disc.cx - disc.r);
    assert!(crop.rect.x1 as f32 >= disc.cx + disc.r);
    assert!(crop.rect.y0 as f32 <= disc.cy - disc.r);
    assert!(crop.rect.y1 as f32 >= disc.cy + disc.r);
    let (a, b) = (
        granulation_contrast(&fixed.image),
        granulation_contrast(&auto.image),
    );
    eprintln!(
        "15_32_09.avi: {}; crop {}x{}; granulation fixed {:.3}%, auto {:.3}%",
        choice.summary(),
        crop.rect.width(),
        crop.rect.height(),
        a.granulation * 100.0,
        b.granulation * 100.0
    );
    assert!(b.granulation >= 0.95 * a.granulation, "{b:?} against {a:?}");
}

/// The limb-darkening fit on the small H-alpha AVI (green plane) lands at a
/// linear coefficient of 0.5 to 0.6, and flattening by
/// either source takes the fit to nothing without touching the granulation
/// (#20). Measured on a 24-core build server with an NVIDIA A100: linear u = 0.559, 0.9% rms.
#[test]
fn limb_darkening_is_fitted_and_flattened_on_a_real_stack() {
    use helia_core::debayer::{Channel, Debayer};
    use helia_core::edit::{EditList, Flatten, Op, apply_cpu};
    use helia_core::limbdark::{Law, Source, fit_image};
    use helia_core::measure::granulation_contrast;
    let Some(path) = capture("15_32_09.avi") else {
        return;
    };
    let mut src = open_capture(&path, &OpenOptions::default()).expect("open capture");
    let out = Pipeline::new(StackConfig {
        keep: Keep::Fraction(0.12),
        debayer: Debayer {
            channel: Channel::Green,
            ..Debayer::default()
        },
        ..StackConfig::default()
    })
    .run(&mut src, &|_, _| {}, &AtomicBool::new(false))
    .expect("pipeline");
    let fit = out.limb.clone().expect("a disc in view");
    let linear = fit_image(&out.image, Law::Linear).expect("linear");
    eprintln!("{}\n{}", fit.summary(), linear.summary());
    assert!((0.45..0.65).contains(&linear.coefficients[0]), "{linear:?}");
    assert!(fit.rms < 0.02, "{fit:?}");
    let before = granulation_contrast(&out.image);
    for source in [Source::Model, Source::Profile] {
        let list = EditList::new(vec![Op::Flatten(Flatten::new(source, Law::Quadratic))]);
        let flat = apply_cpu(&out.image, &list).expect("flatten");
        let after_fit = fit_image(&flat, Law::Linear).expect("fit after");
        assert!(
            after_fit.coefficients[0].abs() < 0.03,
            "{source:?}: {after_fit:?}"
        );
        let g = granulation_contrast(&flat);
        eprintln!(
            "{source:?}: linear u {:.3} to {:.3}, granulation {:.3}% to {:.3}%",
            linear.coefficients[0],
            after_fit.coefficients[0],
            before.granulation * 100.0,
            g.granulation * 100.0
        );
        assert!(
            (g.granulation / before.granulation - 1.0).abs() < 0.05,
            "{source:?}: {g:?} against {before:?}"
        );
    }
}

/// True colour on the one broad-band capture, `14_42_57.avi` (the first 100
/// frames): the planes agree to under a pixel, the gains are finite and none is
/// held at the cap, and the colour picture's luminance is the stack (#72).
#[test]
fn true_colour_on_a_real_broad_band_capture() {
    use helia_core::truecolour::{MAX_AUTO_GAIN, TrueColour};
    let Some(path) = capture("14_42_57.avi") else {
        return;
    };
    let inner = open_capture(&path, &OpenOptions::default()).expect("open capture");
    let mut src = LimitedSource::new(inner, 0, 100);
    let out = Pipeline::new(StackConfig {
        keep: Keep::Fraction(0.12),
        true_colour: Some(TrueColour::default()),
        ..StackConfig::default()
    })
    .run(&mut src, &|_, _| {}, &AtomicBool::new(false))
    .expect("pipeline");
    let c = out.colour.as_ref().expect("colour picture");
    eprintln!("gains {:?}; channels {:?}", c.gains, c.channels);
    assert!(c.gains.iter().all(|g| g.is_finite() && *g > 0.1));
    // The common scale is on top of the balance, so compare the balance alone.
    assert!(c.gains[0] / c.gains[1] < MAX_AUTO_GAIN && c.gains[2] / c.gains[1] < MAX_AUTO_GAIN);
    for ch in c.channels.expect("channels") {
        assert!(ch.shift.length() < 1.0, "{ch:?}");
    }
    assert_eq!(
        out.image,
        helia_core::truecolour::rgb_luminance(&c.rgb).expect("luminance")
    );
    assert_eq!(c.rgb.data.len(), 3 * out.image.data.len());
}

/// Share of the top quarter by gradient energy that another metric also puts in
/// its top quarter, over frames of a real capture.
fn top_quarter_overlap(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len() / 4;
    let top = |s: &[f32]| -> std::collections::HashSet<usize> {
        quality::rank(s).into_iter().take(n).collect()
    };
    let (ta, tb) = (top(a), top(b));
    ta.intersection(&tb).count() as f32 / n as f32
}

/// Every ranking metric works on real frames: finite, positive, and not
/// ranking against gradient energy about which frames are sharp (chance is a
/// quarter overlap of the best quarters). Prints the overlaps for the metric bake-off (#21).
#[test]
fn every_ranking_metric_agrees_with_gradient_energy_on_real_frames() {
    let Some(path) = capture("15_32_09.avi") else {
        return;
    };
    let opts = OpenOptions::default();
    let mut src = LimitedSource::new(open_capture(&path, &opts).expect("open"), 0, 120);
    let cfg = StackConfig::default();
    let frames: Vec<Frame> = (0..src.frame_count())
        .map(|i| cfg.debayer.apply_owned(src.read(i).expect("frame")))
        .collect();
    let reference: Vec<f32> = frames
        .par_iter()
        .map(|f| quality::score(f, Metric::GradientEnergy, None))
        .collect();
    for metric in Metric::ALL {
        let scores: Vec<f32> = frames
            .par_iter()
            .map(|f| quality::score(f, metric, None))
            .collect();
        assert!(
            scores.iter().all(|s| s.is_finite() && *s >= 0.0),
            "{metric:?}: non-finite or negative score"
        );
        let overlap = top_quarter_overlap(&reference, &scores);
        eprintln!(
            "{}: top-quarter overlap with gradient {overlap:.2}",
            metric.name()
        );
        // Chance is 0.25. MFGS and the Fourier band answer to noise as well as
        // sharpness, so only a ranking worse than chance fails.
        assert!(
            overlap > 0.2,
            "{metric:?}: overlap {overlap:.2} is below chance"
        );
    }
}
// ---- Per-band captures of 2026-10-20 (owner, ZWO ASI482MC colour and a mono camera) ----
//
// 15_37_19.avi and 15_32_09.avi are H-alpha (the red plane is clipped, prominences
// show in it, filaments and plage in the green leak); the 2022 SER captures are the
// same scene. 15_36_42.avi is calcium K on a colour camera and 15_44_48.avi the same
// band on a mono camera (their limb darkening agrees); the Seestar MP4 is white
// light, tinted orange by the app. None of the AVIs has a sidecar.

/// Stack the first `n` frames of `path` with `cfg`.
fn stack_first(path: &Path, opts: &OpenOptions, n: usize, cfg: StackConfig) -> StackOutput {
    let inner = open_capture(path, opts).expect("open capture");
    let mut src = LimitedSource::new(inner, 0, n);
    Pipeline::new(cfg)
        .run(&mut src, &|_, _| {}, &AtomicBool::new(false))
        .expect("pipeline")
}

/// A sidecar-less AVI is checked for a mosaic: the colour captures are found as
/// RGGB by detection and the mono camera's stays mono (#120).
#[test]
fn sidecarless_avi_mosaic_is_detected() {
    use helia_core::io::capture::{Origin, open_reported};
    for (name, mosaic) in [
        ("15_37_19.avi", true),
        ("15_36_42.avi", true),
        ("15_44_48.avi", false),
    ] {
        let Some(path) = capture(name) else { continue };
        let (_, report) = open_reported(&path, &OpenOptions::default()).expect("open");
        let pattern = report.effective.bayer.value;
        eprintln!(
            "{name}: {pattern:?} from {:?}",
            report.effective.bayer.origin
        );
        assert_eq!(pattern.is_some(), mosaic, "{name}");
        if mosaic {
            assert_eq!(report.effective.bayer.origin, Origin::Detection, "{name}");
        }
    }
}

/// H-alpha: the red plane is clipped, so the preset's red falls back to green,
/// which holds far more fibril-scale contrast than the flat red disc (#119).
#[test]
fn h_alpha_with_a_clipped_red_plane_uses_green() {
    use helia_core::debayer::{Channel, Debayer};
    use helia_core::measure::granulation_contrast;
    for name in ["15_37_19.avi", "15_32_09.avi"] {
        let Some(path) = capture(name) else { continue };
        let run = |channel, fallback| {
            stack_first(
                &path,
                &OpenOptions::default(),
                200,
                StackConfig {
                    keep: Keep::Fraction(0.25),
                    clip_fallback: fallback,
                    debayer: Debayer {
                        channel,
                        ..Debayer::default()
                    },
                    ..StackConfig::default()
                },
            )
        };
        let auto = run(Channel::Red, true);
        let red = run(Channel::Red, false);
        eprintln!("{name}: {:?}", auto.channel_note);
        assert_eq!(auto.channel, Channel::Green, "{name}");
        assert_eq!(red.channel, Channel::Red);
        let (g, r) = (
            granulation_contrast(&auto.image),
            granulation_contrast(&red.image),
        );
        eprintln!(
            "{name}: granulation band green {:.3}% red {:.3}%",
            g.granulation * 100.0,
            r.granulation * 100.0
        );
        assert!(g.granulation > 3.0 * r.granulation, "{g:?} against {r:?}");
    }
}

/// Calcium K on a colour camera: the brightest plane (the one the Ca K preset
/// calls blue) has a better signal to noise than the luminance, and the limb
/// darkening matches the mono capture of the same band (#119, #110).
#[test]
fn calcium_k_blue_plane_and_limb_match_the_mono_capture() {
    use helia_core::debayer::{Channel, Debayer};
    use helia_core::io::source::BayerSetting;
    use helia_core::limbdark::{Law, fit_image};
    use helia_core::measure::granulation_contrast;
    let (Some(colour), Some(mono)) = (capture("15_36_42.avi"), capture("15_44_48.avi")) else {
        return;
    };
    // The brightest plane sits at the top left, so the pattern is BGGR; the pixels
    // alone cannot name red and blue, and a file name with CaK in it would.
    let opts = OpenOptions {
        bayer: BayerSetting::Pattern(BayerPattern::Bggr),
        ..OpenOptions::default()
    };
    let run = |channel| {
        stack_first(
            &colour,
            &opts,
            150,
            StackConfig {
                keep: Keep::Fraction(0.3),
                debayer: Debayer {
                    channel,
                    ..Debayer::default()
                },
                ..StackConfig::default()
            },
        )
    };
    let (b, l) = (run(Channel::Blue), run(Channel::Luminance));
    let snr = |o: &StackOutput| {
        let m = granulation_contrast(&o.image);
        m.granulation / m.fine
    };
    eprintln!(
        "Ca K colour: blue SNR {:.2}, luminance {:.2}",
        snr(&b),
        snr(&l)
    );
    assert!(snr(&b) > snr(&l));
    let m = stack_first(
        &mono,
        &OpenOptions::default(),
        150,
        StackConfig {
            keep: Keep::Fraction(0.3),
            ..StackConfig::default()
        },
    );
    let at = |o: &StackOutput| {
        let f = fit_image(&o.image, Law::Quadratic).expect("disc");
        let c = &f.coefficients;
        1.0 - c[0] * 0.7 - c[1] * 0.49
    };
    let (a, c) = (at(&b), at(&m));
    eprintln!("limb darkening at mu 0.3: colour {a:.3}, mono {c:.3}");
    assert!((a - c).abs() < 0.04, "{a} against {c}");
}

/// A Seestar MP4 (orange, red clipped, blue empty): the video channel choice
/// picks green. Needs ffmpeg; skipped without it.
#[test]
fn seestar_video_uses_the_green_plane() {
    use helia_core::debayer::Channel;
    use helia_core::io::ffmpeg;
    let Some(path) = capture("2025-09-17-161400-Solar.mp4") else {
        return;
    };
    let Ok(choice) = ffmpeg::choose_channel(&path) else {
        eprintln!("ffmpeg not available: skipping");
        return;
    };
    eprintln!("{:?}", choice.note);
    assert_eq!(choice.channel, Channel::Green);
}
