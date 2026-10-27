// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Design tokens, embedded fonts and the egui style.
//!
//! Every colour lives in [`Palette`]; a light palette can be added beside
//! [`DARK`] without touching the widgets.

use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Stroke,
    TextStyle, Theme, ThemePreference, Vec2,
};

/// Colour tokens.
pub struct Palette {
    /// Window ground.
    pub ground: Color32,
    /// Side panels.
    pub panel: Color32,
    /// Raised controls.
    pub raised: Color32,
    /// Hairline borders.
    pub border: Color32,
    /// Hovered or emphasised borders.
    pub border_strong: Color32,
    /// Primary text.
    pub text: Color32,
    /// Secondary text.
    pub muted: Color32,
    /// Tertiary text and overlines.
    pub faint: Color32,
    /// Sun amber.
    pub accent: Color32,
    /// Hovered accent.
    pub accent_hover: Color32,
    /// Fill of selected rows and chips.
    pub selected_fill: Color32,
    /// Border of selected rows and chips.
    pub selected_border: Color32,
    /// Text on an accent fill.
    pub on_accent: Color32,
    /// "Ready" dots.
    pub success: Color32,
    /// Image viewer background.
    pub viewer: Color32,
    /// Alignment point overlay (premultiplied RGBA 255,236,200 at 45 %).
    pub grid: Color32,
    /// Fill of an error message.
    pub error_fill: Color32,
    /// Border of an error message.
    pub error_border: Color32,
    /// Text of an error message.
    pub error_text: Color32,
}

/// The dark palette.
pub const DARK: Palette = Palette {
    ground: Color32::from_rgb(0x0E, 0x0D, 0x0C),
    panel: Color32::from_rgb(0x14, 0x12, 0x10),
    raised: Color32::from_rgb(0x1F, 0x1C, 0x19),
    border: Color32::from_rgb(0x2C, 0x28, 0x24),
    border_strong: Color32::from_rgb(0x3A, 0x35, 0x30),
    text: Color32::from_rgb(0xEC, 0xE6, 0xDF),
    muted: Color32::from_rgb(0xB4, 0xAA, 0xA0),
    faint: Color32::from_rgb(0x9A, 0x90, 0x87),
    accent: Color32::from_rgb(0xF5, 0xA5, 0x24),
    accent_hover: Color32::from_rgb(0xFF, 0xC8, 0x61),
    selected_fill: Color32::from_rgb(0x2A, 0x21, 0x16),
    selected_border: Color32::from_rgb(0x4A, 0x3A, 0x1E),
    on_accent: Color32::from_rgb(0x1A, 0x12, 0x06),
    success: Color32::from_rgb(0x5F, 0xC0, 0x8A),
    viewer: Color32::from_rgb(0x08, 0x07, 0x07),
    grid: Color32::from_rgba_premultiplied(115, 106, 90, 115),
    error_fill: Color32::from_rgb(0x2A, 0x16, 0x14),
    error_border: Color32::from_rgb(0x6B, 0x2E, 0x28),
    error_text: Color32::from_rgb(0xF0, 0x9A, 0x8E),
};

/// Palette in use.
#[must_use]
pub fn palette() -> &'static Palette {
    &DARK
}

/// Corner radius of controls.
pub const RADIUS: u8 = 8;
/// Corner radius of cards and the viewer.
pub const RADIUS_LARGE: u8 = 10;
/// Minimum hit size of primary controls.
pub const HIT: f32 = 44.0;

const SEMIBOLD: &str = "plex-sans-semibold";

/// Semibold UI font.
#[must_use]
pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}

/// Regular UI font.
#[must_use]
pub fn sans(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

/// Monospace font for numbers and metadata.
#[must_use]
pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

fn fonts() -> FontDefinitions {
    let mut defs = FontDefinitions::empty();
    for (name, bytes) in [
        (
            "plex-sans",
            &include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf")[..],
        ),
        (
            SEMIBOLD,
            &include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.ttf")[..],
        ),
        (
            "plex-mono",
            &include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf")[..],
        ),
    ] {
        defs.font_data
            .insert(name.into(), Arc::new(FontData::from_static(bytes)));
    }
    defs.families
        .insert(FontFamily::Proportional, vec!["plex-sans".into()]);
    defs.families
        .insert(FontFamily::Monospace, vec!["plex-mono".into()]);
    defs.families.insert(
        FontFamily::Name(SEMIBOLD.into()),
        vec![SEMIBOLD.into(), "plex-sans".into()],
    );
    defs
}

/// Install fonts, visuals and spacing.
pub fn apply(ctx: &egui::Context) {
    let p = palette();
    ctx.set_fonts(fonts());
    ctx.set_theme(ThemePreference::Dark);
    ctx.set_visuals_of(Theme::Dark, visuals(p));
    ctx.style_mut_of(Theme::Dark, |s| {
        s.text_styles = [
            (TextStyle::Small, sans(12.0)),
            (TextStyle::Body, sans(14.0)),
            (TextStyle::Button, sans(14.0)),
            (TextStyle::Heading, semibold(22.0)),
            (TextStyle::Monospace, mono(13.0)),
        ]
        .into();
        s.spacing.item_spacing = Vec2::new(8.0, 8.0);
        s.spacing.button_padding = Vec2::new(14.0, 8.0);
        s.spacing.interact_size = Vec2::new(40.0, 32.0);
        s.spacing.window_margin = Margin::same(16);
        s.spacing.slider_width = 200.0;
        s.spacing.slider_rail_height = 6.0;
        s.interaction.tooltip_delay = 0.35;
    });
}

fn visuals(p: &Palette) -> egui::Visuals {
    let r = CornerRadius::same(RADIUS);
    let mut v = egui::Visuals::dark();
    v.override_text_color = None;
    v.panel_fill = p.panel;
    v.window_fill = p.panel;
    v.window_stroke = Stroke::new(1.0, p.border_strong);
    v.window_corner_radius = CornerRadius::same(RADIUS_LARGE);
    v.menu_corner_radius = r;
    v.extreme_bg_color = p.ground;
    v.faint_bg_color = p.raised;
    v.code_bg_color = p.raised;
    v.hyperlink_color = p.accent;
    v.weak_text_color = Some(p.faint);
    v.selection.bg_fill = p.accent;
    v.handle_shape = egui::style::HandleShape::Circle;
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.window_shadow = egui::Shadow::NONE;
    v.popup_shadow = egui::Shadow::NONE;

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.panel;
    w.noninteractive.weak_bg_fill = p.panel;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = r;

    w.inactive.bg_fill = p.raised;
    w.inactive.weak_bg_fill = p.raised;
    w.inactive.bg_stroke = Stroke::new(1.0, p.border);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = r;

    w.hovered.bg_fill = p.raised;
    w.hovered.weak_bg_fill = p.raised;
    w.hovered.bg_stroke = Stroke::new(1.0, p.border_strong);
    w.hovered.fg_stroke = Stroke::new(1.0, p.text);
    w.hovered.corner_radius = r;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = p.selected_fill;
    w.active.weak_bg_fill = p.selected_fill;
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.fg_stroke = Stroke::new(1.0, p.accent_hover);
    w.active.corner_radius = r;
    w.active.expansion = 0.0;

    w.open = w.inactive;
    v
}
