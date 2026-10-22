// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Drawing: a bitmap canvas, the palette and the fonts, so every figure looks
//! alike and needs nothing from the system.

use std::sync::Once;

use plotters::prelude::*;
use plotters::style::FontStyle;

use crate::{Error, Result};

/// Colour-blind-safe palette (Okabe and Ito 2008).
pub mod palette {
    use plotters::style::RGBColor;

    /// Blue, the main series.
    pub const BLUE: RGBColor = RGBColor(0, 114, 178);
    /// Orange, the second series.
    pub const ORANGE: RGBColor = RGBColor(230, 159, 0);
    /// Bluish green, kept or good.
    pub const GREEN: RGBColor = RGBColor(0, 158, 115);
    /// Vermillion, thresholds, limits and warnings.
    pub const RED: RGBColor = RGBColor(213, 94, 0);
    /// Reddish purple.
    pub const PURPLE: RGBColor = RGBColor(204, 121, 167);
    /// Sky blue.
    pub const SKY: RGBColor = RGBColor(86, 180, 233);
    /// Neutral grey for context.
    pub const GREY: RGBColor = RGBColor(120, 120, 120);
    /// Light grey for rejected data.
    pub const LIGHT: RGBColor = RGBColor(190, 190, 190);
}

/// Pixel size of a figure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl Size {
    /// The size reports use: wide enough to read in a browser column.
    pub const REPORT: Size = Size {
        width: 960,
        height: 540,
    };
}

impl Default for Size {
    fn default() -> Self {
        Self::REPORT
    }
}

/// The face every figure uses.
pub const FONT: &str = "sans-serif";

static FONTS: Once = Once::new();

fn register_fonts() {
    FONTS.call_once(|| {
        let regular: &'static [u8] =
            include_bytes!("../../helia-gui/assets/fonts/IBMPlexSans-Regular.ttf");
        let bold: &'static [u8] =
            include_bytes!("../../helia-gui/assets/fonts/IBMPlexSans-SemiBold.ttf");
        // The bundled faces are valid; a failure would only leave text out.
        let _ = plotters::style::register_font(FONT, FontStyle::Normal, regular);
        let _ = plotters::style::register_font(FONT, FontStyle::Bold, bold);
    });
}

/// Turn a plotters failure into this crate's error.
pub fn plot_err<E: std::fmt::Display>(e: E) -> Error {
    Error::Plot(e.to_string())
}

/// Draw on a white bitmap of `size` and return it as PNG bytes.
pub fn render<F>(size: Size, draw: F) -> Result<Vec<u8>>
where
    F: FnOnce(&DrawingArea<BitMapBackend<'_>, plotters::coord::Shift>) -> Result<()>,
{
    register_fonts();
    let (w, h) = (size.width, size.height);
    let mut buf = vec![255u8; w as usize * h as usize * 3];
    {
        let root = BitMapBackend::with_buffer(&mut buf, (w, h)).into_drawing_area();
        root.fill(&WHITE).map_err(plot_err)?;
        draw(&root)?;
        root.present().map_err(plot_err)?;
    }
    encode_png(&buf, w, h)
}

/// Encode packed RGB8 as PNG.
pub fn encode_png(rgb: &[u8], w: u32, h: u32) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().map_err(|e| Error::Png(e.to_string()))?;
        writer
            .write_image_data(rgb)
            .map_err(|e| Error::Png(e.to_string()))?;
    }
    Ok(out)
}

/// A text style in the report face, dark grey.
#[must_use]
pub fn text(size: u32) -> TextStyle<'static> {
    TextStyle::from((FONT, size).into_font()).color(&RGBColor(30, 30, 30))
}

/// Round `lo..hi` outwards to a pleasant axis range, padded by `pad` of the span.
#[must_use]
pub fn padded(lo: f64, hi: f64, pad: f64) -> (f64, f64) {
    let (lo, hi) = if lo.is_finite() && hi.is_finite() {
        (lo, hi)
    } else {
        (0.0, 1.0)
    };
    let span = if hi > lo {
        hi - lo
    } else {
        lo.abs().max(1.0) * 0.1
    };
    (lo - span * pad, hi + span * pad)
}

/// An axis tick or a table number with a sensible count of digits and no
/// trailing zeros.
#[must_use]
pub fn tick(v: f64) -> String {
    let a = v.abs();
    let s = if a == 0.0 {
        return "0".into();
    } else if a >= 1e5 || a < 1e-7 {
        return format!("{v:.2e}");
    } else if a >= 100.0 {
        format!("{v:.0}")
    } else if a >= 10.0 {
        format!("{v:.1}")
    } else if a >= 1.0 {
        format!("{v:.2}")
    } else {
        let decimals = (2.0 - a.log10().floor()).clamp(3.0, 9.0) as usize;
        format!("{v:.decimals$}")
    };
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// An axis tick for an axis spanning `span`: enough decimals to tell the
/// ticks apart (a score axis from 0.0004 to 0.0015 needs four), no trailing zeros.
#[must_use]
pub fn tick_span(v: f64, span: f64) -> String {
    if v == 0.0 {
        return "0".into();
    }
    if v.abs() >= 1e6 || span.abs() < 1e-7 {
        return format!("{v:.2e}");
    }
    let step = (span.abs() / 4.0).max(1e-9);
    let decimals = (-step.log10().floor()).clamp(0.0, 8.0) as usize;
    let s = format!("{v:.decimals$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// A whole-number tick (frame numbers, ranks, counts).
#[must_use]
pub fn tick_int(v: f64) -> String {
    format!("{v:.0}")
}

/// The colour a fraction `t` (0 to 1) of the way from `a` to `b`.
#[must_use]
pub fn mix(a: RGBColor, b: RGBColor, t: f64) -> RGBColor {
    let t = t.clamp(0.0, 1.0);
    let m = |x: u8, y: u8| (f64::from(x) * (1.0 - t) + f64::from(y) * t).round() as u8;
    RGBColor(m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}
