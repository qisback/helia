// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Background work: everything that touches the disk, the GPU or the pipeline.
//!
//! The UI thread only sends requests and drains [`Event`]s, so it never blocks
//! on decoding, scoring, stacking or a file dialog.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};
use helia_core::Frame;
use helia_core::align::multipoint::{ApGrid, ApMask, MultipointConfig};
use helia_core::calibrate::{Calibration, CalibrationSpec, Plan};
use helia_core::debayer::{Channel, Debayer};
use helia_core::edit::EditList;
use helia_core::io::capture::{CaptureReport, open_reported_with};
use helia_core::io::ffmpeg::VideoOptions;
use helia_core::io::sidecar::CameraSettings;
use helia_core::io::solarnet::{CaptureInfo, FitsMeta, Site};
use helia_core::io::source::{FrameSource, OpenOptions, open_capture};
use helia_core::output::{OutputPlan, write_stack};
use helia_core::pipeline::{Pipeline, StackConfig, StackOutput, Stage};
use helia_core::recommend::{Recommendation, capture_texts, recommend};
use helia_core::{Backend, ColourParams, Preset, RgbImage};
use helia_gpu::{AdapterRequest, BackendChoice, GpuBackend, select_backend};

use crate::activity::TaskKind;
use crate::model::{CaptureMeta, ExportFormat, parse_describe, split_adapter};

/// A capture that opened.
pub struct Opened {
    /// Path on disk.
    pub path: PathBuf,
    /// Top bar metadata.
    pub meta: CaptureMeta,
    /// Filter guess and recommended settings for this capture.
    pub rec: Recommendation,
    /// Keeps a decoded video alive while held.
    pub keep_alive: Option<Arc<dyn std::any::Any + Send + Sync>>,
    /// The header as written, what Helia uses instead and why.
    pub report: Box<CaptureReport>,
    /// The colour plane to stack when the open was asked to look for a clipped one
    /// (see [`helia_core::clip`]); `None` when it was not, or nothing is clipped.
    pub channel: Option<Channel>,
    /// Why that plane differs from the usual one, or that none is usable.
    pub channel_note: Option<String>,
}

/// Calibration masters that are ready, and what to tell the user about them.
#[derive(Debug)]
pub struct CalReady {
    /// The masters.
    pub calibration: Arc<Calibration>,
    /// One line per master.
    pub lines: Vec<String>,
    /// Things to be careful about: rings in the flat, a dark that could not
    /// be scaled, a file with more than one exposure.
    pub warnings: Vec<String>,
}

/// A preview that is ready: the stack with the edits and the colour applied.
pub struct Coloured {
    /// Uploaded texture and its size.
    pub texture: (TextureHandle, [usize; 2]),
    /// The same without the sharpening and tone edits (only the geometry), for
    /// the "before" half of a split view.
    pub before: Option<(TextureHandle, [usize; 2])>,
    /// Size of the finished image, which the texture stands for.
    pub logical: [usize; 2],
    /// How long the edits, the colour and the upload took.
    pub took: std::time::Duration,
    /// Whether the sharpening added detail or mostly noise, from the bands of
    /// the preview before and after the edits.
    pub check: Option<helia_core::edit::diagnose::SharpenCheck>,
}

/// What a preview is made from.
pub enum PreviewBase {
    /// The stack at full size.
    Full(Arc<StackOutput>),
    /// The stack shrunk by a power of two.
    Small(Arc<Frame>),
    /// The true-colour stack at full size.
    FullRgb(Arc<StackOutput>),
    /// The true-colour stack shrunk by a power of two.
    SmallRgb(Arc<RgbImage>),
}

impl PreviewBase {
    /// The mono image; for a colour base, its luminance.
    fn frame(&self) -> &Frame {
        match self {
            Self::Full(o) | Self::FullRgb(o) => &o.image,
            Self::Small(f) => f,
            Self::SmallRgb(_) => unreachable!("a colour base is rendered by `render_rgb`"),
        }
    }

    /// The colour picture, for a true-colour base.
    fn rgb(&self) -> Option<&RgbImage> {
        match self {
            Self::FullRgb(o) => o.colour.as_ref().map(|c| &c.rgb),
            Self::SmallRgb(r) => Some(r),
            Self::Full(_) | Self::Small(_) => None,
        }
    }
}

/// One preview to make.
pub struct RenderRequest {
    /// Job number; a stale result is dropped by it.
    pub job: u64,
    /// The image to edit.
    pub base: PreviewBase,
    /// The edits, already scaled to the base.
    pub edits: EditList,
    /// The geometry of `edits` alone, for the "before" half, when wanted.
    pub before: Option<EditList>,
    /// The colour look.
    pub colour: ColourParams,
    /// Backend choice.
    pub backend: BackendChoice,
    /// Size of the finished image.
    pub logical: [usize; 2],
}

/// A decoded preview frame.
pub struct Preview {
    /// Capture generation the request belonged to.
    pub generation: u64,
    /// Frame index.
    pub index: usize,
    /// Uploaded texture, or the error.
    pub texture: Result<(TextureHandle, [usize; 2]), String>,
    /// Alignment point grid when one was requested: (point size, grid).
    pub grid: Option<(usize, Option<ApGrid>)>,
    /// The frame as recorded, before calibration; only with a calibration.
    pub before: Option<(TextureHandle, [usize; 2])>,
}

/// Detected GPU adapters.
#[derive(Clone, Debug, Default)]
pub struct GpuProbe {
    /// Hardware adapter summary, `None` when there is none.
    pub hardware: Option<String>,
    /// Software adapter summary, probed only when there is no hardware one.
    pub software: Option<String>,
}

/// Messages from worker threads to the UI.
pub enum Event {
    /// The open dialog closed.
    Picked(Option<PathBuf>),
    /// A capture finished opening.
    Opened(u64, Result<Opened, String>),
    /// A compressed video is being decoded: fraction done.
    OpenProgress(u64, f32),
    /// A preview frame is ready.
    Preview(Preview),
    /// Every frame scored.
    Scores(u64, Result<Vec<f32>, String>),
    /// Stack progress.
    Progress(u64, Stage, f32),
    /// What a plugin said while a run was going, such as "Uploading 40%".
    Status(u64, String),
    /// What the `analyse` steps of plugins reported for this run, and the lines about the plugins
    /// that took part, for the history of an exported file.
    PluginResults(
        u64,
        Vec<(helia_plugin::StepSpec, helia_plugin::Analysis)>,
        Vec<String>,
        Vec<helia_core::io::fits::HeaderCard>,
    ),
    /// Scoring progress for a scoring epoch.
    ScoreProgress(u64, f32),
    /// Progress of some other long operation (for example a video conversion
    /// reporting how far ffmpeg has got); `None` is indeterminate.
    #[allow(dead_code, reason = "the video import reports through it")]
    Working(TaskKind, Option<f32>),
    /// The operation of this kind is over.
    #[allow(dead_code, reason = "the video import reports through it")]
    Finished(TaskKind),
    /// A colour preview finished (the job number it was asked for).
    Coloured(u64, Result<Coloured, String>),
    /// The dialog for a calibration file closed.
    CalPicked(crate::calibration::Slot, Option<PathBuf>),
    /// The calibration masters are ready (the number asked for), or why not.
    CalReady(u64, Result<CalReady, String>),
    /// The save dialog was accepted and the file is being written.
    Exporting,
    /// Stack finished.
    Done(u64, Result<Box<StackOutput>, String>),
    /// GPU probe finished.
    Gpu(GpuProbe),
    /// Export finished (`None` when the dialog was cancelled).
    Saved(Option<Result<PathBuf, String>>),
    /// The bug report was saved (`None` when the dialog was cancelled).
    BugReport(Option<Result<PathBuf, String>>),
    /// The sunspot table, JSON and overlay were saved (`None` when the dialog
    /// was cancelled).
    SunspotsSaved(Option<Result<PathBuf, String>>),
    /// What the science plots need from the capture, for the stack of this job.
    ScienceExtras(u64, Box<helia_report::Extras>),
    /// The science report was saved into a folder (`None` when the dialog
    /// was cancelled).
    ReportSaved(Option<Result<PathBuf, String>>),
    /// A colour table was saved (`None` when the dialog was cancelled).
    LutSaved(Option<Result<PathBuf, String>>),
    /// News from the Time-lapse step.
    Timelapse(crate::timelapse::TlEvent),
    /// News from the Mosaic step.
    Mosaic(crate::mosaic::MosaicEvent),
}

/// Request for one preview frame.
pub struct PreviewRequest {
    /// Capture generation.
    pub generation: u64,
    /// Capture to read.
    pub path: PathBuf,
    /// How to read the capture.
    pub opts: OpenOptions,
    /// Demosaic settings.
    pub debayer: Debayer,
    /// Frame to decode.
    pub index: usize,
    /// Build an alignment point grid with this point size.
    pub grid: Option<usize>,
    /// Rectangles the user excluded; no point may overlap them.
    pub exclude: Vec<helia_core::Rect>,
    /// Masters to apply to the frame (the picture then also carries the
    /// frame as recorded).
    pub calibration: Option<Arc<Calibration>>,
}

/// The backend kept between previews, with the choice it was opened for.
type CachedBackend = Option<(BackendChoice, Arc<dyn Backend>)>;

/// Handle to the worker threads.
#[derive(Clone)]
pub struct Workers {
    ctx: egui::Context,
    events: Sender<Event>,
    preview: Sender<PreviewRequest>,
    /// The backend of the last preview, kept so a GPU device is opened once
    /// and not for every slider movement.
    backend: Arc<std::sync::Mutex<CachedBackend>>,
}

/// The plugins of a run, locked (a plugin that panicked earlier does not poison the next call).
fn lock(
    r: &std::sync::Mutex<helia_plugin::Runner>,
) -> std::sync::MutexGuard<'_, helia_plugin::Runner> {
    r.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Stack with plugins: frame steps on the way in, or a stack-backend plugin instead of the pipeline,
/// then image steps on the stack, then analysis. The plugins' own errors come back in full.
#[allow(clippy::too_many_arguments, reason = "one run: where, how, with what")]
fn stack_with_plugins(
    w: &Workers,
    job: u64,
    cancel: &Arc<AtomicBool>,
    (path, opts): (&Path, &OpenOptions),
    cfg: StackConfig,
    b: Arc<dyn Backend>,
    src: Box<dyn helia_core::io::source::FrameSource>,
    plan: &crate::plugin_steps::PluginPlan,
) -> Result<Box<StackOutput>, String> {
    use helia_plugin::{PluginFrames, StepStage};
    let (debayer, seeing) = (cfg.debayer, cfg.seeing);
    let progress = |s: Stage, f: f32| w.send(Event::Progress(job, s, f));
    let say = |text: &str| w.send(Event::Status(job, text.to_owned()));
    let mut out = if let Some(name) = &plan.backend {
        let job_json = helia_plugin::job::stack_job(&cfg, opts, plan.filter.as_deref())?;
        let stacked = lock(&plan.runner)
            .stack(name, path, &job_json, cancel, &mut |p| {
                if let Some(t) = p.status.as_deref() {
                    say(t);
                }
                if let Some(f) = p.fraction {
                    progress(Stage::Stack, f as f32);
                }
            })
            .map_err(|e| e.to_string())?;
        if let Some(n) = &stacked.note {
            say(n);
        }
        StackOutput::from_image(stacked.frame, format!("plugin {name}"))
    } else {
        let frame_steps = lock(&plan.runner)
            .frame_steps(&plan.specs)
            .map_err(|e| e.to_string())?;
        let mut piped: Box<dyn helia_core::io::source::FrameSource> = if frame_steps.is_empty() {
            src
        } else {
            Box::new(PluginFrames::new(
                src,
                Arc::clone(&plan.runner),
                frame_steps,
                Arc::clone(cancel),
            ))
        };
        let out = Pipeline::new(cfg)
            .with_backend(b)
            .run(piped.as_mut(), &progress, cancel)
            .map_err(|e| e.to_string())?;
        // What the science plots need: read again, without the plugin steps.
        if let Ok(mut raw) = open_capture(path, opts) {
            let mut extras = helia_report::Extras::gather(&mut raw, debayer, &out);
            extras.aperture_mm = seeing.and_then(|s| s.aperture_mm);
            extras.wavelength_nm = seeing.and_then(|s| s.wavelength_nm);
            w.send(Event::ScienceExtras(job, Box::new(extras)));
        }
        out
    };
    if plan
        .specs
        .iter()
        .any(|s| lock(&plan.runner).check(s).is_ok_and(|c| c == "image-op"))
    {
        let mut forward = |_: &helia_plugin::StepSpec, p: helia_plugin::Progress| {
            if let Some(t) = p.status.as_deref() {
                say(t);
            }
        };
        out.image = lock(&plan.runner)
            .image_steps(
                &plan.specs,
                StepStage::PreEdit,
                out.image.clone(),
                cancel,
                &mut forward,
            )
            .map_err(|e| e.to_string())?;
    }
    let mut forward = |_: &helia_plugin::StepSpec, p: helia_plugin::Progress| {
        if let Some(t) = p.status.as_deref() {
            say(t);
        }
    };
    let results = lock(&plan.runner)
        .analyse_steps(&plan.specs, &out.image, cancel, &mut forward)
        .map_err(|e| e.to_string())?;
    let provenance = lock(&plan.runner).provenance();
    if !results.is_empty() || !provenance.is_empty() {
        let cards = lock(&plan.runner).fits_cards();
        w.send(Event::PluginResults(job, results, provenance, cards));
    }
    lock(&plan.runner).close_all();
    Ok(Box::new(out))
}

impl Workers {
    /// Start the preview thread; returns the handle and the event receiver.
    pub fn start(ctx: &egui::Context) -> (Self, Receiver<Event>) {
        let (events, rx) = channel();
        let (preview, preview_rx) = channel();
        let w = Self {
            ctx: ctx.clone(),
            events,
            preview,
            backend: Arc::default(),
        };
        let (tx, c) = (w.events.clone(), ctx.clone());
        std::thread::spawn(move || preview_loop(&preview_rx, &tx, &c));
        (w, rx)
    }

    pub(crate) fn send(&self, e: Event) {
        let _ = self.events.send(e);
        self.ctx.request_repaint();
    }

    /// Probe GPU adapters.
    pub fn probe_gpu(&self) {
        let w = self.clone();
        std::thread::spawn(move || {
            let hardware = GpuBackend::shared(AdapterRequest::Hardware)
                .ok()
                .map(|g| g.adapter_summary());
            let software = if hardware.is_none() {
                GpuBackend::shared(AdapterRequest::Software)
                    .ok()
                    .map(|g| g.adapter_summary())
            } else {
                None
            };
            w.send(Event::Gpu(GpuProbe { hardware, software }));
        });
    }

    /// Ask for a calibration file or folder for `slot`.
    pub fn pick_cal_path(&self, slot: crate::calibration::Slot, folder: bool) {
        let w = self.clone();
        std::thread::spawn(move || {
            let dlg = rfd::FileDialog::new();
            let p = if folder {
                dlg.pick_folder()
            } else if slot.masters_only() {
                dlg.add_filter("Bad pixels", &["fits", "fit", "fts", "txt"])
                    .pick_file()
            } else {
                dlg.add_filter(
                    "Masters and captures",
                    &[
                        "fits", "fit", "fts", "ser", "avi", "mp4", "mov", "mkv", "webm",
                    ],
                )
                .pick_file()
            };
            w.send(Event::CalPicked(slot, p));
        });
    }

    /// Build or read the calibration masters off the UI thread. `capture` is
    /// the open capture's path, size and mosaic pattern, to take its exposure
    /// and check the masters fit it. Returns the flag that cancels it.
    pub fn resolve_calibration(
        &self,
        token: u64,
        spec: CalibrationSpec,
        capture: Option<(PathBuf, usize, usize, Option<helia_core::BayerPattern>)>,
    ) -> Arc<AtomicBool> {
        let cancel = Arc::new(AtomicBool::new(false));
        let (w, c) = (self.clone(), Arc::clone(&cancel));
        std::thread::spawn(move || {
            let res = (|| -> Result<CalReady, String> {
                let exposure = capture
                    .as_ref()
                    .and_then(|(p, ..)| CaptureInfo::read(p, None).exposure_s);
                let r = spec
                    .resolve(exposure, &|_, _| {}, &c)
                    .map_err(|e| e.to_string())?;
                if let Some((_, width, height, pattern)) = &capture {
                    r.calibration
                        .plan(*width, *height, *pattern)
                        .map_err(|e| e.to_string())?;
                }
                let lines = r.calibration.summary_lines();
                Ok(CalReady {
                    calibration: Arc::new(r.calibration),
                    lines,
                    warnings: r.warnings,
                })
            })();
            w.send(Event::CalReady(token, res));
        });
        cancel
    }

    /// Ask for a folder and write the masters there.
    pub fn save_masters(&self, cal: Arc<Calibration>) {
        let w = self.clone();
        std::thread::spawn(move || {
            let Some(dir) = rfd::FileDialog::new().pick_folder() else {
                w.send(Event::Saved(None));
                return;
            };
            w.send(Event::Exporting);
            let res = (|| -> helia_core::Result<PathBuf> {
                if let Some(m) = &cal.dark {
                    m.write(&dir.join("master-dark.fits"))?;
                }
                if let Some(m) = &cal.bias {
                    m.write(&dir.join("master-bias.fits"))?;
                }
                if let Some(m) = &cal.flat {
                    m.write(&dir.join("master-flat.fits"))?;
                }
                if let Some(b) = &cal.bad {
                    b.write(&dir.join("bad-pixels.fits"))?;
                }
                Ok(dir.clone())
            })()
            .map_err(|e| e.to_string());
            w.send(Event::Saved(Some(res)));
        });
    }

    /// Show the open dialog, starting in `dir` when given.
    pub fn pick_capture(&self, dir: Option<PathBuf>) {
        let w = self.clone();
        std::thread::spawn(move || {
            let mut dlg = rfd::FileDialog::new().add_filter(
                "Captures",
                &["ser", "avi", "mp4", "mov", "mkv", "webm", "m4v"],
            );
            if let Some(d) = dir.filter(|d| d.is_dir()) {
                dlg = dlg.set_directory(d);
            }
            w.send(Event::Picked(dlg.pick_file()));
        });
    }

    /// Ask where to save the bug report and write it.
    pub fn save_bug_report(
        &self,
        report: helia_feedback::bugreport::BugReport,
        dir: Option<PathBuf>,
    ) {
        let w = self.clone();
        std::thread::spawn(move || {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let mut dlg = rfd::FileDialog::new()
                .set_file_name(helia_feedback::bugreport::zip_name(secs))
                .add_filter("Zip archive", &["zip"]);
            if let Some(d) = dir.filter(|d| d.is_dir()) {
                dlg = dlg.set_directory(d);
            }
            let Some(path) = dlg.save_file() else {
                w.send(Event::BugReport(None));
                return;
            };
            let res = helia_feedback::bugreport::build_zip(&report)
                .map_err(|e| e.to_string())
                .and_then(|bytes| std::fs::write(&path, bytes).map_err(|e| e.to_string()))
                .map(|()| path);
            w.send(Event::BugReport(Some(res)));
        });
    }

    /// Open `path` and report its metadata; returns the flag that cancels it.
    /// A compressed video is decoded by ffmpeg first, with progress events.
    ///
    /// With `auto_channel` (the user has not named a plane) a clipped colour plane is
    /// replaced by the brightest one that is not, as `helia stack` does: for a video
    /// before it is decoded, for a raw colour capture once it is open.
    pub fn open(
        &self,
        generation: u64,
        path: PathBuf,
        mut opts: OpenOptions,
        auto_channel: bool,
    ) -> Arc<AtomicBool> {
        let cancel = Arc::new(AtomicBool::new(false));
        let (w, c) = (self.clone(), Arc::clone(&cancel));
        std::thread::spawn(move || {
            let progress = {
                let w = w.clone();
                Arc::new(move |done: u64, total: u64| {
                    w.send(Event::OpenProgress(
                        generation,
                        done as f32 / total.max(1) as f32,
                    ));
                })
            };
            let video = VideoOptions {
                cancel: Some(c),
                progress: Some(progress),
                ..VideoOptions::default()
            };
            // ffmpeg extracts the plane while decoding, so a video is judged first.
            let mut choice = None;
            if auto_channel
                && helia_core::io::ffmpeg::is_video_path(&path)
                && let Ok(c) = helia_core::io::ffmpeg::choose_channel(&path)
            {
                opts.channel = c.channel;
                choice = Some(c);
            }
            let res = open_meta(&path, opts, &video, auto_channel).map(
                |(meta, rec, keep_alive, report, raw_choice)| {
                    let choice = choice.or(raw_choice);
                    Opened {
                        path,
                        meta,
                        rec,
                        keep_alive,
                        report: Box::new(report),
                        channel: choice.as_ref().map(|c| c.channel),
                        channel_note: choice
                            .and_then(|c| c.note)
                            .map(|n| helia_core::clip::app_note(&n)),
                    }
                },
            );
            w.send(Event::Opened(generation, res));
        });
        cancel
    }

    /// Ask for a preview frame; only the newest pending request is served.
    pub fn request_preview(&self, req: PreviewRequest) {
        let _ = self.preview.send(req);
    }

    /// Score every frame; returns the cancel flag.
    pub fn score(
        &self,
        generation: u64,
        path: PathBuf,
        opts: OpenOptions,
        cfg: StackConfig,
        backend: BackendChoice,
    ) -> Arc<AtomicBool> {
        let cancel = Arc::new(AtomicBool::new(false));
        let (w, c) = (self.clone(), Arc::clone(&cancel));
        std::thread::spawn(move || {
            let res = (|| -> Result<Vec<f32>, String> {
                let mut src = open_capture(&path, &opts).map_err(|e| e.to_string())?;
                let b = select_backend(backend).map_err(|e| e.to_string())?;
                // About a hundred updates is plenty for a progress bar.
                let last = std::cell::Cell::new(-1.0f32);
                let progress = |_: Stage, f: f32| {
                    if f - last.get() >= 0.01 || f >= 1.0 {
                        last.set(f);
                        w.send(Event::ScoreProgress(generation, f));
                    }
                };
                Pipeline::new(cfg)
                    .with_backend(b)
                    .score(&mut src, &progress, &c)
                    .map_err(|e| e.to_string())
            })();
            w.send(Event::Scores(generation, res));
        });
        cancel
    }

    /// Run the whole pipeline; returns the cancel flag.
    pub fn stack(
        &self,
        job: u64,
        path: PathBuf,
        opts: OpenOptions,
        cfg: StackConfig,
        backend: BackendChoice,
        plugins: Option<crate::plugin_steps::PluginPlan>,
    ) -> Arc<AtomicBool> {
        let cancel = Arc::new(AtomicBool::new(false));
        let (w, c) = (self.clone(), Arc::clone(&cancel));
        std::thread::spawn(move || {
            let res = (|| -> Result<Box<StackOutput>, String> {
                let mut src = open_capture(&path, &opts).map_err(|e| e.to_string())?;
                let b = select_backend(backend).map_err(|e| e.to_string())?;
                let progress = |s: Stage, f: f32| w.send(Event::Progress(job, s, f));
                let (debayer, seeing) = (cfg.debayer, cfg.seeing);
                let Some(plan) = plugins else {
                    let out = Pipeline::new(cfg)
                        .with_backend(b)
                        .run(&mut src, &progress, &c)
                        .map_err(|e| e.to_string())?;
                    // What the science plots need from the capture, read while it is open.
                    let mut extras = helia_report::Extras::gather(&mut src, debayer, &out);
                    extras.aperture_mm = seeing.and_then(|s| s.aperture_mm);
                    extras.wavelength_nm = seeing.and_then(|s| s.wavelength_nm);
                    w.send(Event::ScienceExtras(job, Box::new(extras)));
                    return Ok(Box::new(out));
                };
                stack_with_plugins(&w, job, &c, (path.as_path(), &opts), cfg, b, src, &plan)
            })();
            w.send(Event::Done(job, res));
        });
        cancel
    }

    /// The backend for `choice`, opened once and kept for the next preview.
    fn backend_for(&self, choice: BackendChoice) -> Result<Arc<dyn Backend>, String> {
        let mut slot = self
            .backend
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((c, b)) = slot.as_ref()
            && *c == choice
        {
            return Ok(Arc::clone(b));
        }
        let b = select_backend(choice).map_err(|e| e.to_string())?;
        *slot = Some((choice, Arc::clone(&b)));
        Ok(b)
    }

    /// Apply the edits and the colour to the stack and upload the result for
    /// the viewer. `req.job` comes back with it so a stale preview can be dropped.
    pub fn render(&self, req: RenderRequest) {
        let w = self.clone();
        std::thread::spawn(move || {
            let job = req.job;
            let res = render_now(&w, &req);
            w.send(Event::Coloured(job, res));
        });
    }

    /// Ask for a destination and write `out` in `format`. A colour look
    /// reaches TIFF and PNG; FITS stays mono and only notes the look.
    pub fn export(
        &self,
        out: Arc<StackOutput>,
        format: ExportFormat,
        stem: String,
        plan: OutputPlan,
        backend: BackendChoice,
        origin: ExportOrigin,
    ) {
        let w = self.clone();
        std::thread::spawn(move || {
            let ext = format.ext();
            let mut dlg = rfd::FileDialog::new().set_file_name(format!("{stem}.{ext}"));
            if let Some(d) = origin.start_dir.clone().filter(|d| d.is_dir()) {
                dlg = dlg.set_directory(d);
            }
            let tiff_filter = ("TIFF (16-bit)", &["tif", "tiff"][..]);
            let png_filter = ("PNG (8-bit)", &["png"][..]);
            let fits_filter = ("FITS (32-bit float)", &["fits", "fit"][..]);
            let order = match format {
                ExportFormat::Tiff => [tiff_filter, png_filter, fits_filter],
                ExportFormat::Png => [png_filter, tiff_filter, fits_filter],
                ExportFormat::Fits => [fits_filter, tiff_filter, png_filter],
            };
            for (n, e) in order {
                dlg = dlg.add_filter(n, e);
            }
            let Some(mut path) = dlg.save_file() else {
                w.send(Event::Saved(None));
                return;
            };
            if path.extension().is_none() {
                path.set_extension(ext);
            }
            w.send(Event::Exporting);
            let fits = FitsMeta::from_stack(
                &CaptureInfo::read(&origin.capture, origin.filter),
                &out,
                Site::from_env(),
            )
            .with_sunspots(origin.sunspots.clone())
            .with_plugin_cards(origin.plugin_cards.clone());
            let res = write_result(&out.image, &path, &plan, backend, Some(fits), &origin.notes)
                .map(|()| path)
                .map_err(|e| e.to_string());
            w.send(Event::Saved(Some(res)));
        });
    }
}

impl Workers {
    /// Ask where to save and write the sunspot table (`.csv`), the report
    /// (`.json`) and the overlay (`.png`) under one name.
    pub fn save_sunspots(
        &self,
        det: helia_core::sunspots::Detection,
        stem: String,
        start_dir: Option<PathBuf>,
    ) {
        let w = self.clone();
        std::thread::spawn(move || {
            let mut dlg = rfd::FileDialog::new()
                .set_file_name(format!("{stem}-sunspots.csv"))
                .add_filter(
                    "Sunspot table (CSV; JSON and PNG are saved beside it)",
                    &["csv"],
                );
            if let Some(d) = start_dir.filter(|d| d.is_dir()) {
                dlg = dlg.set_directory(d);
            }
            let Some(path) = dlg.save_file() else {
                w.send(Event::SunspotsSaved(None));
                return;
            };
            let write = || -> Result<(), String> {
                let err = |p: &std::path::Path, e: &dyn std::fmt::Display| {
                    format!("{}: {e}", p.display())
                };
                std::fs::write(&path, det.report.csv()).map_err(|e| err(&path, &e))?;
                let json = path.with_extension("json");
                std::fs::write(&json, det.report.json()).map_err(|e| err(&json, &e))?;
                let png = path.with_extension("png");
                let f = std::fs::File::create(&png).map_err(|e| err(&png, &e))?;
                helia_core::io::png::write_png8_rgb(
                    std::io::BufWriter::new(f),
                    &det.overlay,
                    Some("Helia sunspot overlay"),
                )
                .map_err(|e| err(&png, &e))
            };
            w.send(Event::SunspotsSaved(Some(write().map(|()| path))));
        });
    }
}

impl Workers {
    /// Ask for a folder and write the science report into a sub-folder named
    /// for the capture.
    pub fn save_report(
        &self,
        bundle: Arc<helia_report::Bundle>,
        stem: String,
        start_dir: Option<PathBuf>,
    ) {
        let w = self.clone();
        std::thread::spawn(move || {
            let mut dlg = rfd::FileDialog::new().set_title("Folder for the science report");
            if let Some(d) = start_dir.filter(|d| d.is_dir()) {
                dlg = dlg.set_directory(d);
            }
            let Some(parent) = dlg.pick_folder() else {
                w.send(Event::ReportSaved(None));
                return;
            };
            let dir = parent.join(format!("{stem}-science"));
            let res = bundle
                .write(&dir)
                .map_err(|e| format!("{}: {e}", dir.display()))
                .map(|()| dir);
            w.send(Event::ReportSaved(Some(res)));
        });
    }
}

impl Workers {
    /// Ask where to save and write the colour table of `params` in `format`.
    pub fn save_lut(
        &self,
        params: ColourParams,
        format: helia_core::lut_export::LutFormat,
        start_dir: Option<PathBuf>,
    ) {
        let w = self.clone();
        std::thread::spawn(move || {
            let name = params.preset.map_or("mono", |p| p.id());
            let mut dlg = rfd::FileDialog::new()
                .set_file_name(format!("helia-{name}.{}", format.ext()))
                .add_filter(format.id(), &[format.ext()]);
            if let Some(d) = start_dir.filter(|d| d.is_dir()) {
                dlg = dlg.set_directory(d);
            }
            let Some(path) = dlg.save_file() else {
                w.send(Event::LutSaved(None));
                return;
            };
            let title = params.preset.map_or_else(
                || "Helia mono".to_owned(),
                |p| format!("Helia {}", p.name()),
            );
            let res = helia_core::lut_export::export(&params.lut(), &title, format, 256)
                .map_err(|e| e.to_string())
                .and_then(|b| std::fs::write(&path, b).map_err(|e| e.to_string()))
                .map(|()| path);
            w.send(Event::LutSaved(Some(res)));
        });
    }
}

/// Where an exported stack came from, for the FITS header.
pub struct ExportOrigin {
    /// Sunspots found on the stack, for the `SPOT*` cards.
    pub sunspots: Option<helia_core::sunspots::Report>,
    /// Folder the save dialog starts in (the last export's), when known.
    pub start_dir: Option<PathBuf>,
    /// The capture file.
    pub capture: PathBuf,
    /// The filter chosen or detected for it.
    pub filter: Option<Preset>,
    /// How the capture was read (`CaptureReport::note_lines`), recorded in the file.
    pub notes: Vec<String>,
    /// FITS cards for the plugins that made the stack.
    pub plugin_cards: Vec<helia_core::io::fits::HeaderCard>,
}

/// Write `image` to `path` as its extension says: edits first (sharpening
/// before any colour), then colour for TIFF and PNG. FITS is the unedited stack
/// unless the plan asks for the edits. Everything chosen is recorded in the file.
pub fn write_result(
    image: &Frame,
    path: &Path,
    plan: &OutputPlan,
    backend: BackendChoice,
    fits: Option<FitsMeta>,
    capture_notes: &[String],
) -> helia_core::Result<()> {
    let mut plan = plan.clone();
    plan.lead_notes.extend(capture_notes.iter().cloned());
    plan.fits = fits;
    let b = select_backend(backend)?;
    write_stack(b.as_ref(), image, path, &plan).map(|_| ())
}

/// Make one preview (see [`Workers::render`]).
fn render_now(w: &Workers, req: &RenderRequest) -> Result<Coloured, String> {
    let started = std::time::Instant::now();
    let b = w.backend_for(req.backend)?;
    if let Some(rgb) = req.base.rgb() {
        return render_rgb(w, req, b.as_ref(), rgb, started);
    }
    let base = req.base.frame();
    let edited = if req.edits.is_empty() {
        None
    } else {
        Some(b.apply_edits(base, &req.edits).map_err(|e| e.to_string())?)
    };
    let lut = req.colour.lut();
    let shown = b
        .colourise(edited.as_ref().unwrap_or(base), &lut)
        .map_err(|e| e.to_string())?;
    let texture = rgb_texture(&w.ctx, &format!("preview-{}", req.job), &shown);
    let before = match &req.before {
        Some(list) => {
            let moved = if list.is_empty() {
                None
            } else {
                Some(b.apply_edits(base, list).map_err(|e| e.to_string())?)
            };
            let rgb = b
                .colourise(moved.as_ref().unwrap_or(base), &lut)
                .map_err(|e| e.to_string())?;
            Some(rgb_texture(&w.ctx, &format!("before-{}", req.job), &rgb))
        }
        None => None,
    };
    let check = edited.as_ref().and_then(|e| {
        let psf = req.edits.ops.iter().find_map(|op| match op {
            helia_core::Op::Deconvolve(d) => Some(d.psf),
            _ => None,
        });
        let sharpens = req.edits.ops.iter().any(|op| {
            matches!(
                op,
                helia_core::Op::Deconvolve(_)
                    | helia_core::Op::Wavelet(_)
                    | helia_core::Op::Unsharp(_)
            )
        });
        sharpens
            .then(|| helia_core::edit::diagnose::check(base, e, psf))
            .flatten()
    });
    Ok(Coloured {
        texture,
        before,
        logical: req.logical,
        took: started.elapsed(),
        check,
    })
}

/// A preview of a true-colour stack: the edits on each channel, no colour look
/// (the picture is already colour).
fn render_rgb(
    w: &Workers,
    req: &RenderRequest,
    b: &dyn Backend,
    rgb: &RgbImage,
    started: std::time::Instant,
) -> Result<Coloured, String> {
    let shown = helia_core::output::edit_rgb(b, rgb, &req.edits).map_err(|e| e.to_string())?;
    let texture = rgb_texture(&w.ctx, &format!("preview-{}", req.job), &shown);
    let before = match &req.before {
        Some(list) => {
            let moved = helia_core::output::edit_rgb(b, rgb, list).map_err(|e| e.to_string())?;
            Some(rgb_texture(&w.ctx, &format!("before-{}", req.job), &moved))
        }
        None => None,
    };
    Ok(Coloured {
        texture,
        before,
        logical: req.logical,
        took: started.elapsed(),
        check: None,
    })
}

/// Upload a colour image as a texture; returns the handle and its size.
pub fn rgb_texture(ctx: &egui::Context, name: &str, rgb: &RgbImage) -> (TextureHandle, [usize; 2]) {
    let opts = TextureOptions {
        mipmap_mode: Some(egui::TextureFilter::Linear),
        ..TextureOptions::LINEAR
    };
    let bytes = rgb.to_u8();
    let image = ColorImage::from_rgb([rgb.width, rgb.height], &bytes);
    (ctx.load_texture(name, image, opts), [rgb.width, rgb.height])
}

type KeepAlive = Option<Arc<dyn std::any::Any + Send + Sync>>;

type OpenedMeta = (
    CaptureMeta,
    Recommendation,
    KeepAlive,
    CaptureReport,
    Option<helia_core::clip::ChannelChoice>,
);

fn open_meta(
    path: &Path,
    opts: OpenOptions,
    video: &VideoOptions,
    auto_channel: bool,
) -> Result<OpenedMeta, String> {
    let (mut src, report) = open_reported_with(path, &opts, video).map_err(|e| e.to_string())?;
    let (w, h) = src.dimensions();
    let (container, bits) = parse_describe(&src.describe());
    let bayer = report.bayer();
    let camera = CameraSettings::for_capture(path)
        .and_then(|s| s.sections.first().cloned())
        .or_else(|| {
            report
                .ser
                .as_ref()
                .map(|h| h.instrument.clone())
                .filter(|i| !i.is_empty())
        });
    let rec = recommend(w, h, bayer.is_some(), &capture_texts(path), None);
    // A raw colour capture: the plane its filter prefers, unless that one is clipped.
    let choice = (auto_channel && bayer.is_some())
        .then(|| helia_core::clip::choose_channel(rec.channel, src.as_mut()));
    let keep_alive = src.keep_alive();
    let meta = CaptureMeta {
        file: path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into(),
        ),
        container,
        camera,
        width: w,
        height: h,
        bits,
        bayer,
        frames: src.frame_count(),
        video: keep_alive.is_some(),
        note: src.quality_note(),
    };
    Ok((meta, rec, keep_alive, report, choice))
}

struct OpenSource {
    path: PathBuf,
    opts: OpenOptions,
    src: Box<dyn FrameSource>,
    /// Burned-in text found in this capture, looked for on first use.
    overlay: Option<Vec<helia_core::Rect>>,
    /// The calibration plan for the masters last asked for, with the address of
    /// the masters it was made from.
    plan: Option<(usize, Plan)>,
}

fn preview_loop(rx: &Receiver<PreviewRequest>, tx: &Sender<Event>, ctx: &egui::Context) {
    let mut open: Option<OpenSource> = None;
    while let Ok(mut req) = rx.recv() {
        while let Ok(newer) = rx.try_recv() {
            req = newer;
        }
        let reusable = open
            .as_ref()
            .is_some_and(|o| o.path == req.path && o.opts == req.opts);
        if !reusable {
            open = open_capture(&req.path, &req.opts)
                .ok()
                .map(|src| OpenSource {
                    path: req.path.clone(),
                    opts: req.opts,
                    src,
                    overlay: None,
                    plan: None,
                });
        }
        let result = match open.as_mut() {
            None => Err(format!("could not open {}", req.path.display())),
            Some(o) => decode(o, &req, ctx),
        };
        let (texture, grid, before) = match result {
            Ok((t, g, b)) => (Ok(t), g, b),
            Err(e) => (Err(e), None, None),
        };
        let _ = tx.send(Event::Preview(Preview {
            generation: req.generation,
            index: req.index,
            texture,
            grid,
            before,
        }));
        ctx.request_repaint();
    }
}

type Texture = (TextureHandle, [usize; 2]);
type Decoded = (Texture, Option<(usize, Option<ApGrid>)>, Option<Texture>);

fn decode(
    open: &mut OpenSource,
    req: &PreviewRequest,
    ctx: &egui::Context,
) -> Result<Decoded, String> {
    let raw = open
        .src
        .read(req.index)
        .map_err(|e| format!("could not read frame {}: {e}", req.index + 1))?;
    // With masters: the frame as recorded for the "before" half, then the
    // calibrated frame, which is what every other step then sees.
    let mut before = None;
    let raw = match &req.calibration {
        None => raw,
        Some(cal) => {
            let key = Arc::as_ptr(cal) as usize;
            if open.plan.as_ref().is_none_or(|(k, _)| *k != key) {
                let (w, h) = open.src.dimensions();
                let plan = cal
                    .plan(w, h, raw.bayer)
                    .map_err(|e| format!("calibration: {e}"))?;
                open.plan = Some((key, plan));
            }
            let recorded = req.debayer.apply_owned(raw.clone());
            before = Some(texture(
                ctx,
                &format!("frame-before-{}-{}", req.generation, req.index),
                &recorded,
            ));
            let mut calibrated = raw;
            if let Some((_, plan)) = &open.plan {
                plan.apply(&mut calibrated.data);
            }
            calibrated
        }
    };
    let mono = req.debayer.apply_owned(raw);
    let grid = req.grid.map(|size| {
        let cfg = MultipointConfig {
            ap_size: size,
            ..MultipointConfig::default()
        };
        // The same places the stack keeps off: burned-in text, the user's
        // rectangles and everything outside the limb.
        let overlay = open
            .overlay
            .get_or_insert_with(|| find_overlay(open.src.as_mut(), req.debayer))
            .clone();
        let mut exclude = req.exclude.clone();
        exclude.extend(overlay);
        let mask = ApMask {
            disc: helia_core::mask::fit_disc(&mono),
            band: 0.0,
            exclude,
        };
        (size, ApGrid::build_masked(&mono, &cfg, &mask).ok())
    });
    let tex = texture(
        ctx,
        &format!("frame-{}-{}", req.generation, req.index),
        &mono,
    );
    Ok((tex, grid, before))
}

/// Burned-in text of a capture, from a few frames spread over it.
fn find_overlay(src: &mut dyn FrameSource, debayer: Debayer) -> Vec<helia_core::Rect> {
    let frames: Vec<Frame> =
        helia_core::mask::sample_indices(src.frame_count(), helia_core::mask::OVERLAY_SAMPLES)
            .into_iter()
            .filter_map(|i| src.read(i).ok())
            .map(|f| debayer.apply_owned(f))
            .collect();
    helia_core::mask::detect_overlay(&frames)
}

/// Upload a frame as a grey texture; returns the handle and its size.
pub fn texture(ctx: &egui::Context, name: &str, f: &Frame) -> (TextureHandle, [usize; 2]) {
    let opts = TextureOptions {
        mipmap_mode: Some(egui::TextureFilter::Linear),
        ..TextureOptions::LINEAR
    };
    (
        ctx.load_texture(name, to_image(f), opts),
        [f.width, f.height],
    )
}

/// Grey image with a percentile stretch so dim solar frames are visible.
#[must_use]
pub fn to_image(f: &Frame) -> ColorImage {
    let mut sample: Vec<f32> = f
        .data
        .iter()
        .step_by(7)
        .copied()
        .filter(|v| v.is_finite())
        .collect();
    if sample.is_empty() {
        return ColorImage::from_gray([f.width, f.height], &vec![0u8; f.width * f.height]);
    }
    let k = ((sample.len() as f64 * 0.999) as usize).min(sample.len() - 1);
    let hi = (*sample.select_nth_unstable_by(k, f32::total_cmp).1).max(1e-6);
    let bytes: Vec<u8> = f
        .data
        .iter()
        .map(|&v| ((v / hi).clamp(0.0, 1.0).powf(0.8) * 255.0) as u8)
        .collect();
    ColorImage::from_gray([f.width, f.height], &bytes)
}

/// Chip text for the selected backend: (label, adapter name, is a GPU).
#[must_use]
pub fn gpu_chip(choice: BackendChoice, probe: Option<&GpuProbe>) -> (String, String, bool) {
    let cpu = || ("CPU".to_string(), String::new(), false);
    let Some(p) = probe else {
        return ("Detecting".into(), String::new(), false);
    };
    let pick = match choice {
        BackendChoice::Cpu => None,
        BackendChoice::Auto => p.hardware.as_ref(),
        BackendChoice::Gpu => p.hardware.as_ref().or(p.software.as_ref()),
    };
    match pick {
        None => cpu(),
        Some(s) => {
            let (name, api, _) = split_adapter(s);
            (api, name, true)
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use super::*;

    fn probe(hw: Option<&str>, sw: Option<&str>) -> GpuProbe {
        GpuProbe {
            hardware: hw.map(Into::into),
            software: sw.map(Into::into),
        }
    }

    #[test]
    fn chip_follows_the_selected_backend() {
        let p = probe(Some("RTX 3050 (Vulkan, DiscreteGpu)"), None);
        assert_eq!(
            gpu_chip(BackendChoice::Auto, Some(&p)),
            ("Vulkan".into(), "RTX 3050".into(), true)
        );
        assert_eq!(gpu_chip(BackendChoice::Cpu, Some(&p)).0, "CPU");
        assert_eq!(gpu_chip(BackendChoice::Auto, None).0, "Detecting");
    }

    #[test]
    fn auto_never_picks_software_but_gpu_does() {
        let p = probe(None, Some("WARP (Dx12, Cpu)"));
        assert_eq!(gpu_chip(BackendChoice::Auto, Some(&p)).0, "CPU");
        assert_eq!(gpu_chip(BackendChoice::Gpu, Some(&p)).0, "Dx12");
    }

    #[test]
    fn grey_image_stretches_to_the_percentile() {
        let f = Frame::new(2, 2, vec![0.0, 0.25, 0.5, 0.5]).unwrap();
        let img = to_image(&f);
        assert_eq!(img.size, [2, 2]);
        assert_eq!(img.pixels[0].r(), 0);
        assert_eq!(img.pixels[3].r(), 255);
    }
}
