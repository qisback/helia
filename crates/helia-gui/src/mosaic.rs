// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The Mosaic step: panels stacked with the settings of the Stack step, matched
//! on their overlaps, placed together and joined into one picture.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, Label, Layout, Rect, Sense, Shape, Stroke,
    StrokeKind, TextureHandle, Ui, pos2, vec2,
};
use helia_core::captime::{TimeHints, capture_time};
use helia_core::derotate::{Orientation, RotationLaw};
use helia_core::mosaic::{self as mo, Derotation, Mosaic, Options, Target};
use helia_core::picture::Picture;

use crate::activity::ToastKind;
use crate::app::App;
use crate::model::ZOOM_STEP;
use crate::theme::{RADIUS_LARGE, mono, palette, sans};
use crate::timelapse::{INPUT_TYPES, StackJob, stack_all, to_texture};
use crate::widgets::{self, Icon, Kind, mono_text};
use crate::worker::Event;

/// Most panels the step takes.
pub const MAX_PANELS: usize = 64;

/// Messages from the mosaic worker threads.
pub enum MosaicEvent {
    /// The file dialog closed with these captures (empty when cancelled).
    Picked(Vec<PathBuf>),
    /// Progress: what is being done and how far, 0 to 1.
    Progress(u64, String, f32),
    /// The mosaic is joined, or why not.
    Done(u64, Result<Box<MosaicBuilt>, String>),
    /// The mosaic was written (`None` when the dialog was cancelled).
    Saved(Option<Result<PathBuf, String>>),
}

/// A finished mosaic and where it came from.
pub struct MosaicBuilt {
    /// The mosaic.
    pub mosaic: Mosaic,
    /// The file name of each panel.
    pub names: Vec<String>,
    /// The size of each panel.
    pub sizes: Vec<(usize, usize)>,
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
pub struct MosaicState {
    /// The panels, in the order added; the first is the reference.
    pub panels: Vec<PathBuf>,
    /// The most the panels may be turned against each other, degrees.
    pub max_rotation: f32,
    /// Width of the blend, pixels.
    pub feather: f32,
    /// Match the gain and offset of the panels.
    pub gain_match: bool,
    /// Join along seams of least difference.
    pub seams: bool,
    /// Bring the panels to one time first.
    pub derotate: bool,
    /// The rotation law.
    pub law: RotationLaw,
    /// The panels have celestial north up.
    pub celestial_north_up: bool,
    /// UTC date for file names that hold only a time of day.
    pub date: String,
    /// Hours east of UTC of the clock that named the files.
    pub utc_offset: f32,
    /// A run in progress.
    pub running: Option<Running>,
    /// Number of the newest run.
    pub run_counter: u64,
    /// The joined mosaic.
    pub built: Option<Arc<MosaicBuilt>>,
    /// Its texture.
    pub shown: Option<(TextureHandle, [usize; 2])>,
    /// Draw the outline of every panel on the mosaic.
    pub outlines: bool,
    /// What went wrong, in words.
    pub error: Option<String>,
}

impl Default for MosaicState {
    fn default() -> Self {
        let d = Options::default();
        Self {
            panels: Vec::new(),
            max_rotation: d.max_rotation_deg as f32,
            feather: d.feather,
            gain_match: true,
            seams: true,
            derotate: false,
            law: RotationLaw::Howard,
            celestial_north_up: false,
            date: String::new(),
            utc_offset: 0.0,
            running: None,
            run_counter: 0,
            built: None,
            shown: None,
            outlines: true,
            error: None,
        }
    }
}

impl MosaicState {
    /// The joining settings as the core takes them.
    #[must_use]
    pub fn options(&self) -> Options {
        Options {
            max_rotation_deg: f64::from(self.max_rotation),
            feather: self.feather,
            match_levels: self.gain_match,
            seams: self.seams,
            derotate: self.derotate.then_some(Derotation {
                law: self.law,
                orient: Orientation {
                    celestial_north_up: self.celestial_north_up,
                    ..Orientation::default()
                },
                target: Target::Middle,
                preserve_contrast: true,
            }),
            ..Options::default()
        }
    }

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

impl App {
    /// Add panels to the list, skipping repeats.
    pub fn mo_add(&mut self, paths: Vec<PathBuf>) {
        for p in paths {
            if self.mo.panels.len() < MAX_PANELS && !self.mo.panels.contains(&p) {
                self.mo.panels.push(p);
            }
        }
    }

    fn mo_pick(&self) {
        let w = self.workers.clone();
        let dir = self.prefs.last_export_dir.clone();
        std::thread::spawn(move || {
            let mut dlg =
                rfd::FileDialog::new().add_filter("Captures and stacked images", &INPUT_TYPES);
            if let Some(d) = dir.filter(|d| d.is_dir()) {
                dlg = dlg.set_directory(d);
            }
            w.send(Event::Mosaic(MosaicEvent::Picked(
                dlg.pick_files().unwrap_or_default(),
            )));
        });
    }

    /// Stack every panel with the settings of the Stack step and join them.
    pub fn mo_start(&mut self) {
        if self.mo.running.is_some() || self.mo.panels.len() < 2 {
            return;
        }
        self.mo.run_counter += 1;
        let id = self.mo.run_counter;
        let cancel = Arc::new(AtomicBool::new(false));
        self.mo.running = Some(Running {
            id,
            cancel: Arc::clone(&cancel),
            text: "Starting".into(),
            fraction: 0.0,
        });
        self.mo.error = None;
        let panels = self.mo.panels.clone();
        let (cfg, opts, backend) = (
            self.stack_config(),
            self.settings.open_options(),
            self.settings.backend,
        );
        let plan = self.settings.output_plan(None);
        let (options, hints) = (self.mo.options(), self.mo.hints());
        let w = self.workers.clone();
        std::thread::spawn(move || {
            let report = |t: String, f: f32| w.send(Event::Mosaic(MosaicEvent::Progress(id, t, f)));
            let res = (|| -> Result<MosaicBuilt, String> {
                let job = StackJob {
                    cfg: &cfg,
                    opts: &opts,
                    backend,
                    plan: &plan,
                };
                let pictures = stack_all(&panels, &job, &cancel, 0.7, &report)?;
                let times: Vec<Option<f64>> = panels
                    .iter()
                    .map(|p| capture_time(p, &hints).map(|t| t.unix))
                    .collect();
                let sizes = pictures.iter().map(Picture::size).collect();
                let mosaic = mo::build(
                    &pictures,
                    &times,
                    &options,
                    &|f| report("Joining".into(), 0.7 + 0.3 * f),
                    &cancel,
                )
                .map_err(|e| e.to_string())?;
                let names = panels
                    .iter()
                    .map(|p| {
                        p.file_name()
                            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
                    })
                    .collect();
                Ok(MosaicBuilt {
                    mosaic,
                    names,
                    sizes,
                })
            })();
            w.send(Event::Mosaic(MosaicEvent::Done(id, res.map(Box::new))));
        });
    }

    /// Stop the run in flight.
    pub fn mo_cancel(&self) {
        if let Some(r) = &self.mo.running {
            r.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Ask where to save the mosaic and write it with its tables.
    fn mo_save(&self) {
        let Some(built) = self.mo.built.clone() else {
            return;
        };
        let dir = self.prefs.last_export_dir.clone();
        let w = self.workers.clone();
        std::thread::spawn(move || {
            let mut dlg = rfd::FileDialog::new()
                .set_file_name("mosaic.tif")
                .add_filter("TIFF 16-bit", &["tif", "tiff"])
                .add_filter("PNG 8-bit", &["png"])
                .add_filter("FITS", &["fits"]);
            if let Some(d) = dir.filter(|d| d.is_dir()) {
                dlg = dlg.set_directory(d);
            }
            let Some(path) = dlg.save_file() else {
                w.send(Event::Mosaic(MosaicEvent::Saved(None)));
                return;
            };
            let res = (|| -> Result<PathBuf, String> {
                let m = &built.mosaic;
                let notes = helia_core::io::ImageNotes {
                    lines: mo::summary_lines(m)
                        .into_iter()
                        .map(|l| format!("Helia mosaic: {l}"))
                        .collect(),
                    fits: None,
                };
                helia_core::picture::save(&path, &m.image, &notes).map_err(|e| e.to_string())?;
                let stem = path.with_extension("");
                let name = stem
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                std::fs::write(
                    stem.with_file_name(format!("{name}-panels.csv")),
                    mo::panels_csv(m, &built.names),
                )
                .map_err(|e| e.to_string())?;
                std::fs::write(
                    stem.with_file_name(format!("{name}-pairs.csv")),
                    mo::pairs_csv(m),
                )
                .map_err(|e| e.to_string())?;
                Ok(path)
            })();
            w.send(Event::Mosaic(MosaicEvent::Saved(Some(res))));
        });
    }

    /// Handle a message from the worker.
    pub fn on_mosaic(&mut self, ctx: &egui::Context, ev: MosaicEvent) {
        match ev {
            MosaicEvent::Picked(paths) => self.mo_add(paths),
            MosaicEvent::Progress(id, text, f) => {
                if let Some(r) = self.mo.running.as_mut().filter(|r| r.id == id) {
                    r.text = text;
                    r.fraction = f;
                }
            }
            MosaicEvent::Done(id, res) => {
                if self.mo.running.as_ref().is_none_or(|r| r.id != id) {
                    return;
                }
                self.mo.running = None;
                match res {
                    Ok(b) => {
                        self.mo.shown = Some(to_texture(ctx, "mosaic", &b.mosaic.image));
                        self.mo.built = Some(Arc::new(*b));
                        self.view.reset();
                    }
                    Err(e) if e.contains("cancel") => {
                        self.toast(ToastKind::Info, "Mosaic cancelled.");
                    }
                    Err(e) => {
                        self.mo.error = Some(e.clone());
                        self.toast(ToastKind::Error, format!("Mosaic failed: {e}"));
                    }
                }
            }
            MosaicEvent::Saved(None) => {}
            MosaicEvent::Saved(Some(Ok(p))) => {
                self.prefs.remember_export_dir(&p);
                self.toast(
                    ToastKind::Success,
                    format!("Saved {} and its tables", p.display()),
                );
            }
            MosaicEvent::Saved(Some(Err(e))) => {
                self.toast(ToastKind::Error, format!("Could not save: {e}"));
            }
        }
    }

    // ----- the inspector -----

    pub(crate) fn mosaic_body(&mut self, ui: &mut Ui) {
        let p = palette();
        let idle = self.mo.running.is_none();
        widgets::overline(ui, "Panels");
        if self.mo.panels.is_empty() {
            ui.add(
                Label::new(mono_text(
                    "None yet. Add the captures of the panels, each sharing a strip of the Sun with a neighbour.",
                    12.5,
                    p.muted,
                ))
                .wrap(),
            );
        }
        let mut remove = None;
        for (i, c) in self.mo.panels.iter().enumerate() {
            ui.horizontal(|ui| {
                let name = c
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                let tag = if i == 0 { " (reference)" } else { "" };
                ui.add(
                    Label::new(mono_text(
                        format!("{:>2}  {name}{tag}", i + 1),
                        12.5,
                        p.text,
                    ))
                    .truncate(),
                )
                .on_hover_text(c.display().to_string());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::icon_button(ui, Icon::Minus, "Remove this panel", idle, false)
                        .clicked()
                    {
                        remove = Some(i);
                    }
                });
            });
        }
        if let Some(i) = remove {
            self.mo.panels.remove(i);
        }
        ui.horizontal_wrapped(|ui| {
            if widgets::chip(ui, "Add panels", false, idle)
                .on_hover_text(
                    "Choose captures (SER, AVI, video) or stacked images (TIFF, PNG, FITS)",
                )
                .clicked()
            {
                self.mo_pick();
            }
            let open = self.capture.as_ref().map(|c| c.path.clone());
            let can = idle && open.as_ref().is_some_and(|o| !self.mo.panels.contains(o));
            if widgets::chip(ui, "Add the open capture", false, can).clicked()
                && let Some(o) = open
            {
                self.mo_add(vec![o]);
            }
            if widgets::chip(ui, "Clear", false, idle && !self.mo.panels.is_empty()).clicked() {
                self.mo.panels.clear();
            }
        });
        ui.add(
            Label::new(mono_text(
                format!(
                    "Each is stacked with the Stack step's settings: best {:.0}% of the frames. The first is the reference: the mosaic has its orientation and brightness.",
                    self.settings.keep_percent
                ),
                12.0,
                p.faint,
            ))
            .wrap(),
        );
        ui.add_space(4.0);

        widgets::overline(ui, "Joining");
        Self::row_label(
            ui,
            "Largest turn",
            &format!("{:.1} deg", self.mo.max_rotation),
        );
        ui.add_enabled(
            idle,
            egui::DragValue::new(&mut self.mo.max_rotation)
                .range(0.0..=10.0)
                .speed(0.05)
                .max_decimals(1),
        )
        .on_hover_text(
            "The most the panels may be turned against each other, degrees. 0 holds them square to the first.",
        );
        Self::row_label(ui, "Feather", &format!("{:.0} px", self.mo.feather));
        ui.add_enabled(
            idle,
            egui::DragValue::new(&mut self.mo.feather)
                .range(0.0..=60.0)
                .speed(0.5)
                .max_decimals(0),
        )
        .on_hover_text("Width of the blend across a seam and at a panel's edge");
        let mut gain = self.mo.gain_match;
        if ui
            .add_enabled(idle, egui::Checkbox::new(&mut gain, "Match brightness"))
            .on_hover_text("Scale and shift each panel so that the panels agree on their overlaps")
            .changed()
        {
            self.mo.gain_match = gain;
        }
        let mut seams = self.mo.seams;
        if ui
            .add_enabled(idle, egui::Checkbox::new(&mut seams, "Seams of least difference"))
            .on_hover_text("Join along the path across each overlap where the panels differ least; off blends by distance from each panel's edge")
            .changed()
        {
            self.mo.seams = seams;
        }
        ui.add_space(4.0);

        widgets::overline(ui, "Derotation");
        ui.horizontal_wrapped(|ui| {
            for (label, on, tip) in [
                ("Off", false, "Leave the surface as it was at each capture"),
                (
                    "To middle time",
                    true,
                    "Bring every panel to the middle time of the panels by the Sun's differential rotation before joining. Needs the time of every capture and enough of the limb in the mosaic to fit the disc.",
                ),
            ] {
                if widgets::chip(ui, label, self.mo.derotate == on, idle)
                    .on_hover_text(tip)
                    .clicked()
                {
                    self.mo.derotate = on;
                }
            }
        });
        if self.mo.derotate {
            ui.horizontal_wrapped(|ui| {
                ui.add(Label::new(mono_text("LAW", 11.0, p.faint)));
                for (label, l) in [
                    ("Howard", RotationLaw::Howard),
                    ("Snodgrass", RotationLaw::Snodgrass),
                ] {
                    if widgets::chip(ui, label, self.mo.law == l, idle).clicked() {
                        self.mo.law = l;
                    }
                }
            });
            let mut north = self.mo.celestial_north_up;
            if ui
                .add_enabled(
                    idle,
                    egui::Checkbox::new(&mut north, "Celestial north is up"),
                )
                .changed()
            {
                self.mo.celestial_north_up = north;
            }
        }
        ui.add_space(4.0);

        widgets::overline(ui, "Times");
        ui.horizontal(|ui| {
            ui.add(Label::new(mono_text("DATE", 11.0, p.faint)));
            let bad = self.mo.date_is_bad();
            ui.add_enabled(
                idle,
                egui::TextEdit::singleline(&mut self.mo.date)
                    .hint_text("YYYY-MM-DD")
                    .desired_width(110.0)
                    .text_color(if bad { p.accent } else { p.text }),
            )
            .on_hover_text(
                "UTC date for file names that hold only the time of day. Only the derotation needs the times.",
            );
            ui.add(Label::new(mono_text("UTC+", 11.0, p.faint)));
            ui.add_enabled(
                idle,
                egui::DragValue::new(&mut self.mo.utc_offset)
                    .range(-12.0..=14.0)
                    .speed(0.1)
                    .max_decimals(1),
            );
        });
        ui.add_space(6.0);

        let n = self.mo.panels.len();
        let busy = self.mo.running.is_some() || self.run.is_some();
        let label = if self.mo.running.is_some() {
            "Working\u{2026}".to_string()
        } else {
            format!("Make mosaic of {n}")
        };
        if widgets::button(
            ui,
            &label,
            Kind::Primary,
            n >= 2 && !busy && !self.mo.date_is_bad(),
            ui.available_width(),
        )
        .on_hover_text("Stack every panel, match the overlaps and join them")
        .clicked()
        {
            self.mo_start();
        }
        if let Some(r) = &self.mo.running {
            widgets::progress_bar(ui, r.fraction);
            ui.add(Label::new(mono_text(
                format!("{} \u{b7} {:.0}%", r.text, r.fraction * 100.0),
                12.5,
                p.muted,
            )));
            if widgets::chip(ui, "Cancel", false, true).clicked() {
                self.mo_cancel();
            }
        } else if let Some(e) = &self.mo.error {
            ui.add(Label::new(mono_text(e.clone(), 12.5, p.accent)).wrap());
        }
        if let Some(b) = self.mo.built.clone() {
            for l in mo::summary_lines(&b.mosaic) {
                ui.add(Label::new(mono_text(l, 12.5, p.muted)).wrap());
            }
            ui.horizontal_wrapped(|ui| {
                if widgets::chip(ui, "Panel outlines", self.mo.outlines, true)
                    .on_hover_text("Draw the outline of every panel on the mosaic")
                    .clicked()
                {
                    self.mo.outlines = !self.mo.outlines;
                }
                if widgets::chip(ui, "Save mosaic", false, !busy)
                    .on_hover_text(
                        "Write the mosaic and, beside it, NAME-panels.csv and NAME-pairs.csv",
                    )
                    .clicked()
                {
                    self.mo_save();
                }
            });
        }
    }

    // ----- the centre -----

    /// Zoom controls for the mosaic viewer.
    pub(crate) fn mosaic_toolbar(&mut self, ui: &mut Ui) {
        let has = self.mo.shown.is_some();
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::icon_button(ui, Icon::Plus, "Zoom in", has, false).clicked() {
                    self.view.zoom_about(egui::Vec2::ZERO, ZOOM_STEP);
                }
                if widgets::chip(ui, &format!("{}%", self.view.percent()), self.view.fit, has)
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

    /// The viewer of the Mosaic step.
    pub(crate) fn mosaic_viewer(&mut self, ui: &mut Ui) {
        let p = palette();
        let size = ui.available_size();
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
        resp.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Label,
                true,
                "Mosaic viewer. Scroll to zoom, drag to pan, double-click to fit.",
            )
        });
        let painter = ui.painter_at(rect);
        let radius = CornerRadius::same(RADIUS_LARGE);
        painter.rect_filled(rect, radius, p.viewer);
        let Some((tex, tsize)) = self.mo.shown.clone() else {
            let text = if self.mo.running.is_some() {
                "Stacking and joining the panels\u{2026}"
            } else {
                "Add two or more panels, then press Make mosaic"
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
        if self.mo.outlines
            && let Some(b) = &self.mo.built
        {
            let clip = painter.with_clip_rect(rect);
            let zoom = self.view.zoom;
            let to_screen = |x: f64, y: f64| {
                pos2(
                    image_rect.min.x + x as f32 * zoom,
                    image_rect.min.y + y as f32 * zoom,
                )
            };
            for (k, (pr, &(w, h))) in b.mosaic.panels.iter().zip(&b.sizes).enumerate() {
                let r = helia_core::mosaic::pair::Rigid {
                    tx: pr.x,
                    ty: pr.y,
                    theta: pr.angle_deg.to_radians(),
                };
                let c = ((w as f64 - 1.0) / 2.0, (h as f64 - 1.0) / 2.0);
                let corners: Vec<egui::Pos2> = [
                    (0.0, 0.0),
                    (w as f64 - 1.0, 0.0),
                    (w as f64 - 1.0, h as f64 - 1.0),
                    (0.0, h as f64 - 1.0),
                ]
                .iter()
                .map(|&(x, y)| {
                    let (qx, qy) = r.apply(x, y, c);
                    to_screen(qx - b.mosaic.origin.0, qy - b.mosaic.origin.1)
                })
                .collect();
                let colour = if k == 0 { p.accent } else { p.grid };
                clip.add(Shape::closed_line(
                    corners.clone(),
                    Stroke::new(1.5, colour),
                ));
                clip.text(
                    corners[0] + vec2(6.0, 6.0),
                    Align2::LEFT_TOP,
                    format!("{}", k + 1),
                    mono(12.0),
                    colour,
                );
            }
        }
        painter.rect_stroke(rect, radius, Stroke::new(1.0, p.border), StrokeKind::Inside);
        let tag = format!(
            "Mosaic of {} panels \u{b7} {} x {}",
            self.mo.panels.len(),
            tsize[0],
            tsize[1]
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
    pub fn mo_status(&self) -> (String, crate::model::Tone) {
        use crate::model::Tone;
        if let Some(r) = &self.mo.running {
            return (format!("{:.0}%", r.fraction * 100.0), Tone::Running);
        }
        if let Some(b) = &self.mo.built {
            return (format!("{} panels", b.mosaic.panels.len()), Tone::Done);
        }
        match self.mo.panels.len() {
            0 => ("Optional".into(), Tone::Idle),
            n => (format!("{n} panels"), Tone::Idle),
        }
    }

    /// Put a finished mosaic in the step, for a screenshot.
    #[cfg(test)]
    pub fn mo_set_built(&mut self, ctx: &egui::Context, built: MosaicBuilt) {
        self.mo.shown = Some(to_texture(ctx, "mosaic", &built.mosaic.image));
        self.mo.built = Some(Arc::new(built));
    }

    /// The hint under the Mosaic row.
    #[must_use]
    pub fn mo_hint(&self) -> &'static str {
        if self.mo.built.is_some() {
            "Check the seams, then save"
        } else if self.mo.panels.len() >= 2 {
            "Press Make mosaic"
        } else {
            "Add two or more panels"
        }
    }
}
