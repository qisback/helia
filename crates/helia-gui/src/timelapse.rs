// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The Time-lapse step: several captures stacked with the settings of the Stack
//! step, registered to each other, and played back with a frame scrubber.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Color32, ColorImage, CornerRadius, Label, Layout, Rect, RichText, Sense,
    Slider, Stroke, StrokeKind, TextureHandle, TextureOptions, Ui, pos2, vec2,
};
use helia_core::animate::MovieChoice;
use helia_core::captime::{TimeHints, capture_time};
use helia_core::derotate::{Orientation, RotationLaw};
use helia_core::io::source::open_capture;
use helia_core::output::{OutputPlan, render};
use helia_core::picture::{self, Picture};
use helia_core::pipeline::Pipeline;
use helia_core::timelapse::{
    self as tl, Deflicker, Derotation, FrameFormat, Options, Reference, Sequence,
};
use helia_gpu::select_backend;

use crate::activity::ToastKind;
use crate::app::App;
use crate::model::ZOOM_STEP;
use crate::theme::{RADIUS_LARGE, mono, palette, sans};
use crate::widgets::{self, Icon, Kind, mono_text};
use crate::worker::Event;

/// Largest number of captures the step takes.
pub const MAX_CAPTURES: usize = 200;

/// How the brightness is evened out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bright {
    /// Left alone.
    Off,
    /// Every frame to the median.
    Match,
    /// Every frame to the median of its neighbours.
    Smooth,
}

/// The keys pressed this frame that the step answers.
#[derive(Clone, Copy, Debug, Default)]
#[allow(clippy::struct_excessive_bools, reason = "one flag for each key")]
pub struct Keys {
    /// Left arrow.
    pub left: bool,
    /// Right arrow.
    pub right: bool,
    /// Home.
    pub home: bool,
    /// End.
    pub end: bool,
    /// Space.
    pub space: bool,
}

/// Messages from the time-lapse worker threads.
pub enum TlEvent {
    /// The file dialog closed with these captures (empty when cancelled).
    Picked(Vec<PathBuf>),
    /// Progress: what is being done and how far, 0 to 1.
    Progress(u64, String, f32),
    /// The series is stacked and registered, or why not.
    Done(u64, Result<Box<Built>, String>),
    /// The series was written (`None` when the dialog was cancelled).
    Saved(Option<Result<PathBuf, String>>),
    /// The diagnostics report was written (`None` when the dialog was cancelled).
    DiagnosticsSaved(Option<Result<PathBuf, String>>),
}

/// A registered series and where its frames came from.
pub struct Built {
    /// The frames.
    pub sequence: Sequence,
    /// The capture behind each frame, by file name.
    pub sources: Vec<String>,
}

/// A run in progress.
pub struct Running {
    /// Number of the run, so a stale event is dropped.
    pub id: u64,
    /// Stops it.
    pub cancel: Arc<AtomicBool>,
    /// What it is doing.
    pub text: String,
    /// How far, 0 to 1.
    pub fraction: f32,
}

/// Everything the step keeps.
pub struct TimelapseState {
    /// The captures, in the order added.
    pub captures: Vec<PathBuf>,
    /// The frame the others are registered to.
    pub reference: Reference,
    /// Refine by phase correlation of the disc centre.
    pub refine: bool,
    /// Brightness evening.
    pub bright: Bright,
    /// Frames in the running median of `Bright::Smooth`.
    pub window: usize,
    /// Cut to the common area.
    pub crop: bool,
    /// Frames a second of the movie and of the playback.
    pub fps: f32,
    /// The movie to write.
    pub movie: MovieChoice,
    /// Format of the numbered frames.
    pub format: FrameFormat,
    /// UTC date for file names that hold only a time of day.
    pub date: String,
    /// Hours east of UTC of the clock that named the files.
    pub utc_offset: f32,
    /// A run in progress.
    pub running: Option<Running>,
    /// Number of the newest run.
    pub run_counter: u64,
    /// The registered series.
    pub built: Option<Arc<Built>>,
    /// Bring every frame to the reference frame's time by the Sun's
    /// differential rotation.
    pub derotate: bool,
    /// The rotation law.
    pub law: RotationLaw,
    /// The images have celestial north up.
    pub celestial_north_up: bool,
    /// The frame shown.
    pub index: usize,
    /// Playing.
    pub playing: bool,
    /// When the playback last stepped.
    pub last_step: Option<Instant>,
    /// The texture of frame `.2`.
    pub shown: Option<(TextureHandle, [usize; 2], usize)>,
    /// What went wrong, in words.
    pub error: Option<String>,
}

impl Default for TimelapseState {
    fn default() -> Self {
        Self {
            captures: Vec::new(),
            reference: Reference::Middle,
            refine: true,
            bright: Bright::Off,
            window: 5,
            crop: false,
            fps: 10.0,
            movie: MovieChoice::Auto,
            format: FrameFormat::Png,
            date: String::new(),
            utc_offset: 0.0,
            derotate: false,
            law: RotationLaw::Howard,
            celestial_north_up: false,
            running: None,
            run_counter: 0,
            built: None,
            index: 0,
            playing: false,
            last_step: None,
            shown: None,
            error: None,
        }
    }
}

impl TimelapseState {
    /// Frames in the registered series.
    #[must_use]
    pub fn frames(&self) -> usize {
        self.built.as_ref().map_or(0, |b| b.sequence.frames.len())
    }

    /// The registration settings as the core takes them.
    #[must_use]
    pub fn options(&self) -> Options {
        Options {
            reference: self.reference,
            refine: self.refine,
            deflicker: match self.bright {
                Bright::Off => Deflicker::Off,
                Bright::Match => Deflicker::Match,
                Bright::Smooth => Deflicker::Smooth(self.window),
            },
            crop: self.crop,
            derotate: self.derotate.then_some(Derotation {
                law: self.law,
                orient: Orientation {
                    celestial_north_up: self.celestial_north_up,
                    ..Orientation::default()
                },
                ..Derotation::default()
            }),
            ..Options::default()
        }
    }

    /// The time hints of the date and offset boxes.
    fn hints(&self) -> TimeHints {
        TimeHints {
            date: helia_core::io::solarnet::unix_from_iso(self.date.trim())
                .filter(|_| self.date.trim().len() == 10),
            utc_offset_h: f64::from(self.utc_offset),
        }
    }

    /// The date box holds something that is not a date.
    #[must_use]
    pub fn date_is_bad(&self) -> bool {
        !self.date.trim().is_empty() && self.hints().date.is_none()
    }
}

/// A picture as a texture, with the fixed 0..1 scale it is written with, so a
/// flicker in the series shows as flicker.
pub(crate) fn to_texture(
    ctx: &egui::Context,
    name: &str,
    p: &Picture,
) -> (TextureHandle, [usize; 2]) {
    let (w, h) = p.size();
    let image = ColorImage::from_rgb([w, h], &picture::to_rgb8(p));
    let opts = TextureOptions {
        mipmap_mode: Some(egui::TextureFilter::Linear),
        ..TextureOptions::LINEAR
    };
    (ctx.load_texture(name, image, opts), [w, h])
}

impl App {
    // ----- doing the work -----

    /// Add captures to the list, skipping repeats.
    pub fn tl_add(&mut self, paths: Vec<PathBuf>) {
        for p in paths {
            if self.tl.captures.len() < MAX_CAPTURES && !self.tl.captures.contains(&p) {
                self.tl.captures.push(p);
            }
        }
    }

    /// Ask for captures to add.
    fn tl_pick(&self) {
        let w = self.workers.clone();
        let dir = self.prefs.last_export_dir.clone();
        std::thread::spawn(move || {
            let mut dlg =
                rfd::FileDialog::new().add_filter("Captures and stacked images", &INPUT_TYPES);
            if let Some(d) = dir.filter(|d| d.is_dir()) {
                dlg = dlg.set_directory(d);
            }
            w.send(Event::Timelapse(TlEvent::Picked(
                dlg.pick_files().unwrap_or_default(),
            )));
        });
    }

    /// Stack every capture with the settings of the Stack step, register the
    /// stacks and keep the series for playback.
    pub fn tl_start(&mut self) {
        if self.tl.running.is_some() || self.tl.captures.len() < 2 {
            return;
        }
        self.tl.run_counter += 1;
        let id = self.tl.run_counter;
        let cancel = Arc::new(AtomicBool::new(false));
        self.tl.running = Some(Running {
            id,
            cancel: Arc::clone(&cancel),
            text: "Starting".into(),
            fraction: 0.0,
        });
        self.tl.error = None;
        self.tl.playing = false;
        let captures = self.tl.captures.clone();
        let (cfg, opts, backend) = (
            self.stack_config(),
            self.settings.open_options(),
            self.settings.backend,
        );
        let plan_base = self.settings.output_plan(None);
        let (options, hints) = (self.tl.options(), self.tl.hints());
        let w = self.workers.clone();
        std::thread::spawn(move || {
            let job = StackJob {
                cfg: &cfg,
                opts: &opts,
                backend,
                plan: &plan_base,
            };
            let res = build(&captures, &job, &options, &hints, &cancel, &|t, f| {
                w.send(Event::Timelapse(TlEvent::Progress(id, t, f)));
            });
            w.send(Event::Timelapse(TlEvent::Done(id, res.map(Box::new))));
        });
    }

    /// Stop the run in flight.
    pub fn tl_cancel(&self) {
        if let Some(r) = &self.tl.running {
            r.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Ask for a folder and write the frames, the table and the movie there.
    fn tl_save(&mut self) {
        let Some(built) = self.tl.built.clone() else {
            return;
        };
        let out = (
            self.tl.format,
            self.tl.movie,
            self.tl.fps,
            self.prefs.last_export_dir.clone(),
        );
        let w = self.workers.clone();
        std::thread::spawn(move || {
            let (format, movie, fps, dir) = out;
            let mut dlg = rfd::FileDialog::new().set_title("Folder for the time-lapse");
            if let Some(d) = dir.filter(|d| d.is_dir()) {
                dlg = dlg.set_directory(d);
            }
            let Some(folder) = dlg.pick_folder() else {
                w.send(Event::Timelapse(TlEvent::Saved(None)));
                return;
            };
            let target = tl::Output {
                dir: folder.clone(),
                format,
                movie,
                fps,
            };
            let res = tl::write_all(&built.sequence, &built.sources, &target)
                .map(|(_, m, _)| m.map_or(folder.clone(), |m| m.path))
                .map_err(|e| e.to_string());
            w.send(Event::Timelapse(TlEvent::Saved(Some(res))));
        });
    }

    /// Ask for a folder and write the diagnostics of the series there.
    fn tl_save_diagnostics(&mut self) {
        let Some(built) = self.tl.built.clone() else {
            return;
        };
        let dir = self.prefs.last_export_dir.clone();
        let w = self.workers.clone();
        std::thread::spawn(move || {
            let mut dlg = rfd::FileDialog::new().set_title("Folder for the time-lapse diagnostics");
            if let Some(d) = dir.filter(|d| d.is_dir()) {
                dlg = dlg.set_directory(d);
            }
            let Some(folder) = dlg.pick_folder() else {
                w.send(Event::Timelapse(TlEvent::DiagnosticsSaved(None)));
                return;
            };
            let data = helia_report::ScienceData::from_sequence(&built.sequence);
            let meta = helia_report::html::Meta {
                title: format!("time-lapse of {} frames", built.sources.len()),
                inputs: Vec::new(),
                settings: vec![("captures".into(), built.sources.join(", "))],
                generated: helia_core::io::solarnet::iso_from_unix(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0.0, |d| d.as_secs_f64()),
                ),
                version: helia_core::about::display_version(crate::about::ui_version()),
            };
            let res =
                helia_report::generate(&data, &meta, None, helia_report::canvas::Size::default())
                    .map_err(|e| e.to_string())
                    .and_then(|b| b.write(&folder).map_err(|e| e.to_string()))
                    .map(|()| folder.clone());
            w.send(Event::Timelapse(TlEvent::DiagnosticsSaved(Some(res))));
        });
    }

    /// Handle a message from the worker.
    pub fn on_timelapse(&mut self, ctx: &egui::Context, ev: TlEvent) {
        match ev {
            TlEvent::Picked(paths) => self.tl_add(paths),
            TlEvent::Progress(id, text, f) => {
                if let Some(r) = self.tl.running.as_mut().filter(|r| r.id == id) {
                    r.text = text;
                    r.fraction = f;
                }
            }
            TlEvent::Done(id, res) => {
                if self.tl.running.as_ref().is_none_or(|r| r.id != id) {
                    return;
                }
                self.tl.running = None;
                match res {
                    Ok(b) => {
                        let failed = b.sequence.failed().len();
                        self.tl.index = 0;
                        self.tl.shown = None;
                        self.tl.built = Some(Arc::new(*b));
                        self.view.reset();
                        if failed > 0 {
                            self.toast(
                                ToastKind::Error,
                                format!("{failed} frame(s) could not be registered; see the table"),
                            );
                        }
                    }
                    Err(e) if e.contains("cancel") => {
                        self.toast(ToastKind::Info, "Time-lapse cancelled.");
                    }
                    Err(e) => {
                        self.tl.error = Some(e.clone());
                        self.toast(ToastKind::Error, format!("Time-lapse failed: {e}"));
                    }
                }
            }
            TlEvent::Saved(None) => {}
            TlEvent::Saved(Some(Ok(p))) => {
                self.prefs.remember_export_dir(&p);
                self.toast(ToastKind::Success, format!("Saved {}", p.display()));
            }
            TlEvent::Saved(Some(Err(e))) => {
                self.toast(ToastKind::Error, format!("Could not save: {e}"));
            }
            TlEvent::DiagnosticsSaved(None) => {}
            TlEvent::DiagnosticsSaved(Some(Ok(p))) => {
                self.prefs.remember_export_dir(&p);
                self.toast(
                    ToastKind::Success,
                    format!("Saved the diagnostics in {}", p.display()),
                );
            }
            TlEvent::DiagnosticsSaved(Some(Err(e))) => {
                self.toast(
                    ToastKind::Error,
                    format!("Could not save the diagnostics: {e}"),
                );
            }
        }
        self.tl_sync(ctx);
    }

    /// Make the texture of the frame being shown, and step the playback.
    pub fn tl_sync(&mut self, ctx: &egui::Context) {
        let n = self.tl.frames();
        if n == 0 {
            return;
        }
        if self.tl.playing {
            let now = Instant::now();
            let step = Duration::from_secs_f32(1.0 / self.tl.fps.max(0.5));
            let last = self.tl.last_step.get_or_insert(now);
            if now.saturating_duration_since(*last) >= step {
                self.tl.index = (self.tl.index + 1) % n;
                self.tl.last_step = Some(now);
            }
            ctx.request_repaint_after(step / 2);
        }
        self.tl.index = self.tl.index.min(n - 1);
        if self.tl.shown.as_ref().is_none_or(|s| s.2 != self.tl.index)
            && let Some(b) = &self.tl.built
        {
            let (t, s) = to_texture(
                ctx,
                &format!("timelapse-{}", self.tl.index),
                &b.sequence.frames[self.tl.index],
            );
            self.tl.shown = Some((t, s, self.tl.index));
        }
    }

    // ----- the inspector -----

    pub(crate) fn timelapse_body(&mut self, ui: &mut Ui) {
        let p = palette();
        let idle = self.tl.running.is_none();
        widgets::overline(ui, "Captures");
        if self.tl.captures.is_empty() {
            ui.add(
                Label::new(mono_text(
                    "None yet. Add two or more captures of the same Sun.",
                    12.5,
                    p.muted,
                ))
                .wrap(),
            );
        }
        let mut remove = None;
        for (i, c) in self.tl.captures.iter().enumerate() {
            ui.horizontal(|ui| {
                let name = c
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                ui.add(
                    Label::new(mono_text(format!("{:>2}  {name}", i + 1), 12.5, p.text)).truncate(),
                )
                .on_hover_text(c.display().to_string());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::icon_button(ui, Icon::Minus, "Remove this capture", idle, false)
                        .clicked()
                    {
                        remove = Some(i);
                    }
                });
            });
        }
        if let Some(i) = remove {
            self.tl.captures.remove(i);
        }
        ui.horizontal_wrapped(|ui| {
            if widgets::chip(ui, "Add captures", false, idle)
                .on_hover_text(
                    "Choose captures (SER, AVI, video) or stacked images (TIFF, PNG, FITS)",
                )
                .clicked()
            {
                self.tl_pick();
            }
            let open = self.capture.as_ref().map(|c| c.path.clone());
            let can = idle && open.as_ref().is_some_and(|o| !self.tl.captures.contains(o));
            if widgets::chip(ui, "Add the open capture", false, can)
                .on_hover_text("Add the capture that is open in the other steps")
                .clicked()
                && let Some(o) = open
            {
                self.tl_add(vec![o]);
            }
            if widgets::chip(ui, "Clear", false, idle && !self.tl.captures.is_empty()).clicked() {
                self.tl.captures.clear();
            }
        });
        ui.add(
            Label::new(mono_text(
                format!(
                    "Each is stacked with the Stack step's settings: best {:.0}% of the frames.",
                    self.settings.keep_percent
                ),
                12.0,
                p.faint,
            ))
            .wrap(),
        );
        ui.add_space(4.0);

        widgets::overline(ui, "Register to");
        ui.horizontal_wrapped(|ui| {
            let last = self.tl.captures.len().saturating_sub(1);
            for (label, r, tip) in [
                (
                    "First",
                    Reference::First,
                    "Register every frame to the first",
                ),
                (
                    "Middle",
                    Reference::Middle,
                    "The middle frame: the least shift, so the least cropping",
                ),
                (
                    "Last",
                    Reference::Index(last),
                    "Register every frame to the last",
                ),
            ] {
                let on = match (self.tl.reference, r) {
                    (Reference::Index(_), Reference::Index(_)) => true,
                    (a, b) => a == b,
                };
                if widgets::chip(ui, label, on, idle)
                    .on_hover_text(tip)
                    .clicked()
                {
                    self.tl.reference = r;
                }
            }
        });
        let mut refine = self.tl.refine;
        if ui
            .add_enabled(
                idle,
                egui::Checkbox::new(&mut refine, "Refine by phase correlation"),
            )
            .on_hover_text(
                "After the limb fit, correlate the middle of the disc to follow the granulation and filaments themselves. Switch off to use the limb alone.",
            )
            .changed()
        {
            self.tl.refine = refine;
        }
        let mut crop = self.tl.crop;
        if ui
            .add_enabled(
                idle,
                egui::Checkbox::new(&mut crop, "Crop to the common area"),
            )
            .on_hover_text(
                "Cut every frame to the area that all of them cover, so no frame has an empty edge",
            )
            .changed()
        {
            self.tl.crop = crop;
        }
        ui.add_space(4.0);

        widgets::overline(ui, "Brightness");
        ui.horizontal_wrapped(|ui| {
            for (label, b, tip) in [
                ("Off", Bright::Off, "Leave the brightness as stacked"),
                (
                    "Match",
                    Bright::Match,
                    "Bring every frame to the median brightness of the series",
                ),
                (
                    "Smooth",
                    Bright::Smooth,
                    "Bring every frame to the median of the frames around it: flicker goes, a slow change stays",
                ),
            ] {
                if widgets::chip(ui, label, self.tl.bright == b, idle)
                    .on_hover_text(tip)
                    .clicked()
                {
                    self.tl.bright = b;
                }
            }
        });
        if self.tl.bright == Bright::Smooth {
            Self::row_label(ui, "Window (frames)", &self.tl.window.to_string());
            let mut w = self.tl.window;
            ui.add_enabled(idle, egui::DragValue::new(&mut w).range(3..=21).speed(0.2))
                .on_hover_text("Frames in the running median; odd numbers only");
            self.tl.window = if w.is_multiple_of(2) { w + 1 } else { w };
        }
        ui.add_space(4.0);

        widgets::overline(ui, "Derotation");
        ui.horizontal_wrapped(|ui| {
            for (label, on, tip) in [
                ("Off", false, "Leave the surface as it was at each capture"),
                (
                    "To reference",
                    true,
                    "Bring every frame to the reference frame's time by the Sun's differential rotation, so features hold still. Needs the time of every capture.",
                ),
            ] {
                if widgets::chip(ui, label, self.tl.derotate == on, idle)
                    .on_hover_text(tip)
                    .clicked()
                {
                    self.tl.derotate = on;
                }
            }
        });
        if self.tl.derotate {
            ui.horizontal_wrapped(|ui| {
                ui.add(Label::new(mono_text("LAW", 11.0, p.faint)));
                for (label, l, tip) in [
                    (
                        "Howard",
                        RotationLaw::Howard,
                        "Howard, Harvey and Forgach 1990: sunspots and plage",
                    ),
                    (
                        "Snodgrass",
                        RotationLaw::Snodgrass,
                        "Snodgrass and Ulrich 1990: Doppler features",
                    ),
                ] {
                    if widgets::chip(ui, label, self.tl.law == l, idle)
                        .on_hover_text(tip)
                        .clicked()
                    {
                        self.tl.law = l;
                    }
                }
            });
            let mut north = self.tl.celestial_north_up;
            if ui
                .add_enabled(idle, egui::Checkbox::new(&mut north, "Celestial north is up"))
                .on_hover_text(
                    "An equatorial mount with no flip: solar north is then turned by the angle P from each capture's time. Off: solar north is up.",
                )
                .changed()
            {
                self.tl.celestial_north_up = north;
            }
        }
        ui.add_space(4.0);

        widgets::overline(ui, "Times");
        ui.horizontal(|ui| {
            ui.add(Label::new(mono_text("DATE", 11.0, p.faint)));
            let bad = self.tl.date_is_bad();
            let r = ui.add_enabled(
                idle,
                egui::TextEdit::singleline(&mut self.tl.date)
                    .hint_text("YYYY-MM-DD")
                    .desired_width(110.0)
                    .text_color(if bad { p.accent } else { p.text }),
            );
            r.on_hover_text(
                "UTC date for file names that hold only the time of day (SharpCap's 15_32_09.avi). A SER file stamps its own frames.",
            );
            ui.add(Label::new(mono_text("UTC+", 11.0, p.faint)));
            ui.add_enabled(
                idle,
                egui::DragValue::new(&mut self.tl.utc_offset)
                    .range(-12.0..=14.0)
                    .speed(0.1)
                    .max_decimals(1),
            )
            .on_hover_text("Hours east of UTC of the clock that named the files (1 for British Summer Time)");
        });
        ui.add_space(4.0);

        widgets::overline(ui, "Movie");
        ui.horizontal_wrapped(|ui| {
            for (label, m, tip) in [
                (
                    "Auto",
                    MovieChoice::Auto,
                    "MP4 when ffmpeg is installed, else an animated PNG",
                ),
                ("MP4", MovieChoice::Mp4, "Needs ffmpeg"),
                (
                    "APNG",
                    MovieChoice::Apng,
                    "An animated PNG, which needs nothing else",
                ),
                ("None", MovieChoice::None, "The numbered frames only"),
            ] {
                if widgets::chip(ui, label, self.tl.movie == m, true)
                    .on_hover_text(tip)
                    .clicked()
                {
                    self.tl.movie = m;
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.add(Label::new(mono_text("FRAMES", 11.0, p.faint)));
            for (label, f) in [("PNG", FrameFormat::Png), ("TIFF", FrameFormat::Tiff)] {
                if widgets::chip(ui, label, self.tl.format == f, true).clicked() {
                    self.tl.format = f;
                }
            }
            ui.add(Label::new(mono_text("FPS", 11.0, p.faint)));
            ui.add(
                egui::DragValue::new(&mut self.tl.fps)
                    .range(1.0..=60.0)
                    .speed(0.2)
                    .max_decimals(0),
            )
            .on_hover_text("Frames a second, for the playback and the movie");
        });
        ui.add_space(6.0);

        let n = self.tl.captures.len();
        let busy = self.tl.running.is_some() || self.run.is_some();
        let label = if self.tl.running.is_some() {
            "Working\u{2026}".to_string()
        } else {
            format!("Make time-lapse of {n}")
        };
        if widgets::button(
            ui,
            &label,
            Kind::Primary,
            n >= 2 && !busy && !self.tl.date_is_bad(),
            ui.available_width(),
        )
        .on_hover_text("Stack every capture, register the stacks and play them back")
        .clicked()
        {
            self.tl_start();
        }
        if let Some(r) = &self.tl.running {
            widgets::progress_bar(ui, r.fraction);
            ui.add(Label::new(mono_text(
                format!("{} \u{b7} {:.0}%", r.text, r.fraction * 100.0),
                12.5,
                p.muted,
            )));
            if widgets::chip(ui, "Cancel", false, true).clicked() {
                self.tl_cancel();
            }
        } else if let Some(e) = &self.tl.error {
            ui.add(Label::new(mono_text(e.clone(), 12.5, p.accent)).wrap());
        }
        if let Some(b) = self.tl.built.clone() {
            for l in tl::summary_lines(&b.sequence) {
                ui.add(Label::new(mono_text(l, 12.5, p.muted)).wrap());
            }
            if widgets::chip(ui, "Save time-lapse", false, !busy)
                .on_hover_text("Choose a folder: the numbered frames, the movie and timelapse.csv")
                .clicked()
            {
                self.tl_save();
            }
            if widgets::chip(ui, "Save diagnostics", false, !busy)
                .on_hover_text(
                    "Choose a folder: how each frame was registered, the brightness and the derotation, as plots, data and report.html",
                )
                .clicked()
            {
                self.tl_save_diagnostics();
            }
        }
    }

    // ----- the centre -----

    /// Zoom controls for the time-lapse viewer.
    pub(crate) fn timelapse_toolbar(&mut self, ui: &mut Ui) {
        let has = self.tl.shown.is_some();
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::icon_button(ui, Icon::Plus, "Zoom in", has, false).clicked() {
                    self.view.zoom_about(egui::Vec2::ZERO, ZOOM_STEP);
                }
                if widgets::chip(ui, &format!("{}%", self.view.percent()), self.view.fit, has)
                    .on_hover_text("Fit to window (or double-click the image)")
                    .clicked()
                {
                    self.view.reset();
                }
                if widgets::icon_button(ui, Icon::Minus, "Zoom out", has, false).clicked() {
                    self.view.zoom_about(egui::Vec2::ZERO, 1.0 / ZOOM_STEP);
                }
            });
        });
    }

    /// The label above the scrubber.
    fn tl_label(&self) -> String {
        let Some(b) = &self.tl.built else {
            return String::new();
        };
        let n = b.sequence.frames.len();
        let i = self.tl.index.min(n.saturating_sub(1));
        let r = &b.sequence.report[i];
        let when = r.time.map_or(String::new(), |t| {
            format!(
                "  \u{b7}  {} UTC",
                helia_core::io::solarnet::iso_from_unix(t).replace('T', " ")[..19].to_owned()
            )
        });
        format!(
            "Frame {} of {n}{when}  \u{b7}  shift ({:+.1}, {:+.1}) px  \u{b7}  {}",
            i + 1,
            r.shift.dx,
            r.shift.dy,
            r.method.label()
        )
    }

    /// Play button, slider and the registration of the frame.
    pub(crate) fn timelapse_scrubber(&mut self, ui: &mut Ui) {
        let p = palette();
        let n = self.tl.frames();
        if n == 0 {
            return;
        }
        let label = self.tl_label();
        ui.horizontal(|ui| {
            let play = if self.tl.playing { "Pause" } else { "Play" };
            if widgets::chip(ui, play, self.tl.playing, true)
                .on_hover_text("Play the frames at the chosen rate (Space)")
                .clicked()
            {
                self.tl.playing = !self.tl.playing;
                self.tl.last_step = None;
            }
            ui.add(Label::new(
                RichText::new(label).font(mono(13.0)).color(p.text),
            ));
        });
        let mut idx = self.tl.index + 1;
        ui.spacing_mut().slider_width = (ui.available_width() - 12.0).max(80.0);
        let s = ui
            .add(
                Slider::new(&mut idx, 1..=n)
                    .show_value(false)
                    .trailing_fill(true),
            )
            .on_hover_text("Scrub the frames (left and right arrow keys)");
        if s.changed() {
            self.tl.playing = false;
        }
        self.tl.index = (idx - 1).min(n - 1);
        if let Some(b) = &self.tl.built {
            let r = &b.sequence.report[self.tl.index];
            let mut parts = Vec::new();
            if let Some((_, psr)) = r.phase {
                parts.push(format!("phase PSR {psr:.0}"));
            }
            if let Some(l) = r.limb {
                parts.push(format!("limb confidence {:.2}", l.3));
            }
            if let Some(d) = r.derotated_px {
                parts.push(format!("surface moved up to {d:.1} px"));
            }
            if (r.gain - 1.0).abs() > 1e-4 {
                parts.push(format!("brightness gain {:.3}", r.gain));
            }
            ui.add(Label::new(mono_text(
                parts.join("  \u{b7}  "),
                12.0,
                p.faint,
            )));
        }
    }

    /// The viewer of the Time-lapse step.
    pub(crate) fn timelapse_viewer(&mut self, ui: &mut Ui) {
        let p = palette();
        let size = ui.available_size();
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
        resp.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Label,
                true,
                "Time-lapse viewer. Scroll to zoom, drag to pan, double-click to fit.",
            )
        });
        let painter = ui.painter_at(rect);
        let radius = CornerRadius::same(RADIUS_LARGE);
        painter.rect_filled(rect, radius, p.viewer);
        let Some((tex, tsize, _)) = self.tl.shown.clone() else {
            let text = if self.tl.running.is_some() {
                "Stacking and registering\u{2026}"
            } else {
                "Add two or more captures, then press Make time-lapse"
            };
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                text,
                sans(13.0),
                p.muted,
            );
            painter.rect_stroke(rect, radius, Stroke::new(1.0, p.border), StrokeKind::Inside);
            return;
        };
        let img = vec2(tsize[0] as f32, tsize[1] as f32);
        if resp.hovered() {
            let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let factor = (scroll * 0.0015).exp() * pinch;
            if (factor - 1.0).abs() > 1e-4
                && let Some(pos) = ui.input(|i| i.pointer.hover_pos())
            {
                self.view.settle(rect.size(), img);
                self.view.zoom_about(pos - rect.center(), factor);
            }
        }
        if resp.dragged() {
            self.view.settle(rect.size(), img);
            self.view.fit = false;
            self.view.pan += resp.drag_delta();
        }
        if resp.double_clicked() {
            self.view.reset();
        }
        self.view.settle(rect.size(), img);
        let image_rect = self.view.image_rect(rect, img);
        painter.image(
            tex.id(),
            image_rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        painter.rect_stroke(rect, radius, Stroke::new(1.0, p.border), StrokeKind::Inside);
        let tag = format!(
            "Time-lapse \u{b7} frame {} of {}",
            self.tl.index + 1,
            self.tl.frames()
        );
        let g = painter.layout_no_wrap(tag, mono(12.0), p.muted);
        let pill = Rect::from_min_size(
            rect.left_bottom() + vec2(12.0, -12.0 - 26.0),
            g.size() + vec2(20.0, 10.0),
        );
        painter.rect(
            pill,
            CornerRadius::same(13),
            Color32::from_rgba_unmultiplied(8, 7, 7, 200),
            Stroke::new(1.0, p.border),
            StrokeKind::Inside,
        );
        painter.galley(pill.min + vec2(10.0, 5.0), g, p.muted);
    }

    /// The rail line of the step.
    #[must_use]
    pub fn tl_status(&self) -> (String, crate::model::Tone) {
        use crate::model::Tone;
        if let Some(r) = &self.tl.running {
            return (format!("{:.0}%", r.fraction * 100.0), Tone::Running);
        }
        if let Some(b) = &self.tl.built {
            return (format!("{} frames", b.sequence.frames.len()), Tone::Done);
        }
        match self.tl.captures.len() {
            0 => ("Optional".into(), Tone::Idle),
            n => (format!("{n} captures"), Tone::Idle),
        }
    }

    /// Keys of the step: arrows and Home/End scrub, Space plays.
    pub(crate) fn tl_keys(&mut self, keys: Keys, big: usize) {
        let Keys {
            left,
            right,
            home,
            end,
            space,
        } = keys;
        let n = self.tl.frames();
        if n == 0 {
            return;
        }
        if left || right || home || end {
            self.tl.playing = false;
        }
        if left {
            self.tl.index = self.tl.index.saturating_sub(big);
        }
        if right {
            self.tl.index = (self.tl.index + big).min(n - 1);
        }
        if home {
            self.tl.index = 0;
        }
        if end {
            self.tl.index = n - 1;
        }
        if space {
            self.tl.playing = !self.tl.playing;
            self.tl.last_step = None;
        }
    }

    /// Make the step's own hint for the rail.
    #[must_use]
    pub fn tl_hint(&self) -> &'static str {
        if self.tl.built.is_some() {
            "Scrub, then save"
        } else if self.tl.captures.len() >= 2 {
            "Press Make time-lapse"
        } else {
            "Add two or more captures"
        }
    }

    /// Frame for a screenshot of a small series; used by the tests.
    #[cfg(test)]
    pub fn tl_set_built(&mut self, ctx: &egui::Context, built: Built) {
        self.tl.built = Some(Arc::new(built));
        self.tl.shown = None;
        self.tl_sync(ctx);
    }
}

/// Is `path` an image (already stacked) rather than a capture?
pub(crate) fn is_stacked_image(path: &std::path::Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "tif" | "tiff" | "png" | "fits" | "fit" | "fts"
        )
    })
}

/// File types the step takes: captures, and stacks that are already made.
pub(crate) const INPUT_TYPES: [&str; 13] = [
    "ser", "avi", "mp4", "mov", "mkv", "webm", "m4v", "tif", "tiff", "png", "fits", "fit", "fts",
];

/// How a capture is stacked and what is done to the stack on its way to a picture.
pub(crate) struct StackJob<'a> {
    /// The stack settings.
    pub cfg: &'a helia_core::pipeline::StackConfig,
    /// How to read the capture.
    pub opts: &'a helia_core::io::source::OpenOptions,
    /// The backend.
    pub backend: helia_gpu::BackendChoice,
    /// The edits and the colour look.
    pub plan: &'a OutputPlan,
}

/// Stack every capture in order and return the pictures (the stacks with the
/// edits and the colour look). `share` is the part of the progress, 0 to 1,
/// the stacking takes.
pub(crate) fn stack_all(
    paths: &[PathBuf],
    job: &StackJob<'_>,
    cancel: &AtomicBool,
    share: f32,
    report: &(dyn Fn(String, f32) + Sync),
) -> Result<Vec<Picture>, String> {
    let backend = select_backend(job.backend).map_err(|e| e.to_string())?;
    let n = paths.len();
    let mut pictures = Vec::with_capacity(n);
    for (k, path) in paths.iter().enumerate() {
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let each = share / n as f32;
        let at = k as f32 * each;
        // An image that is already stacked is used as it is.
        if is_stacked_image(path) {
            report(format!("Reading {} of {n}: {name}", k + 1), at);
            let (pic, _) =
                helia_core::io::any::read_any(path).map_err(|e| format!("{name}: {e}"))?;
            pictures.push(pic);
            continue;
        }
        let mut src = open_capture(path, job.opts).map_err(|e| format!("{name}: {e}"))?;
        let progress = |stage: helia_core::pipeline::Stage, f: f32| {
            report(
                format!("Stacking {} of {n}: {name}, {}", k + 1, stage.label()),
                at + each * crate::model::overall_progress(stage, f),
            );
        };
        let out = Pipeline::new(job.cfg.clone())
            .with_backend(Arc::clone(&backend))
            .run(&mut src, &progress, cancel)
            .map_err(|e| format!("{name}: {e}"))?;
        let mut plan = job.plan.clone();
        plan.rgb = out.colour.as_ref().map(|c| c.rgb.clone());
        pictures.push(render(backend.as_ref(), &out.image, &plan).map_err(|e| e.to_string())?);
    }
    Ok(pictures)
}

/// Stack every capture, register the stacks and return the series.
#[allow(clippy::too_many_arguments, reason = "everything the run is made from")]
fn build(
    captures: &[PathBuf],
    job: &StackJob<'_>,
    options: &Options,
    hints: &TimeHints,
    cancel: &AtomicBool,
    report: &(dyn Fn(String, f32) + Sync),
) -> Result<Built, String> {
    let times: Vec<Option<f64>> = captures
        .iter()
        .map(|p| capture_time(p, hints).map(|t| t.unix))
        .collect();
    let order = tl::order_by_time(&times);
    let ordered: Vec<PathBuf> = order.iter().map(|&i| captures[i].clone()).collect();
    let pictures = stack_all(&ordered, job, cancel, 0.85, report)?;
    report("Registering".into(), 0.86);
    let sorted_times: Vec<Option<f64>> = order.iter().map(|&i| times[i]).collect();
    let sequence = tl::register(
        &pictures,
        &sorted_times,
        options,
        &|f| report("Registering".into(), 0.86 + 0.14 * f),
        cancel,
    )
    .map_err(|e| e.to_string())?;
    let sources = ordered
        .iter()
        .map(|p| {
            p.file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
        })
        .collect();
    Ok(Built { sequence, sources })
}
