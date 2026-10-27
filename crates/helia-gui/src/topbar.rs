// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Top bar and pipeline rail.

use eframe::egui::{self, Align, Frame, Label, Layout, Margin, Panel, RichText, Stroke, Ui, vec2};

use helia_core::about;

use crate::app::App;
use crate::branding;
use crate::model::{Step, truncate};
use crate::theme::{mono, palette, semibold};
use crate::widgets::{self, Icon, Kind};
use crate::worker::gpu_chip;

impl App {
    pub(crate) fn topbar(&mut self, ui: &mut Ui) {
        let p = palette();
        Panel::top("topbar")
            .exact_size(64.0)
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(p.panel)
                    .inner_margin(Margin::symmetric(16, 10))
                    .stroke(Stroke::new(1.0, p.border)),
            )
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    let (rect, resp) =
                        ui.allocate_exact_size(vec2(26.0, 26.0), egui::Sense::click());
                    resp.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "About Helia")
                    });
                    if let Some(tex) = branding::texture(ui.ctx(), branding::Image::Small) {
                        egui::Image::new(&tex).paint_at(ui, rect);
                    } else {
                        widgets::paint_sun(ui.painter(), rect.center(), 11.0, p.accent);
                    }
                    let mut about = resp.on_hover_text("About Helia (F1)").clicked();
                    let name = ui
                        .add(
                            Label::new(RichText::new("Helia").font(semibold(20.0)).color(p.text))
                                .sense(egui::Sense::click()),
                        )
                        .on_hover_text("About Helia (F1)");
                    about |= name.clicked();
                    if about {
                        self.show_about = true;
                        self.feedback.open = false;
                    }
                    if about::PRE_RELEASE {
                        ui.add_space(4.0);
                        widgets::badge(
                            ui,
                            "Pre-release",
                            "Helia is below version 1.0.0: it works, but expect rough edges. \
                             Feedback is welcome.",
                        );
                    }
                    ui.add_space(12.0);
                    self.topbar_right(ui);
                });
            });
    }

    /// The "Recent" menu: the captures opened lately, newest first. Not drawn
    /// while there are none.
    fn recent_menu(&mut self, ui: &mut Ui) {
        if self.prefs.recent.is_empty() {
            return;
        }
        let enabled = !self.busy();
        let mut chosen = None;
        ui.add_enabled_ui(enabled, |ui| {
            ui.menu_button("Recent", |ui| {
                for f in &self.prefs.recent {
                    let name = f.file_name().map_or_else(
                        || f.display().to_string(),
                        |n| n.to_string_lossy().into_owned(),
                    );
                    if ui
                        .button(name)
                        .on_hover_text(f.display().to_string())
                        .clicked()
                    {
                        chosen = Some(f.clone());
                        ui.close();
                    }
                }
            });
        });
        if let Some(f) = chosen {
            if f.exists() {
                self.begin_open(f);
            } else {
                self.toast(
                    crate::activity::ToastKind::Error,
                    format!("{} is no longer there", f.display()),
                );
                self.prefs.forget_recent(&f);
            }
        }
    }

    fn topbar_right(&mut self, ui: &mut Ui) {
        let p = palette();
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let can_export = self.result.is_some();
            if widgets::button(ui, "Export", Kind::Primary, can_export, 0.0)
                .on_hover_text("Save the stacked image (Ctrl+E)")
                .clicked()
            {
                self.select(Step::Export);
                self.export();
            }
            if widgets::button(ui, "Open capture", Kind::Outline, !self.busy(), 0.0)
                .on_hover_text("Open a SER, AVI or video capture (Ctrl+O)")
                .clicked()
            {
                self.pick_capture();
            }
            self.recent_menu(ui);
            self.update_topbar_button(ui);
            if widgets::button(ui, "About", Kind::Outline, true, 0.0)
                .on_hover_text("Version, licence and dedication (F1)")
                .clicked()
            {
                self.show_about = !self.show_about;
                self.feedback.open = false;
            }
            if widgets::button(ui, "Plugins", Kind::Outline, true, 0.0)
                .on_hover_text("Plugins: what is found, what is switched on, their settings")
                .clicked()
            {
                if self.show_plugins {
                    self.show_plugins = false;
                } else {
                    self.open_plugins();
                }
            }
            if widgets::button(ui, "Feedback", Kind::Outline, true, 0.0)
                .on_hover_text("Report a bug, ask for a feature or ask a question (Ctrl+Shift+F)")
                .clicked()
            {
                let ctx = ui.ctx().clone();
                self.begin_feedback(&ctx);
            }
            let (main, detail, gpu) = gpu_chip(self.settings.backend, self.gpu.as_ref());
            let dot = if gpu { p.success } else { p.faint };
            let tip = if gpu {
                "GPU adapter used for scoring and accumulation"
            } else {
                "Running on the CPU"
            };
            // The chip shortens a long adapter name; the tooltip has all of it.
            let full = if detail.is_empty() {
                main.clone()
            } else {
                format!("{main}: {detail}")
            };
            widgets::status_chip(
                ui,
                dot,
                &main,
                &truncate(&detail, 34),
                &format!("{full}\n{tip}"),
            );
            if self.was_narrow == Some(true) {
                if widgets::icon_button(
                    ui,
                    Icon::Inspector,
                    "Toggle inspector",
                    true,
                    self.show_inspector,
                )
                .clicked()
                {
                    self.toggle_panel(false);
                }
                if widgets::icon_button(
                    ui,
                    Icon::Rail,
                    "Toggle pipeline rail",
                    true,
                    self.show_rail,
                )
                .clicked()
                {
                    self.toggle_panel(true);
                }
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                let text = match &self.capture {
                    Some(c) => RichText::new(c.meta.fields().join("  \u{b7}  "))
                        .font(mono(12.5))
                        .color(p.muted),
                    None if self.opening => RichText::new(self.opening_text())
                        .font(mono(12.5))
                        .color(p.faint),
                    None => RichText::new("No capture open")
                        .font(mono(12.5))
                        .color(p.faint),
                };
                let r = ui.add(Label::new(text).truncate());
                if let Some(c) = &self.capture {
                    r.on_hover_text(c.path.display().to_string());
                }
            });
        });
    }

    pub(crate) fn rail(&mut self, ui: &mut Ui) {
        let p = palette();
        Panel::left("rail")
            .resizable(false)
            .exact_size(248.0)
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(p.panel)
                    .inner_margin(Margin::same(16))
                    .stroke(Stroke::new(1.0, p.border)),
            )
            .show(ui, |ui| {
                widgets::overline(ui, "Pipeline");
                ui.add_space(6.0);
                let snap = self.snapshot();
                ui.spacing_mut().item_spacing.y = 6.0;
                let mut clicked = None;
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (i, step) in Step::ALL.into_iter().enumerate() {
                            let status = crate::model::step_status(step, &snap);
                            let hint = crate::model::step_hint(step, &snap);
                            let r = widgets::step_row(
                                ui,
                                step,
                                &status,
                                &hint,
                                step == self.step,
                                i == 0,
                                i + 1 == Step::ALL.len(),
                            );
                            if r.clicked() {
                                clicked = Some(step);
                            }
                        }
                        ui.add(Label::new(
                            RichText::new(format!(
                                "Helia {}  \u{b7}  GPL-3.0",
                                crate::about::ui_version()
                            ))
                            .font(mono(11.0))
                            .color(p.faint),
                        ));
                    });
                if let Some(s) = clicked {
                    self.select(s);
                }
            });
    }
}
