// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Alignment points: the grid on the reference, a heatmap of how sharp each
//! point's frames were and of how much each point moves, and the vector plots
//! of the displacement field and of the residual against the finished stack.

use std::fmt::Write as _;

use helia_core::pipeline::StackOutput;
use plotters::prelude::*;
use serde::{Deserialize, Serialize};

use crate::bundle::{Bundle, Figure, Section, Table};
use crate::canvas::{self, Size, palette, plot_err, tick};
use crate::image_plot::{
    GreyImage, PictureChart, arrow, colour_bar, paint_backdrop, picture_axes, picture_chart,
    viridis,
};
use crate::stats;
use crate::{Error, PlotKind, Result};

/// The fitted solar disc.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiscData {
    /// Centre, x, in pixels of the reference.
    pub cx: f32,
    /// Centre, y.
    pub cy: f32,
    /// Radius in pixels.
    pub r: f32,
}

/// One alignment point.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApRow {
    /// Index in the grid (row-major).
    pub index: usize,
    /// Window centre, x.
    pub cx: usize,
    /// Window centre, y.
    pub cy: usize,
    /// False for a window skipped as featureless.
    pub active: bool,
    /// Contrast of the reference window.
    pub contrast: f32,
    /// Frames in which the point was measured.
    pub measured: usize,
    /// Share of the registered frames in which it was measured.
    pub lock_fraction: f32,
    /// Mean sharpness of the window over the frames it was measured in.
    pub quality_mean: f32,
    /// Mean sharpness over the frames the point stacks.
    pub quality_kept: f32,
    /// Static displacement from the global shift, x, pixels.
    pub warp_dx: f32,
    /// Static displacement from the global shift, y, pixels.
    pub warp_dy: f32,
    /// Frame-to-frame scatter of the displacement, pixels RMS.
    pub warp_rms: f32,
    /// Mean residual against the finished stack, x (with the residual pass).
    pub residual_dx: Option<f32>,
    /// Mean residual, y.
    pub residual_dy: Option<f32>,
    /// RMS residual, pixels.
    pub residual_rms: Option<f32>,
}

/// Everything the alignment point plots are drawn from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApData {
    /// Layout version of this file.
    pub version: u32,
    /// Width of the reference in pixels.
    pub width: usize,
    /// Height of the reference in pixels.
    pub height: usize,
    /// Window size in pixels.
    pub window: usize,
    /// Distance between neighbouring centres, pixels.
    pub step: usize,
    /// Registered frames.
    pub frames: usize,
    /// Frames each point stacks.
    pub keep: usize,
    /// Alignment point passes run.
    pub passes: u8,
    /// Shifts the smoothing replaced.
    pub smoothed: usize,
    /// RMS of the residual against the stack over all points, when measured.
    pub residual_rms_px: Option<f32>,
    /// RMS change the second pass made, when it ran.
    pub moved_rms_px: Option<f32>,
    /// The fitted disc, when there is one.
    pub disc: Option<DiscData>,
    /// Every point of the grid.
    pub aps: Vec<ApRow>,
    /// The reference as a picture (written as `ap-reference.png`, not in the JSON).
    #[serde(skip)]
    pub backdrop: Option<GreyImage>,
}

impl ApData {
    /// File name of the JSON.
    pub const JSON: &'static str = "ap-data.json";
    /// File name of the table.
    pub const CSV: &'static str = "ap-table.csv";
    /// File name of the backdrop picture.
    pub const BACKDROP: &'static str = "ap-reference.png";

    /// Gather what a finished stack measured; `None` without alignment points.
    #[must_use]
    pub fn from_output(out: &StackOutput) -> Option<Self> {
        let stats = out.ap_stats.as_ref()?;
        let grid = out.grid.as_ref()?;
        let frames = stats.frames.max(1);
        let aps: Vec<ApRow> = stats
            .aps
            .iter()
            .map(|a| ApRow {
                index: a.index,
                cx: a.cx,
                cy: a.cy,
                active: a.active,
                contrast: stats::finite(a.contrast),
                measured: a.measured,
                lock_fraction: a.measured as f32 / frames as f32,
                quality_mean: stats::finite(a.quality_mean),
                quality_kept: stats::finite(a.quality_kept),
                warp_dx: stats::finite(a.warp_mean.dx),
                warp_dy: stats::finite(a.warp_mean.dy),
                warp_rms: stats::finite(a.warp_rms),
                residual_dx: a.residual_mean.map(|r| stats::finite(r.dx)),
                residual_dy: a.residual_mean.map(|r| stats::finite(r.dy)),
                residual_rms: a.residual_rms.map(stats::finite),
            })
            .collect();
        let (backdrop, _) = GreyImage::from_frame(&out.reference, 640);
        Some(Self {
            version: 1,
            width: grid.width,
            height: grid.height,
            window: grid.size,
            step: grid_step(&aps),
            frames: stats.frames,
            keep: stats.keep,
            passes: out.refine.map_or(1, |r| r.passes),
            smoothed: out.refine.map_or(0, |r| r.smoothed),
            residual_rms_px: out.refine.and_then(|r| r.residual_rms),
            moved_rms_px: out.refine.and_then(|r| r.moved_rms),
            disc: out.disc.map(|d| DiscData {
                cx: d.cx,
                cy: d.cy,
                r: d.r,
            }),
            aps,
            backdrop: Some(backdrop),
        })
    }

    /// Points that were measured.
    fn used(&self) -> impl Iterator<Item = &ApRow> {
        self.aps.iter().filter(|a| a.active && a.measured > 0)
    }

    /// Draw one of the alignment point plots.
    pub fn plot(&self, kind: PlotKind, size: Size) -> Result<Vec<u8>> {
        match kind {
            PlotKind::ApGrid => self.plot_grid(size),
            PlotKind::ApQuality => self.plot_heat(size, Heat::Quality),
            PlotKind::ApShift => self.plot_heat(size, Heat::Shift),
            PlotKind::ApField => self.plot_vectors(size, Vectors::Field),
            PlotKind::ApResidual => self.plot_vectors(size, Vectors::Residual),
            _ => Err(Error::NoData(format!(
                "{} is not an alignment point plot",
                kind.name()
            ))),
        }
    }

    fn need_points(&self) -> Result<()> {
        if self.used().next().is_none() {
            Err(Error::NoData(
                "no alignment point was measured (multipoint is off, or the disc has no detail)"
                    .into(),
            ))
        } else {
            Ok(())
        }
    }

    fn backdrop(&self) -> Result<&GreyImage> {
        self.backdrop.as_ref().ok_or_else(|| {
            Error::NoData(format!(
                "the reference picture {} is missing from the report",
                Self::BACKDROP
            ))
        })
    }

    fn draw_disc(&self, chart: &mut PictureChart<'_, '_>) -> Result<()> {
        if let Some(d) = self.disc {
            let pts: Vec<(f64, f64)> = (0..=180)
                .map(|i| {
                    let a = f64::from(i) * std::f64::consts::PI / 90.0;
                    (
                        f64::from(d.cx) + f64::from(d.r) * a.cos(),
                        f64::from(d.cy) + f64::from(d.r) * a.sin(),
                    )
                })
                .collect();
            chart
                .draw_series(std::iter::once(PathElement::new(
                    pts,
                    palette::SKY.mix(0.8).stroke_width(1),
                )))
                .map_err(plot_err)?;
        }
        Ok(())
    }

    fn plot_grid(&self, size: Size) -> Result<Vec<u8>> {
        self.need_points()?;
        let img = self.backdrop()?;
        let active = self.aps.iter().filter(|a| a.active).count();
        let title = format!(
            "Alignment points: {active} of {} active, {} px windows every {} px",
            self.aps.len(),
            self.window,
            self.step
        );
        canvas::render(size, |root| {
            let (mut chart, layout) =
                picture_chart(root, size, (self.width, self.height), 0, &title)?;
            paint_backdrop(root, layout, img, 0.85)?;
            picture_axes(&mut chart, "x (px)", "y (px)")?;
            let half = (self.window / 2) as f64;
            let (w, h) = (self.width as f64, self.height as f64);
            let clip = |x: f64, y: f64| (x.clamp(0.0, w), y.clamp(0.0, h));
            chart
                .draw_series(self.aps.iter().filter(|a| a.active).map(|a| {
                    let (x0, y0) = clip(a.cx as f64 - half, a.cy as f64 - half);
                    let (x1, y1) = clip(a.cx as f64 + half, a.cy as f64 + half);
                    Rectangle::new(
                        [(x0, y0), (x1, y1)],
                        palette::ORANGE.mix(0.55).stroke_width(1),
                    )
                }))
                .map_err(plot_err)?;
            chart
                .draw_series(self.aps.iter().filter(|a| !a.active).map(|a| {
                    Circle::new(
                        (a.cx as f64, a.cy as f64),
                        1,
                        palette::LIGHT.mix(0.6).filled(),
                    )
                }))
                .map_err(plot_err)?;
            chart
                .draw_series(
                    self.aps
                        .iter()
                        .filter(|a| a.active)
                        .map(|a| Circle::new((a.cx as f64, a.cy as f64), 2, palette::RED.filled())),
                )
                .map_err(plot_err)?;
            self.draw_disc(&mut chart)
        })
    }

    fn plot_heat(&self, size: Size, what: Heat) -> Result<Vec<u8>> {
        self.need_points()?;
        let img = self.backdrop()?;
        let values: Vec<(&ApRow, f64)> =
            self.used().map(|a| (a, f64::from(what.value(a)))).collect();
        // The colour range comes from the points well inside the disc: the
        // limb is an edge, and its sharpness would swamp the rest.
        let inner = |a: &ApRow| {
            self.disc.is_none_or(|d| {
                let dist = (a.cx as f32 - d.cx).hypot(a.cy as f32 - d.cy);
                dist < d.r - 1.5 * self.window as f32
            })
        };
        let mut sorted: Vec<f64> = values.iter().filter(|v| inner(v.0)).map(|v| v.1).collect();
        if sorted.len() < 8 {
            sorted = values.iter().map(|v| v.1).collect();
        }
        sorted.sort_by(f64::total_cmp);
        let q = |f: f64| sorted[((sorted.len() - 1) as f64 * f) as usize];
        let (lo, hi) = if sorted.len() >= 20 {
            (q(0.02), q(0.98))
        } else {
            (q(0.0), q(1.0))
        };
        let (lo, hi) = if hi > lo { (lo, hi) } else { (lo, lo + 1e-6) };
        let mean = values.iter().map(|v| v.1).sum::<f64>() / values.len() as f64;
        let title = format!(
            "{} ({} points, mean {})",
            what.title(),
            values.len(),
            tick(mean)
        );
        let half = self.step as f64 / 2.0;
        let (w, h) = (self.width as f64, self.height as f64);
        canvas::render(size, |root| {
            let (mut chart, layout) =
                picture_chart(root, size, (self.width, self.height), 92, &title)?;
            paint_backdrop(root, layout, img, 0.55)?;
            picture_axes(&mut chart, "x (px)", "y (px)")?;
            chart
                .draw_series(values.iter().map(|(a, v)| {
                    let t = (v - lo) / (hi - lo);
                    let (x0, y0) = ((a.cx as f64 - half).max(0.0), (a.cy as f64 - half).max(0.0));
                    let (x1, y1) = ((a.cx as f64 + half).min(w), (a.cy as f64 + half).min(h));
                    Rectangle::new([(x0, y0), (x1, y1)], viridis(t).mix(0.82).filled())
                }))
                .map_err(plot_err)?;
            self.draw_disc(&mut chart)?;
            colour_bar(root, layout, (lo, hi), what.unit())
        })
    }

    fn plot_vectors(&self, size: Size, what: Vectors) -> Result<Vec<u8>> {
        self.need_points()?;
        let img = self.backdrop()?;
        let rows: Vec<(&ApRow, f64, f64, f64)> = self
            .used()
            .filter_map(|a| {
                what.vector(a)
                    .map(|(dx, dy, m)| (a, f64::from(dx), f64::from(dy), f64::from(m)))
            })
            .collect();
        if rows.is_empty() {
            return Err(Error::NoData(what.missing().into()));
        }
        let mut lengths: Vec<f64> = rows.iter().map(|r| r.1.hypot(r.2)).collect();
        lengths.sort_by(f64::total_cmp);
        let typical = lengths[((lengths.len() - 1) as f64 * 0.9) as usize].max(1e-6);
        // The 90th percentile arrow spans about one and a half grid steps.
        let gain = 1.5 * self.step as f64 / typical;
        let mags: Vec<f64> = rows.iter().map(|r| r.3).collect();
        let hi = mags.iter().copied().fold(0.0f64, f64::max).max(1e-6);
        let rms = (mags.iter().map(|m| m * m).sum::<f64>() / mags.len() as f64).sqrt();
        let title = format!(
            "{} (RMS {} px, arrows x{:.0})",
            what.title(),
            tick(rms),
            gain
        );
        canvas::render(size, |root| {
            let (mut chart, layout) =
                picture_chart(root, size, (self.width, self.height), 92, &title)?;
            paint_backdrop(root, layout, img, 0.55)?;
            picture_axes(&mut chart, "x (px)", "y (px)")?;
            chart
                .draw_series(rows.iter().map(|(a, ..)| {
                    Circle::new((a.cx as f64, a.cy as f64), 1, WHITE.mix(0.6).filled())
                }))
                .map_err(plot_err)?;
            for (a, dx, dy, m) in &rows {
                let colour = viridis(m / hi);
                arrow(
                    &mut chart,
                    (a.cx as f64, a.cy as f64),
                    (dx * gain, dy * gain),
                    colour,
                )?;
            }
            self.draw_disc(&mut chart)?;
            colour_bar(root, layout, (0.0, hi), what.unit())
        })
    }

    /// The table of points as CSV.
    #[must_use]
    pub fn csv(&self) -> String {
        let mut s = String::from(
            "index,cx,cy,active,contrast,measured,lock_fraction,quality_mean,quality_kept,warp_dx_px,warp_dy_px,warp_rms_px,residual_dx_px,residual_dy_px,residual_rms_px\n",
        );
        let opt = |v: Option<f32>| v.map_or(String::new(), |v| format!("{v}"));
        for a in &self.aps {
            let _ = writeln!(
                s,
                "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
                a.index,
                a.cx,
                a.cy,
                u8::from(a.active),
                a.contrast,
                a.measured,
                a.lock_fraction,
                a.quality_mean,
                a.quality_kept,
                a.warp_dx,
                a.warp_dy,
                a.warp_rms,
                opt(a.residual_dx),
                opt(a.residual_dy),
                opt(a.residual_rms)
            );
        }
        s
    }

    /// Draw every plot that has data and add the files and the section to `bundle`.
    pub fn build(&self, bundle: &mut Bundle, size: Size) -> Result<()> {
        let mut json = serde_json::to_vec_pretty(self)?;
        json.push(b'\n');
        bundle.add(Self::JSON, json);
        bundle.add(Self::CSV, self.csv().into_bytes());
        if let Some(b) = &self.backdrop {
            bundle.add(Self::BACKDROP, b.to_png()?);
        }
        let mut figures = Vec::new();
        let mut notes = Vec::new();
        for kind in [
            PlotKind::ApGrid,
            PlotKind::ApQuality,
            PlotKind::ApShift,
            PlotKind::ApField,
            PlotKind::ApResidual,
        ] {
            match self.plot(kind, size) {
                Ok(png) => {
                    let (title, caption) = describe(kind);
                    let name = format!("{}.png", kind.name());
                    bundle.add(&name, png);
                    figures.push(Figure {
                        png: name,
                        title: title.into(),
                        caption: caption.into(),
                        data: vec![Self::JSON.into(), Self::CSV.into()],
                    });
                }
                Err(Error::NoData(why)) => notes.push(format!("{}: {why}", kind.name())),
                Err(e) => return Err(e),
            }
        }
        let used: Vec<&ApRow> = self.used().collect();
        let col = |f: fn(&ApRow) -> f32| -> Vec<f32> { used.iter().map(|a| f(a)).collect() };
        let row = |name: &str, v: Vec<f32>| {
            let s = stats::summarise(&v);
            vec![
                name.to_string(),
                tick(f64::from(s.min)),
                tick(f64::from(s.median)),
                tick(f64::from(s.mean)),
                tick(f64::from(s.max)),
            ]
        };
        let mut rows = vec![
            row("lock fraction", col(|a| a.lock_fraction)),
            row("quality of the kept frames", col(|a| a.quality_kept)),
            row(
                "static displacement (px)",
                col(|a| a.warp_dx.hypot(a.warp_dy)),
            ),
            row("frame-to-frame scatter (px RMS)", col(|a| a.warp_rms)),
        ];
        if used.iter().any(|a| a.residual_rms.is_some()) {
            rows.push(row(
                "residual against the stack (px RMS)",
                used.iter().filter_map(|a| a.residual_rms).collect(),
            ));
        }
        let mut intro = format!(
            "{} of {} alignment points were measured over {} registered frames; each stacks its best {}. {} pass{}, {} shifts replaced by the smoothing.",
            used.len(),
            self.aps.len(),
            self.frames,
            self.keep,
            self.passes,
            if self.passes == 1 { "" } else { "es" },
            self.smoothed
        );
        if let Some(r) = self.residual_rms_px {
            let _ = write!(
                intro,
                " The residual against the finished stack is {r:.3} px RMS."
            );
        }
        bundle.sections.push(Section {
            id: "alignment-points".into(),
            title: "Alignment points".into(),
            intro,
            figures,
            tables: vec![Table {
                title: "Per-point summary".into(),
                header: ["measure", "min", "median", "mean", "max"]
                    .map(String::from)
                    .to_vec(),
                rows,
            }],
            notes,
        });
        Ok(())
    }
}

/// Distance between neighbouring centres: the smallest positive difference.
fn grid_step(aps: &[ApRow]) -> usize {
    let mut xs: Vec<usize> = aps.iter().map(|a| a.cx).collect();
    xs.sort_unstable();
    xs.dedup();
    xs.windows(2).map(|w| w[1] - w[0]).min().unwrap_or(32)
}

#[derive(Clone, Copy)]
enum Heat {
    Quality,
    Shift,
}

impl Heat {
    fn value(self, a: &ApRow) -> f32 {
        match self {
            Self::Quality => a.quality_kept,
            Self::Shift => a.warp_rms,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Quality => "Sharpness of the frames each point stacks",
            Self::Shift => "How much each point moves from frame to frame",
        }
    }

    fn unit(self) -> &'static str {
        match self {
            Self::Quality => "sharpness",
            Self::Shift => "px RMS",
        }
    }
}

#[derive(Clone, Copy)]
enum Vectors {
    Field,
    Residual,
}

impl Vectors {
    /// (dx, dy, magnitude to colour by) of a point.
    fn vector(self, a: &ApRow) -> Option<(f32, f32, f32)> {
        match self {
            Self::Field => Some((a.warp_dx, a.warp_dy, a.warp_dx.hypot(a.warp_dy))),
            Self::Residual => Some((a.residual_dx?, a.residual_dy?, a.residual_rms?)),
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Field => "Shift field: each point's mean displacement from the global shift",
            Self::Residual => "Residual of the shifts against the finished stack",
        }
    }

    fn unit(self) -> &'static str {
        match self {
            Self::Field => "px",
            Self::Residual => "px RMS",
        }
    }

    fn missing(self) -> &'static str {
        match self {
            Self::Field => "no alignment point has a displacement",
            Self::Residual => {
                "the residual was not measured (add --ap-residual, or ap_refine residual in the API)"
            }
        }
    }
}

fn describe(kind: PlotKind) -> (&'static str, &'static str) {
    match kind {
        PlotKind::ApGrid => (
            "Alignment point grid",
            "The reference frame with every alignment point: orange squares are the windows that were measured, grey ones were skipped as featureless (sky, or a flat part of the disc), and the red dots are the centres. The pale circle is the fitted disc. Windows overlap by half, so each pixel is covered by up to four. Check that the points cover the detail you care about and sit off the sky and burned-in text.",
        ),
        PlotKind::ApQuality => (
            "Alignment point quality",
            "The mean sharpness of the frames each point stacked (its own best frames), painted over the reference. Bright cells are where the picture is sharpest; dark ones are flat or hazy. A smooth gradient is normal (the limb and the centre differ); a blotchy one means the points are choosing frames by noise.",
        ),
        PlotKind::ApShift => (
            "Alignment point shift magnitude",
            "How much each point moves from frame to frame once the whole frame's shift is taken out, as RMS pixels: the local distortion the seeing adds. Cells that move more than the rest sit under rougher air or on stronger detail. If every cell is under about 0.3 px, multipoint is buying little over a global alignment.",
        ),
        PlotKind::ApField => (
            "Shift field",
            "Each point's mean displacement from the global shift, as an arrow (scaled so that the longest is about one grid step, the factor is in the title). A smooth, systematic field means the optics or the mount distort the image the same way in every frame; a random one means the displacement is seeing.",
        ),
        PlotKind::ApResidual => (
            "Residual against the stack",
            "After stacking, the points are measured again against the finished stack, and the arrows are the mean difference from the shifts that were used. A short, random residual means the stack agrees with every frame it was made from; long, coherent arrows mean the shifts were biased (try two alignment point passes, or the smoothing).",
        ),
        _ => ("", ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ApData {
        let mut aps = Vec::new();
        for gy in 0..6 {
            for gx in 0..8 {
                let (cx, cy) = (gx * 32, gy * 32);
                let d = ((cx as f32 - 128.0).hypot(cy as f32 - 96.0)) / 160.0;
                aps.push(ApRow {
                    index: aps.len(),
                    cx,
                    cy,
                    active: d < 0.9,
                    contrast: 0.1,
                    measured: if d < 0.9 { 40 } else { 0 },
                    lock_fraction: 1.0,
                    quality_mean: 1.0 + d,
                    quality_kept: 1.5 + d,
                    warp_dx: 0.3 * (1.0 - d),
                    warp_dy: -0.2 * d,
                    warp_rms: 0.1 + 0.3 * d,
                    residual_dx: Some(0.05 * d),
                    residual_dy: Some(0.03),
                    residual_rms: Some(0.08 + 0.1 * d),
                });
            }
        }
        let img = GreyImage {
            width: 64,
            height: 48,
            data: (0..64 * 48).map(|i| ((i * 7) % 255) as u8).collect(),
        };
        ApData {
            version: 1,
            width: 256,
            height: 192,
            window: 64,
            step: 32,
            frames: 40,
            keep: 10,
            passes: 2,
            smoothed: 3,
            residual_rms_px: Some(0.11),
            moved_rms_px: Some(0.2),
            disc: Some(DiscData {
                cx: 128.0,
                cy: 96.0,
                r: 90.0,
            }),
            aps,
            backdrop: Some(img),
        }
    }

    #[test]
    fn every_plot_is_a_png() {
        let d = sample();
        for kind in [
            PlotKind::ApGrid,
            PlotKind::ApQuality,
            PlotKind::ApShift,
            PlotKind::ApField,
            PlotKind::ApResidual,
        ] {
            let png = d.plot(kind, Size::REPORT).expect("plot");
            assert_eq!(&png[1..4], b"PNG", "{}", kind.name());
            if let Some(dir) = std::env::var_os("HELIA_REPORT_DUMP") {
                std::fs::write(
                    std::path::Path::new(&dir).join(format!("{}.png", kind.name())),
                    &png,
                )
                .expect("dump");
            }
        }
    }

    #[test]
    fn without_the_residual_that_plot_says_what_to_add() {
        let mut d = sample();
        for a in &mut d.aps {
            a.residual_dx = None;
            a.residual_dy = None;
            a.residual_rms = None;
        }
        let e = d
            .plot(PlotKind::ApResidual, Size::REPORT)
            .expect_err("no data");
        assert!(e.to_string().contains("--ap-residual"), "{e}");
        let mut b = Bundle::default();
        d.build(&mut b, Size::REPORT).expect("build");
        assert_eq!(b.sections[0].figures.len(), 4);
        assert!(b.file("ap-table.csv").is_some() && b.file("ap-reference.png").is_some());
    }

    #[test]
    fn the_grid_step_is_found_and_the_csv_has_a_row_per_point() {
        let d = sample();
        assert_eq!(grid_step(&d.aps), 32);
        assert_eq!(d.csv().lines().count(), 1 + 48);
    }

    #[test]
    fn the_json_round_trips_without_the_picture() {
        let d = sample();
        let back: ApData =
            serde_json::from_str(&serde_json::to_string(&d).expect("json")).expect("parse");
        assert_eq!(back.aps, d.aps);
        assert!(back.backdrop.is_none());
    }
}
