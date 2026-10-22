// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Plots drawn on a picture of the Sun: a grey backdrop at the right aspect,
//! cells and vectors in image pixels, a colour bar.

use helia_core::Frame;
use plotters::coord::Shift;
use plotters::coord::types::RangedCoordf64;
use plotters::prelude::*;

use crate::canvas::{FONT, Size, plot_err, text, tick_span};
use crate::{Error, Result};

/// An 8-bit grey picture for a backdrop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GreyImage {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// Row-major samples.
    pub data: Vec<u8>,
}

impl GreyImage {
    /// A frame stretched to 8 bits (0.5th to 99.8th percentile, gamma 0.7)
    /// and shrunk by an integer factor so that neither side exceeds `max_side`.
    /// Returns the picture and the factor.
    #[must_use]
    pub fn from_frame(f: &Frame, max_side: usize) -> (Self, usize) {
        let factor = f.width.max(f.height).div_ceil(max_side.max(1)).max(1);
        let (w, h) = (f.width / factor, f.height / factor);
        let mut v = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                let mut s = 0.0f32;
                for j in 0..factor {
                    let row = f.row(y * factor + j);
                    for i in 0..factor {
                        s += row[x * factor + i];
                    }
                }
                v.push(s / (factor * factor) as f32);
            }
        }
        let mut sorted: Vec<f32> = v.iter().copied().filter(|x| x.is_finite()).collect();
        sorted.sort_by(f32::total_cmp);
        let at = |q: f32| {
            sorted
                .get(((sorted.len().saturating_sub(1)) as f32 * q) as usize)
                .copied()
                .unwrap_or(0.0)
        };
        let (lo, hi) = (at(0.005), at(0.998));
        let span = (hi - lo).max(1e-12);
        let data = v
            .iter()
            .map(|&x| (((x - lo) / span).clamp(0.0, 1.0).powf(0.7) * 255.0) as u8)
            .collect();
        (
            Self {
                width: w,
                height: h,
                data,
            },
            factor,
        )
    }

    /// As a PNG.
    pub fn to_png(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, self.width as u32, self.height as u32);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().map_err(|e| Error::Png(e.to_string()))?;
            w.write_image_data(&self.data)
                .map_err(|e| Error::Png(e.to_string()))?;
        }
        Ok(out)
    }

    /// From a grey PNG written by [`Self::to_png`].
    pub fn from_png(bytes: &[u8]) -> Result<Self> {
        let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
        dec.set_transformations(png::Transformations::EXPAND);
        let mut reader = dec.read_info().map_err(|e| Error::Png(e.to_string()))?;
        let mut buf = vec![
            0;
            reader
                .output_buffer_size()
                .ok_or_else(|| Error::Png("picture too large".into()))?
        ];
        let info = reader
            .next_frame(&mut buf)
            .map_err(|e| Error::Png(e.to_string()))?;
        if info.color_type != png::ColorType::Grayscale || info.bit_depth != png::BitDepth::Eight {
            return Err(Error::Png(
                "the backdrop is not an 8-bit grey picture".into(),
            ));
        }
        buf.truncate(info.buffer_size());
        Ok(Self {
            width: info.width as usize,
            height: info.height as usize,
            data: buf,
        })
    }

    fn sample(&self, x: f64, y: f64) -> u8 {
        let xi = (x.floor().max(0.0) as usize).min(self.width.saturating_sub(1));
        let yi = (y.floor().max(0.0) as usize).min(self.height.saturating_sub(1));
        self.data[yi * self.width + xi]
    }
}

/// Viridis, sampled at nine stops: dark purple to yellow, readable in grey.
#[must_use]
pub fn viridis(t: f64) -> RGBColor {
    const STOPS: [(u8, u8, u8); 9] = [
        (68, 1, 84),
        (72, 36, 117),
        (65, 68, 135),
        (53, 95, 141),
        (42, 120, 142),
        (34, 168, 132),
        (68, 191, 112),
        (122, 209, 81),
        (253, 231, 37),
    ];
    let t = t.clamp(0.0, 1.0) * (STOPS.len() - 1) as f64;
    let i = (t.floor() as usize).min(STOPS.len() - 2);
    let f = t - i as f64;
    let (a, b) = (STOPS[i], STOPS[i + 1]);
    let m = |x: u8, y: u8| (f64::from(x) * (1.0 - f) + f64::from(y) * f).round() as u8;
    RGBColor(m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

/// Where the picture sits in the figure.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    /// Left edge of the picture in the figure, pixels.
    pub x0: i32,
    /// Top edge.
    pub y0: i32,
    /// Width of the picture as drawn.
    pub w: i32,
    /// Height of the picture as drawn.
    pub h: i32,
}

/// The chart type the picture plots use.
pub type PictureChart<'a, 'b> =
    ChartContext<'a, BitMapBackend<'b>, Cartesian2d<RangedCoordf64, RangedCoordf64>>;

const TITLE_H: i32 = 44;
const X_LABELS: i32 = 44;
const Y_LABELS: i32 = 64;
const PAD: i32 = 14;

/// A chart whose data coordinates are the picture's pixels (y down), with
/// equal scale on both axes, the title drawn above and `right` pixels kept free on
/// the right (for a colour bar). The picture is `pw` by `ph` pixels.
pub fn picture_chart<'a, 'b>(
    root: &'a DrawingArea<BitMapBackend<'b>, Shift>,
    size: Size,
    (pw, ph): (usize, usize),
    right: i32,
    title: &str,
) -> Result<(PictureChart<'a, 'b>, Layout)> {
    let (w, h) = (
        i32::try_from(size.width).unwrap_or(960),
        i32::try_from(size.height).unwrap_or(540),
    );
    let avail_w = w - Y_LABELS - PAD - right;
    let avail_h = h - TITLE_H - X_LABELS - PAD;
    let scale = (f64::from(avail_w) / pw.max(1) as f64).min(f64::from(avail_h) / ph.max(1) as f64);
    let (dw, dh) = (
        (pw as f64 * scale).floor() as i32,
        (ph as f64 * scale).floor() as i32,
    );
    // Centre the picture in the space that is left.
    let ml = PAD / 2 + (avail_w - dw) / 2;
    let mt = TITLE_H + (avail_h - dh) / 2;
    let mr = w - Y_LABELS - ml - dw;
    let mb = h - mt - dh - X_LABELS;
    root.draw(&Text::new(
        title.to_string(),
        (PAD, 10),
        (FONT, 18).into_font().color(&RGBColor(30, 30, 30)),
    ))
    .map_err(plot_err)?;
    let chart = ChartBuilder::on(root)
        .margin_left(ml.max(0))
        .margin_top(mt.max(0))
        .margin_right(mr.max(0))
        .margin_bottom(mb.max(0))
        .x_label_area_size(X_LABELS)
        .y_label_area_size(Y_LABELS)
        .build_cartesian_2d(0.0..pw as f64, ph as f64..0.0)
        .map_err(plot_err)?;
    let layout = Layout {
        x0: ml + Y_LABELS,
        y0: mt,
        w: dw,
        h: dh,
    };
    Ok((chart, layout))
}

/// Paint `img` over the chart's plotting area, brightened by `fade` towards
/// white (0 none, 1 white) so that overlays stand out.
pub fn paint_backdrop(
    root: &DrawingArea<BitMapBackend<'_>, Shift>,
    layout: Layout,
    img: &GreyImage,
    dim: f64,
) -> Result<()> {
    for py in 0..layout.h {
        let sy = (f64::from(py) + 0.5) / f64::from(layout.h) * img.height as f64;
        for px in 0..layout.w {
            let sx = (f64::from(px) + 0.5) / f64::from(layout.w) * img.width as f64;
            let v = (f64::from(img.sample(sx, sy)) * dim) as u8;
            root.draw_pixel((layout.x0 + px, layout.y0 + py), &RGBColor(v, v, v))
                .map_err(plot_err)?;
        }
    }
    Ok(())
}

/// Axes of a picture chart: ticks and labels, no grid.
pub fn picture_axes(chart: &mut PictureChart<'_, '_>, x_desc: &str, y_desc: &str) -> Result<()> {
    chart
        .configure_mesh()
        .disable_mesh()
        .x_desc(x_desc)
        .y_desc(y_desc)
        .label_style((FONT, 13))
        .axis_desc_style((FONT, 14))
        .x_label_formatter(&|v| format!("{v:.0}"))
        .y_label_formatter(&|v| format!("{v:.0}"))
        .draw()
        .map_err(plot_err)
}

/// A vertical colour bar from `lo` (bottom) to `hi` (top) in the right margin.
pub fn colour_bar(
    root: &DrawingArea<BitMapBackend<'_>, Shift>,
    layout: Layout,
    (lo, hi): (f64, f64),
    label: &str,
) -> Result<()> {
    let x = layout.x0 + layout.w + 18;
    let (top, height) = (layout.y0, layout.h.max(40));
    let steps = height;
    for i in 0..steps {
        let t = 1.0 - f64::from(i) / f64::from(steps - 1);
        root.draw(&Rectangle::new(
            [(x, top + i), (x + 16, top + i + 1)],
            viridis(t).filled(),
        ))
        .map_err(plot_err)?;
    }
    root.draw(&Rectangle::new(
        [(x, top), (x + 16, top + height)],
        RGBColor(90, 90, 90).stroke_width(1),
    ))
    .map_err(plot_err)?;
    let st = text(13);
    for k in 0..=4 {
        let t = f64::from(k) / 4.0;
        let y = top + height - (t * f64::from(height)) as i32;
        root.draw(&Text::new(
            tick_span(lo + t * (hi - lo), hi - lo),
            (x + 22, (y - 7).max(top - 7)),
            st.clone(),
        ))
        .map_err(plot_err)?;
    }
    root.draw(&Text::new(
        label.to_string(),
        (x - 4, top - 24),
        (FONT, 13).into_font().color(&RGBColor(30, 30, 30)),
    ))
    .map_err(plot_err)
}

/// An arrow from `(x, y)` along `(dx, dy)` in picture pixels, with a head
/// a quarter of its length.
pub fn arrow(
    chart: &mut PictureChart<'_, '_>,
    (x, y): (f64, f64),
    (dx, dy): (f64, f64),
    colour: RGBColor,
) -> Result<()> {
    let len = dx.hypot(dy);
    let tip = (x + dx, y + dy);
    let mut shapes = vec![PathElement::new(vec![(x, y), tip], colour.stroke_width(2))];
    if len > 1e-9 {
        let head = 0.3 * len;
        let ang = dy.atan2(dx);
        for s in [-1.0f64, 1.0] {
            let a = ang + std::f64::consts::PI + s * 0.45;
            shapes.push(PathElement::new(
                vec![tip, (tip.0 + head * a.cos(), tip.1 + head * a.sin())],
                colour.stroke_width(2),
            ));
        }
    }
    chart.draw_series(shapes).map_err(plot_err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_backdrop_png_round_trips() {
        let g = GreyImage {
            width: 5,
            height: 3,
            data: (0..15).map(|i| i * 17).collect(),
        };
        let back = GreyImage::from_png(&g.to_png().expect("png")).expect("read");
        assert_eq!(back, g);
    }

    #[test]
    fn a_frame_is_shrunk_and_stretched() {
        let f = Frame::new(40, 20, (0..800).map(|i| i as f32).collect()).expect("frame");
        let (g, factor) = GreyImage::from_frame(&f, 20);
        assert_eq!((g.width, g.height, factor), (20, 10, 2));
        assert!(g.data[0] < 20 && g.data[g.data.len() - 1] > 200);
    }

    #[test]
    fn viridis_runs_dark_to_light() {
        let lum = |c: RGBColor| u32::from(c.0) + u32::from(c.1) + u32::from(c.2);
        assert!(lum(viridis(0.0)) < lum(viridis(0.5)) && lum(viridis(0.5)) < lum(viridis(1.0)));
    }
}
