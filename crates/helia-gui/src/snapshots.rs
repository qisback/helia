// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Image snapshots of the main window, one per pipeline step.
//!
//! The capture is synthetic (`helia_core::testutil`) and the state is injected,
//! so nothing depends on timing, files or the machine's GPU. Rendering uses the
//! software adapter; the tests run only when `HELIA_GPU_TESTS=software` and
//! otherwise print a note and pass. The PNGs live in the user guide:
//! `docs/book/src/images/ui/`. Regenerate them with
//! `UPDATE_SNAPSHOTS=1 HELIA_GPU_TESTS=software cargo test -p helia-gui snapshots`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use eframe::egui::{self, vec2};
use egui_kittest::{Harness, SnapshotOptions};
use helia_core::align::multipoint::{ApGrid, ApMask, MultipointConfig};
use helia_core::io::capture::{CaptureReport, inspect};
use helia_core::io::ser::{ColorId, SerHeader};
use helia_core::io::source::{MemorySource, OpenOptions};
use helia_core::pipeline::{Pipeline, StackConfig, StackOutput, Stage};
use helia_core::testutil::{
    CaptureConfig, SyntheticCapture, SyntheticConfig, SyntheticSun, mosaic_disc, ser_bytes_stamped,
};
use helia_core::{BayerPattern, Frame, Keep};

use helia_core::recommend::recommend;
use helia_core::{ColourParams, Preset};

use crate::activity::{TaskKind, ToastKind};
use crate::app::{App, Cached, Capture, Run};
use crate::model::{CaptureMeta, Curve, Scoring, Step};
use crate::worker::{self, GpuProbe};

/// Per-pixel colour tolerance; see the user guide notes on snapshot testing.
const THRESHOLD: f32 = 0.6;
/// Pixels allowed to exceed the tolerance (of 1.3 to 2.1 million), for
/// rasteriser anti-aliasing differences between software adapters. The images
/// are made on lavapipe and compared on Windows' WARP too; the tall About page
/// (2.1 million pixels) differs in 9 of them, so the limit has headroom.
const MAX_FAILED_PIXELS: usize = 40;
const FRAMES: usize = 60;
const AP: usize = 48;

/// One wgpu instance, adapter and device for every snapshot test in the process, created
/// once and never dropped. Each harness otherwise builds and then destroys its own instance,
/// and tests run on parallel threads: destroying the last Vulkan instance unloads the driver
/// while another thread is inside it, or while a finished test thread still has driver
/// destructors to run, and the test binary dies with SIGSEGV (issue #37, #31).
pub(crate) fn shared_wgpu() -> eframe::egui_wgpu::WgpuSetup {
    use eframe::egui_wgpu::{RendererOptions, WgpuSetupExisting};
    static SETUP: std::sync::OnceLock<WgpuSetupExisting> = std::sync::OnceLock::new();
    SETUP
        .get_or_init(|| {
            // The snapshots are drawn by the software Vulkan or DX12 adapter. GL (EGL) is left
            // out: wgpu's EGL adapter makes its context current on whichever thread asks and
            // panics with `BadAccess` when two test threads meet in it.
            let mut setup = egui_kittest::wgpu::default_wgpu_setup();
            if let eframe::egui_wgpu::WgpuSetup::CreateNew(new) = &mut setup {
                new.instance_descriptor
                    .backends
                    .remove(eframe::egui_wgpu::wgpu::Backends::GL);
            }
            let rs = egui_kittest::wgpu::create_render_state(setup, RendererOptions::PREDICTABLE);
            WgpuSetupExisting {
                instance: rs.instance.clone(),
                adapter: rs.adapter.clone(),
                device: rs.device.clone(),
                queue: rs.queue.clone(),
            }
        })
        .clone()
        .into()
}

/// `secs` seconds before `now`.
fn ago(now: std::time::Instant, secs: u64) -> std::time::Instant {
    now.checked_sub(std::time::Duration::from_secs(secs))
        .expect("the clock is older than a few seconds")
}

/// One window at a time: every snapshot test opens its own software GPU device,
/// and lavapipe has crashed the process when several were created and torn down
/// together (see #31 and #37). The tests are fast enough to take turns.
pub(crate) fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A snapshot that tolerates one stray frame. The Windows gate (WARP) has failed now and
/// then with a few hundred pixels different in a frame that matched on the next run
/// (`01-open`, 159 pixels, on a merge commit whose dispatch passed minutes later). The
/// frame is rendered again after more steps, and only a second mismatch fails; the first
/// is printed so a recurring one stays visible. A real regression differs every time.
pub(crate) trait SnapshotSettled {
    fn snapshot_settled(&mut self, name: &str);
    /// As `snapshot_settled`, with more failed pixels allowed for this one snapshot.
    fn snapshot_settled_within(&mut self, name: &str, max_failed_pixels: usize);
}

impl<S> SnapshotSettled for Harness<'_, S> {
    fn snapshot_settled(&mut self, name: &str) {
        self.snapshot_settled_within(name, MAX_FAILED_PIXELS);
    }

    fn snapshot_settled_within(&mut self, name: &str, max_failed_pixels: usize) {
        let opts = options().max_failed_pixels(max_failed_pixels);
        if let Err(first) = self.try_snapshot_options(name, &opts) {
            eprintln!("snapshot {name} differed once, rendering again: {first}");
            self.run_steps(5);
            self.snapshot_options(name, &opts);
        }
    }
}

/// Failed pixels allowed for `inspector-adjust-prominence`. The viewer shows the lunar limb,
/// a long curved bright edge, and WARP (Windows) rasterises its anti-aliasing differently
/// from lavapipe (Linux, where the image is made): the Windows run differed by at most 9
/// levels in 24,889 pixels and 42 of them were over the tolerance, 39 on the limb and 3 on
/// the edge of the photo, against a limit of 40. The inspector text and layout are identical.
/// The allowance is for this one snapshot; a layout change moves thousands of pixels.
const PROMINENCE_EDGE_PIXELS: usize = 150;

pub(crate) fn enabled() -> bool {
    if std::env::var("HELIA_GPU_TESTS").as_deref() == Ok("software") {
        true
    } else {
        eprintln!("skipping GUI snapshots: set HELIA_GPU_TESTS=software to render them");
        false
    }
}

pub(crate) fn options() -> SnapshotOptions {
    SnapshotOptions::new()
        .threshold(THRESHOLD)
        .max_failed_pixels(MAX_FAILED_PIXELS)
        .output_path(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/book/src/images/ui"),
        )
}

/// SER ticks (100 ns since 0001-01-01) of 2022-07-17 15:32:09 UTC.
const TICKS_2022_07_17: i64 = 637_936_687_290_000_000;

/// What the header says of a synthetic capture, for the header view.
fn header(w: usize, h: usize, label: ColorId, depth: u32, instrument: &str) -> SerHeader {
    let mut hd = SerHeader::new(w as u32, h as u32, label, depth);
    hd.observer = "A. Observer".into();
    hd.instrument = instrument.into();
    hd.telescope = "Lunt LS80 Ha".into();
    hd.date_time = TICKS_2022_07_17 + 36_000_000_000;
    hd.date_time_utc = TICKS_2022_07_17;
    hd
}

/// The report for a SER file written from `frames` with `hd`, read back as
/// the application would read it.
fn report_for(name: &str, frames: &[Frame], hd: SerHeader) -> CaptureReport {
    // Tests run in parallel and share the process id; the name keeps files apart.
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("helia-snap-{}-{n}-{name}.ser", std::process::id()));
    std::fs::write(
        &path,
        ser_bytes_stamped(frames, hd, TICKS_2022_07_17, 100_000),
    )
    .expect("write the synthetic capture");
    let report = inspect(&path, &OpenOptions::default());
    let _ = std::fs::remove_file(&path);
    report.expect("inspect the synthetic capture")
}

pub(crate) struct Data {
    pub(crate) shown: usize,
    pub(crate) frames: Vec<Frame>,
    pub(crate) out: Arc<StackOutput>,
    pub(crate) grid: ApGrid,
}

pub(crate) fn data() -> Data {
    let sun = SyntheticSun::new(&SyntheticConfig {
        width: 384,
        height: 288,
        radius_frac: 0.38,
        spots: 4,
        ..SyntheticConfig::default()
    });
    let cap = SyntheticCapture::generate(
        &sun,
        &CaptureConfig {
            frames: FRAMES,
            warp_amp: 1.2,
            warp_floor: 0.3,
            warp_free: 6,
            ..CaptureConfig::default()
        },
    );
    let cfg = StackConfig {
        keep: Keep::Fraction(0.25),
        multipoint: Some(MultipointConfig {
            ap_size: AP,
            ..MultipointConfig::default()
        }),
        ..StackConfig::default()
    };
    let out = Pipeline::new(cfg)
        .run(
            &mut MemorySource(cap.frames.clone()),
            &|_, _| {},
            &AtomicBool::new(false),
        )
        .expect("synthetic stack");
    let shown = out.ranking[2];
    // Points on the disc only, as the stack and the preview place them.
    let frame = &cap.frames[shown];
    let grid = ApGrid::build_masked(
        frame,
        &MultipointConfig {
            ap_size: AP,
            ..MultipointConfig::default()
        },
        &ApMask {
            disc: helia_core::mask::fit_disc(frame),
            ..ApMask::default()
        },
    )
    .expect("grid");
    Data {
        shown,
        frames: cap.frames,
        out: Arc::new(out),
        grid,
    }
}

/// Put `app` into the state of an opened, scored capture.
pub(crate) fn load(app: &mut App, ctx: &egui::Context, d: &Data) {
    let f = &d.frames[d.shown];
    let (texture, size) = worker::texture(ctx, "snap-frame", f);
    let report = report_for(
        "sun",
        &d.frames,
        header(f.width, f.height, ColorId::Mono, 16, "Synthetic camera"),
    );
    app.capture = Some(Capture {
        path: PathBuf::from("synthetic-sun.ser"),
        report,
        meta: CaptureMeta {
            file: "synthetic-sun.ser".into(),
            container: "SER".into(),
            camera: Some("Synthetic camera".into()),
            width: f.width,
            height: f.height,
            bits: Some(16),
            bayer: None,
            frames: FRAMES,
            video: false,
            note: None,
        },
        keep_alive: None,
    });
    app.index = d.shown;
    app.shown = Some((texture.clone(), size));
    app.cache.insert(
        d.shown,
        Cached {
            texture,
            before: None,
            size,
            grid: Some((AP, Some(d.grid.clone()))),
        },
    );
    app.scores.clone_from(&d.out.scores);
    app.curve = Curve::new(&d.out.scores);
    app.scoring = Scoring::Done;
    app.gpu = Some(GpuProbe {
        hardware: Some("Example GPU (Vulkan, DiscreteGpu)".into()),
        software: None,
    });
    app.show_grid = false;
    // A fixed clock keeps every time-dependent word in the pictures the same.
    app.frozen_now = Some(std::time::Instant::now());
    // The capture's file name tells the filter, as a real one would.
    let rec = recommend(
        f.width,
        f.height,
        false,
        &["synthetic-sun_Ha_001".to_string()],
        None,
    );
    app.apply_recommendation(&rec);
    // The grid the preview carries is the one of this size.
    app.settings.ap_size = AP;
}

pub(crate) fn give_result(app: &mut App, ctx: &egui::Context, d: &Data) {
    app.result_tex = Some(worker::texture(ctx, "snap-result", &d.out.image));
    app.result = Some(Arc::clone(&d.out));
    app.show_result = true;
    app.auto_sigma = Some(helia_core::edit::deconv::heuristic_sigma(
        d.out.image.width,
        d.out.image.height,
    ));
}

/// The preview the worker would make, made here on the CPU so the picture
/// does not depend on timing: the stack with the edits and the colour, and the
/// geometry-only version for the "before" half.
fn give_preview(app: &mut App, ctx: &egui::Context, d: &Data) {
    use helia_core::Backend as _;
    let list = app.edit_list();
    let colour = app.settings.colour;
    let edited = if list.is_empty() {
        d.out.image.clone()
    } else {
        helia_core::CpuBackend
            .apply_edits(&d.out.image, &list)
            .expect("edits")
    };
    let rgb = helia_core::colour::colourise(&edited, &colour);
    app.colour_tex = Some(worker::rgb_texture(ctx, "snap-preview", &rgb));
    let before = helia_core::colour::colourise(&d.out.image, &colour);
    app.before_tex = Some(worker::rgb_texture(ctx, "snap-before", &before));
    app.preview_size = Some([rgb.width, rgb.height]);
    app.preview_took = Some(std::time::Duration::from_millis(38));
}

#[test]
fn window_at_each_step() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let d = data();
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 900.0))
        .with_options(options())
        .wgpu_setup(shared_wgpu())
        .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, false));
    let ctx = h.ctx.clone();
    load(h.state_mut(), &ctx, &d);

    let shot = |h: &mut Harness<'_, App>, name: &str| {
        h.run_steps(3);
        h.snapshot_settled(name);
    };

    // First run: the tips strip and the Quick stack card.
    h.state_mut().select(Step::Open);
    h.state_mut().show_grid = false;
    shot(&mut h, "01-open");
    h.state_mut().prefs.tips_dismissed = true;

    h.state_mut().select(Step::Analyse);
    shot(&mut h, "02-analyse");

    h.state_mut().select(Step::Align);
    // One exclusion rectangle, as a user would add for a logo or a stray label.
    h.state_mut().settings.exclude = vec![[8, 8, 110, 24]];
    shot(&mut h, "03-align");
    h.state_mut().settings.exclude.clear();

    h.state_mut().select(Step::Stack);
    h.state_mut().show_grid = false;
    shot(&mut h, "04-stack-settings");

    // Stacking: the notice names the stage, shows the percentage and the time left.
    let now = h.state().now();
    h.state_mut().run = Some(Run {
        id: 1,
        cancel: Arc::new(AtomicBool::new(false)),
        stage: Stage::LocalAlign,
        fraction: 0.55,
        peak: crate::model::overall_progress(Stage::LocalAlign, 0.55),
        started: ago(now, 4),
    });
    {
        let app = h.state_mut();
        let started = ago(now, 4);
        app.tasks
            .begin(TaskKind::Stack(Stage::LocalAlign), started, true);
        app.tasks
            .update(TaskKind::Stack(Stage::LocalAlign), Some(0.5), started);
    }
    shot(&mut h, "05-stacking");

    h.state_mut().run = None;
    h.state_mut().tasks.end(TaskKind::Stack(Stage::Finish));
    give_result(h.state_mut(), &ctx, &d);
    shot(&mut h, "06-result");

    // Colour: H-alpha, the look the file name suggested.
    {
        let app = h.state_mut();
        app.settings.edits.denoise_on = false;
        app.settings
            .edits
            .reset_adjust(d.out.image.width, d.out.image.height);
        app.settings.colour = ColourParams::preset(Preset::Ha);
        give_preview(app, &ctx, &d);
        app.select(Step::Colour);
    }
    shot(&mut h, "07-colour");

    h.state_mut().select(Step::Export);
    shot(&mut h, "08-export");
}

/// The Open step for a capture whose header says mono but whose pixels are an
/// RGGB mosaic (a colour camera behind an H-alpha filter, written by a
/// recorder that labelled it mono): the notice, the header as written, what
/// Helia will use and why, and the overrides.
#[test]
fn open_step_shows_the_header_and_the_mislabel() {
    if !enabled() {
        return;
    }
    let (w, hgt) = (480, 360);
    let frames: Vec<Frame> = (0..30)
        .map(|i| mosaic_disc(w, hgt, Some(BayerPattern::Rggb), [0.9, 1.0, 0.02], 5 + i))
        .collect();
    let report = report_for(
        "mislabel",
        &frames,
        header(w, hgt, ColorId::Mono, 8, "ZWO ASI482MC"),
    );
    assert_eq!(report.bayer(), Some(BayerPattern::Rggb));
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 2600.0))
        .with_options(options())
        .wgpu()
        .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, false));
    let ctx = h.ctx.clone();
    let mut shown = frames[0].clone();
    shown.bayer = Some(BayerPattern::Rggb);
    let shown = helia_core::debayer::Debayer::default().apply_owned(shown);
    let (texture, size) = worker::texture(&ctx, "snap-mislabel", &shown);
    let app = h.state_mut();
    app.capture = Some(Capture {
        path: PathBuf::from("Sun_Test.ser"),
        report,
        meta: CaptureMeta {
            file: "Sun_Test.ser".into(),
            container: "SER".into(),
            camera: Some("ZWO ASI482MC".into()),
            width: w,
            height: hgt,
            bits: Some(8),
            bayer: Some(BayerPattern::Rggb),
            frames: frames.len(),
            video: false,
            note: None,
        },
        keep_alive: None,
    });
    app.shown = Some((texture.clone(), size));
    app.cache.insert(
        0,
        Cached {
            texture,
            before: None,
            size,
            grid: None,
        },
    );
    app.scoring = Scoring::Done;
    app.gpu = Some(GpuProbe {
        hardware: Some("Example GPU (Vulkan, DiscreteGpu)".into()),
        software: None,
    });
    app.frozen_now = Some(std::time::Instant::now());
    app.show_grid = false;
    app.prefs.tips_dismissed = true;
    let rec = recommend(w, hgt, true, &["Sun_Test_Ha".to_string()], None);
    app.apply_recommendation(&rec);
    app.select(Step::Open);
    h.run_steps(3);
    h.snapshot_settled("11-open-header");
}

/// The Sharpen and Adjust steps, in a window tall enough to show every
/// control: wavelets (the default), the split view, deconvolution and the
/// adjustments.
#[test]
fn sharpen_and_adjust_steps() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let d = data();
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 1450.0))
        .with_options(options())
        .wgpu()
        .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, false));
    let ctx = h.ctx.clone();
    load(h.state_mut(), &ctx, &d);
    h.state_mut().prefs.tips_dismissed = true;
    give_result(h.state_mut(), &ctx, &d);
    let shot = |h: &mut Harness<'_, App>, name: &str| {
        h.run_steps(3);
        h.snapshot_settled(name);
    };

    // Sharpen: the recommended H-alpha wavelets, with the picture split into
    // the stack as it was and with the edits.
    {
        let app = h.state_mut();
        app.settings.colour = ColourParams::default();
        app.select(Step::Sharpen);
        give_preview(app, &ctx, &d);
    }
    shot(&mut h, "sharpen");
    h.state_mut().show_split = true;
    h.state_mut().split_pos = 0.45;
    give_preview(h.state_mut(), &ctx, &d);
    shot(&mut h, "sharpen-compare");
    h.state_mut().show_split = false;

    // Deconvolution, with its blur, iterations and the limb guard.
    {
        let app = h.state_mut();
        app.settings.edits.method = helia_core::SharpenMethod::RichardsonLucy;
        app.settings.edits.rl.iterations = 12;
        give_preview(app, &ctx, &d);
    }
    shot(&mut h, "sharpen-deconvolution");
    {
        let app = h.state_mut();
        app.settings
            .edits
            .reset_sharpen(Some(Preset::Ha), d.out.image.width, d.out.image.height);
        // Adjust: a little denoise, levels and a gentle curve.
        app.settings.edits.denoise_on = true;
        app.settings.edits.levels.black = 0.02;
        app.settings.edits.levels.gamma = 1.2;
        app.settings.edits.curve.points = vec![[0.25, 0.18], [0.5, 0.45], [0.8, 0.88]];
        app.select(Step::Adjust);
        give_preview(app, &ctx, &d);
    }
    shot(&mut h, "adjust");
}

/// A scoring run in progress (determinate, with a count and a time left) and
/// the toast that reports a finished stack.
#[test]
fn notices_while_working_and_when_done() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let d = data();
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 900.0))
        .with_options(options())
        .wgpu_setup(shared_wgpu())
        .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, false));
    let ctx = h.ctx.clone();
    load(h.state_mut(), &ctx, &d);
    h.state_mut().prefs.tips_dismissed = true;
    h.state_mut().select(Step::Analyse);

    let now = h.state().now();
    {
        let app = h.state_mut();
        let started = ago(now, 3);
        app.scoring = Scoring::Running;
        app.tasks.begin(TaskKind::Scoring, started, true);
        app.tasks.update(TaskKind::Scoring, Some(0.5), started);
    }
    h.run_steps(3);
    h.snapshot_settled("09-scoring-notice");

    {
        let app = h.state_mut();
        app.tasks.end(TaskKind::Scoring);
        app.scoring = Scoring::Done;
        app.toast(
            ToastKind::Success,
            crate::activity::stacked_message(15, 60, std::time::Duration::from_millis(2100)),
        );
    }
    give_result(h.state_mut(), &ctx, &d);
    h.state_mut().select(Step::Stack);
    h.run_steps(3);
    h.snapshot_settled("10-done-toast");
}

/// The About page, tall enough to show all of it. The version block is fixed
/// text: the live one holds the build date and commit.
#[test]
fn about_page() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let d = data();
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 1620.0))
        .with_options(options())
        .wgpu_setup(shared_wgpu())
        .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, false));
    let ctx = h.ctx.clone();
    load(h.state_mut(), &ctx, &d);
    h.state_mut().version_override = Some(
        [
            "helia-gui 0.7.0",
            "release: pre-release",
            "commit: 1a2b3c4d",
            "built: 2026-10-19 09:30 UTC",
            "profile: release",
            "target: x86_64-unknown-linux-gnu",
            "rust: rustc 1.95.0",
            "wgpu: 30.0.1",
            "gpu: Vulkan: Example GPU",
        ]
        .join("\n"),
    );
    h.state_mut().show_about = true;
    h.run_steps(3);
    h.snapshot_settled("about");
}

/// The About page with a backend's settings block under Updates. The block is plain
/// data, so the wording here is a neutral example; a build with a gated update service
/// draws its own.
#[test]
fn update_backend_panel() {
    use helia_update::backend::{Panel, PanelButton, PanelInput, PanelLine};

    use crate::update_ui::{UpdateStatus, UpdateUi};
    if !enabled() {
        return;
    }
    let _serial = serial();
    let d = data();
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 1900.0))
        .with_options(options())
        .wgpu_setup(shared_wgpu())
        .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, false));
    let ctx = h.ctx.clone();
    load(h.state_mut(), &ctx, &d);
    h.state_mut().version_override = Some(
        [
            "helia-gui 0.8.0",
            "release: pre-release",
            "commit: 1a2b3c4d",
            "built: 2026-10-25 09:30 UTC",
            "profile: release",
            "target: x86_64-unknown-linux-gnu",
        ]
        .join("\n"),
    );
    let panel = Panel {
        heading: "Example backend".into(),
        lines: vec![
            PanelLine {
                text: "Example: automatic updates are on".into(),
                muted: false,
            },
            PanelLine {
                text: "A second line of detail from the backend.".into(),
                muted: true,
            },
        ],
        link: Some(("Open the page".into(), "https://example.org/".into())),
        input: Some(PanelInput {
            hint: "Paste your code".into(),
        }),
        buttons: vec![
            PanelButton {
                id: "save".into(),
                label: "Save".into(),
                hover: "Keep the code in Helia's settings folder".into(),
                needs_input: true,
            },
            PanelButton {
                id: "remove".into(),
                label: "Remove".into(),
                hover: "Forget the code".into(),
                needs_input: false,
            },
        ],
        footer: None,
    };
    h.state_mut().update = UpdateUi::showing_with_panel(
        UpdateStatus::UpToDate {
            version: "0.8.0".into(),
        },
        Some(panel),
    );
    h.state_mut().show_about = true;
    h.run_steps(3);
    // Tall About page: WARP (Windows) differs from lavapipe in 42 pixels at the 0.6 threshold (limit 40),
    // all text anti-aliasing; the same headroom as the other About snapshots needs.
    h.snapshot_settled_within("update-backend-panel", 120);
}

/// The Open step for a compressed video: container, lossy-input note.
#[test]
fn open_step_for_a_video() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let d = data();
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 900.0))
        .with_options(options())
        .wgpu_setup(shared_wgpu())
        .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, false));
    let ctx = h.ctx.clone();
    load(h.state_mut(), &ctx, &d);
    if let Some(c) = h.state_mut().capture.as_mut() {
        c.path = PathBuf::from("sun.mp4");
        c.meta.file = "sun.mp4".into();
        c.meta.container = "MP4".into();
        c.meta.camera = None;
        c.meta.bits = Some(8);
        c.meta.video = true;
        c.meta.note = helia_core::io::ffmpeg::parse_probe(
            r#"{"streams":[{"codec_type":"video","codec_name":"h264","width":384,"height":288,
            "pix_fmt":"yuv420p","avg_frame_rate":"25/1","nb_frames":"60"}],"format":{}}"#,
        )
        .ok()
        .and_then(|i| i.quality_note());
    }
    h.state_mut().select(Step::Open);
    h.state_mut().show_grid = false;
    h.run_steps(3);
    h.snapshot_settled("open-video");
}

/// What a sender may attach, fixed so the images do not hold a build date.
fn feedback_details() -> crate::feedback::SystemDetails {
    crate::feedback::SystemDetails {
        app: helia_feedback::AppInfo {
            name: "helia".into(),
            version: "0.3.0".into(),
            commit: "1a2b3c4d".into(),
            target: "x86_64-unknown-linux-gnu".into(),
            gpu: Some("Vulkan: Example GPU".into()),
        },
        os: "Debian GNU/Linux 13 (trixie) (x86_64)".into(),
        capture: Some("Sun_Ha_001.ser, 384x288, 8-bit, 60 frames".into()),
    }
}

/// The Feedback page over a screenshot of the real window, with a rectangle
/// drawn round the problem and the report filled in.
#[test]
fn feedback_page() {
    use helia_feedback::Highlight;
    use helia_feedback::protocol::Kind as FbKind;
    use helia_feedback::screenshot::Screenshot;

    if !enabled() {
        return;
    }
    let _serial = serial();
    let d = data();
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 900.0))
        .with_options(options())
        .wgpu()
        .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, false));
    let ctx = h.ctx.clone();
    load(h.state_mut(), &ctx, &d);
    h.state_mut().select(Step::Sharpen);
    h.run_steps(3);
    // The picture the Feedback button would capture: the window as it is.
    let img = h.render().expect("render the window");
    let shot =
        Screenshot::new(img.width(), img.height(), img.into_raw()).expect("a window screenshot");
    let app = h.state_mut();
    app.feedback.open_with(Some(shot), feedback_details());
    app.feedback.highlights.push(Highlight {
        x: 1010,
        y: 90,
        w: 400,
        h: 150,
    });
    app.feedback.kind = FbKind::Bug;
    app.feedback.title = "The sharpening preview is blurry at 100%".into();
    app.feedback.description =
        "I zoomed to 100% on the Sharpen step and the preview looks softer than the exported file."
            .into();
    h.set_size(vec2(1440.0, 1800.0));
    h.run_steps(4);
    h.snapshot_settled("feedback");
}

/// The same page when the service could not be reached: the two fallbacks.
#[test]
fn feedback_page_when_the_service_is_down() {
    use crate::feedback::Stage;
    if !enabled() {
        return;
    }
    let _serial = serial();
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 1000.0))
        .with_options(options())
        .wgpu()
        .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, false));
    let app = h.state_mut();
    app.prefs.tips_dismissed = true;
    app.feedback.open_with(None, feedback_details());
    app.feedback.title = "Please add a dark theme for the viewer".into();
    app.feedback.kind = helia_feedback::protocol::Kind::Feature;
    app.feedback.stage = Stage::Failed {
        message: "could not reach the feedback service: connection refused".into(),
        fallbacks: true,
    };
    h.run_steps(3);
    h.snapshot_settled("feedback-service-down");
}

/// The top bar with the Pre-release badge and the Feedback button.
#[test]
fn top_bar_shows_the_pre_release_badge() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 220.0))
        .with_options(options())
        .wgpu()
        .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, false));
    h.state_mut().prefs.tips_dismissed = true;
    h.state_mut().gpu = Some(GpuProbe {
        hardware: Some("Example GPU (Vulkan, DiscreteGpu)".into()),
        software: None,
    });
    h.run_steps(3);
    h.snapshot_settled("topbar-pre-release");
}

/// The Calibrate step: empty with its help text, then with a dark, a flat and
/// the frame before (left) and after (right) calibration. The sensor is
/// synthetic and made badly on purpose: vignetting, dust, hot pixels.
#[test]
fn calibrate_step() {
    use crate::calibration::CalState;
    use crate::worker::CalReady;
    use helia_core::calibrate::master::{
        BuildOptions, CalInput, MasterKind, build_dark, build_flat,
    };
    use helia_core::calibrate::{Calibration, badpix};
    use helia_core::testutil::{DefectConfig, SensorDefects};

    if !enabled() {
        return;
    }
    let _serial = serial();
    let d = data();
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 1100.0))
        .with_options(options())
        .wgpu()
        .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, false));
    let ctx = h.ctx.clone();
    load(h.state_mut(), &ctx, &d);
    h.state_mut().prefs.tips_dismissed = true;
    h.state_mut().select(Step::Calibrate);
    h.run_steps(3);
    h.snapshot_settled("calibrate-off");

    // The same frame through a poor sensor, and the masters made from its darks and flats.
    let frame = &d.frames[d.shown];
    let sensor = SensorDefects::new(
        frame.width,
        frame.height,
        13,
        &DefectConfig {
            vignette: 0.9,
            dust: 45,
            dust_depth: 0.55,
            hot: 60,
            cold: 10,
            ..DefectConfig::default()
        },
    );
    let mut recorded = frame.clone();
    recorded.data.iter_mut().for_each(|v| *v *= 0.75);
    sensor.apply(&mut recorded);
    let opts = BuildOptions::default();
    let never = AtomicBool::new(false);
    let mut darks = CalInput::from_frames(sensor.dark_frames(40, 0.004, 1), Some(0.005), "darks")
        .expect("darks");
    let dark = build_dark(MasterKind::Dark, &mut darks, &opts, &|_| {}, &never).expect("dark");
    let mut flats =
        CalInput::from_frames(sensor.flat_frames(30, 0.6, 0.004, 2), Some(0.005), "flats")
            .expect("flats");
    let flat = build_flat(&mut flats, Some(&dark), &opts, &|_| {}, &never).expect("flat");
    let bad = badpix::detect(
        Some(&dark.frame),
        Some(&flat.master.frame),
        false,
        &badpix::DetectOptions::default(),
        "the dark and flat",
    );
    let cal = Calibration {
        dark: Some(dark),
        flat: Some(flat.master),
        bad: Some(bad),
        ..Calibration::default()
    };
    let mut calibrated = recorded.clone();
    cal.plan(frame.width, frame.height, None)
        .expect("plan")
        .apply(&mut calibrated.data);
    // The fingerprint of a master comes from floating-point maths that is not
    // bit-identical across platforms; the picture does not show it.
    let lines: Vec<String> = cal
        .summary_lines()
        .iter()
        .map(|l| l.split(", fp:").next().unwrap_or(l).to_owned())
        .collect();
    {
        let app = h.state_mut();
        let after = worker::texture(&ctx, "snap-calibrated", &calibrated);
        let before = worker::texture(&ctx, "snap-recorded", &recorded);
        app.shown = Some(after.clone());
        app.shown_before = Some(before.clone());
        app.cache.insert(
            d.shown,
            Cached {
                texture: after.0,
                before: Some(before),
                size: after.1,
                grid: None,
            },
        );
        app.settings.calibration.dark = Some(PathBuf::from("masters/master-dark.fits"));
        app.settings.calibration.flat = Some(PathBuf::from("flats/2022-07-17"));
        app.settings.calibration.flat_dark = Some(PathBuf::from("masters/master-dark.fits"));
        app.cal = CalState::Ready(Arc::new(CalReady {
            calibration: Arc::new(cal),
            lines,
            warnings: flat.warnings,
        }));
        app.split_pos = 0.5;
        app.show_cal_split = true;
        app.show_grid = false;
    }
    h.run_steps(3);
    h.snapshot_settled("calibrate");
}

/// The Align step with Auto chosen and the Stack step with the disc crop on,
/// after a stack that used both (#25).
#[test]
fn automatic_alignment_points_and_crop() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let d = data();
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 900.0))
        .with_options(options())
        .wgpu_setup(shared_wgpu())
        .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, false));
    let ctx = h.ctx.clone();
    load(h.state_mut(), &ctx, &d);
    h.state_mut().prefs.tips_dismissed = true;
    h.state_mut().show_grid = false;

    // Auto is on but nothing has run: the step says what will happen.
    h.state_mut().settings.ap_auto = true;
    h.state_mut().select(Step::Align);
    h.run_steps(3);
    h.snapshot_settled("align-auto");

    // The same capture, stacked with the automatic options on.
    let cfg = StackConfig {
        keep: Keep::Fraction(0.25),
        auto_ap: Some(helia_core::autoap::AutoAp::photosphere(AP)),
        autocrop: Some(helia_core::autocrop::DEFAULT_MARGIN),
        ..StackConfig::default()
    };
    let out = Pipeline::new(cfg)
        .run(
            &mut MemorySource(d.frames.clone()),
            &|_, _| {},
            &AtomicBool::new(false),
        )
        .expect("synthetic stack");
    let chosen = out.auto_ap.expect("auto alignment points ran");
    assert!(out.crop.is_some());
    {
        let app = h.state_mut();
        // The preview grid of the size that was chosen, as the worker would send it.
        let shown = d.shown;
        if let Some(c) = app.cache.get_mut(&shown) {
            c.grid = Some((chosen.size, out.grid.clone()));
        }
        app.result_tex = Some(worker::texture(&ctx, "snap-result", &out.image));
        app.result = Some(Arc::new(out));
        app.show_result = true;
        app.settings.autocrop = true;
        app.select(Step::Stack);
    }
    h.run_steps(3);
    h.snapshot_settled("stack-crop");

    h.state_mut().select(Step::Align);
    h.run_steps(3);
    h.snapshot_settled("align-auto-chosen");
}

/// The Stack step of a white-light capture after a stack: the granulation
/// (#56) and the seeing (#49) lines under the alignment line. A taller window,
/// so that the whole panel shows.
#[test]
fn stack_step_reports_granulation_and_seeing() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let d = data();
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 1150.0))
        .with_options(options())
        .wgpu_setup(shared_wgpu())
        .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, false));
    let ctx = h.ctx.clone();
    load(h.state_mut(), &ctx, &d);
    h.state_mut().prefs.tips_dismissed = true;
    h.state_mut().show_grid = false;
    let cfg = StackConfig {
        keep: Keep::Fraction(0.25),
        autocrop: Some(helia_core::autocrop::DEFAULT_MARGIN),
        ..StackConfig::default()
    };
    let out = Pipeline::new(cfg)
        .run(
            &mut MemorySource(d.frames.clone()),
            &|_, _| {},
            &AtomicBool::new(false),
        )
        .expect("synthetic stack");
    {
        let app = h.state_mut();
        app.result_tex = Some(worker::texture(&ctx, "snap-result", &out.image));
        app.result = Some(Arc::new(out));
        app.show_result = true;
        app.select(Step::Stack);
    }
    // A white-light stack also reports its granulation.
    {
        let app = h.state_mut();
        app.settings.aperture_mm = 80.0;
        let mut out = (*app.result.clone().expect("stacked")).clone();
        // A fixed report: a measured one differs in the last digit between
        // machines (the FFT planner picks by CPU), which a snapshot cannot take.
        out.seeing = Some(helia_core::seeing::SeeingReport {
            frames: 120,
            tiles: 12,
            motion: Some(helia_core::seeing::motion::MotionIndex {
                rms_px: 0.31,
                separation_px: 72.0,
                pairs: 20,
                frames: 120,
            }),
            arcsec_per_px: Some(0.3),
            spectral: Some(helia_core::seeing::SpectralFit {
                d_over_r0: 9.0,
                r0_cm: 8.9,
                r0_500_cm: 8.4,
                rings: 14,
                tiles: 12,
                spread: Some(0.12),
                cutoff: 0.35,
            }),
            grade: Some(helia_core::seeing::Grade::Good),
            notes: Vec::new(),
            series: Vec::new(),
        });
        app.granulation = Some(helia_core::granulation::Granulation {
            region: helia_core::Rect::new(0, 0, 240, 240),
            rms_contrast: 0.0412,
            spectrum: Vec::new(),
            noise_floor: 1e-5,
            peak_freq: 0.02,
            rolloff_freq: 0.15,
            resolved_freq: 0.30,
            cutoff_freq: None,
            cell_pitch_px: 12.0,
            cell_diameter_px: 7.0,
            cells: 300.0,
            arcsec_per_px: Some(0.3),
        });
        app.result = Some(Arc::new(out));
    }
    h.run_steps(3);
    h.snapshot_settled("stack-granulation");
}

/// A limb-darkened disc with three spots, for the sunspot snapshot.
#[allow(
    clippy::many_single_char_names,
    reason = "image maths uses c, r, x, y and the like"
)]
fn spotted_disc() -> helia_core::Frame {
    let n = 720usize;
    let (c, r) = (n as f32 / 2.0, 300.0f32);
    let spots = [
        (-40.0f32, 60.0f32, 16.0f32),
        (90.0, -30.0, 11.0),
        (120.0, 80.0, 7.0),
    ];
    let mut data = vec![0.02f32; n * n];
    for y in 0..n {
        for x in 0..n {
            let (dx, dy) = (x as f32 - c, y as f32 - c);
            let u2 = (dx * dx + dy * dy) / (r * r);
            if u2 >= 1.0 {
                continue;
            }
            let mu = (1.0 - u2).sqrt();
            let mut v = 1.0 - 0.6 * (1.0 - mu) - 0.2 * (1.0 - mu).powi(2);
            for (sx, sy, rad) in spots {
                let d = (dx - sx).hypot(dy - sy);
                if d < rad * 0.4 {
                    v *= 0.3;
                } else if d < rad {
                    v *= 0.75;
                }
            }
            data[y * n + x] = v;
        }
    }
    helia_core::Frame::new(n, n, data).expect("frame")
}

/// The Stack step after a stack with the sunspot search on: the count, the
/// Wolf number and the area under the alignment line, and the overlay in the
/// viewer.
#[test]
fn stack_step_finds_sunspots() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let d = data();
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 1400.0))
        .with_options(options())
        .wgpu_setup(shared_wgpu())
        .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, false));
    let ctx = h.ctx.clone();
    load(h.state_mut(), &ctx, &d);
    h.state_mut().prefs.tips_dismissed = true;
    h.state_mut().show_grid = false;
    let cfg = StackConfig {
        keep: Keep::Fraction(0.25),
        ..StackConfig::default()
    };
    let out = Pipeline::new(cfg)
        .run(
            &mut MemorySource(d.frames.clone()),
            &|_, _| {},
            &AtomicBool::new(false),
        )
        .expect("synthetic stack");
    {
        let app = h.state_mut();
        let det = helia_core::sunspots::detect(
            &spotted_disc(),
            Some(1_758_125_640.0),
            &helia_core::sunspots::Params::default(),
        )
        .expect("a disc");
        app.sunspot_tex = Some(worker::rgb_texture(&ctx, "snap-sunspots", &det.overlay));
        app.sunspots = Some(det);
        app.settings.sunspots = true;
        app.result_tex = Some(worker::texture(&ctx, "snap-result", &out.image));
        app.result = Some(Arc::new(out));
        app.show_result = true;
        app.show_sunspots = true;
        app.select(Step::Stack);
    }
    h.run_steps(3);
    h.snapshot_settled("stack-sunspots");
}

/// The Stack step with the Science panel open: the plot chips, the caption
/// and the frame quality graph in the viewer.
#[test]
fn stack_step_science_plots() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let d = data();
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 1400.0))
        .with_options(options())
        .wgpu_setup(shared_wgpu())
        .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, false));
    let ctx = h.ctx.clone();
    load(h.state_mut(), &ctx, &d);
    h.state_mut().prefs.tips_dismissed = true;
    h.state_mut().show_grid = false;
    let cfg = StackConfig {
        keep: Keep::Fraction(0.25),
        seeing: Some(helia_core::seeing::SeeingSettings::default()),
        ..StackConfig::default()
    };
    let out = Pipeline::new(cfg)
        .run(
            &mut MemorySource(d.frames.clone()),
            &|_, _| {},
            &AtomicBool::new(false),
        )
        .expect("synthetic stack");
    {
        let app = h.state_mut();
        app.result_tex = Some(worker::texture(&ctx, "snap-result", &out.image));
        app.result = Some(Arc::new(out));
        app.show_result = true;
        app.select(Step::Stack);
        app.build_science(&ctx);
        app.show_science = true;
    }
    h.run_steps(3);
    h.snapshot_settled("stack-science");
}

/// The Sharpen step with the limb darkening flattened by the fitted law, the
/// fit under the chips and the flat disc in the viewer (#20).
#[test]
fn limb_darkening_flatten() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let d = data();
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 1450.0))
        .with_options(options())
        .wgpu_setup(shared_wgpu())
        .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, false));
    let ctx = h.ctx.clone();
    load(h.state_mut(), &ctx, &d);
    h.state_mut().prefs.tips_dismissed = true;
    give_result(h.state_mut(), &ctx, &d);
    {
        let app = h.state_mut();
        app.settings.colour = ColourParams::default();
        app.settings.edits.method = helia_core::SharpenMethod::None;
        app.settings.edits.flatten = Some(helia_core::limbdark::Source::Model);
        app.select(Step::Sharpen);
        give_preview(app, &ctx, &d);
        app.show_split = true;
        app.split_pos = 0.5;
    }
    h.run_steps(3);
    h.snapshot("sharpen-flatten");
}

/// The Adjust step with the prominence layer on (#23): the sliders under the
/// toggle and the composite in the viewer.
#[test]
fn adjust_step_prominence_layer() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let d = data();
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 1450.0))
        .with_options(options())
        .wgpu_setup(shared_wgpu())
        .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, false));
    let ctx = h.ctx.clone();
    load(h.state_mut(), &ctx, &d);
    h.state_mut().prefs.tips_dismissed = true;
    give_result(h.state_mut(), &ctx, &d);
    {
        let app = h.state_mut();
        app.settings.colour = ColourParams::preset(helia_core::Preset::Ha);
        app.settings.edits.method = helia_core::SharpenMethod::None;
        app.settings.edits.prominence_on = true;
        app.select(Step::Adjust);
        give_preview(app, &ctx, &d);
    }
    h.run_steps(3);
    h.snapshot("adjust-prominence");
}

/// The Colour step with the colour table export chips (#23).
#[test]
fn colour_step_table_export() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let d = data();
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 1450.0))
        .with_options(options())
        .wgpu_setup(shared_wgpu())
        .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, false));
    let ctx = h.ctx.clone();
    load(h.state_mut(), &ctx, &d);
    h.state_mut().prefs.tips_dismissed = true;
    give_result(h.state_mut(), &ctx, &d);
    {
        let app = h.state_mut();
        app.settings.colour = ColourParams::preset(helia_core::Preset::Ha);
        app.settings.edits.method = helia_core::SharpenMethod::None;
        app.select(Step::Colour);
        give_preview(app, &ctx, &d);
    }
    h.run_steps(3);
    h.snapshot("colour-table");
}

/// The Stack step with the advanced settings open and the field rotation
/// correction on (#24): the site, the mirror switch and what the capture's
/// timestamps say about the rotation.
#[test]
fn stack_step_field_rotation() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let d = data();
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 1250.0))
        .with_options(options())
        .wgpu_setup(shared_wgpu())
        .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, false));
    let ctx = h.ctx.clone();
    load(h.state_mut(), &ctx, &d);
    h.state_mut().prefs.tips_dismissed = true;
    h.state_mut().show_grid = false;
    // A capture whose frames are stamped half a minute apart: half an hour.
    let path = std::env::temp_dir().join(format!("helia-snap-{}-rot.ser", std::process::id()));
    std::fs::write(
        &path,
        ser_bytes_stamped(
            &d.frames,
            header(
                d.frames[0].width,
                d.frames[0].height,
                ColorId::Mono,
                16,
                "Synthetic camera",
            ),
            TICKS_2022_07_17,
            300_000_000,
        ),
    )
    .expect("write the stamped capture");
    {
        let app = h.state_mut();
        if let Some(c) = app.capture.as_mut() {
            c.path.clone_from(&path);
        }
        app.settings.field_rotation = true;
        app.settings.site = [51.5, -0.1, 30.0];
        app.settings.site_set = true;
        app.show_advanced = true;
        app.select(Step::Stack);
    }
    h.run_steps(3);
    h.snapshot_settled("stack-field-rotation");
    let _ = std::fs::remove_file(&path);
}

/// True colour from a raw colour camera (#72): the Stack step with the option
/// chosen, and the Colour step after a true-colour stack.
#[test]
fn true_colour_steps() {
    use helia_core::Shift;
    use helia_core::truecolour::{TrueColour, WhiteBalance};
    if !enabled() {
        return;
    }
    let _serial = serial();
    let d = data();
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 900.0))
        .with_options(options())
        .wgpu_setup(shared_wgpu())
        .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, false));
    let ctx = h.ctx.clone();
    load(h.state_mut(), &ctx, &d);
    h.state_mut().prefs.tips_dismissed = true;
    h.state_mut().show_grid = false;
    {
        let app = h.state_mut();
        if let Some(c) = app.capture.as_mut() {
            c.meta.bayer = Some(BayerPattern::Rggb);
        }
        app.settings.true_colour = true;
        app.select(Step::Stack);
    }
    h.run_steps(3);
    h.snapshot_settled("stack-true-colour");

    // The same capture as a colour camera saw it, stacked in true colour.
    let frames = helia_core::testutil::colour_mosaic(
        &d.frames,
        [1.0, 0.86, 0.62],
        [
            Shift::new(-0.4, -0.5),
            Shift::default(),
            Shift::new(0.3, 0.4),
        ],
        BayerPattern::Rggb,
    );
    let cfg = StackConfig {
        keep: Keep::Fraction(0.25),
        multipoint: Some(helia_core::align::multipoint::MultipointConfig {
            ap_size: AP,
            ..helia_core::align::multipoint::MultipointConfig::default()
        }),
        true_colour: Some(TrueColour {
            white_balance: WhiteBalance::Off,
            align_channels: true,
        }),
        ..StackConfig::default()
    };
    let out = Pipeline::new(cfg)
        .run(
            &mut MemorySource(frames),
            &|_, _| {},
            &AtomicBool::new(false),
        )
        .expect("true colour stack");
    {
        let app = h.state_mut();
        let rgb = &out.colour.as_ref().expect("a colour picture").rgb;
        app.result_tex = Some(worker::rgb_texture(&ctx, "snap-result", rgb));
        app.result = Some(Arc::new(out));
        app.show_result = true;
        app.settings.white_balance = WhiteBalance::Off;
        app.settings.edits.method = helia_core::SharpenMethod::None;
        app.select(Step::Colour);
    }
    h.run_steps(3);
    h.snapshot_settled("colour-true");
    h.state_mut().select(Step::Stack);
    h.run_steps(3);
    h.snapshot_settled("stack-true-colour-done");
}

/// The Time-lapse step with a registered series of five stacks, one shown.
#[test]
fn timelapse_step() {
    use helia_core::Shift;
    use helia_core::picture::{self, Picture};
    use helia_core::timelapse::{self as tl, Deflicker, Options, Reference};
    if !enabled() {
        return;
    }
    let _serial = serial();
    let d = data();
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 900.0))
        .with_options(options())
        .wgpu_setup(shared_wgpu())
        .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, false));
    let ctx = h.ctx.clone();
    load(h.state_mut(), &ctx, &d);
    h.state_mut().prefs.tips_dismissed = true;
    // Five stacks of the same disc, each a little off and a little brighter or darker.
    let moves = [
        (0.0f32, 0.0f32, 1.0f32),
        (4.2, -2.6, 0.93),
        (8.1, -4.0, 1.07),
        (-3.0, 3.1, 0.97),
        (-6.5, 5.2, 1.03),
    ];
    let stacks: Vec<Picture> = moves
        .iter()
        .map(|&(dx, dy, g)| {
            let f = helia_core::resample::shifted(
                &d.out.image,
                Shift::new(-dx, -dy),
                helia_core::Kernel::Lanczos3,
            );
            picture::gain_offset(&Picture::Mono(f), g, 0.0)
        })
        .collect();
    let start = helia_core::io::solarnet::unix_from_iso("2026-10-20T14:32:09").expect("time");
    let times: Vec<Option<f64>> = (0..5).map(|i| Some(start + 300.0 * f64::from(i))).collect();
    let sequence = tl::register(
        &stacks,
        &times,
        &Options {
            reference: Reference::Middle,
            deflicker: Deflicker::Match,
            crop: true,
            ..Options::default()
        },
        &|_| {},
        &AtomicBool::new(false),
    )
    .expect("register");
    let sources: Vec<String> = (0..5)
        .map(|i| format!("15_{:02}_09.avi", 32 + 5 * i))
        .collect();
    {
        let app = h.state_mut();
        app.tl.captures = sources.iter().map(PathBuf::from).collect();
        app.tl.bright = crate::timelapse::Bright::Match;
        app.tl.crop = true;
        app.tl.date = "2026-10-20".into();
        app.tl.index = 1;
        app.tl_set_built(&ctx, crate::timelapse::Built { sequence, sources });
        app.select(Step::Timelapse);
    }
    h.set_size(vec2(1440.0, 1400.0));
    h.run_steps(3);
    h.snapshot_settled("stack-timelapse");
}

/// The Mosaic step with four panels cut from the synthetic stack and joined.
#[test]
#[allow(
    clippy::many_single_char_names,
    reason = "x, y, u, v, s, c are the natural names in the cutting of panels"
)]
fn mosaic_step() {
    use helia_core::mosaic::{self as mo, Options};
    use helia_core::picture::Picture;
    use helia_core::resample::{Kernel, sample};
    if !enabled() {
        return;
    }
    let _serial = serial();
    let d = data();
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 1400.0))
        .with_options(options())
        .wgpu_setup(shared_wgpu())
        .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, false));
    let ctx = h.ctx.clone();
    load(h.state_mut(), &ctx, &d);
    h.state_mut().prefs.tips_dismissed = true;
    let truth = &d.out.image;
    let cuts = [
        ((6.0f64, 4.0f64), 0.0f64, (1.0f32, 0.0f32)),
        ((150.0, 2.0), 1.0, (0.93, 0.01)),
        ((4.0, 104.0), -0.8, (1.06, -0.01)),
        ((152.0, 106.0), 0.6, (0.97, 0.008)),
    ];
    let (pw, ph) = (232usize, 186usize);
    let panels: Vec<Picture> = cuts
        .iter()
        .map(|&((x0, y0), deg, (g, o))| {
            let mut f = Frame::zeros(pw, ph);
            let (s, c) = deg.to_radians().sin_cos();
            let (cx, cy) = ((pw as f64 - 1.0) / 2.0, (ph as f64 - 1.0) / 2.0);
            for y in 0..ph {
                for x in 0..pw {
                    let (u, v) = (x as f64 - cx, y as f64 - cy);
                    let (qx, qy) = (c * u - s * v + cx + x0, s * u + c * v + cy + y0);
                    f.data[y * pw + x] =
                        sample(truth, qx as f32, qy as f32, Kernel::Lanczos3) * g + o;
                }
            }
            Picture::Mono(f)
        })
        .collect();
    let mosaic = mo::build(
        &panels,
        &[],
        &Options::default(),
        &|_| {},
        &AtomicBool::new(false),
    )
    .expect("mosaic");
    let names: Vec<String> = (1..=4).map(|i| format!("panel-{i}.ser")).collect();
    {
        let app = h.state_mut();
        app.mo.panels = names.iter().map(PathBuf::from).collect();
        app.mo_set_built(
            &ctx,
            crate::mosaic::MosaicBuilt {
                mosaic,
                names,
                sizes: vec![(pw, ph); 4],
            },
        );
        app.select(Step::Mosaic);
    }
    h.run_steps(3);
    h.snapshot_settled("stack-mosaic");
}

/// Width of the inspector, in points (see `inspector::inspector`).
const INSPECTOR_W: f32 = 360.0;

/// Every widget in the inspector column stays inside it: nothing runs past the
/// window edge, and nothing the inspector draws spills left over the viewer
/// (#229, #261, #262).
fn assert_inspector_contained<S>(h: &Harness<'_, S>, win_w: f32, ppp: f32, what: &str) {
    use egui_kittest::kittest::NodeT;
    let left = win_w - INSPECTOR_W;
    let mut bad = Vec::new();
    for n in h.root().children_recursive() {
        let Some(b) = n.accesskit_node().bounding_box() else {
            continue;
        };
        let (x0, y0, x1) = (b.x0 as f32 / ppp, b.y0 as f32 / ppp, b.x1 as f32 / ppp);
        // The top bar and anything as wide as the window are not the inspector.
        if y0 < 66.0 || x1 - x0 >= win_w - 1.0 {
            continue;
        }
        if x1 > win_w + 0.5 || (x1 > left + 1.0 && x0 < left - 0.5) {
            bad.push(format!(
                "{:?} {:?} x {x0:.1}..{x1:.1} y {y0:.0}",
                n.accesskit_node().role(),
                n.accesskit_node()
                    .label()
                    .or_else(|| n.accesskit_node().value())
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{what}: widgets outside the inspector column ({left}..{win_w}):\n{}",
        bad.join("\n")
    );
}

/// The Sharpen and Adjust panels at the reporter's window (1904 px wide) and at
/// 1.25 and 1.5 display scaling: wrapped text and rows that fit the 360 pt
/// column, the On/Off row keeps its description clear of the pill.
#[test]
fn inspector_panels_respect_the_column_width() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let d = data();
    for ppp in [1.0_f32, 1.25, 1.5] {
        let win_w = (1904.0 / ppp).round();
        let win_h = (1500.0 / ppp).round();
        let mut h = Harness::builder()
            .with_size(vec2(win_w, win_h))
            .with_pixels_per_point(ppp)
            .with_options(options())
            .wgpu_setup(shared_wgpu())
            .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, false));
        let ctx = h.ctx.clone();
        load(h.state_mut(), &ctx, &d);
        h.state_mut().prefs.tips_dismissed = true;
        give_result(h.state_mut(), &ctx, &d);
        let snap = (ppp - 1.0).abs() < f32::EPSILON;
        let check = |h: &mut Harness<'_, App>, what: &str, shot: &str| {
            h.run_steps(4);
            assert_inspector_contained(h, win_w, ppp, &format!("{what} at {ppp}x"));
            if snap {
                let limit = if shot == "inspector-adjust-prominence" {
                    PROMINENCE_EDGE_PIXELS
                } else {
                    MAX_FAILED_PIXELS
                };
                h.snapshot_settled_within(shot, limit);
            }
        };

        // Wavelets with the Linear scheme: the Step box makes the widest row.
        {
            let app = h.state_mut();
            app.settings.colour = ColourParams::default();
            app.settings.edits.method = helia_core::SharpenMethod::Wavelet;
            app.settings.edits.wavelet.scheme = helia_core::edit::wavelet::Scheme::Linear;
            app.select(Step::Sharpen);
            give_preview(app, &ctx, &d);
        }
        check(&mut h, "wavelets, linear", "inspector-wavelets-linear");

        // Deconvolution with the noise warning, the longest wrapped text.
        {
            let app = h.state_mut();
            app.settings.edits.method = helia_core::SharpenMethod::RichardsonLucy;
            app.settings.edits.rl.iterations = 12;
            app.sharpen_check = Some(helia_core::edit::diagnose::SharpenCheck {
                detail_gain: 1.35,
                noise_gain: 2.11,
                residual: None,
                noise_level: 0.01,
                verdict: helia_core::edit::diagnose::Verdict::NoiseDominated,
            });
        }
        check(
            &mut h,
            "deconvolution warning",
            "inspector-deconvolution-warning",
        );

        // Adjust with the prominence layer: the On pill beside a long description.
        {
            let app = h.state_mut();
            app.settings.edits.method = helia_core::SharpenMethod::None;
            app.settings.edits.prominence_on = true;
            app.select(Step::Adjust);
            give_preview(app, &ctx, &d);
        }
        check(&mut h, "adjust, prominences", "inspector-adjust-prominence");
    }
}

/// Every step's inspector at the common laptop windows (1366x768, and 1920x1080
/// at 125% and 150% display scaling, which leave 1536x864 and 1280x720 points):
/// no widget runs past the window or over the viewer.
#[test]
fn every_inspector_fits_the_common_laptop_windows() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let d = data();
    for (win_w, win_h, ppp) in [
        (1366.0_f32, 768.0_f32, 1.0_f32),
        (1536.0, 864.0, 1.25),
        (1280.0, 720.0, 1.5),
    ] {
        let mut h = Harness::builder()
            .with_size(vec2(win_w, win_h))
            .with_pixels_per_point(ppp)
            .with_options(options())
            .wgpu_setup(shared_wgpu())
            .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, false));
        let ctx = h.ctx.clone();
        load(h.state_mut(), &ctx, &d);
        h.state_mut().prefs.tips_dismissed = true;
        give_result(h.state_mut(), &ctx, &d);
        {
            let app = h.state_mut();
            app.settings.edits.method = helia_core::SharpenMethod::Wavelet;
            app.settings.edits.wavelet.scheme = helia_core::edit::wavelet::Scheme::Linear;
            app.settings.edits.prominence_on = true;
        }
        for step in Step::ALL {
            {
                let app = h.state_mut();
                app.select(step);
                give_preview(app, &ctx, &d);
            }
            h.run_steps(4);
            assert_inspector_contained(
                &h,
                win_w,
                ppp,
                &format!("{} at {win_w}x{win_h} ({ppp}x)", step.title()),
            );
        }
    }
}
