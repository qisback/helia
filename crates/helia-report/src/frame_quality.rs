// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Frame quality, the way `AutoStakkert` shows it: the quality graph sorted and
//! in capture order with the keep threshold, a histogram, the global shift of
//! each frame and its drift, and the seeing over the capture.

use std::fmt::Write as _;

use helia_core::pipeline::StackOutput;
use plotters::coord::ranged1d::ValueFormatter;
use plotters::prelude::*;
use serde::{Deserialize, Serialize};

use crate::bundle::{Bundle, Figure, Section, Table};
use crate::canvas::{self, FONT, Size, palette, plot_err, tick, tick_int, tick_span};
use crate::stats::{self, Summary};
use crate::{Error, PlotKind, Result};

/// A score an artefact raised, with the score it measured before it was put back.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Outlier {
    /// Frame index in the capture.
    pub frame: usize,
    /// The raised score the metric measured.
    pub measured: f32,
}

/// Global shift of one registered frame against the reference.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShiftRow {
    /// Frame index in the capture.
    pub frame: usize,
    /// Horizontal shift in pixels (positive: content moved right).
    pub dx: f32,
    /// Vertical shift in pixels (positive: content moved down).
    pub dy: f32,
}

/// Straight-line drift of the shifts and what is left over.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DriftFit {
    /// Drift of x in pixels per 1000 frames.
    pub dx_per_1000: f32,
    /// Drift of y in pixels per 1000 frames.
    pub dy_per_1000: f32,
    /// Intercept of x at frame 0.
    pub dx0: f32,
    /// Intercept of y at frame 0.
    pub dy0: f32,
    /// RMS of x about the line: the jitter, in pixels.
    pub dx_jitter: f32,
    /// RMS of y about the line, in pixels.
    pub dy_jitter: f32,
}

/// One frame of the seeing series.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SeeingPoint {
    /// Frame index in the capture.
    pub frame: usize,
    /// Differential motion of this frame, pixels per axis.
    pub rms_px: f32,
    /// RMS of the points in a window about this frame (smoother).
    pub window_rms_px: f32,
}

/// The seeing over the capture.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SeeingSeries {
    /// Points, in capture order.
    pub points: Vec<SeeingPoint>,
    /// Points averaged either side for `window_rms_px` (a window is `2 * half + 1` frames).
    pub half_window: usize,
    /// The index of the whole capture, pixels per axis.
    pub overall_rms_px: Option<f32>,
    /// Image scale, arcseconds per pixel, when known.
    pub arcsec_per_px: Option<f32>,
    /// Fried parameter at 500 nm in centimetres, when measurable.
    pub r0_500_cm: Option<f32>,
    /// Seeing grade, when given.
    pub grade: Option<String>,
}

/// Everything the frame quality plots are drawn from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrameQualityData {
    /// Layout version of this file.
    pub version: u32,
    /// Name of the ranking metric.
    pub metric: String,
    /// Frames in the capture.
    pub frames: usize,
    /// Frames kept.
    pub keep: usize,
    /// Score of the lowest kept frame: the keep threshold.
    pub threshold: f32,
    /// Score of each frame, in capture order, as ranked.
    pub scores: Vec<f32>,
    /// Rank of each frame, 0 for the best.
    pub rank: Vec<usize>,
    /// Kept frames, best first.
    pub kept: Vec<usize>,
    /// Scores an artefact had raised.
    pub outliers: Vec<Outlier>,
    /// Global shift of each registered frame (the candidates), in capture order.
    pub shifts: Vec<ShiftRow>,
    /// Line fitted to the shifts.
    pub drift: Option<DriftFit>,
    /// Summary of all the scores.
    pub score_summary: Summary,
    /// Summary of the kept scores.
    pub kept_summary: Summary,
    /// Seeing over the capture, when it was measured.
    pub seeing: Option<SeeingSeries>,
}

/// Frames averaged either side for the smoothed seeing line.
const SEEING_HALF_WINDOW: usize = 4;

impl FrameQualityData {
    /// File name of the JSON.
    pub const JSON: &'static str = "frame-quality.json";
    /// File name of the table of frames.
    pub const CSV: &'static str = "frame-quality.csv";
    /// File name of the seeing series.
    pub const SEEING_CSV: &'static str = "seeing-time.csv";

    /// Gather what a finished stack measured.
    #[must_use]
    pub fn from_output(out: &StackOutput, metric: &str) -> Self {
        let scores: Vec<f32> = out.scores.iter().copied().map(stats::finite).collect();
        let mut rank = vec![0usize; scores.len()];
        for (r, &f) in out.ranking.iter().enumerate() {
            if let Some(slot) = rank.get_mut(f) {
                *slot = r;
            }
        }
        let threshold = out
            .kept
            .last()
            .and_then(|&i| scores.get(i))
            .copied()
            .unwrap_or(0.0);
        let kept_scores: Vec<f32> = out
            .kept
            .iter()
            .filter_map(|&i| scores.get(i))
            .copied()
            .collect();
        let mut shifts: Vec<ShiftRow> = out
            .shifts
            .iter()
            .map(|(f, s)| ShiftRow {
                frame: *f,
                dx: stats::finite(s.dx),
                dy: stats::finite(s.dy),
            })
            .collect();
        shifts.sort_by_key(|s| s.frame);
        let drift = fit_drift(&shifts);
        Self {
            version: 1,
            metric: metric.to_string(),
            frames: scores.len(),
            keep: out.kept.len(),
            threshold,
            rank,
            kept: out.kept.clone(),
            outliers: out
                .score_outliers
                .iter()
                .map(|&(frame, measured)| Outlier {
                    frame,
                    measured: stats::finite(measured),
                })
                .collect(),
            shifts,
            drift,
            score_summary: stats::summarise(&scores),
            kept_summary: stats::summarise(&kept_scores),
            scores,
            seeing: out.seeing.as_ref().and_then(seeing_series),
        }
    }

    /// Draw one of the frame quality plots.
    pub fn plot(&self, kind: PlotKind, size: Size) -> Result<Vec<u8>> {
        match kind {
            PlotKind::QualitySorted => self.plot_sorted(size),
            PlotKind::QualityOrder => self.plot_order(size),
            PlotKind::QualityHistogram => self.plot_histogram(size),
            PlotKind::ShiftDrift => self.plot_shift(size),
            PlotKind::SeeingTime => self.plot_seeing(size),
            #[allow(unreachable_patterns, reason = "other groups draw their own kinds")]
            _ => Err(Error::NoData(format!(
                "{} is not a frame quality plot",
                kind.name()
            ))),
        }
    }

    fn is_kept(&self) -> Vec<bool> {
        let mut k = vec![false; self.scores.len()];
        for &f in &self.kept {
            if let Some(s) = k.get_mut(f) {
                *s = true;
            }
        }
        k
    }

    fn require_scores(&self) -> Result<()> {
        if self.scores.is_empty() {
            Err(Error::NoData("the capture has no frame scores".into()))
        } else {
            Ok(())
        }
    }

    fn plot_sorted(&self, size: Size) -> Result<Vec<u8>> {
        self.require_scores()?;
        let mut sorted = self.scores.clone();
        sorted.sort_by(|a, b| b.total_cmp(a));
        let n = sorted.len() as f64;
        let (lo, hi) = canvas::padded(
            f64::from(sorted[sorted.len() - 1]),
            f64::from(sorted[0]),
            0.06,
        );
        let keep = self.keep as f64;
        let thr = f64::from(self.threshold);
        let title = format!(
            "Frame quality, best first ({}): keep {} of {} ({:.0}%)",
            self.metric,
            self.keep,
            self.frames,
            100.0 * keep / n
        );
        canvas::render(size, |root| {
            let mut chart = ChartBuilder::on(root)
                .caption(&title, (FONT, 20))
                .margin(12)
                .x_label_area_size(44)
                .y_label_area_size(70)
                .build_cartesian_2d(0.0..n, lo..hi)
                .map_err(plot_err)?;
            mesh(&mut chart, "rank (1 = best)", "quality score")?;
            chart
                .draw_series(std::iter::once(Rectangle::new(
                    [(0.0, lo), (keep, hi)],
                    palette::GREEN.mix(0.12).filled(),
                )))
                .map_err(plot_err)?
                .label(format!("kept ({})", self.keep))
                .legend(|(x, y)| {
                    Rectangle::new(
                        [(x, y - 5), (x + 20, y + 5)],
                        palette::GREEN.mix(0.3).filled(),
                    )
                });
            chart
                .draw_series(LineSeries::new(
                    sorted
                        .iter()
                        .enumerate()
                        .map(|(i, &s)| (i as f64 + 1.0, f64::from(s))),
                    palette::BLUE.stroke_width(2),
                ))
                .map_err(plot_err)?
                .label("score")
                .legend(|(x, y)| {
                    PathElement::new(vec![(x, y), (x + 20, y)], palette::BLUE.stroke_width(2))
                });
            chart
                .draw_series(DashedLineSeries::new(
                    vec![(0.0, thr), (n, thr)],
                    6,
                    4,
                    palette::RED.stroke_width(2),
                ))
                .map_err(plot_err)?
                .label(format!("keep threshold {}", tick(thr)))
                .legend(|(x, y)| {
                    PathElement::new(vec![(x, y), (x + 20, y)], palette::RED.stroke_width(2))
                });
            chart
                .draw_series(DashedLineSeries::new(
                    vec![(keep, lo), (keep, hi)],
                    6,
                    4,
                    palette::GREEN.stroke_width(2),
                ))
                .map_err(plot_err)?;
            legend(&mut chart, SeriesLabelPosition::UpperRight)
        })
    }

    fn plot_order(&self, size: Size) -> Result<Vec<u8>> {
        self.require_scores()?;
        let kept = self.is_kept();
        let n = self.scores.len() as f64;
        let mut all: Vec<f32> = self.scores.clone();
        all.extend(self.outliers.iter().map(|o| o.measured));
        let (lo, hi) = canvas::padded(
            f64::from(all.iter().copied().fold(f32::INFINITY, f32::min)),
            f64::from(all.iter().copied().fold(f32::NEG_INFINITY, f32::max)),
            0.06,
        );
        let thr = f64::from(self.threshold);
        let window = (self.scores.len() / 60).clamp(5, 301) | 1;
        let trend = stats::running_median(&self.scores, window);
        let title = format!(
            "Frame quality in capture order ({}): {} kept above {}",
            self.metric,
            self.keep,
            tick(thr)
        );
        canvas::render(size, |root| {
            let mut chart = ChartBuilder::on(root)
                .caption(&title, (FONT, 20))
                .margin(12)
                .x_label_area_size(44)
                .y_label_area_size(70)
                .build_cartesian_2d(0.0..n, lo..hi)
                .map_err(plot_err)?;
            mesh(&mut chart, "frame number (capture order)", "quality score")?;
            let radius = if self.scores.len() > 3000 { 1 } else { 2 };
            chart
                .draw_series(
                    self.scores
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| !kept[*i])
                        .map(|(i, &s)| {
                            Circle::new((i as f64, f64::from(s)), radius, palette::LIGHT.filled())
                        }),
                )
                .map_err(plot_err)?
                .label("not kept")
                .legend(|(x, y)| Circle::new((x + 10, y), 3, palette::LIGHT.filled()));
            chart
                .draw_series(
                    self.scores
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| kept[*i])
                        .map(|(i, &s)| {
                            Circle::new((i as f64, f64::from(s)), radius, palette::BLUE.filled())
                        }),
                )
                .map_err(plot_err)?
                .label("kept")
                .legend(|(x, y)| Circle::new((x + 10, y), 3, palette::BLUE.filled()));
            chart
                .draw_series(LineSeries::new(
                    trend
                        .iter()
                        .enumerate()
                        .map(|(i, &s)| (i as f64, f64::from(s))),
                    palette::ORANGE.stroke_width(2),
                ))
                .map_err(plot_err)?
                .label(format!("running median ({window} frames)"))
                .legend(|(x, y)| {
                    PathElement::new(vec![(x, y), (x + 20, y)], palette::ORANGE.stroke_width(2))
                });
            if !self.outliers.is_empty() {
                chart
                    .draw_series(self.outliers.iter().map(|o| {
                        Cross::new(
                            (o.frame as f64, f64::from(o.measured)),
                            4,
                            palette::RED.stroke_width(2),
                        )
                    }))
                    .map_err(plot_err)?
                    .label(format!("raised by an artefact ({})", self.outliers.len()))
                    .legend(|(x, y)| Cross::new((x + 10, y), 4, palette::RED.stroke_width(2)));
            }
            chart
                .draw_series(DashedLineSeries::new(
                    vec![(0.0, thr), (n, thr)],
                    6,
                    4,
                    palette::RED.stroke_width(2),
                ))
                .map_err(plot_err)?
                .label(format!("keep threshold {}", tick(thr)))
                .legend(|(x, y)| {
                    PathElement::new(vec![(x, y), (x + 20, y)], palette::RED.stroke_width(2))
                });
            legend(&mut chart, SeriesLabelPosition::UpperRight)
        })
    }

    fn plot_histogram(&self, size: Size) -> Result<Vec<u8>> {
        self.require_scores()?;
        let hist = self.histogram();
        let (lo, hi) = (
            hist.first().map_or(0.0, |b| b.0),
            hist.last().map_or(1.0, |b| b.1),
        );
        let top = hist.iter().map(|b| b.2).max().unwrap_or(1) as f64 * 1.12;
        let thr = f64::from(self.threshold);
        let title = format!(
            "Distribution of frame quality ({}): {} frames, median {}",
            self.metric,
            self.frames,
            tick(f64::from(self.score_summary.median))
        );
        canvas::render(size, |root| {
            let mut chart = ChartBuilder::on(root)
                .caption(&title, (FONT, 20))
                .margin(12)
                .x_label_area_size(44)
                .y_label_area_size(70)
                .build_cartesian_2d(lo..hi, 0.0..top)
                .map_err(plot_err)?;
            mesh(&mut chart, "quality score", "frames")?;
            chart
                .draw_series(hist.iter().map(|&(a, b, c)| {
                    let colour = if a >= thr - (b - a) * 0.5 {
                        palette::GREEN
                    } else {
                        palette::LIGHT
                    };
                    Rectangle::new([(a, 0.0), (b, c as f64)], colour.mix(0.85).filled())
                }))
                .map_err(plot_err)?
                .label("frames")
                .legend(|(x, y)| {
                    Rectangle::new(
                        [(x, y - 5), (x + 20, y + 5)],
                        palette::GREEN.mix(0.85).filled(),
                    )
                });
            chart
                .draw_series(hist.iter().map(|&(a, b, c)| {
                    Rectangle::new([(a, 0.0), (b, c as f64)], BLACK.mix(0.35).stroke_width(1))
                }))
                .map_err(plot_err)?;
            chart
                .draw_series(DashedLineSeries::new(
                    vec![(thr, 0.0), (thr, top)],
                    6,
                    4,
                    palette::RED.stroke_width(2),
                ))
                .map_err(plot_err)?
                .label(format!("keep threshold {}", tick(thr)))
                .legend(|(x, y)| {
                    PathElement::new(vec![(x, y), (x + 20, y)], palette::RED.stroke_width(2))
                });
            legend(&mut chart, SeriesLabelPosition::UpperRight)
        })
    }

    /// Histogram bins as (low, high, count).
    fn histogram(&self) -> Vec<(f64, f64, usize)> {
        let s = &self.score_summary;
        let (min, max) = (f64::from(s.min), f64::from(s.max));
        let bins = ((self.scores.len() as f64).sqrt().round() as usize).clamp(10, 60);
        let width = if max > min {
            (max - min) / bins as f64
        } else {
            1.0
        };
        let mut counts = vec![0usize; bins];
        for &v in &self.scores {
            let i = (((f64::from(v) - min) / width) as usize).min(bins - 1);
            counts[i] += 1;
        }
        counts
            .into_iter()
            .enumerate()
            .map(|(i, c)| (min + i as f64 * width, min + (i + 1) as f64 * width, c))
            .collect()
    }

    fn plot_shift(&self, size: Size) -> Result<Vec<u8>> {
        if self.shifts.len() < 2 {
            return Err(Error::NoData(
                "fewer than two frames were registered, so there is no shift to plot".into(),
            ));
        }
        let first = self.shifts[0].frame as f64;
        let last = self.shifts[self.shifts.len() - 1].frame as f64;
        let extent = |f: fn(&ShiftRow) -> f32| {
            let v: Vec<f64> = self.shifts.iter().map(|s| f64::from(f(s))).collect();
            (
                v.iter().copied().fold(f64::INFINITY, f64::min),
                v.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            )
        };
        let (xl, xh) = extent(|s| s.dx);
        let (yl, yh) = extent(|s| s.dy);
        let (tl, th) = canvas::padded(xl.min(yl), xh.max(yh), 0.08);
        let drift = self.drift;
        let title = drift.map_or_else(
            || "Global shift of each registered frame".to_string(),
            |d| {
                format!(
                    "Global shift: drift ({:+.2}, {:+.2}) px per 1000 frames, jitter ({:.2}, {:.2}) px",
                    d.dx_per_1000, d.dy_per_1000, d.dx_jitter, d.dy_jitter
                )
            },
        );
        canvas::render(size, |root| {
            let (left, right) =
                root.split_horizontally(i32::try_from(size.width * 62 / 100).unwrap_or(600));
            let mut chart = ChartBuilder::on(&left)
                .caption(&title, (FONT, 16))
                .margin(12)
                .x_label_area_size(44)
                .y_label_area_size(60)
                .build_cartesian_2d(first..last.max(first + 1.0), tl..th)
                .map_err(plot_err)?;
            mesh(
                &mut chart,
                "frame number (capture order)",
                "shift against reference (px)",
            )?;
            for (name, colour, pick) in [
                (
                    "x",
                    palette::BLUE,
                    (|s: &ShiftRow| s.dx) as fn(&ShiftRow) -> f32,
                ),
                ("y", palette::ORANGE, |s: &ShiftRow| s.dy),
            ] {
                chart
                    .draw_series(self.shifts.iter().map(|s| {
                        Circle::new(
                            (s.frame as f64, f64::from(pick(s))),
                            2,
                            colour.mix(0.7).filled(),
                        )
                    }))
                    .map_err(plot_err)?
                    .label(name)
                    .legend(move |(x, y)| Circle::new((x + 10, y), 3, colour.filled()));
            }
            if let Some(d) = drift {
                for (colour, a, b) in [
                    (palette::BLUE, d.dx0, d.dx_per_1000),
                    (palette::ORANGE, d.dy0, d.dy_per_1000),
                ] {
                    let at = |f: f64| f64::from(a) + f64::from(b) * f / 1000.0;
                    chart
                        .draw_series(LineSeries::new(
                            vec![(first, at(first)), (last, at(last))],
                            colour.stroke_width(2),
                        ))
                        .map_err(plot_err)?;
                }
            }
            legend(&mut chart, SeriesLabelPosition::UpperRight)?;

            let mut path = ChartBuilder::on(&right)
                .caption("Path of the image", (FONT, 16))
                .margin(12)
                .x_label_area_size(44)
                .y_label_area_size(60)
                .build_cartesian_2d(tl..th, th..tl)
                .map_err(plot_err)?;
            mesh(&mut path, "x shift (px)", "y shift (px)")?;
            let m = self.shifts.len().max(2) as f64 - 1.0;
            path.draw_series(LineSeries::new(
                self.shifts
                    .iter()
                    .map(|s| (f64::from(s.dx), f64::from(s.dy))),
                palette::GREY.mix(0.35).stroke_width(1),
            ))
            .map_err(plot_err)?;
            path.draw_series(self.shifts.iter().enumerate().map(|(i, s)| {
                let t = i as f64 / m;
                let c = canvas::mix(palette::SKY, palette::RED, t);
                Circle::new((f64::from(s.dx), f64::from(s.dy)), 3, c.filled())
            }))
            .map_err(plot_err)?;
            path.draw_series(std::iter::once(Text::new(
                "start (blue) to end (red)",
                (tl + (th - tl) * 0.03, tl + (th - tl) * 0.06),
                (FONT, 13),
            )))
            .map_err(plot_err)?;
            Ok(())
        })
    }

    fn plot_seeing(&self, size: Size) -> Result<Vec<u8>> {
        let Some(s) = self.seeing.as_ref().filter(|s| s.points.len() >= 2) else {
            return Err(Error::NoData(
                "the seeing was not measured (it needs a disc with detail and `--seeing`, which --report turns on)".into(),
            ));
        };
        let last = s.points[s.points.len() - 1].frame as f64;
        let hi = s
            .points
            .iter()
            .map(|p| f64::from(p.rms_px))
            .fold(0.0, f64::max)
            * 1.1;
        let mut title = String::from("Seeing over the capture: differential motion per frame");
        if let Some(r0) = s.r0_500_cm {
            let _ = write!(title, " (r0 {r0:.1} cm at 500 nm");
            if let Some(g) = &s.grade {
                let _ = write!(title, ", {g}");
            }
            title.push(')');
        }
        let overall = s.overall_rms_px.map(f64::from);
        let overall_label = overall.map(|o| match s.arcsec_per_px {
            Some(a) => format!("whole capture {o:.3} px ({:.3} arcsec)", o * f64::from(a)),
            None => format!("whole capture {o:.3} px"),
        });
        canvas::render(size, |root| {
            let mut chart = ChartBuilder::on(root)
                .caption(&title, (FONT, 18))
                .margin(12)
                .x_label_area_size(44)
                .y_label_area_size(70)
                .build_cartesian_2d(s.points[0].frame as f64..last, 0.0..hi.max(1e-3))
                .map_err(plot_err)?;
            mesh(
                &mut chart,
                "frame number (capture order)",
                "differential motion (px per axis, lower is steadier)",
            )?;
            chart
                .draw_series(s.points.iter().map(|p| {
                    Circle::new(
                        (p.frame as f64, f64::from(p.rms_px)),
                        3,
                        palette::LIGHT.filled(),
                    )
                }))
                .map_err(plot_err)?
                .label("each frame")
                .legend(|(x, y)| Circle::new((x + 10, y), 3, palette::LIGHT.filled()));
            chart
                .draw_series(LineSeries::new(
                    s.points
                        .iter()
                        .map(|p| (p.frame as f64, f64::from(p.window_rms_px))),
                    palette::BLUE.stroke_width(3),
                ))
                .map_err(plot_err)?
                .label(format!("window of {} frames", 2 * s.half_window + 1))
                .legend(|(x, y)| {
                    PathElement::new(vec![(x, y), (x + 20, y)], palette::BLUE.stroke_width(3))
                });
            if let (Some(o), Some(label)) = (overall, overall_label) {
                chart
                    .draw_series(DashedLineSeries::new(
                        vec![(s.points[0].frame as f64, o), (last, o)],
                        6,
                        4,
                        palette::ORANGE.stroke_width(2),
                    ))
                    .map_err(plot_err)?
                    .label(label)
                    .legend(|(x, y)| {
                        PathElement::new(vec![(x, y), (x + 20, y)], palette::ORANGE.stroke_width(2))
                    });
            }
            legend(&mut chart, SeriesLabelPosition::UpperRight)
        })
    }

    /// The table of frames as CSV.
    #[must_use]
    pub fn csv(&self) -> String {
        let kept = self.is_kept();
        let mut shifts = std::collections::HashMap::new();
        for s in &self.shifts {
            shifts.insert(s.frame, (s.dx, s.dy));
        }
        let mut measured = std::collections::HashMap::new();
        for o in &self.outliers {
            measured.insert(o.frame, o.measured);
        }
        let mut s = String::from("frame,score,rank,kept,measured_score,dx_px,dy_px\n");
        for (i, &v) in self.scores.iter().enumerate() {
            let m = measured.get(&i).map_or(String::new(), |m| format!("{m}"));
            let sh = shifts
                .get(&i)
                .map_or(",".to_string(), |(dx, dy)| format!("{dx},{dy}"));
            let _ = writeln!(
                s,
                "{i},{v},{},{},{m},{sh}",
                self.rank.get(i).copied().unwrap_or(0),
                u8::from(kept[i])
            );
        }
        s
    }

    /// The seeing series as CSV.
    #[must_use]
    pub fn seeing_csv(&self) -> Option<String> {
        let sv = self.seeing.as_ref()?;
        let mut s = String::from("frame,rms_px,window_rms_px\n");
        for p in &sv.points {
            let _ = writeln!(s, "{},{},{}", p.frame, p.rms_px, p.window_rms_px);
        }
        Some(s)
    }

    /// Draw every plot that has data and add the files and the section to `bundle`.
    pub fn build(&self, bundle: &mut Bundle, size: Size) -> Result<()> {
        let mut figures = Vec::new();
        let mut notes = Vec::new();
        let mut json = serde_json::to_vec_pretty(self)?;
        json.push(b'\n');
        bundle.add(Self::JSON, json);
        bundle.add(Self::CSV, self.csv().into_bytes());
        let seeing_csv = self.seeing_csv();
        if let Some(c) = &seeing_csv {
            bundle.add(Self::SEEING_CSV, c.clone().into_bytes());
        }
        for kind in [
            PlotKind::QualitySorted,
            PlotKind::QualityOrder,
            PlotKind::QualityHistogram,
            PlotKind::ShiftDrift,
            PlotKind::SeeingTime,
        ] {
            match self.plot(kind, size) {
                Ok(png) => {
                    let (title, caption) = describe(kind);
                    let name = format!("{}.png", kind.name());
                    bundle.add(&name, png);
                    let mut data = vec![Self::JSON.to_string()];
                    data.push(if kind == PlotKind::SeeingTime {
                        Self::SEEING_CSV.to_string()
                    } else {
                        Self::CSV.to_string()
                    });
                    figures.push(Figure {
                        png: name,
                        title: title.to_string(),
                        caption: caption.to_string(),
                        data,
                    });
                }
                Err(Error::NoData(why)) => notes.push(format!("{}: {why}", kind.name())),
                Err(e) => return Err(e),
            }
        }
        let s = &self.score_summary;
        let k = &self.kept_summary;
        let row = |name: &str, v: &Summary| {
            vec![
                name.to_string(),
                v.n.to_string(),
                tick(f64::from(v.min)),
                tick(f64::from(v.median)),
                tick(f64::from(v.mean)),
                tick(f64::from(v.max)),
                tick(f64::from(v.std)),
            ]
        };
        let mut tables = vec![Table {
            title: format!("Frame scores ({})", self.metric),
            header: ["frames", "count", "min", "median", "mean", "max", "std"]
                .map(String::from)
                .to_vec(),
            rows: vec![row("all", s), row("kept", k)],
        }];
        if let Some(d) = &self.drift {
            tables.push(Table {
                title: "Drift of the global shift".into(),
                header: ["axis", "drift (px per 1000 frames)", "jitter RMS (px)"]
                    .map(String::from)
                    .to_vec(),
                rows: vec![
                    vec![
                        "x".into(),
                        format!("{:+.3}", d.dx_per_1000),
                        format!("{:.3}", d.dx_jitter),
                    ],
                    vec![
                        "y".into(),
                        format!("{:+.3}", d.dy_per_1000),
                        format!("{:.3}", d.dy_jitter),
                    ],
                ],
            });
        }
        bundle.sections.push(Section {
            id: "frame-quality".into(),
            title: "Frame quality".into(),
            intro: format!(
                "{} frames scored by the {} metric; the best {} are stacked. The keep threshold is the score of the lowest frame kept.",
                self.frames, self.metric, self.keep
            ),
            figures,
            tables,
            notes,
        });
        Ok(())
    }
}

fn describe(kind: PlotKind) -> (&'static str, &'static str) {
    match kind {
        PlotKind::QualitySorted => (
            "Quality graph, sorted",
            "Every frame's score from best to worst. The green band is the frames that were stacked; its right edge, and the red line, mark the keep threshold. A long flat top means many equally good frames; a steep fall near the left means only a few moments of good seeing, so keep fewer. Compare with AutoStakkert's quality graph.",
        ),
        PlotKind::QualityOrder => (
            "Quality graph, in capture order",
            "The same scores in the order they were captured. Blue points were kept, grey were not. The orange line is a running median: it follows the seeing and focus over the session, and a slow fall means the target drifted out of focus. Red crosses are scores that an artefact (a satellite trail, a hot pixel line) had raised; the pipeline put them back to their neighbours' level before ranking.",
        ),
        PlotKind::QualityHistogram => (
            "Quality histogram",
            "How the scores are spread. A single narrow hump means steady seeing and little to choose between frames; a long tail to the right is the rare good moment, and the green bars are what the keep threshold lets through. Two humps often mean the focus or the exposure changed part-way.",
        ),
        PlotKind::ShiftDrift => (
            "Global shift and drift",
            "Left: each registered frame's shift in x and y against the reference, with the straight line of the drift. The scatter about the line is the jitter that seeing and tracking add. Right: the same shifts as a path in the image plane, coloured from the start (blue) to the end (red). Only the candidate frames are registered, not every frame.",
        ),
        PlotKind::SeeingTime => (
            "Seeing over time",
            "The differential motion between neighbouring tiles of the disc, per frame sampled across the capture, in pixels per axis; lower is steadier. The blue line is a window average and the dashed line the whole-capture index. It is a relative index, not r0: compare runs through the same optics.",
        ),
        #[allow(unreachable_patterns, reason = "other groups describe their own kinds")]
        _ => ("", ""),
    }
}

fn fit_drift(shifts: &[ShiftRow]) -> Option<DriftFit> {
    let f: Vec<f32> = shifts.iter().map(|s| s.frame as f32).collect();
    let xs: Vec<f32> = shifts.iter().map(|s| s.dx).collect();
    let ys: Vec<f32> = shifts.iter().map(|s| s.dy).collect();
    let (ax, bx) = stats::linear_fit(&f, &xs)?;
    let (ay, by) = stats::linear_fit(&f, &ys)?;
    let rms = |v: &[f32], a: f32, b: f32| {
        let n = v.len().max(1) as f32;
        (v.iter()
            .zip(&f)
            .map(|(y, x)| (y - (a + b * x)).powi(2))
            .sum::<f32>()
            / n)
            .sqrt()
    };
    Some(DriftFit {
        dx_per_1000: bx * 1000.0,
        dy_per_1000: by * 1000.0,
        dx0: ax,
        dy0: ay,
        dx_jitter: rms(&xs, ax, bx),
        dy_jitter: rms(&ys, ay, by),
    })
}

fn seeing_series(r: &helia_core::seeing::SeeingReport) -> Option<SeeingSeries> {
    if r.series.len() < 2 {
        return None;
    }
    let half = SEEING_HALF_WINDOW;
    let v: Vec<f32> = r.series.iter().map(|s| stats::finite(s.rms_px)).collect();
    let points = r
        .series
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let lo = i.saturating_sub(half);
            let hi = (i + half + 1).min(v.len());
            let w = &v[lo..hi];
            SeeingPoint {
                frame: s.frame,
                rms_px: v[i],
                window_rms_px: (w.iter().map(|x| x * x).sum::<f32>() / w.len() as f32).sqrt(),
            }
        })
        .collect();
    Some(SeeingSeries {
        points,
        half_window: half,
        overall_rms_px: r.motion.map(|m| m.rms_px),
        arcsec_per_px: r.arcsec_per_px,
        r0_500_cm: r.spectral.map(|s| s.r0_500_cm),
        grade: r.grade.map(|g| g.label().to_string()),
    })
}

/// The mesh every chart shares.
fn mesh<DB, X, Y>(
    chart: &mut ChartContext<'_, DB, Cartesian2d<X, Y>>,
    x_desc: &str,
    y_desc: &str,
) -> Result<()>
where
    DB: DrawingBackend,
    X: Ranged<ValueType = f64> + ValueFormatter<f64>,
    Y: Ranged<ValueType = f64> + ValueFormatter<f64>,
{
    let (xr, yr) = (
        chart.as_coord_spec().x_spec().range(),
        chart.as_coord_spec().y_spec().range(),
    );
    let (xs, ys) = (xr.end - xr.start, yr.end - yr.start);
    chart
        .configure_mesh()
        .x_desc(x_desc)
        .y_desc(y_desc)
        .label_style((FONT, 14))
        .axis_desc_style((FONT, 15))
        .x_label_formatter(&|v| {
            if x_desc.starts_with("frame") || x_desc.starts_with("rank") {
                tick_int(*v)
            } else {
                tick_span(*v, xs)
            }
        })
        .y_label_formatter(&|v| {
            if y_desc == "frames" {
                tick_int(*v)
            } else {
                tick_span(*v, ys)
            }
        })
        .light_line_style(RGBColor(238, 238, 238))
        .bold_line_style(RGBColor(215, 215, 215))
        .draw()
        .map_err(plot_err)
}

/// The legend every chart shares.
fn legend<'a, DB, X, Y>(
    chart: &mut ChartContext<'a, DB, Cartesian2d<X, Y>>,
    at: SeriesLabelPosition,
) -> Result<()>
where
    DB: DrawingBackend + 'a,
    X: Ranged<ValueType = f64>,
    Y: Ranged<ValueType = f64>,
{
    chart
        .configure_series_labels()
        .background_style(WHITE.mix(0.88))
        .border_style(RGBColor(160, 160, 160))
        .label_font((FONT, 13))
        .position(at)
        .draw()
        .map_err(plot_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> FrameQualityData {
        let scores: Vec<f32> = (0..200)
            .map(|i| 1.0 + 0.5 * ((i as f32) / 17.0).sin() + (i % 7) as f32 * 0.05)
            .collect();
        let mut order: Vec<usize> = (0..scores.len()).collect();
        order.sort_by(|&a, &b| scores[b].total_cmp(&scores[a]));
        let kept: Vec<usize> = order[..40].to_vec();
        let mut rank = vec![0; scores.len()];
        for (r, &f) in order.iter().enumerate() {
            rank[f] = r;
        }
        let shifts: Vec<ShiftRow> = order[..80]
            .iter()
            .map(|&f| ShiftRow {
                frame: f,
                dx: 0.01 * f as f32 + (f % 5) as f32 * 0.1,
                dy: -0.005 * f as f32,
            })
            .collect();
        let mut shifts = shifts;
        shifts.sort_by_key(|s| s.frame);
        FrameQualityData {
            version: 1,
            metric: "gradient".into(),
            frames: scores.len(),
            keep: kept.len(),
            threshold: scores[kept[kept.len() - 1]],
            drift: fit_drift(&shifts),
            shifts,
            rank,
            outliers: vec![Outlier {
                frame: 10,
                measured: 3.0,
            }],
            score_summary: stats::summarise(&scores),
            kept_summary: stats::summarise(&scores),
            seeing: Some(SeeingSeries {
                points: (0..30)
                    .map(|i| SeeingPoint {
                        frame: i * 6,
                        rms_px: 0.2 + 0.1 * ((i as f32) / 4.0).sin().abs(),
                        window_rms_px: 0.25,
                    })
                    .collect(),
                half_window: 4,
                overall_rms_px: Some(0.25),
                arcsec_per_px: Some(0.4),
                r0_500_cm: Some(7.5),
                grade: Some("good".into()),
            }),
            kept,
            scores,
        }
    }

    #[test]
    fn every_plot_is_a_png() {
        let d = sample();
        for kind in [
            PlotKind::QualitySorted,
            PlotKind::QualityOrder,
            PlotKind::QualityHistogram,
            PlotKind::ShiftDrift,
            PlotKind::SeeingTime,
        ] {
            let png = d.plot(kind, Size::REPORT).expect("plot");
            assert_eq!(&png[1..4], b"PNG", "{}", kind.name());
            assert!(png.len() > 2000, "{} is {} bytes", kind.name(), png.len());
            // HELIA_REPORT_DUMP=dir keeps the pictures for looking at.
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
    fn the_csv_has_a_row_per_frame_and_blank_shifts_where_unregistered() {
        let d = sample();
        let csv = d.csv();
        assert_eq!(csv.lines().count(), 201);
        assert!(
            csv.lines().any(|l| l.ends_with(",,")),
            "an unregistered frame has empty shifts"
        );
        assert!(
            csv.lines().nth(11).is_some_and(|l| l.contains(",3,")),
            "the outlier's measured score"
        );
    }

    #[test]
    fn the_json_round_trips() {
        let d = sample();
        let s = serde_json::to_string(&d).expect("json");
        let back: FrameQualityData = serde_json::from_str(&s).expect("parse");
        assert_eq!(back, d);
    }

    #[test]
    fn the_histogram_counts_every_frame() {
        let d = sample();
        assert_eq!(d.histogram().iter().map(|b| b.2).sum::<usize>(), d.frames);
    }

    #[test]
    fn a_report_without_seeing_says_why_instead_of_failing() {
        let mut d = sample();
        d.seeing = None;
        let mut b = Bundle::default();
        d.build(
            &mut b,
            Size {
                width: 480,
                height: 300,
            },
        )
        .expect("build");
        let sec = &b.sections[0];
        assert_eq!(sec.figures.len(), 4);
        assert!(sec.notes.iter().any(|n| n.starts_with("seeing-time")));
        assert!(b.file("quality-sorted.png").is_some() && b.file("frame-quality.csv").is_some());
    }

    #[test]
    fn the_drift_fit_finds_the_slope() {
        let d = sample();
        let fit = d.drift.expect("drift");
        assert!((fit.dx_per_1000 - 10.0).abs() < 1.5, "{fit:?}");
        assert!((fit.dy_per_1000 + 5.0).abs() < 0.1, "{fit:?}");
    }
}
