// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Right inspector: per-step settings and the footer.

use eframe::egui::{
    Align, DragValue, Frame, Label, Layout, Margin, Panel, RichText, ScrollArea, Slider, Stroke, Ui,
};
use helia_core::align::multipoint::ApGrid;
use helia_core::debayer::{Channel, Method};
use helia_core::pipeline::Stage;
use helia_core::quality::Metric;
use helia_core::stack::RobustKind;
use helia_core::truecolour::WhiteBalance;
use helia_core::{ColourParams, Preset};
use helia_gpu::BackendChoice;

use crate::app::App;
use crate::model::truncate;
use crate::model::{
    ExportFormat, STEP_COUNT, Scoring, Step, export_summary, median, quick_lines, snap_keep,
};
use crate::theme::{palette, semibold};
use crate::widgets::{self, Icon, Kind, mono_text};

const AP_SIZES: [usize; 9] = [32, 40, 48, 56, 64, 72, 80, 88, 96];

impl App {
    pub(crate) fn inspector(&mut self, ui: &mut Ui) {
        let p = palette();
        Panel::right("inspector")
            .resizable(false)
            .exact_size(360.0)
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(p.panel)
                    .inner_margin(Margin::same(20))
                    .stroke(Stroke::new(1.0, p.border)),
            )
            .show(ui, |ui| {
                Panel::bottom("inspector_footer")
                    .resizable(false)
                    .show_separator_line(false)
                    .frame(Frame::new().fill(p.panel).inner_margin(Margin {
                        top: 12,
                        ..Margin::ZERO
                    }))
                    .show(ui, |ui| self.footer(ui));
                ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 10.0;
                        widgets::overline(ui, &format!("{} of {STEP_COUNT}", self.step.number()));
                        ui.add(Label::new(
                            RichText::new(self.step.title())
                                .font(widgets::title_font())
                                .color(p.text),
                        ));
                        ui.add(Label::new(
                            RichText::new(self.step.explanation()).color(p.muted),
                        ));
                        ui.add_space(8.0);
                        match self.step {
                            Step::Open => self.open_body(ui),
                            Step::Calibrate => self.calibrate_body(ui),
                            Step::Sharpen => self.sharpen_body(ui),
                            Step::Adjust => self.adjust_body(ui),
                            Step::Analyse => self.analyse_body(ui),
                            Step::Align => self.align_body(ui),
                            Step::Stack => self.stack_body(ui),
                            Step::Colour => self.colour_body(ui),
                            Step::Export => self.export_body(ui),
                            Step::Timelapse => self.timelapse_body(ui),
                            Step::Mosaic => self.mosaic_body(ui),
                        }
                    });
            });
    }

    /// Hue, saturation, brightness, black and white points and per-channel
    /// curves on top of the look, with saved presets.
    fn grade_controls(&mut self, ui: &mut Ui) {
        use helia_core::grade::Grade;
        let p = palette();
        widgets::overline(ui, "Grade");
        let mut changed = false;
        {
            let g = &mut self.settings.colour.grade;
            changed |= grade_row(
                ui,
                "Hue",
                &mut g.hue,
                -180.0..=180.0,
                (1.0, 0),
                "Rotate the colours round the colour wheel, in degrees",
            );
            changed |= grade_row(
                ui,
                "Saturation",
                &mut g.saturation,
                0.0..=2.0,
                (0.02, 2),
                "0 is grey, 1 leaves the look as it is, above 1 is stronger",
            );
            changed |= grade_row(
                ui,
                "Brightness",
                &mut g.gamma,
                0.3..=3.0,
                (0.02, 2),
                "Gamma: above 1 brightens the mid-tones, below 1 darkens them",
            );
            changed |= grade_row(
                ui,
                "Black point",
                &mut g.black,
                0.0..=0.9,
                (0.005, 3),
                "The mono value that becomes black",
            );
            changed |= grade_row(
                ui,
                "White point",
                &mut g.white,
                0.1..=1.0,
                (0.005, 3),
                "The mono value that becomes full brightness",
            );
            for (i, name) in ["Red curve", "Green curve", "Blue curve"]
                .into_iter()
                .enumerate()
            {
                changed |= grade_row(
                    ui,
                    name,
                    &mut g.curves[i],
                    0.3..=3.0,
                    (0.02, 2),
                    "A curve for this one channel: above 1 lifts its mid-tones, below 1 lowers them",
                );
            }
        }
        let is_identity = self.settings.colour.grade.is_identity();
        if widgets::button(ui, "Reset grade", Kind::Outline, !is_identity, 0.0)
            .on_hover_text("Back to the look as the band defines it")
            .clicked()
        {
            self.settings.colour.grade = Grade::default();
            changed = true;
        }
        let mut load = None;
        let mut delete = None;
        widgets::overline(ui, "My colour presets");
        if self.grade_presets.is_empty() {
            ui.add(Label::new(mono_text(
                "None yet. Name the current grade to keep it.",
                11.5,
                p.faint,
            )));
        } else {
            ui.horizontal_wrapped(|ui| {
                for pr in &self.grade_presets {
                    if widgets::chip(ui, &pr.name, false, true)
                        .on_hover_text(format!("Use this grade: {}", pr.grade.to_text()))
                        .clicked()
                    {
                        load = Some(pr.grade);
                    }
                    if widgets::icon_button(
                        ui,
                        Icon::Minus,
                        &format!("Delete {}", pr.name),
                        true,
                        false,
                    )
                    .clicked()
                    {
                        delete = Some(pr.name.clone());
                    }
                }
            });
        }
        ui.horizontal(|ui| {
            ui.add(
                eframe::egui::TextEdit::singleline(&mut self.grade_name)
                    .hint_text("Preset name")
                    .desired_width(ui.available_width() - 90.0),
            );
            let ok = !self.grade_name.trim().is_empty() && self.grade_dir.is_some();
            if widgets::button(ui, "Save", Kind::Outline, ok, 0.0)
                .on_hover_text("Keep the current grade as a preset")
                .clicked()
            {
                let name = self.grade_name.clone();
                self.save_grade_preset(&name);
            }
        });
        if let Some(g) = load {
            self.settings.colour.grade = g;
            changed = true;
        }
        if let Some(name) = delete {
            self.delete_grade_preset(&name);
        }
        if changed {
            self.colour_changed();
        }
    }

    pub(crate) fn row_label(ui: &mut Ui, left: &str, right: &str) {
        let p = palette();
        ui.horizontal(|ui| {
            widgets::overline(ui, left);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add(Label::new(mono_text(right, 13.0, p.text)));
            });
        });
    }

    /// The one-button path: what was detected, what will be used, and a
    /// single primary button. Everything else stays one click away in the
    /// other steps.
    fn quick_card(&mut self, ui: &mut Ui) {
        let p = palette();
        let Some(rec) = self.rec.clone() else { return };
        let frames = self.count();
        Frame::new()
            .fill(p.selected_fill)
            .stroke(Stroke::new(1.0, p.selected_border))
            .corner_radius(eframe::egui::CornerRadius::same(crate::theme::RADIUS_LARGE))
            .inner_margin(Margin::same(14))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                widgets::overline(ui, "Quick stack");
                let (title, source) = match (&self.settings.preset, &self.settings.preset_source) {
                    (Some(pr), Some(src)) if src == "chosen" => {
                        (format!("{} (your choice)", pr.name()), None)
                    }
                    (Some(pr), Some(src)) => (
                        format!("{} detected", pr.name()),
                        Some(format!("from \"{}\"", truncate(src, 34))),
                    ),
                    (Some(pr), None) => (pr.name().to_string(), None),
                    _ => ("Which filter was this?".to_string(), None),
                };
                ui.add(Label::new(
                    RichText::new(title).font(semibold(16.0)).color(p.text),
                ));
                if let Some(src) = source {
                    ui.add(Label::new(mono_text(src, 12.0, p.muted)).truncate());
                }
                let current = self.settings.preset;
                let mut pick = None;
                ui.horizontal_wrapped(|ui| {
                    for pr in Preset::ALL {
                        if widgets::chip(ui, pr.name(), current == Some(pr), !self.busy())
                            .on_hover_text(format!(
                                "{} ({})",
                                pr.info().summary,
                                pr.info().wavelength
                            ))
                            .clicked()
                        {
                            pick = Some(Some(pr));
                        }
                    }
                    if widgets::chip(ui, "Not sure", current.is_none(), !self.busy())
                        .on_hover_text("Use plain defaults: all channels, 25% of the frames")
                        .clicked()
                    {
                        pick = Some(None);
                    }
                });
                if let Some(choice) = pick {
                    self.choose_filter(choice);
                }
                let rec = self.rec.clone().unwrap_or(rec);
                let bayer = self
                    .capture
                    .as_ref()
                    .is_some_and(|c| c.meta.bayer.is_some());
                for line in quick_lines(&rec, frames, bayer) {
                    ui.add(Label::new(mono_text(
                        format!("\u{b7} {line}"),
                        12.5,
                        p.muted,
                    )));
                }
                if let Some(pr) = rec.preset {
                    ui.add(Label::new(mono_text(pr.info().reason, 11.5, p.faint)).wrap());
                }
                ui.add_space(2.0);
                let ready = self.capture.is_some() && !self.busy();
                if widgets::button(
                    ui,
                    "Stack with recommended settings",
                    Kind::Primary,
                    ready,
                    ui.available_width(),
                )
                .on_hover_text("Score, align and stack with the settings listed above")
                .clicked()
                {
                    self.quick_stack();
                }
                ui.add(
                    Label::new(mono_text(
                        "Want to change something? The other steps hold the settings.",
                        11.5,
                        p.faint,
                    ))
                    .wrap(),
                );
            });
        ui.add_space(8.0);
    }

    fn open_body(&mut self, ui: &mut Ui) {
        let p = palette();
        self.capture_notices(ui);
        self.quick_card(ui);
        if let Some(c) = &self.capture {
            let m = &c.meta;
            let rows = [
                ("File", m.file.clone()),
                (
                    "Camera",
                    m.camera.clone().unwrap_or_else(|| "Unknown".into()),
                ),
                ("Size", format!("{} x {} px", m.width, m.height)),
                ("Frames", m.frames.to_string()),
            ];
            for (k, v) in rows {
                ui.horizontal(|ui| {
                    ui.add(Label::new(mono_text(k, 12.5, p.faint)));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.add(Label::new(mono_text(v, 12.5, p.text)).truncate());
                    });
                });
            }
            if let Some(note) = &m.note {
                ui.add_space(4.0);
                ui.add(Label::new(mono_text(note, 12.0, p.muted)).wrap());
            }
            ui.add_space(4.0);
        }
        if widgets::button(
            ui,
            "Open capture",
            Kind::Outline,
            !self.busy(),
            ui.available_width(),
        )
        .on_hover_text("Open a SER, AVI or video capture (Ctrl+O)")
        .clicked()
        {
            self.pick_capture();
        }
        ui.add_space(8.0);
        self.header_cards(ui);
        ui.add_space(2.0);
        self.override_controls(ui);
    }

    fn analyse_body(&mut self, ui: &mut Ui) {
        let p = palette();
        widgets::overline(ui, "Sharpness metric");
        let mut pick = None;
        ui.horizontal_wrapped(|ui| {
            for (label, m, tip) in [
                (
                    "Gradient energy",
                    Metric::GradientEnergy,
                    "Mean squared gradient; the default",
                ),
                (
                    "Laplacian variance",
                    Metric::LaplacianVariance,
                    "Variance of the Laplacian",
                ),
            ] {
                if widgets::chip(
                    ui,
                    label,
                    self.settings.metric == m,
                    self.capture.is_some() && !self.busy(),
                )
                .on_hover_text(tip)
                .clicked()
                {
                    pick = Some(m);
                }
            }
        });
        if let Some(m) = pick
            && m != self.settings.metric
        {
            self.settings.metric = m;
            self.start_scoring();
        }
        ui.add_space(4.0);
        let text = match (self.scoring, self.curve.len()) {
            (Scoring::Running, _) => "Scoring frames\u{2026}".to_string(),
            (Scoring::Done, n) if n > 0 => {
                let best = self.curve.frame_at_rank(0).map_or(0, |f| f + 1);
                format!("Scored {n} frames. Frame {best} is the sharpest.")
            }
            _ => "Open a capture to score its frames.".to_string(),
        };
        ui.add(Label::new(mono_text(text, 12.5, p.muted)));
    }

    fn align_body(&mut self, ui: &mut Ui) {
        let p = palette();
        widgets::overline(ui, "Alignment point size");
        let mut pick = None;
        let mut auto = None;
        ui.horizontal_wrapped(|ui| {
            if widgets::chip(ui, "Auto", self.settings.ap_auto, !self.busy())
                .on_hover_text(
                    "Choose the size, spacing and search range from the image scale and the seeing",
                )
                .clicked()
            {
                auto = Some(!self.settings.ap_auto);
            }
            for s in AP_SIZES {
                if widgets::chip(
                    ui,
                    &s.to_string(),
                    !self.settings.ap_auto && self.settings.ap_size == s,
                    true,
                )
                .on_hover_text(format!("{s} px windows; smaller follows finer detail"))
                .clicked()
                {
                    pick = Some(s);
                }
            }
        });
        if let Some(on) = auto {
            self.settings.ap_auto = on;
            self.show_grid = true;
        }
        if let Some(s) = pick {
            self.settings.ap_size = s;
            self.settings.ap_auto = false;
            self.show_grid = true;
        }
        ui.add_space(2.0);
        let note = match (
            self.settings.ap_auto,
            self.result.as_ref().and_then(|r| r.auto_ap.as_ref()),
        ) {
            (true, Some(c)) => format!("Chosen by the last stack: {}.", c.summary()),
            (true, None) => "When you stack, Helia fits the limb for the image scale and probes the seeing on a dozen frames, then picks the size, spacing and search range. The grid shown is the recommended size until then.".to_string(),
            (false, _) => "Pick Auto to let Helia choose from the image scale and the seeing when it stacks.".to_string(),
        };
        ui.add(Label::new(mono_text(note, 12.0, p.faint)).wrap());
        ui.add_space(4.0);
        let count = self.display_grid().map(ApGrid::active_count);
        let drift = self.result.as_ref().and_then(|r| {
            let mags: Vec<f32> = r.shifts.iter().map(|(_, s)| s.dx.hypot(s.dy)).collect();
            median(&mags)
        });
        let w = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
        ui.horizontal(|ui| {
            widgets::stat_card(
                ui,
                "Alignment points",
                &count.map_or_else(|| "...".into(), |n| n.to_string()),
                if drift.is_some() {
                    w
                } else {
                    ui.available_width()
                },
            );
            if let Some(d) = drift {
                widgets::stat_card(ui, "Median drift", &format!("{d:.1} px"), w);
            }
        });
        ui.add_space(14.0);
        self.exclude_list(ui);
    }

    /// The "Exclude region" list: rectangles the alignment must stay off.
    fn exclude_list(&mut self, ui: &mut Ui) {
        let p = palette();
        widgets::overline(ui, "Exclude regions");
        ui.add_space(2.0);
        ui.add(
            Label::new(mono_text(
                "Burned-in timestamps are found automatically. Add a rectangle for anything else to keep alignment away from. Regions are flat in the result.",
                12.0,
                p.faint,
            ))
            .wrap(),
        );
        ui.add_space(4.0);
        let usable = self.capture.is_some() && !self.busy();
        let mut remove = None;
        let mut edited = false;
        let mut committed = false;
        for (i, r) in self.settings.exclude.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                for (axis, v) in ["x", "y", "w", "h"].into_iter().zip(r.iter_mut()) {
                    ui.add(Label::new(mono_text(axis, 12.0, p.muted)));
                    let resp = ui
                        .add_enabled(usable, DragValue::new(v).range(0..=99_999).speed(1.0))
                        .on_hover_text(match axis {
                            "x" => "Left edge, pixels from the left of the frame",
                            "y" => "Top edge, pixels from the top of the frame",
                            "w" => "Width in pixels",
                            _ => "Height in pixels",
                        });
                    edited |= resp.changed();
                    committed |= resp.drag_stopped() || resp.lost_focus();
                }
                if widgets::icon_button(ui, Icon::Minus, "Remove region", usable, false).clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            self.settings.exclude.remove(i);
            self.exclusions_changed();
        } else if edited {
            self.exclude_dirty = true;
        }
        if committed && self.exclude_dirty {
            self.exclusions_changed();
        }
        ui.add_space(4.0);
        if widgets::button(ui, "Add region", Kind::Outline, usable, 0.0)
            .on_hover_text("Keep alignment and scoring off a rectangle of the frame")
            .clicked()
        {
            self.settings.exclude.push([0, 0, 120, 24]);
            self.exclusions_changed();
        }
    }

    fn stack_body(&mut self, ui: &mut Ui) {
        let p = palette();
        let n = self.count();
        let kept = self.kept();
        Self::row_label(
            ui,
            "Keep best frames",
            &format!("{:.0}%", self.settings.keep_percent),
        );
        let mut v = self.settings.keep_percent;
        ui.spacing_mut().slider_width = ui.available_width();
        let r = ui
            .add(
                Slider::new(&mut v, 5.0..=100.0)
                    .step_by(5.0)
                    .show_value(false)
                    .trailing_fill(true),
            )
            .on_hover_text("Share of frames, ranked by quality, that go into the stack");
        if r.changed() {
            self.settings.keep_percent = snap_keep(v);
        }
        ui.add(Label::new(mono_text(
            format!("{kept} of {n} frames"),
            12.0,
            p.faint,
        )));
        ui.add_space(4.0);

        widgets::overline(ui, "Combine");
        ui.horizontal_wrapped(|ui| {
            widgets::chip(ui, "Weighted mean", true, true)
                .on_hover_text("Frames are weighted by quality");
            widgets::chip(ui, "Mean", false, false)
                .on_hover_text("An unweighted mean is not available in the stacker yet");
        });
        ui.add_space(4.0);
        ui.add_space(4.0);
        widgets::overline(ui, "Outliers");
        ui.horizontal_wrapped(|ui| {
            let idle = self.run.is_none();
            for (label, kind, tip) in [
                ("Off", None, "Plain weighted mean"),
                (
                    "Sigma",
                    Some(RobustKind::Sigma),
                    "Reject samples beyond 2.5 sigma of the mean",
                ),
                (
                    "Huber",
                    Some(RobustKind::Huber),
                    "Down-weight samples beyond 1.345 sigma in proportion to their distance",
                ),
                (
                    "Winsor",
                    Some(RobustKind::Winsor),
                    "Clamp samples to 2 sigma of the mean",
                ),
            ] {
                let on = self.settings.robust == kind;
                if widgets::chip(ui, label, on, idle)
                    .on_hover_text(tip)
                    .clicked()
                    && idle
                {
                    self.settings.robust = kind;
                }
            }
        });
        ui.add_space(6.0);
        self.true_colour_controls(ui);
        ui.add_space(6.0);
        self.crop_controls(ui);
        ui.add_space(6.0);
        self.sunspot_controls(ui);
        ui.add_space(6.0);

        let ready = self.scoring == Scoring::Done && self.run.is_none() && n > 0;
        let label = if self.run.is_some() {
            "Stacking\u{2026}".to_string()
        } else {
            format!("Stack {kept} frames")
        };
        if widgets::button(ui, &label, Kind::Primary, ready, ui.available_width())
            .on_hover_text("Run the full pipeline on the kept frames")
            .clicked()
        {
            self.start_stack();
        }
        widgets::progress_bar(ui, self.run.as_ref().map_or(0.0, |r| r.peak));
        let status = match &self.run {
            Some(r) => format!(
                "{} \u{b7} {:.0}%",
                capitalise(stage_label(r.stage)),
                r.peak * 100.0
            ),
            None => match (&self.result, self.scoring) {
                (Some(out), _) => format!(
                    "Stacked {} of {} frames",
                    crate::activity::group(out.kept.len()),
                    crate::activity::group(out.scores.len())
                ),
                (None, Scoring::Running) => "Scoring frames\u{2026}".into(),
                (None, Scoring::Done) => format!("Ready to stack {kept} of {n} frames"),
                _ => "Waiting for a capture".into(),
            },
        };
        ui.add(Label::new(mono_text(status, 12.5, p.muted)));
        // The colour plane was moved off a clipped one: said before and after stacking.
        let plane_note = match (&self.result, &self.run) {
            (Some(out), None) => out.channel_note.as_deref().map(helia_core::clip::app_note),
            _ => None,
        }
        .or_else(|| self.channel_note.clone());
        if let Some(note) = plane_note {
            ui.add(Label::new(mono_text(format!("Channel: {note}"), 12.5, p.muted)).wrap())
                .on_hover_text("Helia moves off a colour plane that sits at the sensor limit");
        }
        if let (Some(out), None) = (&self.result, &self.run) {
            ui.add(Label::new(mono_text(alignment_line(&out.alignment), 12.5, p.muted)).wrap())
                .on_hover_text(out.alignment.summary());
            if let Some(s) = &out.seeing {
                ui.add(Label::new(mono_text(seeing_line(s), 12.5, p.muted)).wrap())
                    .on_hover_text(s.lines().join("\n"));
            }
            if let Some(g) = &self.granulation {
                ui.add(Label::new(mono_text(granulation_line(g), 12.5, p.muted)).wrap())
                    .on_hover_text(g.lines().join("\n"));
            }
            self.sunspot_report(ui);
            self.science_panel(ui);
        }
        if self.run.is_some()
            && widgets::button(ui, "Cancel", Kind::Outline, true, ui.available_width())
                .on_hover_text("Stop stacking")
                .clicked()
        {
            self.cancel_stack();
        }
    }

    /// "Output" in the Stack step: one plane that the Colour step then colours
    /// from a filter's look, or true colour from a raw colour camera.
    fn true_colour_controls(&mut self, ui: &mut Ui) {
        let p = palette();
        let idle = self.run.is_none();
        let bayer = self
            .capture
            .as_ref()
            .is_some_and(|c| c.meta.bayer.is_some());
        widgets::overline(ui, "Colour");
        ui.horizontal_wrapped(|ui| {
            for (label, on, tip, usable) in [
                (
                    "Filter look",
                    false,
                    "Stack one plane and colour it with a filter's look in the Colour step",
                    idle,
                ),
                (
                    "True colour",
                    true,
                    if bayer {
                        "Stack red, green and blue on one alignment and balance them: white light with a colour camera"
                    } else {
                        "Needs a raw colour (Bayer) capture"
                    },
                    idle && bayer,
                ),
            ] {
                if widgets::chip(ui, label, self.settings.true_colour == on, usable)
                    .on_hover_text(tip)
                    .clicked()
                    && usable
                {
                    self.settings.true_colour = on;
                }
            }
        });
        if self.settings.true_colour {
            ui.horizontal_wrapped(|ui| {
                for (label, wb, tip) in [
                    (
                        "Auto balance",
                        WhiteBalance::Auto,
                        "Make the middle of the disc neutral",
                    ),
                    (
                        "No balance",
                        WhiteBalance::Off,
                        "Keep the sensor's own balance of the three colours",
                    ),
                ] {
                    if widgets::chip(ui, label, self.settings.white_balance == wb, idle)
                        .on_hover_text(tip)
                        .clicked()
                        && idle
                    {
                        self.settings.white_balance = wb;
                    }
                }
            });
            widgets::toggle_row(
                ui,
                "Align channels",
                "Red and blue onto green (dispersion)",
                &mut self.settings.align_channels,
                idle,
            );
        }
        let note = match (&self.result, self.settings.true_colour) {
            (Some(out), true) => out.colour.as_ref().map_or_else(
                || "The last stack was not true colour. Stack again to make it.".to_string(),
                |c| {
                    let [r, g, b] = c.gains;
                    let moved = c.channels.map_or_else(String::new, |[a, z]| {
                        format!(
                            "; red moved ({:+.2}, {:+.2}) px, blue ({:+.2}, {:+.2}) px",
                            a.shift.dx, a.shift.dy, z.shift.dx, z.shift.dy
                        )
                    });
                    format!("Last stack: gains {r:.2}, {g:.2}, {b:.2}{moved}.")
                },
            ),
            (_, true) => "Red, green and blue are stacked on one alignment, then aligned and balanced. For white light: a narrow-band filter leaves a colour empty.".to_string(),
            (_, false) if bayer => "One plane is stacked and coloured from the filter's look in the Colour step. True colour is for white light.".to_string(),
            (_, false) => "One plane is stacked and coloured from the filter's look in the Colour step. True colour needs a raw colour capture.".to_string(),
        };
        ui.add(Label::new(mono_text(note, 12.0, p.faint)).wrap());
    }

    /// "Crop" in the Stack step: leave the picture as it is, or cut it to the
    /// solar disc plus a margin.
    /// "Sunspots" in the Stack step: whether to look for them, and the observer
    /// factor of the Wolf number.
    fn sunspot_controls(&mut self, ui: &mut Ui) {
        let idle = self.run.is_none();
        widgets::overline(ui, "Sunspots");
        ui.horizontal_wrapped(|ui| {
            for (label, on, tip) in [
                ("Off", false, "Do not look for sunspots"),
                (
                    "Find",
                    true,
                    "Count groups and spots on a white-light stack, with the Wolf number and areas corrected for foreshortening",
                ),
            ] {
                if widgets::chip(ui, label, self.settings.sunspots == on, idle)
                    .on_hover_text(tip)
                    .clicked()
                    && idle
                {
                    self.settings.sunspots = on;
                }
            }
        });
        if self.settings.sunspots {
            ui.add_space(2.0);
            Self::row_label(
                ui,
                "Observer factor k",
                &format!("{:.2}", self.settings.wolf_k),
            );
            ui.add_enabled(
                idle,
                eframe::egui::DragValue::new(&mut self.settings.wolf_k)
                    .range(0.1..=5.0)
                    .speed(0.01)
                    .max_decimals(2),
            )
            .on_hover_text("Wolf number R = k (10 groups + spots); 1 is the raw count");
            let mut north = self.settings.celestial_north_up;
            if ui
                .add_enabled(
                    idle,
                    eframe::egui::Checkbox::new(&mut north, "Celestial north is up"),
                )
                .on_hover_text(
                    "An equatorial mount with no flip: solar north is then turned by the angle P from the capture time. Off: solar north is up.",
                )
                .changed()
            {
                self.settings.celestial_north_up = north;
            }
        }
    }

    /// What the sunspot search found, with the overlay toggle and the save button.
    fn sunspot_report(&mut self, ui: &mut Ui) {
        let p = palette();
        let Some(d) = &self.sunspots else {
            return;
        };
        let r = &d.report;
        ui.add(Label::new(mono_text(sunspot_line(r), 12.5, p.muted)).wrap())
            .on_hover_text(r.lines().join("\n"));
        ui.horizontal_wrapped(|ui| {
            if widgets::chip(ui, "Show overlay", self.show_sunspots, true)
                .on_hover_text("Outline the spots on the flattened disc, with group numbers")
                .clicked()
            {
                self.show_sunspots = !self.show_sunspots;
            }
            if widgets::chip(ui, "Save table and overlay", false, true)
                .on_hover_text(
                    "Write the spot table (CSV), the report (JSON) and the overlay (PNG)",
                )
                .clicked()
            {
                let stem = self
                    .capture
                    .as_ref()
                    .and_then(|c| c.path.file_stem())
                    .map_or_else(|| "stack".to_owned(), |s| s.to_string_lossy().into_owned());
                let det = d.clone();
                self.workers
                    .save_sunspots(det, stem, self.prefs.last_export_dir.clone());
            }
        });
    }

    fn crop_controls(&mut self, ui: &mut Ui) {
        let p = palette();
        let idle = self.run.is_none();
        widgets::overline(ui, "Crop");
        ui.horizontal_wrapped(|ui| {
            for (label, on, tip) in [
                ("Off", false, "Keep the whole frame"),
                (
                    "Disc",
                    true,
                    "Crop to a square around the fitted solar disc, with a margin for prominences",
                ),
            ] {
                if widgets::chip(ui, label, self.settings.autocrop == on, idle)
                    .on_hover_text(tip)
                    .clicked()
                    && idle
                {
                    self.settings.autocrop = on;
                }
            }
        });
        if self.settings.autocrop {
            ui.add_space(2.0);
            Self::row_label(
                ui,
                "Margin",
                &format!("{:.0}% of the radius", self.settings.autocrop_margin),
            );
            let mut v = self.settings.autocrop_margin;
            ui.spacing_mut().slider_width = ui.available_width();
            let r = ui
                .add_enabled(
                    idle,
                    Slider::new(&mut v, 0.0..=50.0)
                        .step_by(1.0)
                        .show_value(false)
                        .trailing_fill(true),
                )
                .on_hover_text("Room left beyond the limb; prominences reach about 5%");
            if r.changed() {
                self.settings.autocrop_margin = v;
            }
        }
        let note = match (&self.result, self.settings.autocrop) {
            (Some(out), true) => out.crop.as_ref().map_or_else(
                || {
                    out.crop_skipped.map_or_else(
                        || "The last stack was not cropped.".to_string(),
                        |why| format!("The last stack was not cropped: {}.", why.reason()),
                    )
                },
                |c| {
                    format!(
                        "The last stack was cropped to {} x {} px.",
                        c.rect.width(),
                        c.rect.height()
                    )
                },
            ),
            (_, true) => "The stack is cut to a square around the fitted limb, FITS too. Without a clear disc it is left whole and the status line says why.".to_string(),
            (_, false) => "The whole frame is kept. Disc crops to the Sun when you stack.".to_string(),
        };
        ui.add(Label::new(mono_text(note, 12.0, p.faint)).wrap());
    }

    /// The Colour step: pick the look of the filter, invert it, mix it with
    /// grey. The viewer shows the result live.
    fn colour_body(&mut self, ui: &mut Ui) {
        let p = palette();
        if self.result.is_none() {
            ui.add(Label::new(mono_text(
                "Stack a capture first.",
                12.5,
                p.faint,
            )));
            return;
        }
        if let Some(c) = self.result.as_ref().and_then(|r| r.colour.clone()) {
            widgets::overline(ui, "True colour");
            let [r, g, b] = c.gains;
            ui.add(
                Label::new(mono_text(
                    format!("Gains applied: red {r:.3}, green {g:.3}, blue {b:.3}."),
                    12.5,
                    p.muted,
                ))
                .wrap(),
            );
            for line in c.notes.iter().skip(1) {
                ui.add(Label::new(mono_text(line, 12.0, p.muted)).wrap());
            }
            ui.add_space(4.0);
            ui.add(
                Label::new(mono_text(
                    "This stack is colour already, so the filter looks do not apply. Sharpening and adjustments are applied to each channel. Export as TIFF, PNG or a three-plane FITS file. To colour from a filter's look instead, stack again with Filter look in the Stack step.",
                    12.0,
                    p.faint,
                ))
                .wrap(),
            );
            return;
        }
        widgets::overline(ui, "Look");
        let current = self.settings.colour.preset;
        let mut pick = None;
        let w = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
        let mono_lut = ColourParams::default().lut();
        let cards: Vec<(Option<Preset>, String, String)> =
            std::iter::once((None, "Mono".to_string(), "no colour".to_string()))
                .chain(Preset::ALL.into_iter().map(|pr| {
                    let i = pr.info();
                    (Some(pr), i.name.to_string(), i.wavelength.to_string())
                }))
                .collect();
        for pair in cards.chunks(2) {
            ui.horizontal(|ui| {
                for (preset, name, sub) in pair {
                    let lut = preset
                        .map_or_else(|| mono_lut.clone(), |pr| ColourParams::preset(pr).lut());
                    if widgets::swatch(ui, name, sub, &lut, current == *preset, w).clicked() {
                        pick = Some(*preset);
                    }
                }
            });
        }
        if let Some(choice) = pick {
            self.settings.colour.preset = choice;
            self.colour_changed();
        }
        ui.add_space(4.0);
        let reason = match current {
            Some(pr) => {
                let i = pr.info();
                format!("{} {}: {}. {}", i.name, i.wavelength, i.hue, i.reason)
            }
            None => "Plain greyscale, as stacked.".to_string(),
        };
        ui.add(Label::new(mono_text(reason, 12.0, p.muted)).wrap());
        ui.add_space(4.0);
        let mut changed = false;
        changed |= widgets::toggle_row(
            ui,
            "Invert",
            "Dark disc, bright prominences and filaments",
            &mut self.settings.colour.invert,
            true,
        );
        ui.add_space(2.0);
        Self::row_label(
            ui,
            "Strength",
            &format!("{:.0}%", self.settings.colour.strength * 100.0),
        );
        let mut v = self.settings.colour.strength * 100.0;
        ui.spacing_mut().slider_width = ui.available_width();
        let r = ui
            .add(
                Slider::new(&mut v, 0.0..=100.0)
                    .step_by(5.0)
                    .show_value(false)
                    .trailing_fill(true),
            )
            .on_hover_text("0% is grey, 100% is the full colour of the look");
        if r.changed() {
            self.settings.colour.strength = v / 100.0;
            changed = true;
        }
        if changed {
            self.colour_changed();
        }
        ui.add_space(6.0);
        self.grade_controls(ui);
        ui.add_space(6.0);
        widgets::overline(ui, "Colour table");
        ui.horizontal_wrapped(|ui| {
            for (label, format, tip) in [
                (
                    ".cube",
                    helia_core::lut_export::LutFormat::Cube,
                    "A 1D LUT for DaVinci Resolve, Adobe and other colour tools",
                ),
                (
                    ".csv",
                    helia_core::lut_export::LutFormat::Csv,
                    "Value, red, green, blue as text",
                ),
                (
                    "ImageJ",
                    helia_core::lut_export::LutFormat::ImageJ,
                    "A .lut for ImageJ and Fiji (256 entries)",
                ),
                (
                    "PNG strip",
                    helia_core::lut_export::LutFormat::Png,
                    "The table as a strip image",
                ),
            ] {
                if widgets::chip(ui, label, false, true)
                    .on_hover_text(tip)
                    .clicked()
                {
                    self.workers.save_lut(
                        self.settings.colour,
                        format,
                        self.prefs.last_export_dir.clone(),
                    );
                }
            }
        });
        ui.add(
            Label::new(mono_text(
                "Save this look as a table to colour the same stack in another program.",
                11.5,
                p.faint,
            ))
            .wrap(),
        );
        ui.add_space(6.0);
        ui.add(
            Label::new(mono_text(
                "Colour is presentation, not measurement. The stack stays mono; FITS is never coloured. The look is recorded in TIFF, PNG and FITS files.",
                11.5,
                p.faint,
            ))
            .wrap(),
        );
        ui.add_space(6.0);
        if widgets::button(
            ui,
            "Continue to export",
            Kind::Primary,
            true,
            ui.available_width(),
        )
        .clicked()
        {
            self.select(Step::Export);
        }
    }

    fn export_body(&mut self, ui: &mut Ui) {
        let p = palette();
        widgets::overline(ui, "Format");
        ui.horizontal_wrapped(|ui| {
            for f in [ExportFormat::Tiff, ExportFormat::Png, ExportFormat::Fits] {
                let tip = match f {
                    ExportFormat::Tiff => "16 bits per channel; best for further processing",
                    ExportFormat::Png => "8 bits per channel; for sharing",
                    ExportFormat::Fits => "32-bit float, mono; the unsharpened stack, for science",
                };
                if widgets::chip(ui, f.label(), self.settings.export_format == f, true)
                    .on_hover_text(tip)
                    .clicked()
                {
                    self.settings.export_format = f;
                }
            }
        });
        if let Some(out) = &self.result {
            ui.add(Label::new(mono_text(
                format!(
                    "{} x {} px \u{b7} {} frames",
                    out.image.width,
                    out.image.height,
                    crate::activity::group(out.kept.len())
                ),
                12.5,
                p.muted,
            )));
            let edited = !self.edit_list().is_empty();
            ui.add(
                Label::new(mono_text(
                    format!(
                        "Will write: {}",
                        export_summary(
                            self.settings.export_format,
                            self.settings.colour,
                            edited,
                            self.settings.edits.edit_fits,
                        )
                    ),
                    12.5,
                    p.text,
                ))
                .wrap(),
            );
            if self.settings.export_format == ExportFormat::Fits {
                ui.add(
                    Label::new(mono_text(
                        "FITS is the unsharpened stack, for further processing elsewhere. Sharpening and adjustments go into TIFF and PNG only, unless you ask for them here.",
                        11.5,
                        p.faint,
                    ))
                    .wrap(),
                );
                widgets::toggle_row(
                    ui,
                    "Edit the FITS too",
                    "Apply the Sharpen and Adjust steps to it",
                    &mut self.settings.edits.edit_fits,
                    edited,
                );
            } else if edited {
                ui.add(
                    Label::new(mono_text(
                        format!(
                            "Edits: {} (set in the Sharpen and Adjust steps). They run on the full-size stack, then the colour.",
                            self.edit_list().describe().len()
                        ),
                        11.5,
                        p.faint,
                    ))
                    .wrap(),
                );
            }
            if self.settings.export_format.takes_colour() {
                ui.add(
                    Label::new(mono_text(
                        format!(
                            "Colour: {}. Change it in the Colour step.",
                            self.colour_name()
                        ),
                        11.5,
                        p.faint,
                    ))
                    .wrap(),
                );
            }
        } else {
            ui.add(Label::new(mono_text(
                "Stack a capture first.",
                12.5,
                p.faint,
            )));
        }
        ui.add_space(6.0);
        if widgets::button(
            ui,
            "Save as\u{2026}",
            Kind::Primary,
            self.result.is_some(),
            ui.available_width(),
        )
        .on_hover_text("Choose where to save (Ctrl+E)")
        .clicked()
        {
            self.export();
        }
    }

    /// "Field rotation" in the advanced settings: turn every frame to one
    /// orientation from the Sun's parallactic angle at its timestamp, for an
    /// altazimuth mount.
    fn field_rotation_controls(&mut self, ui: &mut Ui, locked: bool) {
        let p = palette();
        ui.horizontal_wrapped(|ui| {
            ui.add(Label::new(mono_text("FIELD ROTATION", 11.0, p.faint)));
            for (label, on, tip) in [
                ("Off", false, "An equatorial mount, or a capture too short to rotate"),
                (
                    "Alt-az",
                    true,
                    "Turn every frame to one orientation from the Sun's parallactic angle at the frame's timestamp (SER captures), before scoring and aligning",
                ),
            ] {
                if widgets::chip_mono(ui, label, self.settings.field_rotation == on, !locked)
                    .on_hover_text(tip)
                    .clicked()
                {
                    self.settings.field_rotation = on;
                }
            }
        });
        if !self.settings.field_rotation {
            return;
        }
        let mut moved = false;
        ui.horizontal_wrapped(|ui| {
            ui.add(Label::new(mono_text("SITE (N, E, M)", 11.0, p.faint)));
            let [lat, lon, alt] = &mut self.settings.site;
            for (v, range, speed, tip) in [
                (lat, -90.0..=90.0, 0.01, "Latitude in degrees north"),
                (lon, -180.0..=180.0, 0.01, "Longitude in degrees east"),
                (
                    alt,
                    -1000.0..=10_000.0,
                    1.0,
                    "Height above sea level in metres",
                ),
            ] {
                moved |= ui
                    .add_enabled(
                        !locked,
                        eframe::egui::DragValue::new(v)
                            .range(range)
                            .speed(speed)
                            .max_decimals(3),
                    )
                    .on_hover_text(tip)
                    .changed();
            }
        });
        if moved {
            self.settings.site_set = true;
        }
        let mut mirror = self.settings.field_rotation_mirror;
        if ui
            .add_enabled(
                !locked,
                eframe::egui::Checkbox::new(&mut mirror, "Mirror image"),
            )
            .on_hover_text(
                "An odd number of reflections (a star diagonal): the turn is the other way",
            )
            .changed()
        {
            self.settings.field_rotation_mirror = mirror;
        }
        let line = if !self.settings.site_set {
            "Enter the observing site to correct the rotation (or set HELIA_SITE).".to_owned()
        } else if self.capture.is_none() {
            String::new()
        } else {
            match self.field_rotation().map(|f| f.plan()) {
                Some(Ok(plan)) => plan.lines(None).remove(0),
                Some(Err(e)) => e.to_string(),
                None => "This capture has no timestamps (only SER files carry them), so there is nothing to correct from.".to_owned(),
            }
        };
        ui.add(Label::new(mono_text(line, 11.5, p.muted)).wrap());
    }

    fn footer(&mut self, ui: &mut Ui) {
        let p = palette();
        ui.spacing_mut().item_spacing = eframe::egui::vec2(6.0, 6.0);
        ui.separator();
        let locked = self.run.is_some();
        if widgets::chip_mono(ui, "Advanced settings", self.show_advanced, true)
            .on_hover_text("Backend, demosaic method and colour channel")
            .clicked()
        {
            self.show_advanced = !self.show_advanced;
        }
        if !self.show_advanced {
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.add(Label::new(mono_text("BACKEND", 11.0, p.faint)));
            for (label, b) in [
                ("Auto", BackendChoice::Auto),
                ("CPU", BackendChoice::Cpu),
                ("GPU", BackendChoice::Gpu),
            ] {
                if widgets::chip_mono(ui, label, self.settings.backend == b, !locked)
                    .on_hover_text("Compute backend for scoring and accumulation")
                    .clicked()
                {
                    self.settings.backend = b;
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.add(Label::new(mono_text("APERTURE (MM)", 11.0, p.faint)));
            ui.add_enabled(
                !locked,
                eframe::egui::DragValue::new(&mut self.settings.aperture_mm)
                    .range(0.0..=2000.0)
                    .speed(1.0)
                    .max_decimals(0)
                    .custom_formatter(|v, _| {
                        if v > 0.0 {
                            format!("{v:.0}")
                        } else {
                            "not set".to_string()
                        }
                    }),
            )
            .on_hover_text(
                "Telescope aperture: gives the seeing report an absolute r0 and a grade",
            );
        });
        self.field_rotation_controls(ui, locked);
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            ui.add(Label::new(mono_text("DEBAYER", 11.0, p.faint)));
            for (label, m) in [("Bilinear", Method::Bilinear), ("MHC", Method::Mhc)] {
                if widgets::chip_mono(ui, label, self.settings.method == m, !locked)
                    .on_hover_text("Demosaic method for colour sensors")
                    .clicked()
                    && self.settings.method != m
                {
                    self.settings.method = m;
                    changed = true;
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.add(Label::new(mono_text("CHANNEL", 11.0, p.faint)));
            for (label, c) in [
                ("R", Channel::Red),
                ("G", Channel::Green),
                ("B", Channel::Blue),
                ("Lum", Channel::Luminance),
            ] {
                if widgets::chip_mono(ui, label, self.settings.channel == c, !locked)
                    .on_hover_text("Channel analysed and stacked")
                    .clicked()
                    && self.settings.channel != c
                {
                    self.settings.channel = c;
                    self.settings.channel_chosen = true;
                    self.channel_note = None;
                    changed = true;
                }
            }
        });
        if changed && self.capture.is_some() {
            // ffmpeg extracts a colour channel while decoding, so a video is
            // decoded again for a new channel; other captures just re-score.
            if let Some(c) = self.capture.as_ref().filter(|c| c.meta.video) {
                let path = c.path.clone();
                self.begin_open(path);
            } else {
                self.invalidate_previews();
                self.start_scoring();
            }
        }
    }
}

fn stage_label(s: Stage) -> &'static str {
    s.label()
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
}

/// One line saying how the result was aligned, for the Stack step.
fn alignment_line(a: &helia_core::align::method::AlignReport) -> String {
    a.confidence_mean.map_or_else(
        || format!("Aligned by {}", a.method.name()),
        |c| format!("Aligned by {} \u{b7} confidence {c:.2}", a.method.name()),
    )
}

/// One line of the sunspot report, for the Stack step.
fn sunspot_line(r: &helia_core::sunspots::Report) -> String {
    format!(
        "sunspots: {} group{}, {} spot{}, Wolf {:.0}, {:.0} MSH",
        r.group_count,
        if r.group_count == 1 { "" } else { "s" },
        r.spot_count,
        if r.spot_count == 1 { "" } else { "s" },
        r.wolf,
        r.area_msh
    )
}

/// One line of the seeing report, for the Stack step.
fn seeing_line(s: &helia_core::seeing::SeeingReport) -> String {
    match (&s.spectral, s.grade, &s.motion) {
        (Some(f), Some(g), _) => format!("Seeing {} \u{b7} r0 {:.1} cm", g.label(), f.r0_500_cm),
        (_, _, Some(m)) => format!("Seeing index {:.2} px", m.rms_px),
        _ => "Seeing: not measurable here".to_string(),
    }
}

/// One line of the granulation report, for the Stack step.
fn granulation_line(g: &helia_core::granulation::Granulation) -> String {
    format!("Granulation {:.2}% RMS", g.rms_contrast * 100.0)
}

/// One grade control: a label, a slider and a number box.
fn grade_row(
    ui: &mut Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    (step, decimals): (f64, usize),
    tip: &str,
) -> bool {
    ui.horizontal(|ui| {
        ui.add_sized(
            [96.0, 18.0],
            Label::new(mono_text(label, 11.5, palette().muted)),
        )
        .on_hover_text(tip);
        ui.spacing_mut().slider_width = (ui.available_width() - 70.0).max(40.0);
        let a = ui
            .add(
                Slider::new(value, range.clone())
                    .step_by(step)
                    .show_value(false)
                    .trailing_fill(true),
            )
            .on_hover_text(tip)
            .changed();
        let b = ui
            .add(
                eframe::egui::DragValue::new(value)
                    .range(range)
                    .speed(step * 0.2)
                    .fixed_decimals(decimals),
            )
            .on_hover_text(tip)
            .changed();
        a || b
    })
    .inner
}

#[cfg(test)]
mod tests {
    use super::capitalise;

    #[test]
    fn capitalises_stage_labels() {
        assert_eq!(capitalise("aligning frames"), "Aligning frames");
        assert_eq!(capitalise(""), "");
    }
}
