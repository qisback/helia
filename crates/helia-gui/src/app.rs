// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Application state, event handling and the frame loop.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use eframe::egui::{self, Key, Modifiers, TextureHandle};
use helia_core::align::multipoint::{ApGrid, MultipointConfig};
use helia_core::debayer::{Channel, Debayer, Method};
use helia_core::edit::store::{self, EditPreset};
use helia_core::edit::{EditList, deconv};
use helia_core::io::capture::{CaptureReport, Level};
use helia_core::io::source::{BayerSetting, Endian, OpenOptions, RowOrder};
use helia_core::output::OutputPlan;
use helia_core::pipeline::{StackConfig, StackOutput, Stage};
use helia_core::quality::Metric;
use helia_core::recommend::{Recommendation, recommend};
use helia_core::stack::{RobustKind, StackMode};
use helia_core::{ColourParams, Keep, Preset};
use helia_gpu::BackendChoice;

use helia_core::applog;
use helia_feedback::bugreport::BugReport;
use helia_feedback::sysinfo;

use crate::activity::{self, TaskKind, Tasks, ToastKind, Toasts};
use crate::calibration::{CalSettings, CalState};
use crate::demo::Demo;
use crate::edits::{self, EditSettings};
use crate::model::{
    CaptureMeta, Curve, ExportFormat, Scoring, Snapshot, Step, View, keep_count, snap_keep,
};
use crate::prefs::Prefs;
use crate::theme;
use crate::worker::{
    self, Event, GpuProbe, Opened, Preview, PreviewBase, PreviewRequest, RenderRequest, Workers,
};
use helia_core::autoap::AutoAp;
use helia_core::mask::MaskConfig;
use helia_core::truecolour::{TrueColour, WhiteBalance};

/// Window width below which the side panels become toggles.
pub const NARROW: f32 = 1100.0;
/// Decoded frames kept as textures.
const CACHE_FRAMES: usize = 40;

/// User-editable settings.
#[derive(Clone)]
pub struct Settings {
    pub keep_percent: f32,
    pub ap_size: usize,
    /// Stack the red, green and blue planes of a raw colour capture into a
    /// colour picture (true colour) instead of one plane.
    pub true_colour: bool,
    /// The balance of true colour.
    pub white_balance: WhiteBalance,
    /// Register red and blue to green.
    pub align_channels: bool,
    /// Choose the alignment points from the image scale and the seeing when
    /// stacking, instead of using `ap_size`.
    pub ap_auto: bool,
    /// Crop the stack to the solar disc plus `autocrop_margin`.
    pub autocrop: bool,
    /// Margin of the crop, percent of the disc radius.
    pub autocrop_margin: f32,
    /// Outlier-robust combination, `None` for the weighted mean.
    pub robust: Option<RobustKind>,
    pub backend: BackendChoice,
    pub bayer: BayerSetting,
    /// Bits per sample instead of the SER header's.
    pub bit_depth: Option<u32>,
    /// Byte order instead of the SER header flag.
    pub endian: Option<Endian>,
    /// Read the rows bottom to top.
    pub flip_rows: bool,
    /// Row order of FITS frames.
    pub row_order: RowOrder,
    pub channel: Channel,
    /// The user picked the channel, so a clipped plane is not replaced.
    pub channel_chosen: bool,
    pub method: Method,
    pub metric: Metric,
    pub export_format: ExportFormat,
    /// The filter in force: detected from the capture or chosen by the user.
    pub preset: Option<Preset>,
    /// Where the filter was read from ("chosen" or the text it was found in).
    pub preset_source: Option<String>,
    /// How the result is coloured for export and the Colour step.
    pub colour: ColourParams,
    /// Rectangles (x, y, width, height) the alignment must stay off.
    pub exclude: Vec<[usize; 4]>,
    /// Sharpening and adjustments of the stack.
    pub edits: EditSettings,
    /// Preview at full size instead of shrunk by a power of two.
    pub full_preview: bool,
    /// Dark, flat and bad-pixel calibration.
    pub calibration: CalSettings,
    /// Telescope aperture in millimetres, for r0 in the seeing report; 0 when
    /// it has not been set.
    pub aperture_mm: f32,
    /// Undo the field rotation of an altazimuth mount from the capture's
    /// timestamps.
    pub field_rotation: bool,
    /// The picture is a mirror image, so the turn is the other way.
    pub field_rotation_mirror: bool,
    /// Observing site: latitude north, longitude east (degrees), height (m).
    pub site: [f64; 3],
    /// The site has been given (or came from `HELIA_SITE`).
    pub site_set: bool,
    /// Find the sunspots on the stack (white light).
    pub sunspots: bool,
    /// Observer factor k of the Wolf number.
    pub wolf_k: f32,
    /// The image has celestial north up (an equatorial mount).
    pub celestial_north_up: bool,
    /// Measure the alignment points again against the finished stack, for the
    /// residual plot of the Science panel (one more registration pass).
    pub ap_residual: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            keep_percent: 25.0,
            aperture_mm: 0.0,
            field_rotation: false,
            field_rotation_mirror: false,
            site: helia_core::io::solarnet::Site::from_env()
                .map_or([0.0; 3], |s| [s.lat_deg, s.lon_deg, s.alt_m]),
            site_set: helia_core::io::solarnet::Site::from_env().is_some(),
            sunspots: false,
            wolf_k: 1.0,
            celestial_north_up: false,
            ap_residual: false,
            ap_size: 64,
            true_colour: false,
            white_balance: WhiteBalance::Auto,
            align_channels: true,
            ap_auto: false,
            autocrop: false,
            autocrop_margin: helia_core::autocrop::DEFAULT_MARGIN * 100.0,
            robust: None,
            backend: BackendChoice::Auto,
            bayer: BayerSetting::Auto,
            bit_depth: None,
            endian: None,
            flip_rows: false,
            row_order: RowOrder::Auto,
            channel: Channel::Luminance,
            channel_chosen: false,
            method: Method::Mhc,
            metric: Metric::GradientEnergy,
            export_format: ExportFormat::Tiff,
            preset: None,
            preset_source: None,
            colour: ColourParams::default(),
            exclude: Vec::new(),
            edits: EditSettings::default(),
            full_preview: false,
            calibration: CalSettings::default(),
        }
    }
}

impl Settings {
    /// What is done to the stack on its way to a file. `auto_sigma` is the
    /// blur size measured from the stack, for an automatic PSF.
    pub fn output_plan(&self, auto_sigma: Option<f32>) -> OutputPlan {
        OutputPlan {
            edits: self.edits.list(auto_sigma),
            colour: self.colour,
            edit_fits: self.edits.edit_fits,
            lead_notes: Vec::new(),
            fits: None,
            rgb: None,
        }
    }

    pub fn debayer(&self) -> Debayer {
        Debayer {
            method: self.method,
            channel: self.channel,
        }
    }

    pub fn open_options(&self) -> OpenOptions {
        OpenOptions {
            bayer: self.bayer,
            channel: self.channel,
            bit_depth: self.bit_depth,
            endian: self.endian,
            flip_rows: self.flip_rows,
            row_order: self.row_order,
        }
    }

    /// The exclusion list as rectangles; empty ones are dropped.
    pub fn exclude_rects(&self) -> Vec<helia_core::Rect> {
        self.exclude
            .iter()
            .filter(|[_, _, w, h]| *w > 0 && *h > 0)
            .map(|&[x, y, w, h]| helia_core::Rect::new(x, y, w, h))
            .collect()
    }

    pub fn stack_config(&self) -> StackConfig {
        StackConfig {
            mask: MaskConfig {
                exclude: self.exclude_rects(),
                ..MaskConfig::default()
            },
            keep: Keep::Fraction(self.keep_percent / 100.0),
            ap_refine: helia_core::pipeline::ApRefine {
                residual: self.ap_residual,
                ..helia_core::pipeline::ApRefine::default()
            },
            multipoint: Some(MultipointConfig {
                ap_size: self.ap_size,
                ..MultipointConfig::default()
            }),
            mode: self
                .robust
                .map_or(StackMode::Mean, |k| StackMode::from_kind(k, k.default_k())),
            metric: self.metric,
            debayer: self.debayer(),
            clip_fallback: !self.channel_chosen,
            auto_ap: self
                .ap_auto
                .then(|| AutoAp::for_preset(self.preset, self.ap_size)),
            autocrop: self.autocrop.then_some(self.autocrop_margin / 100.0),
            limb_law: Some(self.edits.limb_law),
            true_colour: self.true_colour.then_some(TrueColour {
                white_balance: self.white_balance,
                align_channels: self.align_channels,
            }),
            seeing: Some(helia_core::seeing::SeeingSettings {
                aperture_mm: (self.aperture_mm > 0.0).then_some(self.aperture_mm),
                wavelength_nm: self.preset.map(helia_core::seeing::preset_wavelength_nm),
            }),
            ..StackConfig::default()
        }
    }
}

/// Limb-darkening fits of one stack, made when first asked for.
#[derive(Default)]
pub struct LimbFits {
    /// The stack they belong to (address of its output).
    for_result: usize,
    fits: Vec<(
        helia_core::limbdark::Law,
        Option<helia_core::limbdark::LimbFit>,
    )>,
}

impl LimbFits {
    /// The fit of `law` to `out`'s image, made on first use.
    pub fn get(
        &mut self,
        out: &Arc<StackOutput>,
        law: helia_core::limbdark::Law,
    ) -> Option<&helia_core::limbdark::LimbFit> {
        let key = Arc::as_ptr(out) as usize;
        if self.for_result != key {
            self.for_result = key;
            self.fits.clear();
        }
        if !self.fits.iter().any(|(l, _)| *l == law) {
            self.fits
                .push((law, helia_core::limbdark::fit_image(&out.image, law)));
        }
        self.fits
            .iter()
            .find(|(l, _)| *l == law)
            .and_then(|(_, f)| f.as_ref())
    }
}

/// A capture that is open.
pub struct Capture {
    pub path: PathBuf,
    pub meta: CaptureMeta,
    /// Keeps a decoded video on disk while the capture is open, so the
    /// preview, scoring and stacking opens reuse it instead of decoding again.
    #[allow(dead_code, reason = "held only so the decoded file stays on disk")]
    pub keep_alive: Option<Arc<dyn std::any::Any + Send + Sync>>,
    /// The header as written, what is used instead and why.
    pub report: CaptureReport,
}

/// A decoded frame with its optional grid.
pub struct Cached {
    pub texture: TextureHandle,
    /// The frame as recorded, before calibration (only with masters in force).
    pub before: Option<(TextureHandle, [usize; 2])>,
    pub size: [usize; 2],
    pub grid: Option<(usize, Option<ApGrid>)>,
}

/// A stack run in flight.
pub struct Run {
    pub id: u64,
    pub cancel: Arc<AtomicBool>,
    pub stage: Stage,
    pub fraction: f32,
    /// Highest overall progress seen; stages interleave in the combined align pass.
    pub peak: f32,
    /// When the run began.
    pub started: Instant,
}

pub struct App {
    pub workers: Workers,
    pub events: Receiver<Event>,
    pub settings: Settings,
    pub capture: Option<Capture>,
    pub opening: bool,
    /// Decoding progress (0 to 1) while a compressed video opens.
    pub open_progress: Option<f32>,
    /// Stops the open in flight (kills ffmpeg).
    pub open_cancel: Option<Arc<AtomicBool>>,
    /// Why the colour plane was moved off a clipped one when the capture opened.
    pub channel_note: Option<String>,
    pub generation: u64,
    pub step: Step,
    pub index: usize,
    pub cache: HashMap<usize, Cached>,
    pub pending: Option<(usize, Option<usize>)>,
    pub shown: Option<(TextureHandle, [usize; 2])>,
    /// The shown frame before calibration, when masters are in force.
    pub shown_before: Option<(TextureHandle, [usize; 2])>,
    /// Where the calibration masters stand.
    pub cal: CalState,
    /// Number of the newest calibration asked for.
    pub cal_token: u64,
    /// Stops the calibration being prepared.
    pub cal_cancel: Option<Arc<AtomicBool>>,
    /// The Calibrate step shows the frame before and after, split.
    pub show_cal_split: bool,
    pub last_grid: Option<ApGrid>,
    pub scores: Vec<f32>,
    pub curve: Curve,
    pub scoring: Scoring,
    pub score_epoch: u64,
    pub score_cancel: Option<Arc<AtomicBool>>,
    pub run: Option<Run>,
    pub run_counter: u64,
    pub result: Option<Arc<StackOutput>>,
    /// Granulation report of `result`, for a photospheric filter.
    pub granulation: Option<helia_core::granulation::Granulation>,
    /// Sunspots found on `result`, when asked for.
    pub sunspots: Option<helia_core::sunspots::Detection>,
    /// The overlay of `sunspots` as a texture.
    pub sunspot_tex: Option<(TextureHandle, [usize; 2])>,
    /// The centre shows the sunspot overlay instead of the stack.
    pub show_sunspots: bool,
    /// The science plots of `result`, drawn when first asked for.
    pub science: Option<crate::science::ScienceView>,
    /// The centre shows a science plot instead of the stack.
    pub show_science: bool,
    /// The best frame and raw histograms of the last stack, for the plots.
    pub science_extras: Option<helia_report::Extras>,
    pub result_tex: Option<(TextureHandle, [usize; 2])>,
    pub show_result: bool,
    pub show_grid: bool,
    /// An exclusion value was edited and not yet applied.
    pub exclude_dirty: bool,
    pub view: View,
    pub gpu: Option<GpuProbe>,
    /// Long operations in progress, shown as notices.
    pub tasks: Tasks,
    /// Messages about outcomes.
    pub toasts: Toasts,
    /// Remembered choices (tips dismissed, colour per filter).
    pub prefs: Prefs,
    /// Recommended settings for the open capture.
    pub rec: Option<Recommendation>,
    /// Preview of the stack with the edits and the colour, when there are any.
    pub colour_tex: Option<(TextureHandle, [usize; 2])>,
    /// The same with only the geometry edits, for the "before" half of a split.
    pub before_tex: Option<(TextureHandle, [usize; 2])>,
    /// Size of the finished image the preview stands for.
    pub preview_size: Option<[usize; 2]>,
    /// How long the last preview took.
    pub preview_took: Option<Duration>,
    /// The stack shrunk for previews, with the exponent of the shrinking.
    pub preview_base: Option<(Arc<helia_core::Frame>, u32)>,
    /// The same for a true-colour stack.
    pub preview_base_rgb: Option<(Arc<helia_core::RgbImage>, u32)>,
    /// Blur size measured from the stack, for an automatic PSF.
    pub auto_sigma: Option<f32>,
    /// PSF sigma measured from the stack's limb, when there is a clear one.
    pub limb_sigma: Option<f32>,
    /// A deconvolution preview stopped at this iteration (the scrub), or the
    /// whole run when `None`.
    pub rl_scrub: Option<u32>,
    /// Whether the last preview's sharpening was mostly noise.
    pub sharpen_check: Option<helia_core::edit::diagnose::SharpenCheck>,
    /// The limb-darkening laws fitted to the stack shown, once each, by law.
    pub limb_fits: LimbFits,
    /// The viewer shows the stack before and after the edits, split.
    pub show_split: bool,
    /// Where the split lies across the image, 0 to 1.
    pub split_pos: f32,
    /// Why the edits cannot be previewed, in words.
    pub edit_error: Option<String>,
    /// Saved edit presets.
    pub user_presets: Vec<EditPreset>,
    /// Where they are kept; `None` in tests and when there is no place.
    pub presets_dir: Option<PathBuf>,
    /// The name typed for a preset to save.
    pub preset_name: String,
    /// Saved colour grades, the folder they live in and the name being typed.
    pub grade_presets: Vec<helia_core::grade::GradePreset>,
    pub grade_dir: Option<PathBuf>,
    pub grade_name: String,
    /// Number of the newest colour preview asked for.
    pub colour_job: u64,
    /// A colour preview is being made.
    pub colour_inflight: bool,
    /// The look changed while one was being made.
    pub colour_dirty: bool,
    /// A change was made while dragging: once the pointer is up, redo the
    /// preview at full size (the preview shrinks while a slider moves).
    pub refine_due: bool,
    /// The next preview is made at full size whatever the setting.
    pub force_full: bool,
    /// Show the less common settings (backend, demosaic, channel).
    pub show_advanced: bool,
    /// A fixed clock for tests; `None` uses the real one.
    pub frozen_now: Option<Instant>,
    pub show_rail: bool,
    pub show_inspector: bool,
    /// The About dialog is open.
    pub show_about: bool,
    /// The Plugins page is open.
    pub show_plugins: bool,
    /// The plugins found, what was approved and the page's state.
    pub plugins: crate::plugins_page::PluginsUi,
    /// The plugin steps and backend chosen for the pipeline, and what the last run reported.
    pub plugin_pipeline: crate::plugin_steps::PluginPipeline,
    /// The feedback page and its report.
    pub feedback: crate::feedback::Feedback,
    /// The About page shows the third-party licence list.
    pub show_third_party: bool,
    /// Fixed text for the version block, so snapshots do not hold a build date.
    pub version_override: Option<String>,
    /// Its text, read on first view.
    pub third_party_text: Option<String>,
    pub was_narrow: Option<bool>,
    /// Inner size of the window on the last frame it was a normal window.
    pub window_size: Option<[f32; 2]>,
    pub demo: Option<Demo>,
    /// The Time-lapse step.
    pub tl: crate::timelapse::TimelapseState,
    /// The Mosaic step.
    pub mo: crate::mosaic::MosaicState,
    /// Update checks and installs.
    pub update: crate::update_ui::UpdateUi,
    /// Optional tools: the ffmpeg offer and the About block.
    pub tools: crate::tools_ui::ToolsUi,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        open: Option<PathBuf>,
        demo: Option<Demo>,
    ) -> Self {
        let mut app = Self::with_ctx(&cc.egui_ctx, demo, true);
        app.prefs.apply_to(&mut app.settings);
        if let Some(p) = open {
            app.begin_open(p);
        }
        app
    }

    /// Build the app on `ctx`; `probe` starts the GPU adapter probe.
    pub fn with_ctx(ctx: &egui::Context, demo: Option<Demo>, probe: bool) -> Self {
        theme::apply(ctx);
        let (workers, events) = Workers::start(ctx);
        if probe {
            workers.probe_gpu();
        }
        let presets_dir = if probe { store::presets_dir() } else { None };
        let user_presets = presets_dir.as_deref().map(store::list).unwrap_or_default();
        let update = crate::update_ui::UpdateUi::new(ctx, probe && demo.is_none());
        let tools = crate::tools_ui::ToolsUi::new(ctx, probe && demo.is_none());
        Self {
            workers,
            events,
            settings: Settings::default(),
            capture: None,
            opening: false,
            open_progress: None,
            open_cancel: None,
            channel_note: None,
            generation: 0,
            step: Step::Open,
            index: 0,
            cache: HashMap::new(),
            pending: None,
            shown: None,
            shown_before: None,
            cal: CalState::default(),
            cal_token: 0,
            cal_cancel: None,
            show_cal_split: true,
            last_grid: None,
            scores: Vec::new(),
            curve: Curve::default(),
            scoring: Scoring::Idle,
            score_epoch: 0,
            score_cancel: None,
            run: None,
            run_counter: 0,
            result: None,
            granulation: None,
            sunspots: None,
            sunspot_tex: None,
            show_sunspots: false,
            science: None,
            show_science: false,
            science_extras: None,
            result_tex: None,
            show_result: false,
            show_grid: true,
            exclude_dirty: false,
            view: View::default(),
            gpu: None,
            tasks: Tasks::default(),
            toasts: Toasts::default(),
            window_size: None,
            prefs: if probe {
                Prefs::load_default()
            } else {
                Prefs::default()
            },
            rec: None,
            colour_tex: None,
            before_tex: None,
            preview_size: None,
            preview_took: None,
            preview_base: None,
            preview_base_rgb: None,
            auto_sigma: None,
            limb_sigma: None,
            rl_scrub: None,
            sharpen_check: None,
            limb_fits: LimbFits::default(),
            show_split: false,
            split_pos: 0.5,
            edit_error: None,
            user_presets,
            presets_dir,
            preset_name: String::new(),
            grade_presets: if probe {
                helia_core::grade::presets_dir()
                    .as_deref()
                    .map(helia_core::grade::list)
                    .unwrap_or_default()
            } else {
                Vec::new()
            },
            grade_dir: if probe {
                helia_core::grade::presets_dir()
            } else {
                None
            },
            grade_name: String::new(),
            colour_job: 0,
            colour_inflight: false,
            colour_dirty: false,
            refine_due: false,
            force_full: false,
            show_advanced: false,
            frozen_now: None,
            show_rail: true,
            show_inspector: true,
            show_about: false,
            show_plugins: false,
            plugins: crate::plugins_page::PluginsUi::new(
                if probe {
                    crate::plugins_page::cli_paths()
                } else {
                    Vec::new()
                },
                probe,
            ),
            plugin_pipeline: crate::plugin_steps::PluginPipeline::default(),
            feedback: crate::feedback::Feedback::default(),
            show_third_party: false,
            version_override: None,
            third_party_text: None,
            was_narrow: None,
            demo,
            tl: crate::timelapse::TimelapseState::default(),
            mo: crate::mosaic::MosaicState::default(),
            update,
            tools,
        }
    }

    // ----- opening -----

    /// The time the interface works to; fixed in tests.
    pub fn now(&self) -> Instant {
        self.frozen_now.unwrap_or_else(Instant::now)
    }

    /// Raise a toast.
    pub fn toast(&mut self, kind: ToastKind, text: impl Into<String>) {
        let now = self.now();
        let text = text.into();
        if kind == ToastKind::Error {
            applog::error(&text);
        }
        self.toasts.push(kind, text, now);
    }

    /// Show the open dialog in the folder of the last capture.
    pub fn pick_capture(&self) {
        self.workers.pick_capture(self.prefs.last_open_dir.clone());
    }

    /// Ask where to save the bug report bundle.
    pub fn save_bug_report(&mut self) {
        applog::info("saving a bug report");
        let capture = self.capture.as_ref().map(|c| c.meta.fields().join(", "));
        let report = BugReport {
            version: self.version_report(),
            os: sysinfo::os_description(),
            gpu: self.gpu_description(),
            capture,
            settings: self.prefs.report_text(),
            log_tail: applog::tail(300),
        };
        self.workers
            .save_bug_report(report, self.prefs.last_export_dir.clone());
    }

    pub fn begin_open(&mut self, path: PathBuf) {
        applog::info(&format!(
            "opening {}",
            path.file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
        ));
        self.prefs.remember_open_dir(&path);
        self.prefs.remember_recent(&path);
        if let Some(c) = &self.score_cancel {
            c.store(true, Ordering::Relaxed);
        }
        if let Some(c) = self.open_cancel.take() {
            c.store(true, Ordering::Relaxed);
        }
        // What a file gets wrong is a fact about that file: a different one
        // starts from what its own header and data say.
        if self.capture.as_ref().is_none_or(|c| c.path != path) {
            self.settings.channel_chosen = false;
            self.settings.bayer = BayerSetting::Auto;
            self.settings.bit_depth = None;
            self.settings.endian = None;
            self.settings.flip_rows = false;
            self.settings.row_order = RowOrder::Auto;
        }
        self.generation += 1;
        self.opening = true;
        self.toasts.clear();
        let now = self.now();
        self.tasks.begin(TaskKind::Opening, now, false);
        self.open_progress = None;
        self.open_cancel = Some(self.workers.open(
            self.generation,
            path,
            self.settings.open_options(),
            !self.settings.channel_chosen,
        ));
    }

    fn finish_open(&mut self, o: Opened) {
        applog::info(&format!("opened {}", o.meta.fields().join(", ")));
        self.tasks.end(TaskKind::Opening);
        self.tasks.end(TaskKind::Transcoding);
        let keep_step = self.capture.as_ref().is_some_and(|c| c.path == o.path);
        let rec = o.rec.clone();
        let (found_channel, found_note) = (o.channel, o.channel_note.clone());
        let bayer_changed = self
            .capture
            .as_ref()
            .is_some_and(|c| c.meta.bayer.is_some() != o.meta.bayer.is_some());
        let warnings: Vec<String> = o
            .report
            .notices
            .iter()
            .filter(|n| n.level == Level::Warning)
            .map(|n| n.summary.clone())
            .collect();
        self.capture = Some(Capture {
            path: o.path,
            meta: o.meta,
            keep_alive: o.keep_alive,
            report: *o.report,
        });
        if !keep_step {
            for w in warnings {
                self.toast(
                    ToastKind::Info,
                    format!("{w}. Details are in the Open step."),
                );
            }
        }
        self.opening = false;
        self.open_progress = None;
        self.open_cancel = None;
        self.index = if keep_step {
            self.index.min(self.count().saturating_sub(1))
        } else {
            0
        };
        if !keep_step {
            self.result = None;
            self.granulation = None;
            self.sunspots = None;
            self.sunspot_tex = None;
            self.show_sunspots = false;
            self.clear_science();
            self.result_tex = None;
            self.clear_preview();
            self.show_result = false;
            self.step = Step::Open;
            self.show_grid = false;
            self.view.reset();
            self.shown = None;
            self.last_grid = None;
            self.apply_recommendation(&rec);
        } else if bayer_changed {
            // An override turned the mosaic on or off: the channel follows.
            self.settings.channel = rec.channel;
        }
        // The plane the capture itself calls for: the filter's, or the next brightest
        // when that one is clipped (the app does what `helia stack` does).
        if !self.settings.channel_chosen {
            if let Some(c) = found_channel {
                self.settings.channel = c;
            }
            self.channel_note = found_note;
        }
        self.invalidate_previews();
        if self.settings.calibration.any() {
            // The masters are checked against this capture (size, mosaic,
            // exposure); scoring waits for them.
            self.cal_changed_for_capture();
        } else {
            self.start_scoring();
        }
    }

    /// Make the recommended settings the current ones: the channel, keep
    /// share and alignment point size, and the colour last used for the filter.
    pub fn apply_recommendation(&mut self, rec: &Recommendation) {
        let bayer = self
            .capture
            .as_ref()
            .is_some_and(|c| c.meta.bayer.is_some());
        self.rec = Some(rec.clone());
        self.settings.preset = rec.preset;
        self.settings.preset_source.clone_from(&rec.detected_from);
        self.settings.ap_size = rec.ap_size;
        if let Keep::Fraction(f) = rec.keep {
            self.settings.keep_percent = snap_keep(f * 100.0);
        }
        if bayer || rec.preset.is_none() {
            self.settings.channel = rec.channel;
        }
        self.settings.channel_chosen = false;
        self.channel_note = None;
        self.settings.colour = match rec.preset {
            Some(p) => self
                .prefs
                .colours
                .get(p.id())
                .copied()
                .unwrap_or_else(|| ColourParams::preset(p)),
            None => ColourParams::default(),
        };
        if let Some(c) = &self.capture {
            let (w, h) = (c.meta.width, c.meta.height);
            self.settings.edits.reset_sharpen(rec.preset, w, h);
            self.settings.edits.reset_adjust(w, h);
        }
    }

    /// The user named the filter (or said they do not know): redo the
    /// recommendation with that answer.
    pub fn choose_filter(&mut self, preset: Option<Preset>) {
        let Some(cap) = &self.capture else { return };
        let m = &cap.meta;
        let rec = recommend(m.width, m.height, m.bayer.is_some(), &[], preset);
        let old = (self.settings.channel, self.settings.keep_percent);
        self.apply_recommendation(&rec);
        if old.0 != self.settings.channel {
            self.invalidate_previews();
            self.start_scoring();
        }
        self.request_preview();
    }

    /// The Quick stack button: take the recommended settings and stack.
    pub fn quick_stack(&mut self) {
        if self.run.is_some() || self.capture.is_none() {
            return;
        }
        if let Some(rec) = self.rec.clone() {
            let old = self.settings.channel;
            self.apply_recommendation(&rec);
            if old != self.settings.channel {
                self.invalidate_previews();
            }
        }
        if let Some(c) = &self.score_cancel {
            c.store(true, Ordering::Relaxed);
        }
        self.select(Step::Stack);
        self.start_stack();
    }

    pub fn count(&self) -> usize {
        self.capture.as_ref().map_or(0, |c| c.meta.frames)
    }

    /// What the top bar and viewer say while a capture opens.
    pub fn opening_text(&self) -> String {
        match self.open_progress {
            Some(f) => format!("Decoding video with ffmpeg\u{2026} {:.0}%", f * 100.0),
            None => "Opening capture\u{2026}".to_owned(),
        }
    }

    pub fn busy(&self) -> bool {
        self.run.is_some() || self.opening
    }

    /// Apply an edited exclusion list: previews and scores depend on it.
    pub fn exclusions_changed(&mut self) {
        self.exclude_dirty = false;
        self.invalidate_previews();
        self.start_scoring();
    }

    pub fn invalidate_previews(&mut self) {
        self.cache.clear();
        self.pending = None;
    }

    // ----- scoring and stacking -----

    /// The configuration for scoring and stacking: the settings, and the
    /// calibration masters when they are ready.
    pub fn stack_config(&self) -> StackConfig {
        StackConfig {
            calibration: self.cal_ready(),
            field_rotation: self.field_rotation(),
            ..self.settings.stack_config()
        }
    }

    /// The field rotation correction the settings ask for, when the site is
    /// given and the capture has timestamps.
    #[must_use]
    pub fn field_rotation(&self) -> Option<helia_core::field_rotation::FieldRotation> {
        let s = &self.settings;
        if !(s.field_rotation && s.site_set) {
            return None;
        }
        let cap = self.capture.as_ref()?;
        let info = helia_core::io::solarnet::CaptureInfo::read(&cap.path, s.preset);
        info.frame_unix.iter().any(Option::is_some).then(|| {
            helia_core::field_rotation::FieldRotation {
                site: helia_core::io::solarnet::Site {
                    lat_deg: s.site[0],
                    lon_deg: s.site[1],
                    alt_m: s.site[2],
                },
                mirror: s.field_rotation_mirror,
                reference: helia_core::field_rotation::Reference::Middle,
                target: None,
                auto_mirror: false,
                frame_unix: info.frame_unix,
            }
        })
    }

    pub fn start_scoring(&mut self) {
        let Some(cap) = &self.capture else { return };
        if matches!(self.cal, CalState::Preparing) {
            // Scoring starts when the masters are ready.
            return;
        }
        if let Some(c) = &self.score_cancel {
            c.store(true, Ordering::Relaxed);
        }
        self.score_epoch += 1;
        self.scoring = Scoring::Running;
        let now = self.now();
        self.tasks.begin(TaskKind::Scoring, now, true);
        self.score_cancel = Some(self.workers.score(
            self.score_epoch,
            cap.path.clone(),
            self.settings.open_options(),
            self.stack_config(),
            self.settings.backend,
        ));
    }

    pub fn start_stack(&mut self) {
        let Some(cap) = &self.capture else { return };
        if self.run.is_some() {
            return;
        }
        if self.settings.field_rotation && self.field_rotation().is_none() {
            self.toast(
                ToastKind::Error,
                "Field rotation is on, but it needs the observing site and a capture with timestamps (a SER file). Turn it off or enter the site.",
            );
            return;
        }
        let plan = match self.plugin_plan() {
            Ok(plan) => plan,
            Err(e) => {
                self.toast(ToastKind::Error, e);
                return;
            }
        };
        self.plugin_pipeline.results.clear();
        self.plugin_pipeline.provenance.clear();
        self.plugin_pipeline.cards.clear();
        self.plugin_pipeline.status = None;
        self.run_counter += 1;
        let cancel = self.workers.stack(
            self.run_counter,
            cap.path.clone(),
            self.settings.open_options(),
            self.stack_config(),
            self.settings.backend,
            plan,
        );
        let now = self.now();
        self.run = Some(Run {
            id: self.run_counter,
            cancel,
            stage: Stage::Quality,
            fraction: 0.0,
            peak: 0.0,
            started: now,
        });
        self.tasks.begin(TaskKind::Stack(Stage::Quality), now, true);
        self.tasks.end(TaskKind::Scoring);
        self.toasts.clear();
    }

    pub fn cancel_stack(&mut self) {
        if let Some(r) = &self.run {
            r.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Stop the operation a notice's Cancel button belongs to.
    pub fn cancel_task(&mut self, kind: TaskKind) {
        match kind {
            TaskKind::Stack(_) => self.cancel_stack(),
            TaskKind::Scoring => {
                if let Some(c) = &self.score_cancel {
                    c.store(true, Ordering::Relaxed);
                }
            }
            TaskKind::FeedbackPreparing | TaskKind::FeedbackSending => self.cancel_feedback(),
            _ => {}
        }
    }

    pub fn kept(&self) -> usize {
        keep_count(self.settings.keep_percent, self.count())
    }

    pub fn export(&mut self) {
        if let (Some(out), Some(cap)) = (&self.result, &self.capture) {
            let stem = std::path::Path::new(&cap.meta.file)
                .file_stem()
                .map_or_else(
                    || "helia".into(),
                    |s| format!("{}-helia", s.to_string_lossy()),
                );
            let mut plan = self.settings.output_plan(self.auto_sigma);
            // The plugins that made this stack are named in the file's history.
            plan.lead_notes
                .extend(self.plugin_pipeline.provenance.iter().cloned());
            if let Some(c) = &out.colour {
                plan.rgb = Some(c.rgb.clone());
                plan.lead_notes.extend(c.notes.iter().cloned());
            }
            self.workers.export(
                Arc::clone(out),
                self.settings.export_format,
                stem,
                plan,
                self.settings.backend,
                crate::worker::ExportOrigin {
                    start_dir: self.prefs.last_export_dir.clone(),
                    capture: cap.path.clone(),
                    sunspots: self.sunspots.as_ref().map(|d| d.report.clone()),
                    filter: self.settings.preset,
                    notes: cap
                        .report
                        .note_lines()
                        .into_iter()
                        .chain(out.calibration_notes.iter().cloned())
                        .collect(),
                    plugin_cards: self.plugin_pipeline.cards.clone(),
                },
            );
        }
    }

    /// Name shown for the chosen colour look.
    pub fn colour_name(&self) -> String {
        if self.result.as_ref().is_some_and(|r| r.colour.is_some()) {
            return "True colour".to_string();
        }
        self.settings
            .colour
            .preset
            .map_or_else(|| "Mono".to_string(), |p| p.name().to_string())
    }

    /// The look changed: remember it for the filter and redo the preview.
    pub fn colour_changed(&mut self) {
        if let Some(f) = self.settings.preset {
            let c = self.settings.colour;
            self.prefs.remember_colour(f, c);
        }
        self.request_preview();
    }

    /// A sharpening or adjustment setting changed: redo the preview.
    pub fn edits_changed(&mut self) {
        self.refine_due = true;
        self.request_preview();
    }

    /// Once the pointer is up after a drag, redo a shrunk preview at full
    /// size, so what is looked at at 100% is the true result.
    fn refine_preview(&mut self, ctx: &egui::Context) {
        if !self.refine_due || self.colour_inflight || ctx.input(|i| i.pointer.any_down()) {
            return;
        }
        self.refine_due = false;
        let shrunk = self.preview_base.as_ref().is_some_and(|(_, k)| *k > 0)
            || self.preview_base_rgb.as_ref().is_some_and(|(_, k)| *k > 0);
        if shrunk && !self.settings.full_preview {
            self.force_full = true;
            self.request_preview();
            self.force_full = false;
        }
    }

    /// Forget the preview (a new capture, or a new stack).
    pub fn clear_preview(&mut self) {
        self.colour_tex = None;
        self.before_tex = None;
        self.preview_size = None;
        self.preview_took = None;
        self.preview_base = None;
        self.preview_base_rgb = None;
        self.auto_sigma = None;
        self.limb_sigma = None;
        self.sharpen_check = None;
        self.edit_error = None;
    }

    /// The edit list as it stands, with the automatic PSF measured.
    pub fn edit_list(&self) -> EditList {
        self.settings.edits.list(self.auto_sigma)
    }

    /// The edit list for the preview: a PSF measured from the limb is a
    /// number here (the preview is shrunk, so it cannot measure it), and a
    /// scrubbed deconvolution stops at the chosen iteration (none at 0).
    pub(crate) fn preview_list(&self) -> EditList {
        let mut list = self.edit_list();
        let sigma = self.limb_sigma.or(self.auto_sigma);
        list.ops.retain_mut(|op| {
            if let helia_core::Op::Deconvolve(d) = op {
                if d.psf == helia_core::edit::Psf::Limb
                    && let Some(s) = sigma
                {
                    d.psf = helia_core::edit::Psf::Gaussian {
                        sigma: s.clamp(0.3, 8.0),
                    };
                }
                if let Some(k) = self.rl_scrub.filter(|&k| k < d.iterations) {
                    if k == 0 {
                        return false;
                    }
                    d.iterations = k;
                }
            }
            true
        });
        list
    }

    /// Make the preview of the stack with the edits and the colour, off the UI
    /// thread. One at a time: a change while one runs is picked up when it
    /// ends, so dragging a slider never queues a pile of work.
    pub fn request_preview(&mut self) {
        let Some(out) = self.result.clone() else {
            return;
        };
        let list = self.preview_list();
        let c = self.settings.colour;
        let true_colour = out.colour.is_some();
        if list.is_empty() && (true_colour || (c.preset.is_none() && !c.invert)) {
            self.clear_preview_images();
            return;
        }
        let (w, h) = (out.image.width, out.image.height);
        let logical = match list.validate(w, h) {
            Ok(d) => d,
            Err(e) => {
                self.edit_error = Some(e.to_string());
                return;
            }
        };
        self.edit_error = None;
        if self.colour_inflight {
            self.colour_dirty = true;
            return;
        }
        self.colour_inflight = true;
        self.colour_dirty = false;
        self.colour_job += 1;
        let full = self.settings.full_preview || self.force_full;
        let (base, shift) = if true_colour {
            match (&self.preview_base_rgb, full) {
                (Some((small, k)), false) if *k > 0 => {
                    (PreviewBase::SmallRgb(Arc::clone(small)), *k)
                }
                _ => (PreviewBase::FullRgb(out), 0),
            }
        } else {
            match (&self.preview_base, full) {
                (Some((small, k)), false) if *k > 0 => (PreviewBase::Small(Arc::clone(small)), *k),
                _ => (PreviewBase::Full(out), 0),
            }
        };
        let before = (self.show_split && !list.is_empty())
            .then(|| edits::geometry_only(&list).at_scale(shift));
        let now = self.now();
        let kind = if shift == 0 && (w.max(h) > edits::PREVIEW_SIDE) {
            TaskKind::PreviewingFull
        } else {
            TaskKind::Previewing
        };
        self.tasks.begin(kind, now, false);
        self.workers.render(RenderRequest {
            job: self.colour_job,
            base,
            edits: list.at_scale(shift),
            before,
            colour: c,
            backend: self.settings.backend,
            logical: [logical.0, logical.1],
        });
    }

    fn clear_preview_images(&mut self) {
        self.colour_tex = None;
        self.before_tex = None;
        self.preview_size = None;
        self.preview_took = None;
        self.sharpen_check = None;
        self.edit_error = None;
    }

    /// Save the current edits under `name` among the user's presets.
    pub fn save_edit_preset(&mut self, name: &str) {
        let Some(dir) = self.presets_dir.clone() else {
            self.toast(
                ToastKind::Error,
                "There is no place to keep presets. Set HELIA_EDIT_PRESETS to a folder.",
            );
            return;
        };
        let preset = EditPreset {
            name: name.trim().to_string(),
            filter: self.settings.preset.map(|p| p.id().to_string()),
            edits: self.edit_list(),
        };
        match store::save(&dir, &preset) {
            Ok(_) => {
                self.user_presets = store::list(&dir);
                self.preset_name.clear();
                self.toast(ToastKind::Success, format!("Saved preset {}", preset.name));
            }
            Err(e) => self.toast(ToastKind::Error, format!("Could not save the preset: {e}")),
        }
    }

    /// Use a saved preset: its edits replace the current ones.
    pub fn load_edit_preset(&mut self, preset: &EditPreset) {
        let (s, dropped) = EditSettings::from_list(&preset.edits, &self.settings.edits);
        let keep_fits = self.settings.edits.edit_fits;
        self.settings.edits = s;
        self.settings.edits.edit_fits = keep_fits;
        if dropped > 0 {
            self.toast(
                ToastKind::Info,
                format!(
                    "{dropped} operation{} of {} could not be shown here and {} left out. The command line runs the whole list.",
                    if dropped == 1 { "" } else { "s" },
                    preset.name,
                    if dropped == 1 { "was" } else { "were" }
                ),
            );
        }
        self.edits_changed();
    }

    /// Save the current colour grade under `name`.
    pub fn save_grade_preset(&mut self, name: &str) {
        let Some(dir) = self.grade_dir.clone() else {
            self.toast(
                ToastKind::Error,
                "There is no place to keep colour presets. Set HELIA_COLOUR_PRESETS to a folder.",
            );
            return;
        };
        let preset = helia_core::grade::GradePreset {
            name: name.trim().to_string(),
            filter: self.settings.preset.map(|p| p.id().to_string()),
            grade: self.settings.colour.grade,
        };
        match helia_core::grade::save(&dir, &preset) {
            Ok(_) => {
                self.grade_presets = helia_core::grade::list(&dir);
                self.grade_name.clear();
                self.toast(
                    ToastKind::Success,
                    format!("Saved colour preset {}", preset.name),
                );
            }
            Err(e) => self.toast(
                ToastKind::Error,
                format!("Could not save the colour preset: {e}"),
            ),
        }
    }

    /// Delete a saved colour preset.
    pub fn delete_grade_preset(&mut self, name: &str) {
        if let Some(dir) = self.grade_dir.clone() {
            match helia_core::grade::remove(&dir, name) {
                Ok(()) => self.grade_presets = helia_core::grade::list(&dir),
                Err(e) => self.toast(ToastKind::Error, format!("Could not delete: {e}")),
            }
        }
    }

    /// Delete a saved preset.
    pub fn delete_edit_preset(&mut self, name: &str) {
        if let Some(dir) = self.presets_dir.clone() {
            match store::remove(&dir, name) {
                Ok(()) => self.user_presets = store::list(&dir),
                Err(e) => self.toast(ToastKind::Error, format!("Could not delete: {e}")),
            }
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            container: self.capture.as_ref().map(|c| c.meta.container.clone()),
            frames: self.count(),
            scoring: self.scoring,
            running: self.run.as_ref().map(|r| r.stage),
            has_result: self.result.is_some(),
            ap_count: self.display_grid().map(ApGrid::active_count),
            colour_name: self.colour_name(),
            sharpen_name: self.settings.edits.sharpen_status(),
            adjust_name: self.settings.edits.adjust_status(),
            calibration: self.cal_brief(),
            timelapse: self.tl_status(),
            timelapse_hint: self.tl_hint().to_string(),
            mosaic: self.mo_status(),
            mosaic_hint: self.mo_hint().to_string(),
        }
    }

    // ----- previews -----

    /// Window size of the grid drawn on the frame: the size chosen by the last
    /// automatic run when that is on, else the one set by hand.
    pub fn preview_ap_size(&self) -> usize {
        self.result
            .as_ref()
            .and_then(|r| r.auto_ap.as_ref())
            .filter(|_| self.settings.ap_auto)
            .map_or(self.settings.ap_size, |c| c.size)
    }

    /// Grid to draw for the current frame.
    pub fn display_grid(&self) -> Option<&ApGrid> {
        let size = self.preview_ap_size();
        if let Some((s, Some(g))) = self.cache.get(&self.index).and_then(|c| c.grid.as_ref())
            && *s == size
        {
            return Some(g);
        }
        self.last_grid.as_ref().filter(|g| g.size == size)
    }

    fn ensure_preview(&mut self) {
        let Some(cap) = &self.capture else { return };
        if self.index >= cap.meta.frames {
            return;
        }
        let want_grid = self.show_grid.then_some(self.preview_ap_size());
        if let Some(c) = self.cache.get(&self.index) {
            let grid_ok = want_grid.is_none_or(|s| c.grid.as_ref().is_some_and(|(gs, _)| *gs == s));
            if grid_ok {
                self.shown = Some((c.texture.clone(), c.size));
                self.shown_before.clone_from(&c.before);
                return;
            }
        }
        if self.pending == Some((self.index, want_grid)) {
            return;
        }
        self.pending = Some((self.index, want_grid));
        if self.tasks.get(TaskKind::Decoding).is_none() {
            let now = self.now();
            self.tasks.begin(TaskKind::Decoding, now, false);
        }
        self.workers.request_preview(PreviewRequest {
            generation: self.generation,
            path: cap.path.clone(),
            opts: self.settings.open_options(),
            debayer: self.settings.debayer(),
            index: self.index,
            grid: want_grid,
            exclude: self.settings.exclude_rects(),
            calibration: self.cal_ready(),
        });
    }

    fn on_preview(&mut self, p: Preview) {
        self.tasks.end(TaskKind::Decoding);
        if p.generation != self.generation {
            return;
        }
        if self.pending.is_some_and(|(i, _)| i == p.index) {
            self.pending = None;
        }
        match p.texture {
            Ok((texture, size)) => {
                if let Some((_, Some(g))) = &p.grid {
                    self.last_grid = Some(g.clone());
                }
                let old_grid = self.cache.remove(&p.index).and_then(|c| c.grid);
                self.cache.insert(
                    p.index,
                    Cached {
                        texture,
                        before: p.before,
                        size,
                        grid: p.grid.or(old_grid),
                    },
                );
                self.evict();
            }
            Err(e) => self.toast(ToastKind::Error, e),
        }
    }

    fn evict(&mut self) {
        while self.cache.len() > CACHE_FRAMES {
            let far = self
                .cache
                .keys()
                .copied()
                .max_by_key(|&i| i.abs_diff(self.index));
            match far {
                Some(i) => self.cache.remove(&i),
                None => break,
            };
        }
    }

    // ----- events -----

    pub(crate) fn pump(&mut self, ctx: &egui::Context) {
        while let Ok(ev) = self.events.try_recv() {
            match ev {
                Event::Picked(Some(p)) => self.begin_open(p),
                Event::Picked(None) => {}
                Event::Opened(g, res) if g == self.generation => match res {
                    Ok(o) => self.finish_open(o),
                    Err(e) => {
                        self.opening = false;
                        self.tasks.end(TaskKind::Opening);
                        self.tasks.end(TaskKind::Transcoding);
                        self.open_progress = None;
                        self.open_cancel = None;
                        if e.contains("ffmpeg was not found") {
                            self.offer_ffmpeg_for_video();
                        } else {
                            self.toast(ToastKind::Error, format!("Could not open file: {e}"));
                        }
                    }
                },
                Event::Opened(..) => {}
                Event::OpenProgress(g, f) if g == self.generation => {
                    self.open_progress = Some(f);
                    let now = self.now();
                    self.tasks.update(TaskKind::Transcoding, Some(f), now);
                }
                Event::OpenProgress(..) => {}
                Event::Preview(p) => self.on_preview(p),
                Event::CalPicked(slot, Some(path)) => {
                    self.settings.calibration.set(slot, Some(path));
                    self.cal_changed();
                }
                Event::CalPicked(_, None) => {}
                Event::CalReady(token, res) => self.on_cal_ready(token, res),
                Event::Scores(epoch, res) if epoch == self.score_epoch => {
                    self.tasks.end(TaskKind::Scoring);
                    match res {
                        Ok(s) => {
                            self.curve = Curve::new(&s);
                            self.scores = s;
                            self.scoring = Scoring::Done;
                        }
                        Err(e) => {
                            self.scoring = Scoring::Idle;
                            if !e.contains("cancel") {
                                self.toast(ToastKind::Error, format!("Scoring failed: {e}"));
                            }
                        }
                    }
                }
                Event::Scores(..) => {}
                Event::ScoreProgress(epoch, f) if epoch == self.score_epoch => {
                    let now = self.now();
                    self.tasks.update(TaskKind::Scoring, Some(f), now);
                }
                Event::ScoreProgress(..) => {}
                Event::Working(kind, f) => {
                    let now = self.now();
                    self.tasks.update(kind, f, now);
                }
                Event::Finished(kind) => self.tasks.end(kind),
                Event::Progress(id, stage, f) => {
                    if let Some(r) = self.run.as_mut().filter(|r| r.id == id) {
                        self.plugin_pipeline.status = None;
                        r.stage = stage;
                        r.fraction = f;
                        r.peak = r.peak.max(crate::model::overall_progress(stage, f));
                        let peak = r.peak;
                        let now = self.now();
                        self.tasks.update(TaskKind::Stack(stage), Some(peak), now);
                    }
                }
                Event::Done(id, res) if self.run.as_ref().is_some_and(|r| r.id == id) => {
                    let started = self.run.as_ref().map(|r| r.started);
                    self.run = None;
                    self.tasks.end(TaskKind::Stack(Stage::Finish));
                    match res {
                        Ok(out) => {
                            let took = started.map_or(Duration::ZERO, |t| {
                                self.now().saturating_duration_since(t)
                            });
                            self.on_result(ctx, *out, took);
                        }
                        Err(e) if e.contains("cancel") => {
                            self.toast(ToastKind::Info, "Stacking cancelled.");
                        }
                        Err(e) => self.toast(ToastKind::Error, format!("Stacking failed: {e}")),
                    }
                }
                Event::Done(..) => {}
                Event::Timelapse(e) => self.on_timelapse(ctx, e),
                Event::Mosaic(e) => self.on_mosaic(ctx, e),
                Event::Coloured(job, res) => {
                    if job == self.colour_job {
                        self.colour_inflight = false;
                        self.tasks.end(TaskKind::Previewing);
                        self.tasks.end(TaskKind::PreviewingFull);
                        match res {
                            Ok(c) => {
                                self.colour_tex = Some(c.texture);
                                self.before_tex = c.before;
                                self.preview_size = Some(c.logical);
                                self.preview_took = Some(c.took);
                                self.sharpen_check = c.check;
                            }
                            Err(e) => {
                                self.toast(
                                    ToastKind::Error,
                                    format!("Could not update the preview: {e}"),
                                );
                            }
                        }
                        if self.colour_dirty {
                            self.request_preview();
                        }
                    }
                }
                Event::Gpu(p) => self.gpu = Some(p),
                Event::Exporting => {
                    let now = self.now();
                    self.tasks.begin(TaskKind::Exporting, now, false);
                }
                Event::Saved(None) => self.tasks.end(TaskKind::Exporting),
                Event::Saved(Some(Ok(p))) => {
                    self.tasks.end(TaskKind::Exporting);
                    self.prefs.remember_export_dir(&p);
                    applog::info(&format!(
                        "exported {}",
                        p.file_name()
                            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
                    ));
                    self.toast(ToastKind::Success, format!("Saved {}", p.display()));
                }
                Event::SunspotsSaved(None) => {}
                Event::SunspotsSaved(Some(Ok(p))) => {
                    self.prefs.remember_export_dir(&p);
                    self.toast(
                        ToastKind::Success,
                        format!("Saved the sunspot table and overlay beside {}", p.display()),
                    );
                }
                Event::SunspotsSaved(Some(Err(e))) => {
                    self.toast(
                        ToastKind::Error,
                        format!("Could not save the sunspots: {e}"),
                    );
                }
                Event::ScienceExtras(id, extras)
                    if self.run.as_ref().is_some_and(|r| r.id == id) =>
                {
                    self.science_extras = Some(*extras);
                }
                Event::ScienceExtras(..) => {}
                Event::Status(id, text) if self.run.as_ref().is_some_and(|r| r.id == id) => {
                    self.plugin_pipeline.status = Some(text);
                }
                Event::Status(..) => {}
                Event::PluginResults(id, results, provenance, cards)
                    if self.run.as_ref().is_some_and(|r| r.id == id) =>
                {
                    self.plugin_pipeline.draw = results.iter().any(|(_, a)| !a.overlays.is_empty());
                    self.plugin_pipeline.results = results;
                    self.plugin_pipeline.provenance = provenance;
                    self.plugin_pipeline.cards = cards;
                }
                Event::PluginResults(..) => {}
                Event::ReportSaved(None) => {}
                Event::ReportSaved(Some(Ok(p))) => {
                    self.prefs.remember_export_dir(&p);
                    self.toast(
                        ToastKind::Success,
                        format!("Saved the science report in {}", p.display()),
                    );
                }
                Event::ReportSaved(Some(Err(e))) => {
                    self.toast(
                        ToastKind::Error,
                        format!("Could not save the science report: {e}"),
                    );
                }
                Event::LutSaved(None) => {}
                Event::LutSaved(Some(Ok(p))) => {
                    self.prefs.remember_export_dir(&p);
                    self.toast(
                        ToastKind::Success,
                        format!("Saved the colour table {}", p.display()),
                    );
                }
                Event::LutSaved(Some(Err(e))) => {
                    self.toast(
                        ToastKind::Error,
                        format!("Could not save the colour table: {e}"),
                    );
                }
                Event::BugReport(None) => {}
                Event::BugReport(Some(Ok(p))) => {
                    self.prefs.remember_export_dir(&p);
                    self.toast(
                        ToastKind::Success,
                        format!(
                            "Saved bug report {}. Read it, then attach it to an issue.",
                            p.display()
                        ),
                    );
                }
                Event::BugReport(Some(Err(e))) => {
                    self.toast(
                        ToastKind::Error,
                        format!("Could not save the bug report: {e}"),
                    );
                }
                Event::Saved(Some(Err(e))) => {
                    self.tasks.end(TaskKind::Exporting);
                    self.toast(ToastKind::Error, format!("Could not save: {e}"));
                }
            }
        }
    }

    fn on_result(&mut self, ctx: &egui::Context, out: StackOutput, took: Duration) {
        let tex = match &out.colour {
            Some(c) => worker::rgb_texture(ctx, &format!("result-{}", self.run_counter), &c.rgb),
            None => worker::texture(ctx, &format!("result-{}", self.run_counter), &out.image),
        };
        // A stack made by a plugin carries no scores: keep the ones from scoring here.
        if !out.scores.is_empty() {
            self.curve = Curve::new(&out.scores);
            self.scores.clone_from(&out.scores);
        }
        self.scoring = Scoring::Done;
        let message = if out.scores.is_empty() {
            format!("Stacked by {} in {}", out.backend, activity::duration(took))
        } else {
            activity::stacked_message(out.kept.len(), out.scores.len(), took)
        };
        self.toast(ToastKind::Success, message);
        self.result_tex = Some(tex);
        self.clear_preview();
        let sigma = out.disc.map_or_else(
            || deconv::heuristic_sigma(out.image.width, out.image.height),
            |d| deconv::sigma_for_disc_radius(d.r),
        );
        self.auto_sigma = Some(sigma);
        self.limb_sigma = helia_core::edit::limbpsf::measure(&out.image).map(|m| m.sigma);
        let shift = edits::preview_shift(out.image.width, out.image.height);
        self.preview_base = Some((Arc::new(edits::shrink(&out.image, shift)), shift));
        self.granulation = helia_core::granulation::for_stack(
            &out.image,
            self.settings.preset,
            out.disc.map(|d| d.r),
            None,
        );
        self.sunspots = None;
        self.sunspot_tex = None;
        self.show_sunspots = false;
        self.clear_science();
        if self.settings.sunspots {
            let info = helia_core::io::solarnet::CaptureInfo::read(
                self.capture
                    .as_ref()
                    .map_or(std::path::Path::new(""), |c| c.path.as_path()),
                self.settings.preset,
            );
            let when = helia_core::io::solarnet::FitsMeta::from_stack(&info, &out, None)
                .date_avg
                .or_else(|| info.mid_unix());
            let params = helia_core::sunspots::Params {
                k: self.settings.wolf_k,
                celestial_north_up: self.settings.celestial_north_up,
                ..helia_core::sunspots::Params::default()
            };
            match helia_core::sunspots::detect(&out.image, when, &params) {
                Some(d) => {
                    self.sunspot_tex = Some(worker::rgb_texture(
                        ctx,
                        &format!("sunspots-{}", self.run_counter),
                        &d.overlay,
                    ));
                    self.sunspots = Some(d);
                }
                None => self.toast(
                    ToastKind::Info,
                    "No clear solar disc, so no sunspots to find",
                ),
            }
        }
        self.preview_base_rgb = out
            .colour
            .as_ref()
            .map(|c| (Arc::new(edits::shrink_rgb(&c.rgb, shift)), shift));
        self.result = Some(Arc::new(out));
        self.show_result = true;
        self.view.reset();
        // The next things to do are to look at the sharpening, pick a colour
        // and export.
        self.select(Step::Sharpen);
        self.request_preview();
    }

    // ----- input -----

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let focused = ctx.memory(|m| m.focused().is_some());
        let (open, export, space, left, right, home, end, shift) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::COMMAND, Key::O),
                i.consume_key(Modifiers::COMMAND, Key::E),
                !focused && i.consume_key(Modifiers::NONE, Key::Space),
                !focused && i.consume_key(Modifiers::NONE, Key::ArrowLeft),
                !focused && i.consume_key(Modifiers::NONE, Key::ArrowRight),
                !focused && i.consume_key(Modifiers::NONE, Key::Home),
                !focused && i.consume_key(Modifiers::NONE, Key::End),
                i.modifiers.shift,
            )
        });
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F1)) {
            self.show_about = !self.show_about;
            self.feedback.open = false;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::F)) {
            self.begin_feedback(ctx);
        }
        if self.feedback.open
            && !focused
            && !matches!(self.feedback.stage, crate::feedback::Stage::Sending(_))
            && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
        {
            self.close_feedback();
        }
        if self.show_about
            && !focused
            && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
        {
            self.show_about = false;
        }
        if self.show_plugins
            && !focused
            && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
        {
            self.show_plugins = false;
        }
        if self.tools.open
            && !self.tools.installing()
            && !focused
            && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
        {
            self.tools.close();
        }
        let big = if shift { 10 } else { 1 };
        if open && !self.busy() {
            self.pick_capture();
        }
        if export && self.result.is_some() {
            self.export();
        }
        if space && self.result.is_some() && self.step != Step::Timelapse {
            self.show_result = !self.show_result;
        }
        if self.step == Step::Timelapse {
            self.tl_keys(
                crate::timelapse::Keys {
                    left,
                    right,
                    home,
                    end,
                    space,
                },
                big,
            );
        }
        let n = self.count();
        if n > 0 && self.step != Step::Timelapse {
            if left {
                self.index = self.index.saturating_sub(big);
            }
            if right {
                self.index = (self.index + big).min(n - 1);
            }
            if home {
                self.index = 0;
            }
            if end {
                self.index = n - 1;
            }
        }
        let dropped = ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()));
        if let Some(p) = dropped
            && !self.busy()
        {
            self.begin_open(p);
        }
    }

    fn responsive(&mut self, ctx: &egui::Context) {
        let narrow = ctx.content_rect().width() < NARROW;
        if self.was_narrow != Some(narrow) {
            self.show_rail = !narrow;
            self.show_inspector = !narrow;
            self.was_narrow = Some(narrow);
        }
    }

    /// Toggle a side panel; in narrow windows only one is open at a time.
    pub fn toggle_panel(&mut self, rail: bool) {
        let narrow = self.was_narrow == Some(true);
        if rail {
            self.show_rail = !self.show_rail;
            if narrow && self.show_rail {
                self.show_inspector = false;
            }
        } else {
            self.show_inspector = !self.show_inspector;
            if narrow && self.show_inspector {
                self.show_rail = false;
            }
        }
    }

    /// Select a step; the viewer follows where it helps.
    pub fn select(&mut self, step: Step) {
        self.step = step;
        match step {
            Step::Align => self.show_grid = true,
            Step::Sharpen | Step::Adjust | Step::Colour | Step::Export if self.result.is_some() => {
                self.show_result = true;
            }
            _ => {}
        }
    }
}

impl eframe::App for App {
    fn on_exit(&mut self) {
        self.update.on_exit();
        if let Some(size) = self.window_size {
            self.prefs.remember_window(size);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // Not while maximised, full screen or minimised: those sizes are not the user's choice.
        ctx.input(|i| {
            let v = i.viewport();
            let normal = v.maximized != Some(true)
                && v.fullscreen != Some(true)
                && v.minimized != Some(true);
            if normal && let Some(r) = v.inner_rect {
                self.window_size = Some([r.width(), r.height()]);
            }
        });
        self.pump(&ctx);
        self.plugins.pump();
        self.feedback_pump(&ctx);
        self.update_poll();
        self.tools_poll();
        self.prefs.remember_settings(&self.settings);
        let now = self.now();
        self.toasts.expire(now);
        if self.tasks.any() || self.toasts.animating(now) {
            // Keeps the time left and the fades moving.
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        self.responsive(&ctx);
        self.shortcuts(&ctx);
        self.ensure_preview();
        self.tl_sync(&ctx);
        self.refine_preview(&ctx);
        if let Some(mut d) = self.demo.take() {
            d.drive(self, &ctx);
            self.demo = Some(d);
        }
        self.topbar(ui);
        // The Plugins page is wide and has its own list: it takes the whole window.
        if self.show_rail && !self.show_plugins {
            self.rail(ui);
        }
        if self.show_inspector && !self.show_plugins {
            self.inspector(ui);
        }
        self.centre(ui);
        self.toasts_overlay(&ctx);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use std::time::Duration;

    use helia_core::testutil::{
        CaptureConfig, SyntheticCapture, SyntheticConfig, SyntheticSun, ser_bytes,
    };

    use super::*;

    fn app() -> (App, egui::Context) {
        let ctx = egui::Context::default();
        let mut app = App::with_ctx(&ctx, None, false);
        app.settings.backend = BackendChoice::Cpu;
        (app, ctx)
    }

    fn write_capture(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("helia-gui-app-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sun = SyntheticSun::new(&SyntheticConfig {
            width: 96,
            height: 80,
            ..SyntheticConfig::default()
        });
        let cap = SyntheticCapture::generate(
            &sun,
            &CaptureConfig {
                frames: 20,
                noise: 0.02,
                ..CaptureConfig::default()
            },
        );
        let path = dir.join(name);
        std::fs::write(&path, ser_bytes(&cap.frames)).unwrap();
        path
    }

    /// Pump worker events until `done` holds, or fail after a generous wait.
    fn until(app: &mut App, ctx: &egui::Context, what: &str, done: impl Fn(&App) -> bool) {
        let t0 = std::time::Instant::now();
        while !done(app) {
            app.pump(ctx);
            assert!(
                t0.elapsed() < Duration::from_secs(60),
                "timed out waiting for {what}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn opening_shows_a_notice_and_the_filter_is_detected() {
        let (mut app, ctx) = app();
        let path = write_capture("sun_Ha_001.ser");
        app.begin_open(path);
        let now = app.now();
        assert!(app.tasks.get(TaskKind::Opening).is_some(), "notice at once");
        assert!(
            app.tasks.visible(now).is_empty(),
            "but not before the delay"
        );
        assert_eq!(app.tasks.visible(now + Duration::from_millis(200)).len(), 1);
        until(&mut app, &ctx, "the capture to open", |a| {
            a.capture.is_some()
        });
        assert!(app.tasks.get(TaskKind::Opening).is_none(), "notice cleared");
        assert_eq!(app.settings.preset, Some(Preset::Ha));
        assert_eq!(app.settings.preset_source.as_deref(), Some("sun_Ha_001"));
        assert_eq!(app.step, Step::Open, "the Quick stack card is shown");
        assert_eq!(app.settings.colour.preset, Some(Preset::Ha));
        // Scoring starts at once and is announced, then clears.
        assert_eq!(app.scoring, Scoring::Running);
        until(&mut app, &ctx, "scoring", |a| a.scoring == Scoring::Done);
        assert!(app.tasks.get(TaskKind::Scoring).is_none());
    }

    /// Captures of one disc, the second moved by `dx` pixels.
    fn write_moving_pair() -> Vec<PathBuf> {
        // Tests run in parallel and share the process id: one folder for each call.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("helia-gui-tl-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sun = SyntheticSun::new(&SyntheticConfig {
            width: 128,
            height: 112,
            radius_frac: 0.36,
            ..SyntheticConfig::default()
        });
        let cap = SyntheticCapture::generate(
            &sun,
            &CaptureConfig {
                frames: 14,
                noise: 0.02,
                ..CaptureConfig::default()
            },
        );
        [("10_00_00.ser", 0.0f32), ("10_05_00.ser", 3.5)]
            .iter()
            .map(|&(name, dx)| {
                let frames: Vec<_> = cap
                    .frames
                    .iter()
                    .map(|f| {
                        helia_core::resample::shifted(
                            f,
                            helia_core::Shift::new(-dx, 0.0),
                            helia_core::Kernel::Bilinear,
                        )
                    })
                    .collect();
                let path = dir.join(name);
                std::fs::write(&path, ser_bytes(&frames)).unwrap();
                path
            })
            .collect()
    }

    #[test]
    fn a_time_lapse_stacks_registers_and_can_be_scrubbed() {
        let (mut app, ctx) = app();
        let caps = write_moving_pair();
        app.select(Step::Timelapse);
        app.tl_add(caps.clone());
        app.tl_add(caps.clone());
        assert_eq!(app.tl.captures.len(), 2, "a repeat is not added twice");
        assert_eq!(app.tl_hint(), "Press Make time-lapse");
        app.tl.date = "2026-10-20".into();
        app.settings.keep_percent = 50.0;
        app.tl.crop = true;
        app.tl_start();
        assert!(app.tl.running.is_some());
        until(&mut app, &ctx, "the time-lapse", |a| a.tl.running.is_none());
        assert_eq!(app.tl.error, None);
        let built = app.tl.built.clone().expect("a series");
        assert_eq!(built.sequence.frames.len(), 2);
        assert_eq!(built.sources, ["10_00_00.ser", "10_05_00.ser"]);
        let moved =
            built.sequence.report[0].shift.dx.abs() + built.sequence.report[1].shift.dx.abs();
        assert!((moved - 3.5).abs() < 0.3, "shifts {moved}");
        assert!(built.sequence.crop.is_some());
        app.tl_sync(&ctx);
        assert!(app.tl.shown.is_some(), "the first frame is a texture");
        // Keys scrub and Space plays.
        let keys = crate::timelapse::Keys::default();
        app.tl_keys(
            crate::timelapse::Keys {
                right: true,
                ..keys
            },
            1,
        );
        assert_eq!(app.tl.index, 1);
        app.tl_keys(
            crate::timelapse::Keys {
                home: true,
                space: true,
                ..keys
            },
            1,
        );
        assert_eq!((app.tl.index, app.tl.playing), (0, true));
        assert_eq!(app.tl_status().0, "2 frames");
    }

    #[test]
    fn a_time_lapse_can_derotate_the_surface_to_one_time() {
        let (mut app, ctx) = app();
        let caps = write_moving_pair();
        app.select(Step::Timelapse);
        app.tl_add(caps);
        app.tl.date = "2026-10-20".into();
        app.settings.keep_percent = 50.0;
        app.tl.derotate = true;
        app.tl.law = helia_core::derotate::RotationLaw::Snodgrass;
        app.tl.celestial_north_up = true;
        let o = app.tl.options();
        assert!(o.derotate.is_some_and(|d| d.orient.celestial_north_up
            && d.law == helia_core::derotate::RotationLaw::Snodgrass));
        app.tl_start();
        until(&mut app, &ctx, "the time-lapse", |a| a.tl.running.is_none());
        assert_eq!(app.tl.error, None);
        let built = app.tl.built.clone().expect("a series");
        assert!(built.sequence.derotated_to.is_some());
        assert!(built.sequence.report[0].derotated_px.is_some());
        // Without a date the names give no time, and the run says why.
        app.tl.date.clear();
        app.tl_start();
        until(&mut app, &ctx, "the refusal", |a| a.tl.running.is_none());
        assert!(
            app.tl
                .error
                .as_deref()
                .is_some_and(|e| e.contains("time of every frame")),
            "{:?}",
            app.tl.error
        );
    }

    /// Two captures that are overlapping windows of one synthetic Sun.
    fn write_panel_pair() -> Vec<PathBuf> {
        let dir = std::env::temp_dir().join(format!("helia-gui-mo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sun = SyntheticSun::new(&SyntheticConfig {
            width: 280,
            height: 200,
            radius_frac: 0.45,
            granulation: 0.15,
            spots: 5,
            ..SyntheticConfig::default()
        });
        let cap = SyntheticCapture::generate(
            &sun,
            &CaptureConfig {
                frames: 12,
                noise: 0.01,
                ..CaptureConfig::default()
            },
        );
        [("panel_a.ser", 0usize), ("panel_b.ser", 110)]
            .iter()
            .map(|&(name, x0)| {
                let frames: Vec<helia_core::Frame> = cap
                    .frames
                    .iter()
                    .map(|f| {
                        let (w, h) = (170usize, 190usize);
                        let mut out = helia_core::Frame::zeros(w, h);
                        for y in 0..h {
                            for x in 0..w {
                                out.data[y * w + x] = f.get(x0 + x, y + 5);
                            }
                        }
                        out
                    })
                    .collect();
                let path = dir.join(name);
                std::fs::write(&path, ser_bytes(&frames)).unwrap();
                path
            })
            .collect()
    }

    #[test]
    fn a_mosaic_stacks_the_panels_and_joins_them() {
        let (mut app, ctx) = app();
        let panels = write_panel_pair();
        app.select(Step::Mosaic);
        app.mo_add(panels.clone());
        app.mo_add(panels);
        assert_eq!(app.mo.panels.len(), 2);
        assert_eq!(app.mo_hint(), "Press Make mosaic");
        app.settings.keep_percent = 50.0;
        app.mo.max_rotation = 1.0;
        app.mo_start();
        assert!(app.mo.running.is_some());
        until(&mut app, &ctx, "the mosaic", |a| a.mo.running.is_none());
        assert_eq!(app.mo.error, None);
        let built = app.mo.built.clone().expect("a mosaic");
        let p = &built.mosaic.panels;
        assert_eq!(p.len(), 2);
        assert!((p[1].x - 110.0).abs() < 1.5, "panel 2 at {}", p[1].x);
        assert!(app.mo.shown.is_some());
        assert_eq!(app.mo_status().0, "2 panels");
        assert_eq!(built.names, ["panel_a.ser", "panel_b.ser"]);
    }

    #[test]
    fn a_bad_date_stops_the_time_lapse_and_options_follow_the_controls() {
        let (mut app, _ctx) = app();
        app.tl.date = "20-10-2026".into();
        assert!(app.tl.date_is_bad());
        app.tl.date = "2026-10-20".into();
        assert!(!app.tl.date_is_bad());
        app.tl.bright = crate::timelapse::Bright::Smooth;
        app.tl.window = 7;
        let o = app.tl.options();
        assert_eq!(o.deflicker, helia_core::timelapse::Deflicker::Smooth(7));
        // One capture is not a series.
        app.tl_add(vec![PathBuf::from("a.ser")]);
        app.tl_start();
        assert!(app.tl.running.is_none());
    }

    /// A SER file that says mono although its frames are an RGGB mosaic.
    fn write_mislabelled(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("helia-gui-app-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let frames: Vec<_> = (0..12)
            .map(|i| {
                helia_core::testutil::mosaic_disc(
                    160,
                    128,
                    Some(helia_core::BayerPattern::Rggb),
                    [0.94, 1.0, 0.01],
                    i,
                )
            })
            .collect();
        let h =
            helia_core::io::ser::SerHeader::new(160, 128, helia_core::io::ser::ColorId::Mono, 8);
        let path = dir.join(name);
        std::fs::write(&path, helia_core::testutil::ser_bytes_with(&frames, h)).unwrap();
        path
    }

    #[test]
    fn a_mosaic_the_header_denies_is_found_announced_and_overridable() {
        use helia_core::io::capture::{Level, Origin};
        let (mut app, ctx) = app();
        let path = write_mislabelled("mono-label.ser");
        app.begin_open(path.clone());
        until(&mut app, &ctx, "the capture to open", |a| {
            a.capture.is_some()
        });
        let cap = app.capture.as_ref().unwrap();
        assert_eq!(cap.meta.bayer, Some(helia_core::BayerPattern::Rggb));
        assert_eq!(cap.report.effective.bayer.origin, Origin::Detection);
        assert_eq!(cap.report.notices[0].level, Level::Warning);
        assert_eq!(cap.report.ser.as_ref().map(|h| h.colour_id), Some(0));
        // A warning toast points at the Open step.
        assert!(
            app.toasts
                .list()
                .iter()
                .any(|t| t.text.contains("header says mono") && t.text.contains("Open step")),
            "{:?}",
            app.toasts.list()
        );
        // H-alpha is not named, but the colour sensor now has channels to choose.
        assert_eq!(app.settings.channel, Channel::Luminance);
        // Saying it is mono reopens the capture and is reported as an override.
        app.settings.bayer = BayerSetting::Mono;
        app.begin_open(path.clone());
        until(&mut app, &ctx, "the reopen", |a| {
            a.capture
                .as_ref()
                .is_some_and(|c| c.report.effective.bayer.origin == Origin::Override)
        });
        assert_eq!(app.capture.as_ref().unwrap().meta.bayer, None);
        assert_eq!(app.settings.bayer, BayerSetting::Mono, "same file: kept");
        // A different file starts from what its own header and data say.
        let other = write_mislabelled("another.ser");
        app.begin_open(other);
        assert_eq!(app.settings.bayer, BayerSetting::Auto);
        assert!(app.settings.bit_depth.is_none() && !app.settings.flip_rows);
        until(&mut app, &ctx, "the other file", |a| {
            a.capture
                .as_ref()
                .is_some_and(|c| c.meta.file == "another.ser")
        });
        assert_eq!(
            app.capture.as_ref().unwrap().meta.bayer,
            Some(helia_core::BayerPattern::Rggb)
        );
    }

    #[test]
    fn a_file_that_cannot_be_opened_raises_an_error_that_stays() {
        let (mut app, ctx) = app();
        let dir = std::env::temp_dir().join(format!("helia-gui-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bad = dir.join("junk.ser");
        std::fs::write(&bad, b"not a capture").unwrap();
        app.begin_open(bad);
        until(&mut app, &ctx, "the error", |a| !a.toasts.list().is_empty());
        assert!(!app.opening || app.capture.is_none());
        assert!(app.tasks.get(TaskKind::Opening).is_none());
        let t = app.toasts.list()[0].clone();
        assert_eq!(t.kind, ToastKind::Error);
        assert!(t.text.starts_with("Could not open file: "), "{}", t.text);
        // Errors outlive any wait; the user has to dismiss them.
        app.toasts.expire(app.now() + Duration::from_secs(3600));
        assert_eq!(app.toasts.list().len(), 1);
        app.toasts.dismiss(t.id);
        assert!(app.toasts.list().is_empty());
    }

    #[test]
    fn quick_stack_runs_to_a_colour_preview_and_reports_it() {
        let (mut app, ctx) = app();
        app.begin_open(write_capture("sun_CaK.ser"));
        until(&mut app, &ctx, "the capture to open", |a| {
            a.capture.is_some()
        });
        assert_eq!(app.settings.preset, Some(Preset::CaK));
        assert_eq!(app.settings.keep_percent, 30.0);
        app.quick_stack();
        assert!(app.run.is_some());
        assert!(
            matches!(app.tasks.get(TaskKind::Stack(Stage::Quality)), Some(t) if t.cancellable),
            "the run is announced and can be cancelled"
        );
        until(&mut app, &ctx, "the stack", |a| a.result.is_some());
        assert!(app.run.is_none() && app.tasks.get(TaskKind::Stack(Stage::Finish)).is_none());
        let msg = app
            .toasts
            .list()
            .iter()
            .find(|t| t.text.starts_with("Stacked "));
        let msg = msg.expect("a completion toast");
        assert_eq!(msg.kind, ToastKind::Success);
        assert!(msg.text.contains(" of 20 frames in "), "{}", msg.text);
        assert_eq!(app.step, Step::Sharpen, "next: look at the sharpening");
        // The recommended wavelets are on, so the preview is made at once.
        until(&mut app, &ctx, "the sharpened preview", |a| {
            a.colour_tex.is_some()
        });
        assert!(app.tasks.get(TaskKind::Previewing).is_none());
        assert!(app.preview_took.is_some() && app.preview_size.is_some());
        assert_eq!(
            app.settings.edits.method,
            helia_core::SharpenMethod::Wavelet
        );
        // The look is remembered for the filter.
        app.settings.colour.preset = Some(Preset::Mg);
        app.colour_changed();
        assert_eq!(app.prefs.colours["cak"].preset, Some(Preset::Mg));
    }

    #[test]
    fn sliders_ask_for_one_preview_at_a_time_and_the_last_value_wins() {
        let (mut app, ctx) = app();
        app.begin_open(write_capture("sun_Ha_002.ser"));
        until(&mut app, &ctx, "the capture to open", |a| {
            a.capture.is_some()
        });
        app.quick_stack();
        until(&mut app, &ctx, "the first preview", |a| {
            a.result.is_some() && a.colour_tex.is_some() && !a.colour_inflight
        });
        // A burst of slider moves: only one render runs, the rest are folded
        // into one more when it ends.
        let first_job = app.colour_job;
        for i in 0..20 {
            app.settings.edits.unsharp.amount = i as f32 * 0.1;
            app.settings.edits.method = helia_core::SharpenMethod::Unsharp;
            app.edits_changed();
        }
        assert_eq!(app.colour_job, first_job + 1, "one render in flight");
        assert!(app.colour_inflight && app.colour_dirty);
        until(&mut app, &ctx, "the burst to settle", |a| {
            !a.colour_inflight && !a.colour_dirty
        });
        assert_eq!(app.colour_job, first_job + 2, "and one follow-up");
        assert!(app.preview_took.is_some());
        assert!(app.tasks.get(TaskKind::Previewing).is_none());
        // Switching everything off leaves the stack as it is, with no preview.
        app.settings.edits.method = helia_core::SharpenMethod::None;
        app.settings.colour = ColourParams::default();
        app.edits_changed();
        assert!(app.colour_tex.is_none());
    }

    #[test]
    fn an_edit_that_does_not_fit_is_explained_not_rendered() {
        let (mut app, ctx) = app();
        app.begin_open(write_capture("sun_Ha_003.ser"));
        until(&mut app, &ctx, "the capture to open", |a| {
            a.capture.is_some()
        });
        app.quick_stack();
        until(&mut app, &ctx, "the stack", |a| {
            a.result.is_some() && !a.colour_inflight
        });
        let jobs = app.colour_job;
        app.settings.edits.crop_on = true;
        app.settings.edits.crop = helia_core::edit::Crop {
            x: 90,
            y: 0,
            width: 50,
            height: 10,
        };
        app.edits_changed();
        assert_eq!(app.colour_job, jobs, "nothing was started");
        assert!(
            app.edit_error
                .as_deref()
                .is_some_and(|e| e.contains("crop"))
        );
    }

    #[test]
    fn presets_are_saved_listed_loaded_and_deleted() {
        let (mut app, _ctx) = app();
        let dir = std::env::temp_dir().join(format!("helia-gui-presets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        app.presets_dir = Some(dir.clone());
        app.settings.edits.method = helia_core::SharpenMethod::Unsharp;
        app.settings.edits.unsharp.amount = 1.7;
        app.settings.edits.levels.gamma = 1.4;
        app.save_edit_preset("Gentle Ha");
        assert_eq!(app.user_presets.len(), 1);
        assert_eq!(app.user_presets[0].name, "Gentle Ha");
        assert!(dir.join("gentle-ha.json").is_file());
        // Change things, then bring the preset back.
        app.settings.edits.method = helia_core::SharpenMethod::None;
        app.settings.edits.levels.gamma = 1.0;
        let preset = app.user_presets[0].clone();
        app.load_edit_preset(&preset);
        assert_eq!(
            app.settings.edits.method,
            helia_core::SharpenMethod::Unsharp
        );
        assert!((app.settings.edits.unsharp.amount - 1.7).abs() < 1e-6);
        assert!((app.settings.edits.levels.gamma - 1.4).abs() < 1e-6);
        app.delete_edit_preset("Gentle Ha");
        assert_eq!(app.user_presets.len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
        // Without a place to keep them, saving says so.
        app.presets_dir = None;
        app.save_edit_preset("x");
        assert!(app.toasts.list().iter().any(|t| t.kind == ToastKind::Error));
    }

    #[test]
    fn choosing_the_filter_by_hand_changes_the_recommendation() {
        let (mut app, ctx) = app();
        app.begin_open(write_capture("capture_0001.ser"));
        until(&mut app, &ctx, "the capture to open", |a| {
            a.capture.is_some()
        });
        app.choose_filter(Some(Preset::Mg));
        assert_eq!(app.settings.preset, Some(Preset::Mg));
        assert_eq!(app.settings.preset_source.as_deref(), Some("chosen"));
        assert_eq!(app.settings.keep_percent, 30.0);
        app.choose_filter(None);
        assert_eq!(app.settings.preset, None);
        assert_eq!(app.settings.keep_percent, 25.0);
        assert_eq!(app.settings.colour, ColourParams::default());
    }
}
