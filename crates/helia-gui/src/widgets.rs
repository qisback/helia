// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Custom-painted widgets. Each one reports an AccessKit label and role.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Response, Sense, Stroke,
    StrokeKind, Ui, Vec2, WidgetInfo, WidgetType, pos2, vec2,
};

use crate::model::{Status, Step, Tone};
use crate::theme::{self, HIT, RADIUS, RADIUS_LARGE, mono, palette, sans, semibold};

/// Colour bands drawn in a [`swatch`] strip.
const SWATCH_STEPS: usize = 32;

fn radius() -> CornerRadius {
    CornerRadius::same(RADIUS)
}

/// Button flavour.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Amber filled.
    Primary,
    /// Outlined.
    Outline,
}

/// A 44 px button. `min_width` of 0 sizes to the label.
pub fn button(ui: &mut Ui, label: &str, kind: Kind, enabled: bool, min_width: f32) -> Response {
    let p = palette();
    let galley =
        ui.painter()
            .layout_no_wrap(label.to_owned(), semibold(14.0), Color32::PLACEHOLDER);
    let size = vec2((galley.size().x + 36.0).max(min_width), HIT);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, label));
    if enabled {
        ui.output_mut(|o| {
            if resp.hovered() {
                o.cursor_icon = CursorIcon::PointingHand;
            }
        });
    }
    let hot = enabled && resp.hovered();
    let (fill, stroke, text) = match (kind, enabled) {
        (Kind::Primary, true) => (
            if hot { p.accent_hover } else { p.accent },
            Stroke::NONE,
            p.on_accent,
        ),
        (Kind::Outline, true) => (
            if hot { p.raised } else { Color32::TRANSPARENT },
            Stroke::new(1.0, if hot { p.accent_hover } else { p.border_strong }),
            p.text,
        ),
        (_, false) => (p.raised, Stroke::new(1.0, p.border), p.faint),
    };
    let painter = ui.painter();
    painter.rect(rect, radius(), fill, stroke, StrokeKind::Inside);
    let g = painter.layout_no_wrap(label.to_owned(), semibold(14.0), text);
    painter.galley(rect.center() - g.size() / 2.0, g, text);
    if resp.has_focus() {
        focus_ring(ui, rect);
    }
    resp
}

fn focus_ring(ui: &Ui, rect: Rect) {
    ui.painter().rect_stroke(
        rect.expand(2.0),
        radius(),
        Stroke::new(2.0, palette().accent_hover),
        StrokeKind::Outside,
    );
}

/// A small amber outlined label, such as "Pre-release", that is not a control.
pub fn badge(ui: &mut Ui, text: &str, tip: &str) -> Response {
    let p = palette();
    let g = ui
        .painter()
        .layout_no_wrap(text.to_owned(), semibold(11.5), p.accent);
    let size = vec2(g.size().x + 16.0, 22.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, text));
    ui.painter().rect(
        rect,
        CornerRadius::same(11),
        p.selected_fill,
        Stroke::new(1.0, p.selected_border),
        StrokeKind::Inside,
    );
    ui.painter()
        .galley(rect.center() - g.size() / 2.0, g, p.accent);
    resp.on_hover_text(tip)
}

/// Toggle or choice chip; `selected` fills it amber-tinted.
pub fn chip(ui: &mut Ui, label: &str, selected: bool, enabled: bool) -> Response {
    chip_ex(ui, label, selected, enabled, false)
}

/// Chip set in the monospace face.
pub fn chip_mono(ui: &mut Ui, label: &str, selected: bool, enabled: bool) -> Response {
    chip_ex(ui, label, selected, enabled, true)
}

fn chip_ex(ui: &mut Ui, label: &str, selected: bool, enabled: bool, monospace: bool) -> Response {
    let p = palette();
    let font = if monospace { mono(12.5) } else { sans(13.0) };
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), Color32::PLACEHOLDER);
    let size = vec2(galley.size().x + 28.0, 34.0);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Button, enabled, selected, label));
    let hot = enabled && resp.hovered();
    if hot {
        ui.output_mut(|o| o.cursor_icon = CursorIcon::PointingHand);
    }
    let (fill, border, text) = if !enabled {
        (p.panel, p.border, p.faint)
    } else if selected {
        (p.selected_fill, p.selected_border, p.accent)
    } else if hot {
        (p.raised, p.border_strong, p.text)
    } else {
        (p.raised, p.border, p.muted)
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        radius(),
        fill,
        Stroke::new(1.0, border),
        StrokeKind::Inside,
    );
    let g = painter.layout_no_wrap(label.to_owned(), font, text);
    painter.galley(rect.center() - g.size() / 2.0, g, text);
    if resp.has_focus() {
        focus_ring(ui, rect);
    }
    resp
}

/// Drawn icons for square buttons.
#[derive(Clone, Copy)]
pub enum Icon {
    /// Minus sign.
    Minus,
    /// Plus sign.
    Plus,
    /// Left rail toggle.
    Rail,
    /// Right inspector toggle.
    Inspector,
}

/// 34 px square icon button with a tooltip that doubles as its accessible name.
pub fn icon_button(ui: &mut Ui, icon: Icon, tip: &str, enabled: bool, active: bool) -> Response {
    let p = palette();
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(34.0), sense);
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Button, enabled, active, tip));
    let hot = enabled && resp.hovered();
    let (fill, border, ink) = if !enabled {
        (p.panel, p.border, p.faint)
    } else if active {
        (p.selected_fill, p.selected_border, p.accent)
    } else if hot {
        (p.raised, p.border_strong, p.text)
    } else {
        (p.raised, p.border, p.muted)
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        radius(),
        fill,
        Stroke::new(1.0, border),
        StrokeKind::Inside,
    );
    let c = rect.center();
    let s = Stroke::new(1.6, ink);
    match icon {
        Icon::Minus => {
            painter.line_segment([c - vec2(6.0, 0.0), c + vec2(6.0, 0.0)], s);
        }
        Icon::Plus => {
            painter.line_segment([c - vec2(6.0, 0.0), c + vec2(6.0, 0.0)], s);
            painter.line_segment([c - vec2(0.0, 6.0), c + vec2(0.0, 6.0)], s);
        }
        Icon::Rail | Icon::Inspector => {
            let body = Rect::from_center_size(c, vec2(16.0, 12.0));
            painter.rect_stroke(body, CornerRadius::same(2), s, StrokeKind::Inside);
            let x = if matches!(icon, Icon::Rail) {
                body.left() + 5.0
            } else {
                body.right() - 5.0
            };
            painter.line_segment([pos2(x, body.top()), pos2(x, body.bottom())], s);
        }
    }
    if resp.has_focus() {
        focus_ring(ui, rect);
    }
    resp.on_hover_text(tip)
}

/// Overline: small uppercase caption.
pub fn overline(ui: &mut Ui, text: &str) {
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .font(semibold(11.0))
            .color(palette().faint),
    );
}

/// Mono text in a colour.
pub fn mono_text(text: impl Into<String>, size: f32, colour: Color32) -> egui::RichText {
    egui::RichText::new(text.into())
        .font(mono(size))
        .color(colour)
}

/// Card with a label and a large mono value.
pub fn stat_card(ui: &mut Ui, label: &str, value: &str, width: f32) {
    let p = palette();
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 72.0), Sense::hover());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, format!("{label}: {value}")));
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(RADIUS_LARGE),
        p.raised,
        Stroke::new(1.0, p.border),
        StrokeKind::Inside,
    );
    painter.text(
        rect.left_top() + vec2(14.0, 12.0),
        Align2::LEFT_TOP,
        label.to_uppercase(),
        semibold(10.5),
        p.faint,
    );
    painter.text(
        rect.left_bottom() + vec2(14.0, -10.0),
        Align2::LEFT_BOTTOM,
        value,
        mono(22.0),
        p.text,
    );
}

/// Row with a title, a subtitle and an On/Off pill. Returns true when toggled.
pub fn toggle_row(ui: &mut Ui, title: &str, subtitle: &str, on: &mut bool, enabled: bool) -> bool {
    let p = palette();
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 56.0), sense);
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, enabled, *on, title));
    let hot = enabled && resp.hovered();
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(RADIUS_LARGE),
        p.raised,
        Stroke::new(1.0, if hot { p.border_strong } else { p.border }),
        StrokeKind::Inside,
    );
    let text = if enabled { p.text } else { p.faint };
    painter.text(
        rect.left_top() + vec2(14.0, 11.0),
        Align2::LEFT_TOP,
        title,
        sans(14.0),
        text,
    );
    painter.text(
        rect.left_bottom() + vec2(14.0, -10.0),
        Align2::LEFT_BOTTOM,
        subtitle,
        sans(12.0),
        p.faint,
    );
    let pill = Rect::from_center_size(
        pos2(rect.right() - 14.0 - 24.0, rect.center().y),
        vec2(48.0, 26.0),
    );
    let (fill, ink, label) = if *on {
        (p.accent, p.on_accent, "On")
    } else {
        (p.ground, p.faint, "Off")
    };
    painter.rect(
        pill,
        CornerRadius::same(13),
        fill,
        Stroke::new(1.0, if *on { p.accent } else { p.border_strong }),
        StrokeKind::Inside,
    );
    painter.text(
        pill.center(),
        Align2::CENTER_CENTER,
        label,
        semibold(12.0),
        ink,
    );
    if resp.has_focus() {
        focus_ring(ui, rect);
    }
    if resp.clicked() {
        *on = !*on;
        return true;
    }
    false
}

/// Thin progress rail.
pub fn progress_bar(ui: &mut Ui, fraction: f32) {
    progress_rail(ui, Some(fraction), "Stack progress");
}

/// Thin progress rail. `None` is an indeterminate bar: a short amber segment
/// sliding along the rail, for work whose length is not known.
pub fn progress_rail(ui: &mut Ui, fraction: Option<f32>, label: &str) -> Response {
    let p = palette();
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 6.0), Sense::hover());
    resp.widget_info(|| {
        let mut i = WidgetInfo::labeled(WidgetType::ProgressIndicator, true, label);
        i.value = fraction.map(f64::from);
        i
    });
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(3), p.ground);
    if let Some(f) = fraction {
        let mut fill = rect;
        fill.set_width(rect.width() * f.clamp(0.0, 1.0));
        painter.rect_filled(fill, CornerRadius::same(3), p.accent);
    } else {
        let t = ui.input(|i| i.time) as f32;
        let phase = (t * 0.9).fract();
        let seg = rect.width() * 0.28;
        let x0 = rect.left() + (rect.width() + seg) * phase - seg;
        let seg_rect = Rect::from_min_max(
            pos2(x0.max(rect.left()), rect.top()),
            pos2((x0 + seg).min(rect.right()), rect.bottom()),
        );
        if seg_rect.width() > 0.0 {
            painter.rect_filled(seg_rect, CornerRadius::same(3), p.accent);
        }
        ui.ctx().request_repaint();
    }
    resp
}

/// A choice card for a colour preset: a strip showing the ramp from black to
/// white through the preset's colours, then its name and wavelength.
pub fn swatch(
    ui: &mut Ui,
    name: &str,
    sub: &str,
    lut: &helia_core::ColourLut,
    selected: bool,
    width: f32,
) -> Response {
    let p = palette();
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 74.0), Sense::click());
    resp.widget_info(|| {
        WidgetInfo::selected(WidgetType::Button, true, selected, format!("{name}, {sub}"))
    });
    let hot = resp.hovered();
    if hot {
        ui.output_mut(|o| o.cursor_icon = CursorIcon::PointingHand);
    }
    let painter = ui.painter();
    let (fill, border) = if selected {
        (p.selected_fill, p.accent)
    } else if hot {
        (p.raised, p.border_strong)
    } else {
        (p.raised, p.border)
    };
    painter.rect(
        rect,
        radius(),
        fill,
        Stroke::new(if selected { 1.5 } else { 1.0 }, border),
        StrokeKind::Inside,
    );
    let strip = Rect::from_min_size(rect.min + vec2(10.0, 10.0), vec2(width - 20.0, 24.0));
    let seg = strip.width() / SWATCH_STEPS as f32;
    for i in 0..SWATCH_STEPS {
        let c = lut.lookup(i as f32 / (SWATCH_STEPS - 1) as f32);
        let col = Color32::from_rgb(
            (c[0] * 255.0).round() as u8,
            (c[1] * 255.0).round() as u8,
            (c[2] * 255.0).round() as u8,
        );
        let x0 = strip.left() + seg * i as f32;
        painter.rect_filled(
            Rect::from_min_max(pos2(x0, strip.top()), pos2(x0 + seg + 0.5, strip.bottom())),
            CornerRadius::ZERO,
            col,
        );
    }
    painter.rect_stroke(
        strip,
        CornerRadius::same(4),
        Stroke::new(1.0, p.border_strong),
        StrokeKind::Outside,
    );
    painter.text(
        rect.left_top() + vec2(10.0, 44.0),
        Align2::LEFT_TOP,
        name,
        semibold(13.0),
        if selected { p.accent } else { p.text },
    );
    painter.text(
        rect.left_top() + vec2(10.0, 58.0),
        Align2::LEFT_TOP,
        sub,
        mono(11.0),
        p.faint,
    );
    if resp.has_focus() {
        focus_ring(ui, rect);
    }
    resp
}

/// Sun glyph: disc with eight rays.
pub fn paint_sun(painter: &egui::Painter, c: Pos2, r: f32, colour: Color32) {
    painter.circle_filled(c, r * 0.5, colour);
    for k in 0..8 {
        let a = k as f32 * std::f32::consts::FRAC_PI_4;
        let d = vec2(a.cos(), a.sin());
        painter.line_segment(
            [
                c + d * (r * 0.72),
                c + d * (r * if k % 2 == 0 { 1.0 } else { 0.9 }),
            ],
            Stroke::new(1.6, colour),
        );
    }
}

/// Status dot.
pub fn dot(painter: &egui::Painter, c: Pos2, r: f32, colour: Color32) {
    painter.circle_filled(c, r, colour);
}

/// Height of one row of the pipeline rail.
pub const STEP_ROW_HEIGHT: f32 = 80.0;

/// One row of the pipeline rail.
pub fn step_row(
    ui: &mut Ui,
    step: Step,
    status: &Status,
    hint: &str,
    selected: bool,
    first: bool,
    last: bool,
) -> Response {
    let p = palette();
    let (rect, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), STEP_ROW_HEIGHT), Sense::click());
    let label = format!(
        "Step {}, {}: {}. {}",
        step.number(),
        step.title(),
        status.text,
        hint
    );
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, selected, &label));
    let hot = resp.hovered();
    if hot {
        ui.output_mut(|o| o.cursor_icon = CursorIcon::PointingHand);
    }
    let painter = ui.painter();
    let cx = rect.left() + 28.0;
    let cy = rect.top() + 29.0;
    // Connector between the number circles.
    let line = Stroke::new(1.0, p.border_strong);
    if !first {
        painter.line_segment(
            [pos2(cx, rect.top() - 4.0), pos2(cx, cy - 13.0 - 2.0)],
            line,
        );
    }
    if !last {
        painter.line_segment(
            [pos2(cx, cy + 13.0 + 2.0), pos2(cx, rect.bottom() + 4.0)],
            line,
        );
    }
    if selected {
        painter.rect(
            rect,
            radius(),
            p.selected_fill,
            Stroke::new(1.0, p.selected_border),
            StrokeKind::Inside,
        );
    } else if hot {
        painter.rect_filled(rect, radius(), p.raised);
    }
    let (circle_fill, circle_stroke, num_col) = if selected {
        (p.accent, p.accent, p.on_accent)
    } else {
        (p.raised, p.border_strong, p.muted)
    };
    painter.circle(
        pos2(cx, cy),
        13.0,
        circle_fill,
        Stroke::new(1.0, circle_stroke),
    );
    painter.text(
        pos2(cx, cy),
        Align2::CENTER_CENTER,
        step.number().to_string(),
        mono(12.5),
        num_col,
    );
    let tx = rect.left() + 54.0;
    painter.text(
        pos2(tx, cy - 9.0),
        Align2::LEFT_CENTER,
        step.title(),
        semibold(15.0),
        p.text,
    );
    let hint_galley = painter.layout(
        hint.to_owned(),
        sans(11.5),
        if selected { p.muted } else { p.faint },
        rect.right() - tx - 8.0,
    );
    painter.galley(pos2(tx, cy + 22.0), hint_galley, p.faint);
    let (ink, dot_col) = match status.tone {
        Tone::Idle => (p.faint, None),
        Tone::Running => (p.accent, Some(p.accent)),
        Tone::Done => (p.muted, Some(p.success)),
    };
    let mut sx = tx;
    if let Some(c) = dot_col {
        dot(painter, pos2(tx + 3.0, cy + 10.0), 3.0, c);
        sx += 13.0;
    }
    painter.text(
        pos2(sx, cy + 10.0),
        Align2::LEFT_CENTER,
        &status.text,
        mono(11.5),
        ink,
    );
    if resp.has_focus() {
        focus_ring(ui, rect);
    }
    resp
}

/// Chip with a status dot, for the GPU indicator.
pub fn status_chip(ui: &mut Ui, dot_colour: Color32, main: &str, detail: &str, tip: &str) {
    let p = palette();
    let g1 = ui
        .painter()
        .layout_no_wrap(main.to_owned(), mono(12.5), p.text);
    let g2 = (!detail.is_empty()).then(|| {
        ui.painter()
            .layout_no_wrap(detail.to_owned(), mono(12.5), p.muted)
    });
    let w = 14.0 + 12.0 + g1.size().x + g2.as_ref().map_or(0.0, |g| g.size().x + 10.0) + 14.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 34.0), Sense::hover());
    resp.widget_info(|| {
        WidgetInfo::labeled(
            WidgetType::Label,
            true,
            format!("Compute backend: {main} {detail}"),
        )
    });
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(17),
        p.raised,
        Stroke::new(1.0, p.border),
        StrokeKind::Inside,
    );
    dot(
        painter,
        pos2(rect.left() + 17.0, rect.center().y),
        4.0,
        dot_colour,
    );
    let mut x = rect.left() + 29.0;
    let y = rect.center().y;
    let w1 = g1.size().x;
    painter.galley(pos2(x, y - g1.size().y / 2.0), g1, p.text);
    x += w1 + 10.0;
    if let Some(g) = g2 {
        painter.galley(pos2(x, y - g.size().y / 2.0), g, p.muted);
    }
    resp.on_hover_text(tip);
}

/// Left-aligned heading font helper for inspector titles.
#[must_use]
pub fn title_font() -> FontId {
    theme::semibold(22.0)
}
