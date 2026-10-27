// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Centre column: toolbar, viewer, frame scrubber and quality curve.

use eframe::egui::{
    self, Align, Align2, CentralPanel, Color32, CornerRadius, Frame, Label, Layout, Margin, Panel,
    Rect, RichText, Sense, Shape, Slider, Stroke, StrokeKind, Ui, pos2, vec2,
};

use crate::app::App;
use crate::curve;
use crate::model::{Scoring, Step, ZOOM_STEP, ap_screen_rect, keep_label, scrub_label};
use crate::theme::{RADIUS_LARGE, mono, palette, sans};
use crate::widgets::{self, Icon, Kind};

impl App {
    pub(crate) fn centre(&mut self, ui: &mut Ui) {
        let p = palette();
        CentralPanel::default()
            .frame(Frame::new().fill(p.ground).inner_margin(Margin::same(16)))
            .show(ui, |ui| {
                if self.show_about {
                    self.about_page(ui);
                    return;
                }
                if self.feedback.open {
                    self.feedback_page(ui);
                    return;
                }
                if self.show_plugins {
                    self.plugins_page(ui);
                    return;
                }
                self.notices(ui);
                self.toolbar(ui);
                ui.add_space(4.0);
                if self.step == Step::Mosaic {
                    // The mosaic is one picture: no scrubber.
                } else if self.step == Step::Timelapse {
                    if self.tl.frames() > 0 {
                        Panel::bottom("scrub")
                            .resizable(false)
                            .show_separator_line(false)
                            .frame(Frame::new().fill(p.ground).inner_margin(Margin {
                                top: 12,
                                ..Margin::ZERO
                            }))
                            .show(ui, |ui| self.timelapse_scrubber(ui));
                    }
                } else if self.count() > 0 {
                    Panel::bottom("scrub")
                        .resizable(false)
                        .show_separator_line(false)
                        .frame(Frame::new().fill(p.ground).inner_margin(Margin {
                            top: 12,
                            ..Margin::ZERO
                        }))
                        .show(ui, |ui| self.scrubber(ui));
                }
                self.viewer(ui);
            });
    }

    fn toolbar(&mut self, ui: &mut Ui) {
        if self.step == Step::Timelapse {
            self.timelapse_toolbar(ui);
            return;
        }
        if self.step == Step::Mosaic {
            self.mosaic_toolbar(ui);
            return;
        }
        let p = palette();
        ui.horizontal(|ui| {
            let has = self.count() > 0;
            if widgets::chip(ui, "Alignment points", self.show_grid, has)
                .on_hover_text("Show the alignment point grid over the frame")
                .clicked()
            {
                self.show_grid = !self.show_grid;
            }
            let label = if self.show_result {
                "Showing stack"
            } else {
                "Showing frame"
            };
            if widgets::chip(ui, label, self.show_result, self.result.is_some())
                .on_hover_text("Switch between the current frame and the stacked result (Space)")
                .clicked()
            {
                self.show_result = !self.show_result;
            }
            if self.step == Step::Calibrate
                && self.shown_before.is_some()
                && widgets::chip(ui, "Before / after", self.show_cal_split, true)
                    .on_hover_text("Split the picture: the frame as recorded on the left, calibrated on the right. Drag the line.")
                    .clicked()
            {
                self.show_cal_split = !self.show_cal_split;
            }
            if matches!(self.step, Step::Sharpen | Step::Adjust) && self.result.is_some() {
                if widgets::chip(ui, "Before / after", self.show_split, true)
                    .on_hover_text("Split the picture: the stack as it was on the left, with your edits on the right. Drag the line.")
                    .clicked()
                {
                    self.show_split = !self.show_split;
                    self.edits_changed();
                }
                if widgets::chip(ui, "Full size", self.settings.full_preview, true)
                    .on_hover_text("Always render the preview at full size. Otherwise it is shrunk while a slider moves and redone at full size when you let go.")
                    .clicked()
                {
                    self.settings.full_preview = !self.settings.full_preview;
                    self.edits_changed();
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::icon_button(ui, Icon::Plus, "Zoom in", has, false).clicked() {
                    self.view.zoom_about(egui::Vec2::ZERO, ZOOM_STEP);
                }
                if widgets::chip(ui, "1:1", !self.view.fit && self.view.percent() == 100, has)
                    .on_hover_text("Show the image at 100%, one image pixel to one screen pixel")
                    .clicked()
                {
                    self.view.fit = false;
                    self.view.zoom = 1.0;
                    self.view.pan = egui::Vec2::ZERO;
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
                ui.add_space(8.0);
                if let Some(g) = self.display_grid() {
                    let text = format!("{} APs  \u{b7}  {} px", g.active_count(), g.size);
                    ui.add(Label::new(
                        RichText::new(text).font(mono(12.5)).color(p.muted),
                    ));
                }
            });
        });
    }

    fn viewer(&mut self, ui: &mut Ui) {
        if self.step == Step::Timelapse {
            self.timelapse_viewer(ui);
            return;
        }
        if self.step == Step::Mosaic {
            self.mosaic_viewer(ui);
            return;
        }
        let p = palette();
        let size = ui.available_size();
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
        resp.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Label,
                true,
                "Image viewer. Scroll to zoom, drag to pan, double-click to fit.",
            )
        });
        let painter = ui.painter_at(rect);
        let radius = CornerRadius::same(RADIUS_LARGE);
        painter.rect_filled(rect, radius, p.viewer);

        // These steps show the stack as it will be written: edited, then coloured.
        let coloured = matches!(
            self.step,
            Step::Sharpen | Step::Adjust | Step::Colour | Step::Export
        )
        .then(|| self.colour_tex.clone())
        .flatten();
        let is_preview = self.show_result && coloured.is_some();
        let overlay = (self.step == Step::Stack && self.show_science)
            .then(|| self.science_overlay())
            .flatten()
            .or_else(|| {
                (self.step == Step::Stack && self.show_sunspots)
                    .then(|| self.sunspot_tex.clone())
                    .flatten()
            });
        let shown = if self.show_result {
            overlay
                .or(coloured)
                .or_else(|| self.result_tex.clone())
                .map(|(t, s)| (t, s, true))
        } else {
            self.shown.clone().map(|(t, s)| (t, s, false))
        };
        let Some((tex, tsize, is_stack)) = shown else {
            self.empty_state(ui, rect);
            painter.rect_stroke(rect, radius, Stroke::new(1.0, p.border), StrokeKind::Inside);
            return;
        };
        // A preview may be shrunk; it stands for the finished image.
        let stands_for = if is_preview {
            self.preview_size.unwrap_or(tsize)
        } else {
            tsize
        };
        let img = vec2(stands_for[0] as f32, stands_for[1] as f32);

        // Zoom and pan input.
        if resp.hovered() {
            let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let factor = (scroll * 0.0015).exp() * pinch;
            if (factor - 1.0).abs() > 1e-4
                && let Some(pos) = ui.input(|i| i.pointer.hover_pos())
            {
                self.settle(rect, img);
                self.view.zoom_about(pos - rect.center(), factor);
            }
        }
        if resp.dragged() {
            self.settle(rect, img);
            self.view.fit = false;
            self.view.pan += resp.drag_delta();
        }
        if resp.double_clicked() {
            self.view.reset();
        }
        self.settle(rect, img);

        let image_rect = self.view.image_rect(rect, img);
        painter.image(
            tex.id(),
            image_rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );

        let split = is_preview
            && self.show_split
            && matches!(self.step, Step::Sharpen | Step::Adjust)
            && self.before_tex.is_some();
        if split {
            if let Some((before, _)) = self.before_tex.clone() {
                self.split_view(ui, rect, image_rect, &painter, &before);
            }
        } else if self.step == Step::Calibrate
            && !self.show_result
            && self.show_cal_split
            && let Some((before, _)) = self.shown_before.clone()
        {
            self.split_view(ui, rect, image_rect, &painter, &before);
        }

        if self.show_grid
            && !is_stack
            && let Some(g) = self.display_grid()
            && (g.width, g.height) == (tsize[0], tsize[1])
        {
            let stroke = Stroke::new(1.0, p.grid);
            let shapes: Vec<Shape> = g
                .aps
                .iter()
                .filter(|a| a.active)
                .map(|a| {
                    ap_screen_rect(
                        image_rect.min,
                        self.view.zoom,
                        [a.rect.x0, a.rect.y0, a.rect.x1, a.rect.y1],
                    )
                })
                .filter(|r| r.intersects(rect))
                .map(|r| Shape::rect_stroke(r, 0.0, stroke, StrokeKind::Inside))
                .collect();
            painter.extend(shapes);
        }

        if !is_stack {
            let stroke = Stroke::new(1.5, p.accent);
            for r in self.settings.exclude_rects() {
                let r = ap_screen_rect(image_rect.min, self.view.zoom, [r.x0, r.y0, r.x1, r.y1]);
                if r.intersects(rect) {
                    painter.rect_stroke(r, 0.0, stroke, StrokeKind::Inside);
                }
            }
        }

        if is_stack && !is_preview {
            self.paint_plugin_overlays(&painter, image_rect, rect);
        }
        painter.rect_stroke(rect, radius, Stroke::new(1.0, p.border), StrokeKind::Inside);
        let tag = if is_stack {
            let k = self.result.as_ref().map_or(0, |r| r.kept.len());
            let mut parts = vec![format!("Stack of {k} frames")];
            if is_preview {
                let e = &self.settings.edits;
                if e.flatten.is_some() {
                    parts.push("flattened".to_string());
                }
                if e.sharpen_status() != "Off" {
                    parts.push(e.sharpen_status());
                }
                if e.any_adjustment() {
                    parts.push("adjusted".to_string());
                }
                if self.result.as_ref().is_some_and(|r| r.colour.is_some()) {
                    parts.push("true colour".to_string());
                } else if self.settings.colour.preset.is_some() || self.settings.colour.invert {
                    parts.push(self.colour_name());
                }
            }
            parts.join(" \u{b7} ")
        } else {
            format!("Frame {}", self.index + 1)
        };
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

    /// Draw the "before" half over the left of the image, with a draggable
    /// divider and a label on each side.
    fn split_view(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        image_rect: Rect,
        painter: &egui::Painter,
        before: &egui::TextureHandle,
    ) {
        let p = palette();
        let x = (image_rect.left() + self.split_pos * image_rect.width())
            .clamp(rect.left(), rect.right());
        let left = Rect::from_min_max(rect.min, pos2(x, rect.bottom()));
        painter.with_clip_rect(left).image(
            before.id(),
            image_rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        painter.line_segment(
            [pos2(x, rect.top()), pos2(x, rect.bottom())],
            Stroke::new(2.0, p.accent),
        );
        let handle = Rect::from_center_size(pos2(x, rect.center().y), vec2(30.0, rect.height()));
        let hr = ui.interact(handle, ui.id().with("split-divider"), Sense::drag());
        hr.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Slider,
                true,
                "Before and after divider. Drag to move.",
            )
        });
        if hr.hovered() || hr.dragged() {
            ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::ResizeHorizontal);
        }
        if hr.dragged()
            && let Some(pos) = ui.input(|i| i.pointer.interact_pos())
            && image_rect.width() > 1.0
        {
            self.split_pos = ((pos.x - image_rect.left()) / image_rect.width()).clamp(0.0, 1.0);
        }
        painter.circle(
            pos2(x, rect.center().y),
            13.0,
            p.accent,
            Stroke::new(1.5, p.on_accent),
        );
        painter.text(
            pos2(x, rect.center().y),
            Align2::CENTER_CENTER,
            "\u{2194}",
            mono(14.0),
            p.on_accent,
        );
        for (text, anchor, side) in [
            (
                "Before",
                rect.left_top() + vec2(12.0, 12.0),
                Align2::LEFT_TOP,
            ),
            (
                "After",
                rect.right_top() + vec2(-12.0, 12.0),
                Align2::RIGHT_TOP,
            ),
        ] {
            let g = painter.layout_no_wrap(text.to_string(), mono(12.0), p.muted);
            let size = g.size() + vec2(20.0, 10.0);
            let pill = side.anchor_size(anchor, size);
            painter.rect(
                pill,
                CornerRadius::same(13),
                Color32::from_rgba_unmultiplied(8, 7, 7, 200),
                Stroke::new(1.0, p.border),
                StrokeKind::Inside,
            );
            painter.galley(pill.min + vec2(10.0, 5.0), g, p.muted);
        }
    }

    fn settle(&mut self, rect: Rect, img: egui::Vec2) {
        self.view.settle(rect.size(), img);
    }

    fn empty_state(&mut self, ui: &mut Ui, rect: Rect) {
        let p = palette();
        if self.count() > 0 {
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                "Decoding frame\u{2026}",
                mono(13.0),
                p.faint,
            );
            return;
        }
        if self.opening {
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                self.opening_text(),
                mono(13.0),
                p.faint,
            );
            return;
        }
        let c = rect.center();
        widgets::paint_sun(ui.painter(), c - vec2(0.0, 70.0), 22.0, p.accent);
        ui.painter().text(
            c - vec2(0.0, 28.0),
            Align2::CENTER_CENTER,
            "Open a solar capture to begin",
            crate::theme::semibold(18.0),
            p.text,
        );
        ui.painter().text(
            c - vec2(0.0, 2.0),
            Align2::CENTER_CENTER,
            "SER, AVI or video, or drop a file here",
            sans(13.0),
            p.muted,
        );
        let btn = Rect::from_center_size(c + vec2(0.0, 44.0), vec2(180.0, 44.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(btn));
        if widgets::button(&mut child, "Open capture", Kind::Primary, true, 180.0).clicked() {
            self.pick_capture();
        }
    }

    fn scrubber(&mut self, ui: &mut Ui) {
        let p = palette();
        let n = self.count();
        let label = scrub_label(
            self.index,
            n,
            self.curve.relative(self.index),
            self.curve.rank_of(self.index),
        );
        let mut idx = self.index + 1;
        ui.horizontal(|ui| {
            let l = ui.add(Label::new(
                RichText::new(label).font(mono(13.0)).color(p.text),
            ));
            ui.spacing_mut().slider_width = (ui.available_width() - 12.0).max(80.0);
            let s = ui
                .add(
                    Slider::new(&mut idx, 1..=n)
                        .show_value(false)
                        .trailing_fill(true),
                )
                .on_hover_text("Scrub frames (left and right arrow keys)");
            s.labelled_by(l.id);
        });
        self.index = (idx - 1).min(n - 1);
        ui.add_space(10.0);

        ui.horizontal(|ui| {
            widgets::overline(ui, "Quality, sorted");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if !self.curve.is_empty() {
                    ui.add(Label::new(
                        RichText::new(keep_label(self.kept(), n))
                            .font(mono(12.5))
                            .color(p.accent),
                    ));
                }
            });
        });
        let jump = curve::show(ui, &self.curve, self.kept(), Some(self.index), 112.0);
        if let Some(f) = jump {
            self.index = f.min(n - 1);
        }
        if self.scoring == Scoring::Running && self.curve.is_empty() {
            ui.ctx().request_repaint();
        }
    }
}
