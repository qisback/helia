// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Drawing of what the application is doing (activity notices), what it has
//! just done (toasts) and the first-run tips. The state is in `activity.rs`
//! and `prefs.rs`.

use eframe::egui::{
    self, Align, Align2, Area, CornerRadius, Frame, Id, Label, Layout, Margin, Order, RichText,
    Stroke, Ui, WidgetInfo, WidgetType, vec2,
};

use crate::activity::{ToastKind, Toasts};
use crate::app::App;
use crate::theme::{mono, palette, semibold};
use crate::widgets;

/// Notices shown at once; the rest wait their turn.
const MAX_NOTICES: usize = 3;

impl App {
    /// The strip above the viewer: first-run tips, then one row per running
    /// operation old enough to announce (see `NOTICE_DELAY`).
    pub(crate) fn notices(&mut self, ui: &mut Ui) {
        self.tips_strip(ui);
        let now = self.now();
        let frames = self.count();
        let shown: Vec<_> = self
            .tasks
            .visible(now)
            .into_iter()
            .take(MAX_NOTICES)
            .cloned()
            .collect();
        let mut cancel = None;
        for task in &shown {
            let p = palette();
            let spoken = task.spoken(now, frames);
            Frame::new()
                .fill(p.raised)
                .stroke(Stroke::new(1.0, p.border_strong))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(14, 10))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    ui.horizontal(|ui| {
                        if task.fraction.is_none() {
                            ui.add(egui::Spinner::new().size(16.0).color(p.accent));
                        }
                        ui.add(Label::new(
                            RichText::new(task.kind.label(frames))
                                .font(semibold(14.0))
                                .color(p.text),
                        ));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if task.cancellable
                                && widgets::chip(ui, "Cancel", false, true)
                                    .on_hover_text("Stop this and keep what you had")
                                    .clicked()
                            {
                                cancel = Some(task.kind);
                            }
                            ui.add(Label::new(
                                RichText::new(task.detail(now, frames))
                                    .font(mono(12.0))
                                    .color(p.muted),
                            ));
                        });
                    });
                    widgets::progress_rail(ui, task.fraction, &spoken);
                });
            ui.add_space(8.0);
        }
        if let Some(kind) = cancel {
            self.cancel_task(kind);
        }
    }

    /// Short how-to shown until dismissed once.
    fn tips_strip(&mut self, ui: &mut Ui) {
        if self.prefs.tips_dismissed {
            return;
        }
        let p = palette();
        let mut dismiss = false;
        let resp = Frame::new()
            .fill(p.selected_fill)
            .stroke(Stroke::new(1.0, p.selected_border))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        if widgets::chip(ui, "Got it", false, true)
                            .on_hover_text("Hide these tips for good")
                            .clicked()
                        {
                            dismiss = true;
                        }
                        ui.with_layout(Layout::top_down(Align::Min), |ui| {
                            ui.add(Label::new(
                                RichText::new("Quick start")
                                    .font(semibold(14.0))
                                    .color(p.accent),
                            ));
                            ui.add(
                                Label::new(
                                    RichText::new(
                                        "1. Open a capture (Ctrl+O, or drop a file here).  \
                                         2. Press Stack with recommended settings.  \
                                         3. Pick a colour and export.",
                                    )
                                    .color(p.muted),
                                )
                                .wrap(),
                            );
                        });
                    });
                });
            });
        resp.response.widget_info(|| {
            WidgetInfo::labeled(
                WidgetType::Label,
                true,
                "Quick start: open a capture, press Stack with recommended settings, pick a colour and export.",
            )
        });
        if dismiss {
            self.prefs.dismiss_tips();
        }
        ui.add_space(8.0);
    }

    /// Toasts, bottom right, over everything. Successes fade; errors stay
    /// until dismissed.
    pub(crate) fn toasts_overlay(&mut self, ctx: &egui::Context) {
        if self.toasts.list().is_empty() {
            return;
        }
        let p = palette();
        let now = self.now();
        let mut dismiss = None;
        Area::new(Id::new("toasts"))
            .anchor(Align2::RIGHT_BOTTOM, vec2(-20.0, -20.0))
            .order(Order::Foreground)
            .interactable(true)
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                for t in self.toasts.list().iter().rev() {
                    let error = t.kind == ToastKind::Error;
                    let alpha = Toasts::opacity(t, now);
                    ui.scope(|ui| {
                        ui.set_opacity(alpha);
                        let (fill, border, ink) = if error {
                            (p.error_fill, p.error_border, p.error_text)
                        } else {
                            (p.raised, p.border_strong, p.text)
                        };
                        let r = Frame::new()
                            .fill(fill)
                            .stroke(Stroke::new(1.0, border))
                            .corner_radius(CornerRadius::same(8))
                            .inner_margin(Margin::symmetric(14, 10))
                            .show(ui, |ui| {
                                ui.set_max_width(420.0);
                                ui.horizontal_top(|ui| {
                                    if !error {
                                        let c = if t.kind == ToastKind::Success {
                                            p.success
                                        } else {
                                            p.accent
                                        };
                                        let (rect, _) = ui.allocate_exact_size(
                                            vec2(10.0, 18.0),
                                            egui::Sense::hover(),
                                        );
                                        widgets::dot(ui.painter(), rect.center(), 3.5, c);
                                    }
                                    ui.add(Label::new(RichText::new(&t.text).color(ink)).wrap());
                                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                                        let label = if error { "Dismiss" } else { "Close" };
                                        if ui.small_button(label).clicked() {
                                            dismiss = Some(t.id);
                                        }
                                    });
                                });
                            });
                        r.response.widget_info(|| {
                            WidgetInfo::labeled(
                                WidgetType::Label,
                                true,
                                format!("{}: {}", if error { "Error" } else { "Notice" }, t.text),
                            )
                        });
                    });
                }
            });
        if let Some(id) = dismiss {
            self.toasts.dismiss(id);
        }
    }
}
