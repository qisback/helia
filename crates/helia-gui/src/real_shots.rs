// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Real-capture screenshots for the manual, rendered off screen.
//!
//! `--shots` needs a window that is actually being painted, which a locked or
//! disconnected desktop session and a headless build host are not. This runs the
//! same application, with its real worker threads, on a real capture through the
//! test harness (no window needed) and writes the picture of every step.
//!
//! It does nothing unless both variables are set, so the normal test run skips it:
//!
//! ```sh
//! HELIA_REAL_CAPTURE=/data/ha-sample.avi HELIA_REAL_SHOTS=/tmp/shots \
//!   HELIA_GUI_CONFIG=/tmp/gui.conf XDG_CONFIG_HOME=/tmp/cfg UPDATE_SNAPSHOTS=1 \
//!   cargo test --release -p helia-gui real_capture_shots -- --nocapture
//! ```
//!
//! `HELIA_REAL_MODE=open` stops after the Open step (for a large capture whose
//! only interest is its header); the default runs the whole pipeline and also
//! writes the plain, sharpened and coloured stacks as PNG files with the
//! `variant-` prefix, so before and after pictures can be cropped from them.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui::vec2;
use egui_kittest::{Harness, SnapshotOptions};
use helia_core::colour::{RgbImage, colourise};
use helia_core::{Backend as _, ColourParams, Preset, SharpenMethod};

use crate::app::App;
use crate::model::{Scoring, Step};

const WAIT: Duration = Duration::from_mins(30);

fn var(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

/// Step the window until `done` holds.
fn drive(h: &mut Harness<'_, App>, what: &str, done: impl Fn(&App) -> bool) {
    let t0 = Instant::now();
    loop {
        h.step();
        if done(h.state()) {
            return;
        }
        assert!(t0.elapsed() < WAIT, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The deconvolution tutorial: the warning when it mostly amplifies noise, the
/// iteration scrub and the wavelets that follow it.
fn deconvolution_walk(
    h: &mut Harness<'_, App>,
    prefix: &str,
    dir: &Path,
    shot: &dyn Fn(&mut Harness<'_, App>, &str),
) {
    use helia_core::edit::Psf;
    let settle = |h: &mut Harness<'_, App>| {
        drive(h, "the deconvolution preview", |a| !a.colour_inflight);
        h.run_steps(3);
    };
    // The image scale's PSF with a TV weight: the case that mostly makes noise.
    {
        let e = &mut h.state_mut().settings.edits;
        e.rl.psf = Psf::Auto;
        e.rl.tv = 0.004;
    }
    h.state_mut().edits_changed();
    settle(h);
    variant(h.state(), prefix, "deconvolution-noisy", dir);
    shot(h, "07-sharpen-deconvolution-warning");
    // The recommended values again, scrubbed to the fourth iteration.
    {
        let a = h.state_mut();
        a.settings.edits.rl = helia_core::edit::Deconvolve::default();
        a.settings.edits.rl.iterations = 12;
        a.rl_scrub = Some(4);
        a.edits_changed();
    }
    settle(h);
    variant(h.state(), prefix, "deconvolution-iteration-4", dir);
    shot(h, "07-sharpen-deconvolution-scrub");
    // Then the wavelets, with a firm denoise on layer 1.
    {
        let a = h.state_mut();
        a.rl_scrub = None;
        a.settings.edits.then_wavelets = true;
        a.settings.edits.wavelet = helia_core::edit::Wavelet::from_registax_spec(
            "registax:l1=100/0.1/0.3,l2=100/0.13/0.15,l3=50/0.11/0.1,filter=gaussian",
        )
        .expect("wavelets");
        a.edits_changed();
    }
    settle(h);
    variant(h.state(), prefix, "deconvolution-then-wavelets", dir);
    shot(h, "07-sharpen-deconvolution-then-wavelets");
    h.state_mut().settings.edits.then_wavelets = false;
}

fn write_rgb(path: &Path, rgb: &RgbImage) {
    let file = std::fs::File::create(path).expect("create png");
    let mut enc = png::Encoder::new(
        std::io::BufWriter::new(file),
        rgb.width as u32,
        rgb.height as u32,
    );
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().expect("png header");
    w.write_image_data(&rgb.to_u8()).expect("png data");
}

/// The stack with the current edits and colour, as the Export step would write it.
fn variant(app: &App, prefix: &str, name: &str, dir: &Path) {
    let Some(out) = &app.result else { return };
    let list = app.preview_list();
    let edited = if list.is_empty() {
        out.image.clone()
    } else {
        helia_core::CpuBackend
            .apply_edits(&out.image, &list)
            .expect("edits")
    };
    write_rgb(
        &dir.join(format!("{prefix}-variant-{name}.png")),
        &colourise(&edited, &app.settings.colour),
    );
}

#[test]
fn real_capture_shots() {
    let (Some(capture), Some(dir)) = (var("HELIA_REAL_CAPTURE"), var("HELIA_REAL_SHOTS")) else {
        eprintln!("skipping real-capture shots: set HELIA_REAL_CAPTURE and HELIA_REAL_SHOTS");
        return;
    };
    let prefix = std::env::var("HELIA_REAL_PREFIX").unwrap_or_else(|_| "real".into());
    let open_only = std::env::var("HELIA_REAL_MODE").as_deref() == Ok("open");
    std::fs::create_dir_all(&dir).expect("shots dir");
    let opts = || {
        SnapshotOptions::new()
            .threshold(0.6)
            .max_failed_pixels(usize::MAX / 2)
            .output_path(dir.clone())
    };
    let tall = if open_only { 2600.0 } else { 900.0 };
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, tall))
        .with_options(opts())
        .wgpu()
        .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, true));
    let shot = |h: &mut Harness<'_, App>, name: &str| {
        h.run_steps(4);
        h.snapshot(format!("{prefix}-{name}"));
    };

    h.state_mut().begin_open(capture);
    drive(&mut h, "the capture to open and score", |a| {
        a.capture.is_some() && a.scoring == Scoring::Done
    });
    h.state_mut().show_grid = false;
    h.state_mut().select(Step::Open);
    shot(&mut h, "01-open");
    if open_only {
        return;
    }
    h.state_mut().prefs.tips_dismissed = true;
    h.set_size(vec2(1440.0, 900.0));

    h.state_mut().select(Step::Analyse);
    shot(&mut h, "02-analyse");

    h.state_mut().select(Step::Align);
    h.state_mut().show_grid = true;
    shot(&mut h, "03-align");
    // One exclusion rectangle in the top-left corner, as for a logo or a stray label.
    h.state_mut().settings.exclude = vec![[8, 8, 110, 24]];
    h.state_mut().exclusions_changed();
    drive(&mut h, "scoring after the exclusion", |a| {
        a.scoring == Scoring::Done
    });
    shot(&mut h, "03-align-exclusion");
    h.state_mut().settings.exclude.clear();
    h.state_mut().exclusions_changed();
    drive(&mut h, "scoring after clearing it", |a| {
        a.scoring == Scoring::Done
    });
    h.state_mut().show_grid = false;

    h.state_mut().select(Step::Stack);
    shot(&mut h, "04-stack-settings");

    h.state_mut().start_stack();
    drive(&mut h, "the stack to be part way", |a| {
        a.result.is_some()
            || a.run.as_ref().is_some_and(|r| {
                r.fraction > 0.3 && r.stage != helia_core::pipeline::Stage::Quality
            })
    });
    shot(&mut h, "05-stacking");
    drive(&mut h, "the stack", |a| a.result.is_some());
    h.run_steps(6);
    shot(&mut h, "06-result");

    // Sharpen: the plain stack, then each method.
    h.set_size(vec2(1440.0, 1150.0));
    h.state_mut().select(Step::Sharpen);
    {
        let a = h.state_mut();
        a.settings.edits.method = SharpenMethod::None;
        a.settings.colour = ColourParams::default();
        a.edits_changed();
    }
    drive(&mut h, "the plain preview", |a| {
        a.colour_tex.is_some() && !a.colour_inflight
    });
    variant(h.state(), &prefix, "stack-plain", &dir);
    for (method, name) in [
        (SharpenMethod::Wavelet, "wavelet"),
        (SharpenMethod::Unsharp, "unsharp"),
        (SharpenMethod::RichardsonLucy, "deconvolution"),
    ] {
        let a = h.state_mut();
        a.settings.edits.method = method;
        a.edits_changed();
        drive(&mut h, "the sharpened preview", |a| !a.colour_inflight);
        h.run_steps(3);
        variant(h.state(), &prefix, name, &dir);
        shot(&mut h, &format!("07-sharpen-{name}"));
        if method == SharpenMethod::RichardsonLucy {
            deconvolution_walk(&mut h, &prefix, &dir, &shot);
        }
        if method == SharpenMethod::Wavelet {
            h.state_mut().show_split = true;
            h.state_mut().split_pos = 0.5;
            shot(&mut h, "07-sharpen-split");
            h.state_mut().show_split = false;
            // The settings of the "Sharpening like RegiStax" tutorial.
            h.state_mut().settings.edits.wavelet = helia_core::edit::Wavelet::from_registax_spec(
                "registax:l1=100/0.14/0.2,l2=100/0.13/0.15,l3=50/0.11/0.1,filter=gaussian",
            )
            .expect("tutorial settings");
            h.state_mut().edits_changed();
            drive(&mut h, "the RegiStax-style preview", |a| !a.colour_inflight);
            h.run_steps(3);
            variant(h.state(), &prefix, "wavelet-registax", &dir);
            shot(&mut h, "07-sharpen-registax");
            h.state_mut().show_split = true;
            h.state_mut().split_pos = 0.5;
            shot(&mut h, "07-sharpen-registax-split");
            h.state_mut().show_split = false;
        }
    }
    {
        let a = h.state_mut();
        let (w, hgt) = a
            .result
            .as_ref()
            .map_or((1, 1), |o| (o.image.width, o.image.height));
        a.settings.edits.reset_sharpen(a.settings.preset, w, hgt);
        a.edits_changed();
    }

    // Adjust: a little denoise, levels and curve.
    {
        let a = h.state_mut();
        a.settings.edits.denoise_on = true;
        a.settings.edits.levels.black = 0.02;
        a.settings.edits.levels.gamma = 1.15;
        a.settings.edits.curve.points = vec![[0.5, 0.45]];
        a.select(Step::Adjust);
        a.edits_changed();
    }
    drive(&mut h, "the adjusted preview", |a| !a.colour_inflight);
    shot(&mut h, "08-adjust");
    {
        let a = h.state_mut();
        let (w, hgt) = a
            .result
            .as_ref()
            .map_or((1, 1), |o| (o.image.width, o.image.height));
        a.settings.edits.denoise_on = false;
        a.settings.edits.reset_adjust(w, hgt);
        a.edits_changed();
    }

    // Colour: every look.
    h.set_size(vec2(1440.0, 1150.0));
    h.state_mut().select(Step::Colour);
    for p in [
        Preset::Ha,
        Preset::CaK,
        Preset::CaH,
        Preset::Mg,
        Preset::Na,
        Preset::HeD3,
        Preset::Hbeta,
        Preset::White,
    ] {
        let a = h.state_mut();
        a.settings.colour = ColourParams::preset(p);
        a.colour_changed();
        drive(&mut h, "the colour preview", |a| !a.colour_inflight);
        h.run_steps(3);
        variant(h.state(), &prefix, &format!("colour-{}", p.id()), &dir);
        if p == Preset::Ha {
            shot(&mut h, "09-colour");
        }
    }
    // The grading tutorial: the Ha look as it is, then a personal grade.
    for (name, spec) in [
        (
            "grade-warm",
            "hue=-6,sat=1.25,gamma=1.1,black=0.03,r=1.05,b=0.9",
        ),
        ("grade-cool", "hue=8,sat=0.85,gamma=0.95,b=1.2"),
    ] {
        let a = h.state_mut();
        a.settings.colour = ColourParams {
            grade: helia_core::grade::Grade::parse(spec).expect("grade"),
            ..ColourParams::preset(Preset::Ha)
        };
        a.colour_changed();
        drive(&mut h, "the graded preview", |a| !a.colour_inflight);
        h.run_steps(3);
        variant(h.state(), &prefix, &format!("colour-{name}"), &dir);
        if name == "grade-warm" {
            h.set_size(vec2(1440.0, 1500.0));
            shot(&mut h, "09-colour-grade");
            h.set_size(vec2(1440.0, 1150.0));
        }
    }
    {
        let a = h.state_mut();
        a.settings.colour = ColourParams::preset(Preset::Ha);
        a.colour_changed();
    }
    drive(&mut h, "the colour preview", |a| !a.colour_inflight);
    h.state_mut().select(Step::Export);
    shot(&mut h, "10-export");

    h.set_size(vec2(1440.0, 1500.0));
    h.state_mut().show_about = true;
    shot(&mut h, "11-about");
}

/// The Time-lapse step on real captures: `HELIA_REAL_CAPTURES` lists them (separated as
/// `PATH` is) and `HELIA_REAL_SHOTS` is where the pictures go. `HELIA_REAL_FILTER` is the filter id (`ha`), `HELIA_REAL_CHANNEL` the plane (`r`, `g`, `b`, `l`), `HELIA_REAL_DEROTATE` (any value) turns the derotation on, `HELIA_REAL_DATE` and
/// `HELIA_REAL_UTC_OFFSET` give the day and the clock of file names that hold only a time.
/// Writes `<prefix>-tl-timelapse.png`, the step with the first frame shown, and
/// `<prefix>-tl-timelapse-last.png`, with the last.
#[test]
fn real_timelapse_shots() {
    let (Some(list), Some(dir)) = (var("HELIA_REAL_CAPTURES"), var("HELIA_REAL_SHOTS")) else {
        eprintln!(
            "skipping the real time-lapse shots: set HELIA_REAL_CAPTURES and HELIA_REAL_SHOTS"
        );
        return;
    };
    let captures: Vec<PathBuf> = std::env::split_paths(&list).collect();
    assert!(captures.len() >= 2, "give two or more captures");
    let prefix = std::env::var("HELIA_REAL_PREFIX").unwrap_or_else(|_| "real".into());
    std::fs::create_dir_all(&dir).expect("shots dir");
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 1400.0))
        .with_options(
            SnapshotOptions::new()
                .threshold(0.6)
                .max_failed_pixels(usize::MAX / 2)
                .output_path(dir.clone()),
        )
        .wgpu()
        .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, true));
    h.state_mut().begin_open(captures[0].clone());
    drive(&mut h, "the first capture to open and score", |a| {
        a.capture.is_some() && a.scoring == Scoring::Done
    });
    {
        let a = h.state_mut();
        a.prefs.tips_dismissed = true;
        if let Some(p) = std::env::var("HELIA_REAL_FILTER")
            .ok()
            .and_then(|v| Preset::from_id(&v))
        {
            a.choose_filter(Some(p));
        }
        // A plane the stack should use instead of the filter's (this capture's red is clipped).
        match std::env::var("HELIA_REAL_CHANNEL").as_deref() {
            Ok("g") => a.settings.channel = helia_core::debayer::Channel::Green,
            Ok("r") => a.settings.channel = helia_core::debayer::Channel::Red,
            Ok("b") => a.settings.channel = helia_core::debayer::Channel::Blue,
            Ok("l") => a.settings.channel = helia_core::debayer::Channel::Luminance,
            _ => {}
        }
        a.tl_add(captures.clone());
        a.tl.date = std::env::var("HELIA_REAL_DATE").unwrap_or_default();
        a.tl.utc_offset = std::env::var("HELIA_REAL_UTC_OFFSET")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0);
        a.tl.crop = true;
        a.tl.derotate = std::env::var_os("HELIA_REAL_DEROTATE").is_some();
        a.tl.bright = crate::timelapse::Bright::Match;
        a.select(Step::Timelapse);
        a.tl_start();
    }
    drive(&mut h, "the time-lapse", |a| a.tl.running.is_none());
    assert!(h.state().tl.built.is_some(), "{:?}", h.state().tl.error);
    h.state_mut().show_grid = false;
    h.state_mut().toasts.clear();
    h.run_steps(4);
    h.snapshot(format!("{prefix}-tl-timelapse"));
    let final_frame = h.state().tl.frames().saturating_sub(1);
    h.state_mut().tl.index = final_frame;
    h.run_steps(4);
    h.snapshot(format!("{prefix}-tl-timelapse-last"));
}

/// The Mosaic step on panels: `HELIA_REAL_PANELS` lists the panel files (captures, or
/// stacked images; separated as `PATH` is) and `HELIA_REAL_SHOTS` is where the picture
/// goes (`<prefix>-mo-mosaic.png`).
#[test]
fn real_mosaic_shots() {
    let (Some(list), Some(dir)) = (var("HELIA_REAL_PANELS"), var("HELIA_REAL_SHOTS")) else {
        eprintln!("skipping the real mosaic shots: set HELIA_REAL_PANELS and HELIA_REAL_SHOTS");
        return;
    };
    let panels: Vec<PathBuf> = std::env::split_paths(&list).collect();
    assert!(panels.len() >= 2, "give two or more panels");
    let prefix = std::env::var("HELIA_REAL_PREFIX").unwrap_or_else(|_| "real".into());
    std::fs::create_dir_all(&dir).expect("shots dir");
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 1500.0))
        .with_options(
            SnapshotOptions::new()
                .threshold(0.6)
                .max_failed_pixels(usize::MAX / 2)
                .output_path(dir.clone()),
        )
        .wgpu()
        .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, true));
    {
        let a = h.state_mut();
        a.prefs.tips_dismissed = true;
        a.mo_add(panels);
        a.select(Step::Mosaic);
        a.mo_start();
    }
    drive(&mut h, "the mosaic", |a| a.mo.running.is_none());
    assert!(h.state().mo.built.is_some(), "{:?}", h.state().mo.error);
    h.state_mut().toasts.clear();
    h.run_steps(4);
    h.snapshot(format!("{prefix}-mo-mosaic"));
}

/// A clipped H-alpha capture (the Seestar video of 2026-10-20: red 96 to 100% at the sensor
/// limit) opens on its green plane and the Stack step says why, as `helia stack` does
/// (issue #184). Needs the capture: set `HELIA_CLIPPED_HA_AVI` to its path, and
/// `HELIA_REAL_SHOTS` to a folder to also write the picture of the Stack step.
#[test]
fn clipped_ha_video_opens_on_green_and_the_stack_step_says_so() {
    use egui_kittest::kittest::Queryable as _;
    let Some(capture) = var("HELIA_CLIPPED_HA_AVI") else {
        eprintln!("skipping the clipped H-alpha test: set HELIA_CLIPPED_HA_AVI");
        return;
    };
    let shots = var("HELIA_REAL_SHOTS");
    let mut options = SnapshotOptions::new()
        .threshold(0.6)
        .max_failed_pixels(usize::MAX / 2);
    if let Some(dir) = &shots {
        std::fs::create_dir_all(dir).expect("shots dir");
        options = options.output_path(dir.clone());
    }
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 1150.0))
        .with_options(options)
        .wgpu()
        .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, true));
    h.state_mut().prefs.tips_dismissed = true;
    h.state_mut().begin_open(capture);
    drive(&mut h, "the capture to open and score", |a| {
        a.capture.is_some() && a.scoring == Scoring::Done
    });
    assert_eq!(
        h.state().settings.channel,
        helia_core::debayer::Channel::Green,
        "the clipped red plane must not be stacked"
    );
    assert!(!h.state().settings.channel_chosen);
    let note = h.state().channel_note.clone().expect("a notice");
    assert!(note.contains("red plane is clipped"), "{note}");
    assert!(note.contains("stacking the green plane instead"), "{note}");
    assert!(!note.contains("--channel"), "a flag in the app: {note}");

    h.state_mut().select(Step::Stack);
    h.run_steps(4);
    h.get_by_label_contains("Channel: the red plane is clipped");
    if shots.is_some() {
        h.snapshot("clipped-ha-stack-step");
    }

    // A channel the user names is left alone, and the notice goes.
    h.state_mut().settings.channel_chosen = true;
    assert!(!h.state().settings.stack_config().clip_fallback);
}
