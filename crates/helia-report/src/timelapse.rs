// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Time-lapse and derotation diagnostics: how each frame of a series was
//! registered (shift, method, how far to trust it), how the brightness was
//! evened out, and how far the Sun's differential rotation moved the surface.

use std::fmt::Write as _;

use helia_core::timelapse::Sequence;
use plotters::prelude::*;
use serde::{Deserialize, Serialize};

use crate::bundle::{Bundle, Figure, Section, Table};
use crate::canvas::{self, FONT, Size, palette, plot_err, tick, tick_int, tick_span};
use crate::{Error, PlotKind, Result};

/// One frame of the series.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrameRow {
    /// Position in the series, from 0.
    pub index: usize,
    /// Capture time, unix seconds UTC, when known.
    pub time_unix: Option<f64>,
    /// Shift applied, x, pixels.
    pub dx: f32,
    /// Shift applied, y, pixels.
    pub dy: f32,
    /// How the frame was registered.
    pub method: String,
    /// Trust in the frame's limb fit, 0 to 1, when it had one.
    pub limb_confidence: Option<f32>,
    /// Peak-to-sidelobe ratio of the phase correlation refinement, when it ran.
    pub phase_psr: Option<f32>,
    /// Mean brightness of the inner disc before the brightness was evened out.
    pub level: f32,
    /// Gain applied for the brightness.
    pub gain: f32,
    /// The largest distance the derotation moved the surface, pixels.
    pub derotated_px: Option<f32>,
}

/// Everything the time-lapse diagnostics are drawn from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimelapseData {
    /// Layout version of this file.
    pub version: u32,
    /// The frame the others were registered to.
    pub reference: usize,
    /// The time the derotation brought every frame to, ISO 8601.
    pub derotated_to: Option<String>,
    /// One row per frame.
    pub frames: Vec<FrameRow>,
}

impl TimelapseData {
    /// File name of the JSON.
    pub const JSON: &'static str = "timelapse.json";
    /// File name of the table.
    pub const CSV: &'static str = "timelapse-frames.csv";

    /// Gather what a registered series measured.
    #[must_use]
    pub fn from_sequence(seq: &Sequence) -> Self {
        Self {
            version: 1,
            reference: seq.reference,
            derotated_to: seq
                .derotated_to
                .map(helia_core::io::solarnet::iso_from_unix),
            frames: seq
                .report
                .iter()
                .map(|r| FrameRow {
                    index: r.index,
                    time_unix: r.time,
                    dx: crate::stats::finite(r.shift.dx),
                    dy: crate::stats::finite(r.shift.dy),
                    method: r.method.label().to_string(),
                    limb_confidence: r.limb.map(|l| crate::stats::finite(l.3)),
                    phase_psr: r.phase.map(|p| crate::stats::finite(p.1)),
                    level: crate::stats::finite(r.level),
                    gain: crate::stats::finite(r.gain),
                    derotated_px: r.derotated_px.map(crate::stats::finite),
                })
                .collect(),
        }
    }

    /// Whether every frame has a time.
    fn timed(&self) -> bool {
        !self.frames.is_empty() && self.frames.iter().all(|f| f.time_unix.is_some())
    }

    /// The x of a frame: minutes since the first frame, or its index.
    fn x(&self, f: &FrameRow) -> f64 {
        if self.timed() {
            let t0 = self.frames[0].time_unix.unwrap_or(0.0);
            (f.time_unix.unwrap_or(t0) - t0) / 60.0
        } else {
            f.index as f64
        }
    }

    fn x_label(&self) -> &'static str {
        if self.timed() {
            "minutes since the first frame"
        } else {
            "frame number"
        }
    }

    fn x_range(&self) -> (f64, f64) {
        let xs: Vec<f64> = self.frames.iter().map(|f| self.x(f)).collect();
        let lo = xs.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        if hi > lo {
            (lo, hi)
        } else {
            (lo - 1.0, lo + 1.0)
        }
    }

    fn need_frames(&self) -> Result<()> {
        if self.frames.len() < 2 {
            Err(Error::NoData(
                "a time-lapse needs at least two frames".into(),
            ))
        } else {
            Ok(())
        }
    }

    /// Draw one of the diagnostics.
    pub fn plot(&self, kind: PlotKind, size: Size) -> Result<Vec<u8>> {
        self.need_frames()?;
        match kind {
            PlotKind::TimelapseRegistration => self.plot_registration(size),
            PlotKind::TimelapseBrightness => self.plot_brightness(size),
            PlotKind::TimelapseDerotation => self.plot_derotation(size),
            _ => Err(Error::NoData(format!(
                "{} is not a time-lapse plot",
                kind.name()
            ))),
        }
    }

    fn plot_registration(&self, size: Size) -> Result<Vec<u8>> {
        let (x0, x1) = self.x_range();
        let (lo, hi) = {
            let v = self.frames.iter().flat_map(|f| [f.dx, f.dy]);
            canvas::padded(
                f64::from(v.clone().fold(f32::INFINITY, f32::min)),
                f64::from(v.fold(f32::NEG_INFINITY, f32::max)),
                0.1,
            )
        };
        let failed = self.frames.iter().filter(|f| f.method == "failed").count();
        let title = format!(
            "Registration of {} frames to frame {}{}",
            self.frames.len(),
            self.reference + 1,
            if failed > 0 {
                format!(" ({failed} could not be registered)")
            } else {
                String::new()
            }
        );
        let psr_hi = f64::from(
            self.frames
                .iter()
                .filter_map(|f| f.phase_psr)
                .fold(1.0f32, f32::max),
        ) * 1.1;
        canvas::render(size, |root| {
            let (top, bottom) = root.split_vertically(size.height * 58 / 100);
            let mut chart = ChartBuilder::on(&top)
                .caption(&title, (FONT, 18))
                .margin(12)
                .x_label_area_size(8)
                .y_label_area_size(66)
                .build_cartesian_2d(x0..x1, lo..hi)
                .map_err(plot_err)?;
            chart
                .configure_mesh()
                .y_desc("shift applied (px)")
                .x_label_formatter(&|_| String::new())
                .label_style((FONT, 14))
                .axis_desc_style((FONT, 15))
                .y_label_formatter(&|v| tick(*v))
                .light_line_style(RGBColor(238, 238, 238))
                .draw()
                .map_err(plot_err)?;
            for (name, colour, pick) in [
                (
                    "x",
                    palette::BLUE,
                    (|f: &FrameRow| f.dx) as fn(&FrameRow) -> f32,
                ),
                ("y", palette::ORANGE, |f: &FrameRow| f.dy),
            ] {
                chart
                    .draw_series(LineSeries::new(
                        self.frames.iter().map(|f| (self.x(f), f64::from(pick(f)))),
                        colour.stroke_width(2),
                    ))
                    .map_err(plot_err)?
                    .label(name)
                    .legend(move |(x, y)| {
                        PathElement::new(vec![(x, y), (x + 20, y)], colour.stroke_width(2))
                    });
                chart
                    .draw_series(self.frames.iter().map(|f| {
                        let c = if f.method == "failed" {
                            palette::RED
                        } else {
                            colour
                        };
                        Circle::new((self.x(f), f64::from(pick(f))), 3, c.filled())
                    }))
                    .map_err(plot_err)?;
            }
            chart
                .configure_series_labels()
                .background_style(WHITE.mix(0.9))
                .border_style(RGBColor(160, 160, 160))
                .label_font((FONT, 13))
                .position(SeriesLabelPosition::UpperRight)
                .draw()
                .map_err(plot_err)?;

            let mut conf = ChartBuilder::on(&bottom)
                .margin(12)
                .x_label_area_size(44)
                .y_label_area_size(66)
                .right_y_label_area_size(56)
                .build_cartesian_2d(x0..x1, 0.0..1.05)
                .map_err(plot_err)?
                .set_secondary_coord(x0..x1, 0.0..psr_hi);
            conf.configure_mesh()
                .x_desc(self.x_label())
                .y_desc("limb fit confidence")
                .label_style((FONT, 13))
                .axis_desc_style((FONT, 14))
                .y_label_formatter(&|v| tick(*v))
                .light_line_style(RGBColor(238, 238, 238))
                .draw()
                .map_err(plot_err)?;
            conf.configure_secondary_axes()
                .y_desc("phase correlation PSR")
                .label_style((FONT, 13))
                .axis_desc_style((FONT, 14))
                .draw()
                .map_err(plot_err)?;
            conf.draw_series(self.frames.iter().filter_map(|f| {
                f.limb_confidence
                    .map(|c| Circle::new((self.x(f), f64::from(c)), 3, palette::GREEN.filled()))
            }))
            .map_err(plot_err)?
            .label("limb confidence")
            .legend(|(x, y)| Circle::new((x + 10, y), 3, palette::GREEN.filled()));
            conf.draw_secondary_series(self.frames.iter().filter_map(|f| {
                f.phase_psr.map(|p| {
                    Cross::new(
                        (self.x(f), f64::from(p)),
                        4,
                        palette::PURPLE.stroke_width(2),
                    )
                })
            }))
            .map_err(plot_err)?
            .label("phase PSR (right axis)")
            .legend(|(x, y)| Cross::new((x + 10, y), 4, palette::PURPLE.stroke_width(2)));
            conf.configure_series_labels()
                .background_style(WHITE.mix(0.9))
                .border_style(RGBColor(160, 160, 160))
                .label_font((FONT, 12))
                .position(SeriesLabelPosition::LowerRight)
                .draw()
                .map_err(plot_err)
        })
    }

    fn plot_brightness(&self, size: Size) -> Result<Vec<u8>> {
        let (x0, x1) = self.x_range();
        let before: Vec<f64> = self.frames.iter().map(|f| f64::from(f.level)).collect();
        let after: Vec<f64> = self
            .frames
            .iter()
            .map(|f| f64::from(f.level * f.gain))
            .collect();
        let hi = before
            .iter()
            .chain(&after)
            .copied()
            .fold(0.0f64, f64::max)
            .max(1e-6)
            * 1.1;
        let lo = before
            .iter()
            .chain(&after)
            .copied()
            .fold(f64::INFINITY, f64::min)
            / 1.1;
        let evened = self.frames.iter().any(|f| (f.gain - 1.0).abs() > 1e-4);
        let spread = |v: &[f64]| {
            let m = v.iter().sum::<f64>() / v.len() as f64;
            (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / v.len() as f64).sqrt() / m.max(1e-12)
        };
        let title = if evened {
            format!(
                "Brightness of the inner disc: spread {:.1}% before, {:.1}% after evening out",
                100.0 * spread(&before),
                100.0 * spread(&after)
            )
        } else {
            format!(
                "Brightness of the inner disc: spread {:.1}% (not evened out)",
                100.0 * spread(&before)
            )
        };
        canvas::render(size, |root| {
            let mut chart = ChartBuilder::on(root)
                .caption(&title, (FONT, 18))
                .margin(12)
                .x_label_area_size(46)
                .y_label_area_size(76)
                .build_cartesian_2d(x0..x1, lo.max(0.0)..hi)
                .map_err(plot_err)?;
            chart
                .configure_mesh()
                .x_desc(self.x_label())
                .y_desc("mean level of the inner disc")
                .label_style((FONT, 14))
                .axis_desc_style((FONT, 15))
                .y_label_formatter(&|v| tick_span(*v, hi - lo))
                .light_line_style(RGBColor(238, 238, 238))
                .bold_line_style(RGBColor(215, 215, 215))
                .draw()
                .map_err(plot_err)?;
            chart
                .draw_series(LineSeries::new(
                    self.frames
                        .iter()
                        .zip(&before)
                        .map(|(f, &v)| (self.x(f), v)),
                    palette::ORANGE.stroke_width(2),
                ))
                .map_err(plot_err)?
                .label("as measured")
                .legend(|(x, y)| {
                    PathElement::new(vec![(x, y), (x + 20, y)], palette::ORANGE.stroke_width(2))
                });
            if evened {
                chart
                    .draw_series(LineSeries::new(
                        self.frames.iter().zip(&after).map(|(f, &v)| (self.x(f), v)),
                        palette::BLUE.stroke_width(2),
                    ))
                    .map_err(plot_err)?
                    .label("after the gain")
                    .legend(|(x, y)| {
                        PathElement::new(vec![(x, y), (x + 20, y)], palette::BLUE.stroke_width(2))
                    });
            }
            chart
                .draw_series(
                    self.frames
                        .iter()
                        .zip(&before)
                        .map(|(f, &v)| Circle::new((self.x(f), v), 3, palette::ORANGE.filled())),
                )
                .map_err(plot_err)?;
            chart
                .configure_series_labels()
                .background_style(WHITE.mix(0.9))
                .border_style(RGBColor(160, 160, 160))
                .label_font((FONT, 13))
                .position(SeriesLabelPosition::UpperRight)
                .draw()
                .map_err(plot_err)
        })
    }

    fn plot_derotation(&self, size: Size) -> Result<Vec<u8>> {
        let pts: Vec<(&FrameRow, f32)> = self
            .frames
            .iter()
            .filter_map(|f| f.derotated_px.map(|d| (f, d)))
            .collect();
        if pts.len() < 2 {
            return Err(Error::NoData(
                "no derotation ran (add --derotate to the time-lapse; it needs the time of every frame)".into(),
            ));
        }
        let (x0, x1) = self.x_range();
        let hi = pts
            .iter()
            .map(|p| f64::from(p.1))
            .fold(0.0f64, f64::max)
            .max(0.1)
            * 1.15;
        let title = format!(
            "Derotation: the largest displacement of the surface, brought to {}",
            self.derotated_to
                .as_deref()
                .unwrap_or("the reference frame")
        );
        canvas::render(size, |root| {
            let mut chart = ChartBuilder::on(root)
                .caption(&title, (FONT, 17))
                .margin(12)
                .x_label_area_size(46)
                .y_label_area_size(66)
                .build_cartesian_2d(x0..x1, 0.0..hi)
                .map_err(plot_err)?;
            chart
                .configure_mesh()
                .x_desc(self.x_label())
                .y_desc("largest displacement (px)")
                .label_style((FONT, 14))
                .axis_desc_style((FONT, 15))
                .y_label_formatter(&|v| tick(*v))
                .x_label_formatter(&|v| if self.timed() { tick(*v) } else { tick_int(*v) })
                .light_line_style(RGBColor(238, 238, 238))
                .draw()
                .map_err(plot_err)?;
            chart
                .draw_series(LineSeries::new(
                    pts.iter().map(|(f, d)| (self.x(f), f64::from(*d))),
                    palette::BLUE.stroke_width(2),
                ))
                .map_err(plot_err)?;
            chart
                .draw_series(pts.iter().map(|(f, d)| {
                    Circle::new((self.x(f), f64::from(*d)), 3, palette::BLUE.filled())
                }))
                .map_err(plot_err)?;
            Ok(())
        })
    }

    /// The table of frames as CSV.
    #[must_use]
    pub fn csv(&self) -> String {
        let mut s = String::from(
            "frame,time_utc,shift_x_px,shift_y_px,method,limb_confidence,phase_psr,level,gain,derotated_px\n",
        );
        let opt = |v: Option<f32>| v.map_or(String::new(), |v| format!("{v}"));
        for f in &self.frames {
            let _ = writeln!(
                s,
                "{},{},{},{},{},{},{},{},{},{}",
                f.index + 1,
                f.time_unix
                    .map_or(String::new(), helia_core::io::solarnet::iso_from_unix),
                f.dx,
                f.dy,
                f.method,
                opt(f.limb_confidence),
                opt(f.phase_psr),
                f.level,
                f.gain,
                opt(f.derotated_px)
            );
        }
        s
    }

    /// Draw every diagnostic that has data and add the files and the section to `bundle`.
    pub fn build(&self, bundle: &mut Bundle, size: Size) -> Result<()> {
        let mut json = serde_json::to_vec_pretty(self)?;
        json.push(b'\n');
        bundle.add(Self::JSON, json);
        bundle.add(Self::CSV, self.csv().into_bytes());
        let mut figures = Vec::new();
        let mut notes = Vec::new();
        for kind in [
            PlotKind::TimelapseRegistration,
            PlotKind::TimelapseBrightness,
            PlotKind::TimelapseDerotation,
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
        let count = |m: &str| self.frames.iter().filter(|f| f.method == m).count();
        let rows: Vec<Vec<String>> = ["reference", "limb", "limb+phase", "phase", "failed"]
            .iter()
            .map(|m| vec![(*m).to_string(), count(m).to_string()])
            .filter(|r| r[1] != "0")
            .collect();
        bundle.sections.push(Section {
            id: "timelapse".into(),
            title: "Time-lapse diagnostics".into(),
            intro: format!(
                "{} frames registered to frame {}{}.",
                self.frames.len(),
                self.reference + 1,
                self.derotated_to
                    .as_ref()
                    .map_or(String::new(), |t| format!(
                        ", brought to {t} by the Sun's differential rotation"
                    ))
            ),
            figures,
            tables: vec![Table {
                title: "How the frames were registered".into(),
                header: ["method", "frames"].map(String::from).to_vec(),
                rows,
            }],
            notes,
        });
        Ok(())
    }
}

fn describe(kind: PlotKind) -> (&'static str, &'static str) {
    match kind {
        PlotKind::TimelapseRegistration => (
            "Registration of the series",
            "Top: the shift each frame needed to land on the reference, x and y. A steady slope is drift; a jump is a pointing change or a failed frame (red). Bottom: how far each frame's limb fit can be trusted (green, 0 to 1) and, where the disc centre was refined by phase correlation, the peak-to-sidelobe ratio of that correlation (purple, right axis): a frame with both low is the one to look at.",
        ),
        PlotKind::TimelapseBrightness => (
            "Brightness of the series",
            "The mean level of the inner disc of each frame, as measured (orange) and after the gain that evens the series out (blue), when --deflicker is on. The spread in the title is the standard deviation as a share of the mean. A slow trend is the Sun climbing or sinking in the sky; sudden steps are cloud or a gain change in the camera.",
        ),
        PlotKind::TimelapseDerotation => (
            "Derotation",
            "How far the Sun's differential rotation (Howard or Snodgrass) moved the surface to bring each frame to the common time: the largest displacement over the disc, in pixels. It grows in step with the time from the target, about 13 degrees of longitude a day at the equator, which is why a series over hours needs it for features to hold still.",
        ),
        _ => ("", ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(timed: bool, derot: bool) -> TimelapseData {
        TimelapseData {
            version: 1,
            reference: 0,
            derotated_to: derot.then(|| "2026-10-20T11:00:00".into()),
            frames: (0..12)
                .map(|i| FrameRow {
                    index: i,
                    time_unix: timed.then_some(1_760_000_000.0 + i as f64 * 600.0),
                    dx: i as f32 * 0.3,
                    dy: -(i as f32) * 0.1,
                    method: if i == 0 {
                        "reference"
                    } else if i == 7 {
                        "failed"
                    } else {
                        "limb+phase"
                    }
                    .into(),
                    limb_confidence: Some(0.9),
                    phase_psr: (i != 7).then_some(12.0),
                    level: 0.5 + 0.01 * i as f32,
                    gain: 1.0 / (1.0 + 0.02 * i as f32),
                    derotated_px: derot.then_some(i as f32 * 1.5),
                })
                .collect(),
        }
    }

    #[test]
    fn the_three_plots_draw() {
        let d = data(true, true);
        for k in [
            PlotKind::TimelapseRegistration,
            PlotKind::TimelapseBrightness,
            PlotKind::TimelapseDerotation,
        ] {
            let png = d.plot(k, Size::REPORT).expect("plot");
            assert_eq!(&png[1..4], b"PNG");
            if let Some(dir) = std::env::var_os("HELIA_REPORT_DUMP") {
                std::fs::write(
                    std::path::Path::new(&dir).join(format!("{}.png", k.name())),
                    &png,
                )
                .expect("dump");
            }
        }
    }

    #[test]
    fn a_series_without_times_plots_against_the_frame_number_and_says_no_derotation() {
        let d = data(false, false);
        assert!(
            d.plot(PlotKind::TimelapseRegistration, Size::REPORT)
                .is_ok()
        );
        let e = d
            .plot(PlotKind::TimelapseDerotation, Size::REPORT)
            .expect_err("no derotation");
        assert!(e.to_string().contains("--derotate"), "{e}");
        let mut b = Bundle::default();
        d.build(&mut b, Size::REPORT).expect("build");
        assert_eq!(b.sections[0].figures.len(), 2);
        assert_eq!(d.csv().lines().count(), 13);
    }
}
