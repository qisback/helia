// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! View model: everything the GUI decides that is not drawing.
//!
//! Step statuses, the sorted-quality curve, keep and threshold maths, alignment
//! point geometry, zoom maths and label formatting. No egui widgets and no I/O,
//! so all of it is unit tested.

use std::fmt::Write as _;

use eframe::egui::{Pos2, Rect, Vec2, pos2, vec2};
use helia_core::BayerPattern;
use helia_core::pipeline::Stage;
use helia_core::quality::{Keep, rank};

/// Number of pipeline steps shown in the rail.
pub const STEP_COUNT: usize = 11;

/// One step of the pipeline rail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Choose a capture.
    Open,
    /// Dark, flat and bad-pixel calibration of every frame.
    Calibrate,
    /// Score every frame.
    Analyse,
    /// Alignment points.
    Align,
    /// Combine the best frames.
    Stack,
    /// Sharpen the stack (wavelets, unsharp mask or deconvolution).
    Sharpen,
    /// Levels, curve, crop, rotate, resize and denoise.
    Adjust,
    /// Choose a colour for sharing.
    Colour,
    /// Write the result.
    Export,
    /// Stack several captures the same way and play them back as a time-lapse.
    Timelapse,
    /// Join panels of the Sun into a full-disc mosaic.
    Mosaic,
}

impl Step {
    /// All steps in order.
    pub const ALL: [Step; STEP_COUNT] = [
        Step::Open,
        Step::Calibrate,
        Step::Analyse,
        Step::Align,
        Step::Stack,
        Step::Sharpen,
        Step::Adjust,
        Step::Colour,
        Step::Export,
        Step::Timelapse,
        Step::Mosaic,
    ];

    /// One-based position.
    #[must_use]
    pub fn number(self) -> usize {
        Self::ALL.iter().position(|&s| s == self).unwrap_or(0) + 1
    }

    /// Rail label and inspector title.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Step::Open => "Open",
            Step::Calibrate => "Calibrate",
            Step::Analyse => "Analyse",
            Step::Align => "Align",
            Step::Stack => "Stack",
            Step::Sharpen => "Sharpen",
            Step::Adjust => "Adjust",
            Step::Colour => "Colour",
            Step::Export => "Export",
            Step::Timelapse => "Time-lapse",
            Step::Mosaic => "Mosaic",
        }
    }

    /// One-line explanation for the inspector.
    #[must_use]
    pub fn explanation(self) -> &'static str {
        match self {
            Step::Open => {
                "Choose a SER, AVI or video capture. Frames stay on disk and are decoded as you scrub."
            }
            Step::Calibrate => {
                "Takes the sensor's own pattern out of every frame, before it is scored or aligned: dust, vignetting, fringes, hot pixels. Optional."
            }
            Step::Analyse => {
                "Every frame is scored for sharpness so the best ones can be picked. Higher is sharper."
            }
            Step::Align => {
                "Alignment points follow the local distortion that seeing adds across the disc."
            }
            Step::Stack => "Combines the best frames into one image, each weighted by its quality.",
            Step::Sharpen => {
                "Brings out detail in the mono stack, before any colour. Wavelets are the usual choice; the preview follows every slider."
            }
            Step::Adjust => {
                "Everyday edits: denoise, levels, curve, crop, rotate and resize. Each is optional and is recorded in the file."
            }
            Step::Colour => {
                "Colour is for sharing. A filter stack stays mono: pick the look of your filter, or keep it grey. A true-colour stack is colour already."
            }
            Step::Export => {
                "Save the result as a 16-bit TIFF, an 8-bit PNG or a 32-bit float FITS file."
            }
            Step::Timelapse => {
                "Stack several captures with the settings above, register the stacks to each other and play them back. Optional."
            }
            Step::Mosaic => {
                "Stack the panels of a mosaic with the settings above, match their overlaps and join them into one picture. Optional."
            }
        }
    }

    /// Step that shows `stage` while a stack is running.
    #[must_use]
    pub fn of_stage(stage: Stage) -> Step {
        match stage {
            Stage::Quality => Step::Analyse,
            Stage::Reference | Stage::GlobalAlign | Stage::LocalAlign => Step::Align,
            Stage::Stack | Stage::Finish => Step::Stack,
        }
    }
}

/// Share of the whole run taken by each stage, for one overall progress bar.
/// Rough wall-time weights; they only shape the bar.
const STAGE_WEIGHTS: [(Stage, f32); 6] = [
    (Stage::Quality, 0.15),
    (Stage::Reference, 0.05),
    (Stage::GlobalAlign, 0.15),
    (Stage::LocalAlign, 0.30),
    (Stage::Stack, 0.30),
    (Stage::Finish, 0.05),
];

/// Overall progress in `0..=1` from a stage and its own fraction.
#[must_use]
pub fn overall_progress(stage: Stage, fraction: f32) -> f32 {
    let mut done = 0.0;
    for (s, w) in STAGE_WEIGHTS {
        if s == stage {
            return (done + w * fraction.clamp(0.0, 1.0)).clamp(0.0, 1.0);
        }
        done += w;
    }
    1.0
}

/// How a rail status is coloured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Nothing to report yet.
    Idle,
    /// Work in progress.
    Running,
    /// Finished.
    Done,
}

/// Rail status line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    /// Text under the step label.
    pub text: String,
    /// Colour class.
    pub tone: Tone,
}

impl Status {
    fn new(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            text: text.into(),
            tone,
        }
    }
}

/// Scoring progress.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scoring {
    /// Not started.
    Idle,
    /// Running on the worker.
    Running,
    /// Scores are in.
    Done,
}

/// Inputs of [`step_status`].
#[derive(Clone, Debug)]
pub struct Snapshot {
    /// Container name, "SER" or "AVI".
    pub container: Option<String>,
    /// Frames in the capture.
    pub frames: usize,
    /// Scoring state.
    pub scoring: Scoring,
    /// Stage of the running stack, when one is running.
    pub running: Option<Stage>,
    /// A stack result exists.
    pub has_result: bool,
    /// Active alignment points in the current grid.
    pub ap_count: Option<usize>,
    /// Name of the chosen colour look ("Mono" or a preset name).
    pub colour_name: String,
    /// The sharpening method, or "Off".
    pub sharpen_name: String,
    /// "None", "1 adjustment" and so on.
    pub adjust_name: String,
    /// The calibration masters.
    pub calibration: CalBrief,
    /// What the Time-lapse step has to say, for the rail: the line and its tone.
    pub timelapse: (String, Tone),
    /// The hint under the Time-lapse row.
    pub timelapse_hint: String,
    /// What the Mosaic step has to say, for the rail: the line and its tone.
    pub mosaic: (String, Tone),
    /// The hint under the Mosaic row.
    pub mosaic_hint: String,
}

/// What the Calibrate step has to say, for the rail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CalBrief {
    /// No masters chosen.
    Off,
    /// Building or reading them.
    Preparing,
    /// In force: "Dark, flat".
    Ready(String),
    /// They could not be used.
    Failed,
}

/// Status line of `step`.
#[must_use]
pub fn step_status(step: Step, s: &Snapshot) -> Status {
    let loaded = s.container.is_some();
    let active = s.running.map(Step::of_stage);
    match step {
        Step::Open => match &s.container {
            Some(c) => Status::new(format!("{c} \u{b7} {} frames", s.frames), Tone::Done),
            None => Status::new("No capture", Tone::Idle),
        },
        Step::Calibrate => match &s.calibration {
            CalBrief::Off => Status::new("Optional", Tone::Idle),
            CalBrief::Preparing => Status::new("Preparing", Tone::Running),
            CalBrief::Ready(what) => Status::new(what.clone(), Tone::Done),
            CalBrief::Failed => Status::new("Needs attention", Tone::Idle),
        },
        Step::Sharpen => {
            if !s.has_result {
                Status::new("No result yet", Tone::Idle)
            } else if s.sharpen_name == "Off" {
                Status::new("Off", Tone::Idle)
            } else {
                Status::new(s.sharpen_name.clone(), Tone::Done)
            }
        }
        Step::Adjust => {
            if !s.has_result {
                Status::new("No result yet", Tone::Idle)
            } else if s.adjust_name == "None" {
                Status::new("Optional", Tone::Idle)
            } else {
                Status::new(s.adjust_name.clone(), Tone::Done)
            }
        }
        Step::Colour => {
            if s.has_result {
                Status::new(s.colour_name.clone(), Tone::Done)
            } else {
                Status::new("No result yet", Tone::Idle)
            }
        }
        Step::Analyse => {
            if active == Some(Step::Analyse) || (s.scoring == Scoring::Running && loaded) {
                Status::new("Scoring", Tone::Running)
            } else if s.scoring == Scoring::Done || s.has_result {
                Status::new("Scored", Tone::Done)
            } else {
                Status::new("Waiting", Tone::Idle)
            }
        }
        Step::Align => {
            if active == Some(Step::Align) {
                Status::new("Running", Tone::Running)
            } else if s.has_result || active == Some(Step::Stack) {
                Status::new("Done", Tone::Done)
            } else if let Some(n) = s.ap_count {
                Status::new(format!("{n} points"), Tone::Idle)
            } else {
                Status::new("Waiting", Tone::Idle)
            }
        }
        Step::Stack => {
            if active == Some(Step::Stack) {
                Status::new("Running", Tone::Running)
            } else if s.has_result {
                Status::new("Done", Tone::Done)
            } else if s.scoring == Scoring::Done {
                Status::new("Ready", Tone::Idle)
            } else {
                Status::new("Waiting", Tone::Idle)
            }
        }
        Step::Export => {
            if s.has_result {
                Status::new("Ready", Tone::Done)
            } else {
                Status::new("No result yet", Tone::Idle)
            }
        }
        Step::Timelapse => Status::new(s.timelapse.0.clone(), s.timelapse.1),
        Step::Mosaic => Status::new(s.mosaic.0.clone(), s.mosaic.1),
    }
}

/// One plain line telling the user what to do next on `step`.
#[must_use]
pub fn step_hint(step: Step, s: &Snapshot) -> String {
    let loaded = s.container.is_some();
    let active = s.running.map(Step::of_stage);
    let text = match step {
        Step::Open if loaded => "Capture open. Press Stack to carry on",
        Step::Open => "Open a capture",
        Step::Calibrate => match &s.calibration {
            CalBrief::Off => "Optional: darks and flats",
            CalBrief::Preparing => "Preparing the masters",
            CalBrief::Ready(_) => "Check the before and after",
            CalBrief::Failed => "Check the files",
        },
        Step::Sharpen if s.has_result => "Check the preview",
        Step::Sharpen => "Stack first",
        Step::Adjust if s.has_result => "Optional: tone, crop, rotate",
        Step::Adjust => "Stack first",
        Step::Analyse if !loaded => "Open a capture first",
        Step::Analyse if active == Some(Step::Analyse) || s.scoring == Scoring::Running => {
            "Scoring the frames, please wait"
        }
        Step::Analyse => "Done: the sharpest frames are ranked",
        Step::Align if !loaded => "Open a capture first",
        Step::Align if active == Some(Step::Align) => "Aligning, please wait",
        Step::Align if s.has_result => "Done",
        Step::Align => "Optional: check the points",
        Step::Stack if !loaded => "Open a capture first",
        Step::Stack if active.is_some() => "Stacking, please wait",
        Step::Stack if s.has_result => "Done: sharpen, colour and export",
        Step::Stack if s.scoring == Scoring::Done => "Ready: press Stack",
        Step::Stack => "Scoring first, then press Stack",
        Step::Colour if s.has_result => "Pick a colour, or keep mono",
        Step::Colour => "Stack first",
        Step::Export if s.has_result => "Save as TIFF, PNG or FITS",
        Step::Export => "Stack first",
        Step::Timelapse => return s.timelapse_hint.clone(),
        Step::Mosaic => return s.mosaic_hint.clone(),
    };
    text.to_string()
}

/// Output file kinds the Export step offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    /// 16-bit TIFF, grey or RGB.
    Tiff,
    /// 8-bit PNG for sharing, grey or RGB.
    Png,
    /// 32-bit float FITS, always mono.
    Fits,
}

impl ExportFormat {
    /// File extension.
    #[must_use]
    pub fn ext(self) -> &'static str {
        match self {
            Self::Tiff => "tif",
            Self::Png => "png",
            Self::Fits => "fits",
        }
    }

    /// Chip label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Tiff => "TIFF 16-bit",
            Self::Png => "PNG 8-bit",
            Self::Fits => "FITS",
        }
    }

    /// Whether the colour choice reaches the file.
    #[must_use]
    pub fn takes_colour(self) -> bool {
        self != Self::Fits
    }
}

/// What the Export step will write, in a sentence.
#[must_use]
pub fn export_summary(
    format: ExportFormat,
    colour: helia_core::ColourParams,
    edited: bool,
    fits_edited: bool,
) -> String {
    let bits = match format {
        ExportFormat::Tiff => "16-bit",
        ExportFormat::Png => "8-bit",
        ExportFormat::Fits => "32-bit float",
    };
    let suffix = if edited { ", with the edits" } else { "" };
    match (format, colour.preset) {
        (ExportFormat::Fits, _) => match (edited, fits_edited) {
            (false, _) => "32-bit float FITS, mono and unaltered".to_string(),
            (true, false) => "32-bit float FITS, mono, the unsharpened stack".to_string(),
            (true, true) => "32-bit float FITS, mono, with the edits".to_string(),
        },
        (f, Some(p)) => format!(
            "{bits} RGB {}, {} colours{suffix}",
            if f == ExportFormat::Png {
                "PNG"
            } else {
                "TIFF"
            },
            p.name()
        ),
        (f, None) => format!(
            "{bits} greyscale {}{suffix}",
            if f == ExportFormat::Png {
                "PNG"
            } else {
                "TIFF"
            },
        ),
    }
}

/// The settings a Quick stack uses, one short phrase each.
#[must_use]
pub fn quick_lines(
    rec: &helia_core::recommend::Recommendation,
    frames: usize,
    bayer: bool,
) -> Vec<String> {
    use helia_core::debayer::Channel;
    let channel = match rec.channel {
        Channel::Red => "Red channel",
        Channel::Green => "Green channel",
        Channel::Blue => "Blue channel",
        Channel::Luminance => "All channels combined",
    };
    let keep = match rec.keep {
        Keep::Fraction(f) => {
            let n = Keep::Fraction(f).count(frames);
            format!(
                "Keep the best {:.0}% ({} of {} frames)",
                f * 100.0,
                crate::activity::group(n),
                crate::activity::group(frames)
            )
        }
        Keep::Count(n) => format!("Keep the best {n} frames"),
    };
    let mut lines = Vec::new();
    // A mono camera has no channels to choose from.
    if bayer {
        lines.push(channel.to_string());
    }
    lines.push(keep);
    lines.push(format!("Alignment points {} px", rec.ap_size));
    lines
}

/// Frames kept for `percent` of `total`, as the pipeline will count them.
#[must_use]
pub fn keep_count(percent: f32, total: usize) -> usize {
    Keep::Fraction(percent / 100.0).count(total)
}

/// Percentage that `kept` of `total` frames represent.
#[must_use]
pub fn keep_percent(kept: usize, total: usize) -> f32 {
    if total == 0 {
        0.0
    } else {
        kept as f32 / total as f32 * 100.0
    }
}

/// "keeping best K of N (P%)".
#[must_use]
pub fn keep_label(kept: usize, total: usize) -> String {
    format!(
        "keeping best {kept} of {total} ({:.0}%)",
        keep_percent(kept, total)
    )
}

/// Slider value snapped to the 5 % grid and clamped to 5..=100.
#[must_use]
pub fn snap_keep(percent: f32) -> f32 {
    ((percent / 5.0).round() * 5.0).clamp(5.0, 100.0)
}

/// Quality scores sorted best first, with the lookups the UI needs.
#[derive(Clone, Debug, Default)]
pub struct Curve {
    /// Scores, best first.
    pub sorted: Vec<f32>,
    ranking: Vec<usize>,
    rank_of: Vec<usize>,
    lo: f32,
    hi: f32,
}

impl Curve {
    /// Sort `scores` (one per frame).
    #[must_use]
    pub fn new(scores: &[f32]) -> Self {
        let ranking = rank(scores);
        let mut rank_of = vec![0; scores.len()];
        for (r, &f) in ranking.iter().enumerate() {
            rank_of[f] = r;
        }
        let sorted: Vec<f32> = ranking.iter().map(|&f| scores[f]).collect();
        let finite = sorted.iter().copied().filter(|v| v.is_finite());
        let hi = finite.clone().fold(f32::NEG_INFINITY, f32::max);
        let lo = finite.fold(f32::INFINITY, f32::min);
        let (lo, hi) = if lo.is_finite() && hi.is_finite() {
            (lo, hi)
        } else {
            (0.0, 0.0)
        };
        Self {
            sorted,
            ranking,
            rank_of,
            lo,
            hi,
        }
    }

    /// Number of frames.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sorted.len()
    }

    /// True when no frame was scored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sorted.is_empty()
    }

    /// Zero-based rank of `frame`, best is 0.
    #[must_use]
    pub fn rank_of(&self, frame: usize) -> Option<usize> {
        self.rank_of.get(frame).copied()
    }

    /// Frame at zero-based `rank`.
    #[must_use]
    pub fn frame_at_rank(&self, rank: usize) -> Option<usize> {
        self.ranking.get(rank).copied()
    }

    /// Displayed quality of `frame` in `0..=1`: where its score sits between
    /// the worst frame (0) and the best (1). This is the scale the curve is
    /// drawn on, so the number and the curve agree, and quality falls
    /// monotonically with rank. (A ratio to the best score would not: when
    /// scores cluster, the 700th frame of 1000 still reads 0.90.)
    #[must_use]
    pub fn relative(&self, frame: usize) -> Option<f32> {
        let r = self.rank_of(frame)?;
        let s = self.sorted[r];
        let span = self.hi - self.lo;
        if !s.is_finite() {
            Some(0.0)
        } else if span <= f32::EPSILON {
            Some(1.0)
        } else {
            Some(((s - self.lo) / span).clamp(0.0, 1.0))
        }
    }

    /// Height in `0..=1` of the curve at `rank`. The lowest score sits a
    /// little above the baseline so the curve never hides on the axis.
    #[must_use]
    pub fn height(&self, rank: usize) -> f32 {
        const FLOOR: f32 = 0.08;
        let s = self.sorted.get(rank).copied().unwrap_or(self.lo);
        let span = self.hi - self.lo;
        if span <= f32::EPSILON || !s.is_finite() {
            return if span <= f32::EPSILON { 0.5 } else { FLOOR };
        }
        FLOOR + (1.0 - FLOOR) * ((s - self.lo) / span).clamp(0.0, 1.0)
    }

    /// Horizontal position in `0..=1` of the centre of `rank`.
    #[must_use]
    pub fn x_of_rank(&self, rank: usize) -> f32 {
        (rank as f32 + 0.5) / self.len().max(1) as f32
    }

    /// Rank under horizontal position `t` in `0..=1`.
    #[must_use]
    pub fn rank_at(&self, t: f32) -> Option<usize> {
        let n = self.len();
        (n > 0).then(|| ((t.clamp(0.0, 1.0) * n as f32) as usize).min(n - 1))
    }

    /// Horizontal position of the keep threshold: the edge after the last
    /// kept frame.
    #[must_use]
    pub fn threshold_x(&self, kept: usize) -> f32 {
        kept.min(self.len()) as f32 / self.len().max(1) as f32
    }

    /// Polyline for `plot`, thinned to at most `max_points` vertices.
    #[must_use]
    pub fn points(&self, plot: Rect, max_points: usize) -> Vec<Pos2> {
        let n = self.len();
        if n == 0 || max_points == 0 {
            return Vec::new();
        }
        let stride = n.div_ceil(max_points).max(1);
        let mut ranks: Vec<usize> = (0..n).step_by(stride).collect();
        if ranks.last() != Some(&(n - 1)) {
            ranks.push(n - 1);
        }
        ranks.into_iter().map(|r| self.point(plot, r)).collect()
    }

    /// Screen position of `rank` inside `plot`.
    #[must_use]
    pub fn point(&self, plot: Rect, rank: usize) -> Pos2 {
        pos2(
            plot.left() + self.x_of_rank(rank) * plot.width(),
            plot.bottom() - self.height(rank) * plot.height(),
        )
    }
}

/// Lower and upper zoom limits, in screen pixels per image pixel.
pub const ZOOM_RANGE: (f32, f32) = (0.02, 32.0);
/// Factor of one zoom button press.
pub const ZOOM_STEP: f32 = 1.25;

/// Pan and zoom of the viewer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    /// Screen pixels per image pixel.
    pub zoom: f32,
    /// Offset of the image centre from the viewer centre, in screen pixels.
    pub pan: Vec2,
    /// Follow the window size instead of the stored zoom.
    pub fit: bool,
}

impl Default for View {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan: Vec2::ZERO,
            fit: true,
        }
    }
}

/// Zoom at which `image` fills `viewport` with a small margin.
#[must_use]
pub fn fit_zoom(viewport: Vec2, image: Vec2) -> f32 {
    if image.x <= 0.0 || image.y <= 0.0 || viewport.x <= 0.0 || viewport.y <= 0.0 {
        return 1.0;
    }
    ((viewport.x / image.x).min(viewport.y / image.y) * 0.96).clamp(ZOOM_RANGE.0, ZOOM_RANGE.1)
}

impl View {
    /// Back to fit-to-window.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Zoom by `factor` keeping the image point under `anchor` (cursor offset
    /// from the viewer centre) fixed.
    pub fn zoom_about(&mut self, anchor: Vec2, factor: f32) {
        let new = (self.zoom * factor).clamp(ZOOM_RANGE.0, ZOOM_RANGE.1);
        let k = new / self.zoom;
        self.pan = anchor - (anchor - self.pan) * k;
        self.zoom = new;
        self.fit = false;
    }

    /// Settle the effective zoom for this frame (resolving fit mode) and keep
    /// the image from being dragged out of sight.
    pub fn settle(&mut self, viewport: Vec2, image: Vec2) {
        if self.fit {
            self.zoom = fit_zoom(viewport, image);
            self.pan = Vec2::ZERO;
        } else {
            let reach = (image * self.zoom + viewport) * 0.5 - vec2(64.0, 64.0);
            let reach = reach.max(Vec2::ZERO);
            self.pan = self.pan.clamp(-reach, reach);
        }
    }

    /// Screen rectangle of the image.
    #[must_use]
    pub fn image_rect(&self, viewer: Rect, image: Vec2) -> Rect {
        Rect::from_center_size(viewer.center() + self.pan, image * self.zoom)
    }

    /// Zoom readout, percent of native size.
    #[must_use]
    pub fn percent(&self) -> u32 {
        (self.zoom * 100.0).round() as u32
    }
}

/// Screen rectangle of an alignment point with image-space bounds
/// `[x0, x1) x [y0, y1)`, given the screen position of the image origin.
#[must_use]
pub fn ap_screen_rect(origin: Pos2, zoom: f32, bounds: [usize; 4]) -> Rect {
    let [x0, y0, x1, y1] = bounds;
    Rect::from_min_max(
        origin + vec2(x0 as f32, y0 as f32) * zoom,
        origin + vec2(x1 as f32, y1 as f32) * zoom,
    )
}

/// "Frame 42 / 100 \u{b7} quality 0.71 \u{b7} rank 18" (all one-based).
#[must_use]
pub fn scrub_label(
    index: usize,
    count: usize,
    relative: Option<f32>,
    rank: Option<usize>,
) -> String {
    let mut s = format!("Frame {} / {count}", index + 1);
    if let Some(q) = relative {
        let _ = write!(s, " \u{b7} quality {q:.2}");
    }
    if let Some(r) = rank {
        let _ = write!(s, " \u{b7} rank {}", r + 1);
    }
    s
}

/// Median of `values`, ignoring non-finite ones.
#[must_use]
pub fn median(values: &[f32]) -> Option<f32> {
    let mut v: Vec<f32> = values.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(f32::total_cmp);
    let m = v.len() / 2;
    Some(if v.len() % 2 == 1 {
        v[m]
    } else {
        f32::midpoint(v[m - 1], v[m])
    })
}

/// What the top bar shows about a capture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureMeta {
    /// File name.
    pub file: String,
    /// "SER", "AVI", or the extension of a video ("MP4", "MOV", ...).
    pub container: String,
    /// Camera model from a `SharpCap` sidecar.
    pub camera: Option<String>,
    /// Frame width.
    pub width: usize,
    /// Frame height.
    pub height: usize,
    /// Bits per sample.
    pub bits: Option<u8>,
    /// Bayer pattern of the stored frames; `None` is mono or colour.
    pub bayer: Option<BayerPattern>,
    /// Frame count.
    pub frames: usize,
    /// Compressed video decoded by ffmpeg.
    pub video: bool,
    /// Quality caveat for lossy input, shown in the Open step.
    pub note: Option<String>,
}

/// Container and bit depth parsed from `FrameSource::describe`.
#[must_use]
pub fn parse_describe(desc: &str) -> (String, Option<u8>) {
    let container = desc.split_whitespace().next().unwrap_or("").to_string();
    let bits = desc
        .split(|c: char| c.is_whitespace() || c == ',')
        .find_map(|t| t.strip_suffix("-bit").and_then(|n| n.parse().ok()))
        .or_else(|| {
            [("Gray8", 8), ("Gray16", 16), ("Bgr24", 8)]
                .into_iter()
                .find_map(|(k, b)| desc.contains(k).then_some(b))
        });
    (container, bits)
}

/// Name of a Bayer pattern.
#[must_use]
pub fn bayer_name(p: BayerPattern) -> &'static str {
    match p {
        BayerPattern::Rggb => "RGGB",
        BayerPattern::Grbg => "GRBG",
        BayerPattern::Gbrg => "GBRG",
        BayerPattern::Bggr => "BGGR",
    }
}

impl CaptureMeta {
    /// Fields for the top bar, in order, skipping unknown ones.
    #[must_use]
    pub fn fields(&self) -> Vec<String> {
        let mut out = vec![self.file.clone()];
        out.extend(self.camera.clone());
        out.push(format!("{}x{}", self.width, self.height));
        let depth = match (self.bits, self.bayer) {
            (Some(b), Some(p)) => Some(format!("{b}-bit {}", bayer_name(p))),
            (Some(b), None) => Some(format!("{b}-bit")),
            (None, Some(p)) => Some(bayer_name(p).to_string()),
            (None, None) => None,
        };
        out.extend(depth);
        out.push(format!("{} frames", self.frames));
        out
    }
}

/// Backend chip text split from `GpuBackend::adapter_summary`
/// (`NVIDIA GeForce RTX 3080 (Vulkan, DiscreteGpu)`) into
/// (adapter name, API, is software).
#[must_use]
pub fn split_adapter(summary: &str) -> (String, String, bool) {
    let Some(open) = summary.rfind(" (") else {
        return (summary.to_string(), String::new(), false);
    };
    let name = summary[..open].to_string();
    let inner = summary[open + 2..].trim_end_matches(')');
    let mut parts = inner.split(", ");
    let api = parts.next().unwrap_or("").to_string();
    let software = parts.next().is_some_and(|t| t.eq_ignore_ascii_case("cpu"));
    (name, api, software)
}

/// Shorten `s` to `max` characters with an ellipsis.
#[must_use]
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max.saturating_sub(1)).collect();
        // No dangling space before the ellipsis.
        format!("{}\u{2026}", head.trim_end())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use super::*;

    fn snap() -> Snapshot {
        Snapshot {
            container: None,
            frames: 0,
            scoring: Scoring::Idle,
            running: None,
            has_result: false,
            ap_count: None,
            colour_name: "Mono".into(),
            sharpen_name: "Wavelets".into(),
            adjust_name: "None".into(),
            calibration: CalBrief::Off,
            timelapse: ("Optional".into(), Tone::Idle),
            timelapse_hint: "Add two or more captures".into(),
            mosaic: ("Optional".into(), Tone::Idle),
            mosaic_hint: "Add two or more panels".into(),
        }
    }

    #[test]
    fn every_step_says_what_to_do_next() {
        let mut s = snap();
        assert_eq!(step_hint(Step::Open, &s), "Open a capture");
        assert_eq!(step_hint(Step::Stack, &s), "Open a capture first");
        s.container = Some("SER".into());
        s.scoring = Scoring::Running;
        assert_eq!(
            step_hint(Step::Open, &s),
            "Capture open. Press Stack to carry on"
        );
        assert_eq!(
            step_hint(Step::Stack, &s),
            "Scoring first, then press Stack"
        );
        s.scoring = Scoring::Done;
        assert_eq!(step_hint(Step::Stack, &s), "Ready: press Stack");
        s.running = Some(Stage::Stack);
        assert_eq!(step_hint(Step::Stack, &s), "Stacking, please wait");
        s.running = None;
        s.has_result = true;
        assert_eq!(
            step_hint(Step::Stack, &s),
            "Done: sharpen, colour and export"
        );
        assert_eq!(step_hint(Step::Sharpen, &s), "Check the preview");
        assert_eq!(step_hint(Step::Adjust, &s), "Optional: tone, crop, rotate");
        assert_eq!(step_hint(Step::Colour, &s), "Pick a colour, or keep mono");
        assert_eq!(step_hint(Step::Export, &s), "Save as TIFF, PNG or FITS");
        for step in Step::ALL {
            assert!(!step_hint(step, &s).is_empty(), "{step:?}");
            assert!(
                step_hint(step, &snap()).len() < 40,
                "{step:?} must fit the rail"
            );
        }
    }

    #[test]
    fn export_summaries_name_the_file_that_will_be_written() {
        use helia_core::{ColourParams, Preset};
        let ha = ColourParams::preset(Preset::Ha);
        assert_eq!(
            export_summary(ExportFormat::Tiff, ha, false, false),
            "16-bit RGB TIFF, H-alpha colours"
        );
        assert_eq!(
            export_summary(ExportFormat::Png, ha, true, false),
            "8-bit RGB PNG, H-alpha colours, with the edits"
        );
        assert_eq!(
            export_summary(ExportFormat::Png, ColourParams::default(), false, false),
            "8-bit greyscale PNG"
        );
        assert_eq!(
            export_summary(ExportFormat::Fits, ha, false, false),
            "32-bit float FITS, mono and unaltered"
        );
        assert_eq!(
            export_summary(ExportFormat::Fits, ha, true, false),
            "32-bit float FITS, mono, the unsharpened stack"
        );
        assert_eq!(
            export_summary(ExportFormat::Fits, ha, true, true),
            "32-bit float FITS, mono, with the edits"
        );
        assert!(!ExportFormat::Fits.takes_colour() && ExportFormat::Png.takes_colour());
    }

    #[test]
    fn quick_stack_lines_state_the_recommended_settings() {
        use helia_core::recommend::recommend;
        let r = recommend(1936, 1096, true, &["Sun_Ha_01".to_string()], None);
        assert_eq!(
            quick_lines(&r, 1000, true),
            vec![
                "Red channel",
                "Keep the best 25% (250 of 1,000 frames)",
                "Alignment points 72 px"
            ]
        );
        assert_eq!(quick_lines(&r, 1000, false).len(), 2, "no channel for mono");
    }

    #[test]
    fn steps_are_numbered() {
        assert_eq!(Step::Open.number(), 1);
        assert_eq!(Step::Mosaic.number(), STEP_COUNT);
        assert_eq!(Step::Export.number() + 2, STEP_COUNT);
        assert_eq!(Step::Calibrate.number(), 2);
        assert_eq!(Step::Adjust.number(), Step::Sharpen.number() + 1);
        assert_eq!(Step::Colour.number(), Step::Adjust.number() + 1);
    }

    #[test]
    fn status_walks_from_empty_to_exported() {
        let mut s = snap();
        assert_eq!(step_status(Step::Open, &s).text, "No capture");
        assert_eq!(step_status(Step::Stack, &s).text, "Waiting");
        s.container = Some("SER".into());
        s.frames = 100;
        s.scoring = Scoring::Running;
        assert_eq!(step_status(Step::Open, &s).text, "SER \u{b7} 100 frames");
        assert_eq!(step_status(Step::Analyse, &s).tone, Tone::Running);
        s.scoring = Scoring::Done;
        assert_eq!(step_status(Step::Analyse, &s).text, "Scored");
        assert_eq!(step_status(Step::Stack, &s).text, "Ready");
        s.ap_count = Some(42);
        assert_eq!(step_status(Step::Align, &s).text, "42 points");
        s.running = Some(Stage::LocalAlign);
        assert_eq!(step_status(Step::Align, &s).text, "Running");
        assert_eq!(step_status(Step::Stack, &s).text, "Ready");
        s.running = Some(Stage::Stack);
        assert_eq!(step_status(Step::Align, &s).text, "Done");
        assert_eq!(step_status(Step::Stack, &s).tone, Tone::Running);
        s.running = None;
        s.has_result = true;
        assert_eq!(step_status(Step::Stack, &s).text, "Done");
        assert_eq!(step_status(Step::Export, &s).text, "Ready");
    }

    #[test]
    fn rescoring_during_a_stack_keeps_analyse_running() {
        let mut s = snap();
        s.container = Some("AVI".into());
        s.running = Some(Stage::Quality);
        assert_eq!(step_status(Step::Analyse, &s).tone, Tone::Running);
    }

    #[test]
    fn calibrate_reports_its_masters() {
        let mut s = snap();
        assert_eq!(step_status(Step::Calibrate, &s).text, "Optional");
        assert_eq!(step_status(Step::Calibrate, &s).tone, Tone::Idle);
        s.calibration = CalBrief::Preparing;
        assert_eq!(step_status(Step::Calibrate, &s).tone, Tone::Running);
        s.calibration = CalBrief::Ready("Dark, flat".into());
        assert_eq!(step_status(Step::Calibrate, &s).text, "Dark, flat");
        assert_eq!(step_status(Step::Calibrate, &s).tone, Tone::Done);
        assert_eq!(step_hint(Step::Calibrate, &s), "Check the before and after");
        s.calibration = CalBrief::Failed;
        assert_eq!(step_status(Step::Calibrate, &s).tone, Tone::Idle);
    }

    #[test]
    fn sharpen_and_adjust_report_their_state_once_there_is_a_result() {
        let mut s = snap();
        assert_eq!(step_status(Step::Sharpen, &s).text, "No result yet");
        assert_eq!(step_status(Step::Adjust, &s).text, "No result yet");
        s.has_result = true;
        assert_eq!(step_status(Step::Sharpen, &s).text, "Wavelets");
        assert_eq!(step_status(Step::Sharpen, &s).tone, Tone::Done);
        assert_eq!(step_status(Step::Adjust, &s).text, "Optional");
        s.sharpen_name = "Off".into();
        s.adjust_name = "2 adjustments".into();
        assert_eq!(step_status(Step::Sharpen, &s).tone, Tone::Idle);
        assert_eq!(step_status(Step::Adjust, &s).text, "2 adjustments");
        assert_eq!(step_status(Step::Adjust, &s).tone, Tone::Done);
    }

    #[test]
    fn overall_progress_is_monotonic_and_ends_at_one() {
        let mut last = 0.0;
        for (s, _) in STAGE_WEIGHTS {
            for f in [0.0, 0.5, 1.0] {
                let p = overall_progress(s, f);
                assert!(p >= last - 1e-6, "{s:?} {f}");
                last = p;
            }
        }
        assert!((last - 1.0).abs() < 1e-6);
        assert_eq!(overall_progress(Stage::Quality, 0.0), 0.0);
        assert_eq!(overall_progress(Stage::Quality, 7.0), 0.15);
    }

    #[test]
    fn stages_map_to_steps() {
        assert_eq!(Step::of_stage(Stage::Quality), Step::Analyse);
        assert_eq!(Step::of_stage(Stage::GlobalAlign), Step::Align);
        assert_eq!(Step::of_stage(Stage::Finish), Step::Stack);
    }

    #[test]
    fn keep_maths_matches_the_pipeline_rounding() {
        assert_eq!(keep_count(25.0, 100), 25);
        assert_eq!(keep_count(5.0, 7), 1);
        assert_eq!(keep_count(100.0, 7), 7);
        assert_eq!(keep_count(5.0, 0), 0);
        assert_eq!(keep_label(18, 72), "keeping best 18 of 72 (25%)");
        assert!((keep_percent(1, 3) - 33.333).abs() < 0.01);
        assert_eq!(keep_percent(1, 0), 0.0);
    }

    #[test]
    fn keep_slider_snaps_to_five_percent() {
        assert_eq!(snap_keep(22.4), 20.0);
        assert_eq!(snap_keep(22.6), 25.0);
        assert_eq!(snap_keep(1.0), 5.0);
        assert_eq!(snap_keep(250.0), 100.0);
    }

    #[test]
    fn curve_ranks_best_first() {
        let c = Curve::new(&[0.2, 0.9, 0.5, 0.7]);
        assert_eq!(c.sorted, vec![0.9, 0.7, 0.5, 0.2]);
        assert_eq!(c.rank_of(1), Some(0));
        assert_eq!(c.rank_of(0), Some(3));
        assert_eq!(c.frame_at_rank(1), Some(3));
        assert_eq!(c.rank_of(9), None);
        assert!((c.relative(2).unwrap() - 0.3 / 0.7).abs() < 1e-6);
        assert_eq!(c.relative(1), Some(1.0));
    }

    #[test]
    fn curve_height_spans_floor_to_one() {
        let c = Curve::new(&[1.0, 2.0, 3.0]);
        assert!((c.height(0) - 1.0).abs() < 1e-6);
        assert!((c.height(2) - 0.08).abs() < 1e-6);
        assert!(c.height(1) > 0.08 && c.height(1) < 1.0);
        let flat = Curve::new(&[2.0, 2.0]);
        assert_eq!(flat.height(0), 0.5);
        assert!(Curve::new(&[]).is_empty());
    }

    #[test]
    fn curve_x_positions_and_threshold() {
        let c = Curve::new(&[4.0, 3.0, 2.0, 1.0]);
        assert_eq!(c.x_of_rank(0), 0.125);
        assert_eq!(c.x_of_rank(3), 0.875);
        assert_eq!(c.threshold_x(1), 0.25);
        assert_eq!(c.threshold_x(99), 1.0);
        assert_eq!(c.rank_at(0.0), Some(0));
        assert_eq!(c.rank_at(0.51), Some(2));
        assert_eq!(c.rank_at(1.5), Some(3));
        assert_eq!(Curve::new(&[]).rank_at(0.5), None);
        // The threshold edge sits between the last kept and first dropped rank.
        assert!(c.x_of_rank(0) < c.threshold_x(1) && c.threshold_x(1) < c.x_of_rank(1));
    }

    #[test]
    fn curve_points_fit_the_plot_and_are_thinned() {
        let scores: Vec<f32> = (0..1000).map(|i| i as f32).collect();
        let c = Curve::new(&scores);
        let plot = Rect::from_min_size(pos2(10.0, 20.0), vec2(200.0, 100.0));
        let pts = c.points(plot, 100);
        assert!(pts.len() <= 101 && pts.len() > 50);
        assert!(pts.iter().all(|p| plot.contains(*p)));
        assert!(pts.windows(2).all(|w| w[0].x < w[1].x && w[0].y <= w[1].y));
        assert_eq!(c.points(plot, 5000).len(), 1000);
        assert_eq!(Curve::new(&[]).points(plot, 10).len(), 0);
        let last = *pts.last().unwrap();
        assert_eq!(last, c.point(plot, 999));
    }

    #[test]
    fn non_finite_scores_do_not_poison_the_curve() {
        let c = Curve::new(&[f32::NAN, 1.0, 3.0]);
        assert!((0..3).all(|r| c.height(r).is_finite()));
        assert_eq!(c.relative(1).map(f32::is_finite), Some(true));
    }

    #[test]
    fn fit_zoom_picks_the_tighter_axis() {
        let z = fit_zoom(vec2(960.0, 540.0), vec2(720.0, 540.0));
        assert!((z - 0.96).abs() < 1e-6);
        let z = fit_zoom(vec2(400.0, 800.0), vec2(800.0, 400.0));
        assert!((z - 0.48).abs() < 1e-6);
        assert_eq!(fit_zoom(Vec2::ZERO, vec2(10.0, 10.0)), 1.0);
        assert_eq!(fit_zoom(vec2(10.0, 10.0), Vec2::ZERO), 1.0);
    }

    #[test]
    fn zoom_keeps_the_anchor_point_fixed() {
        let mut v = View::default();
        v.settle(vec2(800.0, 600.0), vec2(400.0, 300.0));
        let anchor = vec2(100.0, -50.0);
        let image_point = (anchor - v.pan) / v.zoom;
        v.zoom_about(anchor, 2.0);
        assert!(!v.fit);
        let after = (anchor - v.pan) / v.zoom;
        assert!((image_point - after).length() < 1e-4);
        assert!((v.zoom - 2.0 * fit_zoom(vec2(800.0, 600.0), vec2(400.0, 300.0))).abs() < 1e-5);
    }

    #[test]
    fn zoom_is_clamped_both_ways() {
        let mut v = View::default();
        v.zoom_about(Vec2::ZERO, 1e9);
        assert_eq!(v.zoom, ZOOM_RANGE.1);
        v.zoom_about(Vec2::ZERO, 1e-9);
        assert_eq!(v.zoom, ZOOM_RANGE.0);
    }

    #[test]
    fn pan_cannot_lose_the_image() {
        let mut v = View {
            zoom: 1.0,
            pan: vec2(1e6, -1e6),
            fit: false,
        };
        v.settle(vec2(800.0, 600.0), vec2(400.0, 300.0));
        let viewer = Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0));
        assert!(viewer.intersects(v.image_rect(viewer, vec2(400.0, 300.0))));
    }

    #[test]
    fn fit_mode_recentres_and_follows_the_window() {
        let mut v = View {
            pan: vec2(30.0, 30.0),
            ..View::default()
        };
        v.settle(vec2(500.0, 500.0), vec2(250.0, 250.0));
        assert_eq!(v.pan, Vec2::ZERO);
        assert_eq!(v.percent(), 192);
        v.settle(vec2(1000.0, 1000.0), vec2(250.0, 250.0));
        assert_eq!(v.percent(), 384);
        v.zoom_about(Vec2::ZERO, ZOOM_STEP);
        v.reset();
        assert!(v.fit);
    }

    #[test]
    fn ap_rects_scale_and_offset() {
        let r = ap_screen_rect(pos2(100.0, 50.0), 2.0, [10, 20, 74, 84]);
        assert_eq!(r.min, pos2(120.0, 90.0));
        assert_eq!(r.max, pos2(248.0, 218.0));
        assert_eq!(r.size(), vec2(128.0, 128.0));
    }

    #[test]
    fn labels_are_one_based() {
        assert_eq!(
            scrub_label(41, 100, Some(0.714), Some(17)),
            "Frame 42 / 100 \u{b7} quality 0.71 \u{b7} rank 18"
        );
        assert_eq!(scrub_label(0, 5, None, None), "Frame 1 / 5");
    }

    #[test]
    fn median_ignores_non_finite() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&[1.0, 2.0, f32::NAN, 4.0]), Some(2.0));
        assert_eq!(median(&[1.0, 3.0]), Some(2.0));
        assert_eq!(median(&[]), None);
    }

    #[test]
    fn describe_gives_container_and_depth() {
        assert_eq!(
            parse_describe("SER 720x540 16-bit, 100 frames"),
            ("SER".into(), Some(16))
        );
        assert_eq!(
            parse_describe("AVI 720x540 Gray8 (Y800), 100 frames"),
            ("AVI".into(), Some(8))
        );
        assert_eq!(
            parse_describe("AVI 8x8 Gray16 (Y16 ), 1 frames"),
            ("AVI".into(), Some(16))
        );
        assert_eq!(parse_describe("what"), ("what".into(), None));
    }

    #[test]
    fn capture_fields_skip_unknowns() {
        let mut m = CaptureMeta {
            file: "a.avi".into(),
            container: "AVI".into(),
            camera: Some("ZWO ASI482MC".into()),
            width: 720,
            height: 540,
            bits: Some(8),
            bayer: Some(BayerPattern::Rggb),
            frames: 100,
            video: false,
            note: None,
        };
        assert_eq!(
            m.fields(),
            [
                "a.avi",
                "ZWO ASI482MC",
                "720x540",
                "8-bit RGGB",
                "100 frames"
            ]
        );
        m.camera = None;
        m.bayer = None;
        m.bits = None;
        assert_eq!(m.fields(), ["a.avi", "720x540", "100 frames"]);
    }

    #[test]
    fn adapter_summary_splits() {
        let (n, a, sw) = split_adapter("NVIDIA GeForce RTX 3080 (Vulkan, DiscreteGpu)");
        assert_eq!(
            (n.as_str(), a.as_str(), sw),
            ("NVIDIA GeForce RTX 3080", "Vulkan", false)
        );
        let (n, a, sw) = split_adapter("Microsoft Basic Render Driver (Dx12, Cpu)");
        assert_eq!(
            (n.as_str(), a.as_str(), sw),
            ("Microsoft Basic Render Driver", "Dx12", true)
        );
        assert_eq!(
            split_adapter("plain"),
            ("plain".into(), String::new(), false)
        );
    }

    #[test]
    fn displayed_quality_never_rises_with_rank() {
        // Scores bunched between 0.85 and 1.0, as on a capture whose frames
        // are all rather alike: a ratio to the best would read 0.9 or more
        // for nearly every frame, including rank 694.
        let n = 1000;
        let scores: Vec<f32> = (0..n)
            .map(|i| 0.85 + 0.15 * (((i * 7919) % n) as f32 / (n - 1) as f32))
            .collect();
        let curve = Curve::new(&scores);
        let by_rank: Vec<f32> = (0..n)
            .map(|r| curve.relative(curve.frame_at_rank(r).unwrap()).unwrap())
            .collect();
        assert!(
            (by_rank[0] - 1.0).abs() < 1e-6,
            "rank 1 shows {}",
            by_rank[0]
        );
        assert!(
            by_rank[n - 1].abs() < 1e-6,
            "last rank shows {}",
            by_rank[n - 1]
        );
        assert!(
            by_rank.windows(2).all(|w| w[0] >= w[1]),
            "quality rose with rank"
        );
        // Rank 694 of 1000 sits well down the scale, not at 0.9.
        assert!(by_rank[693] < 0.4, "rank 694 shows {}", by_rank[693]);
        // The number and the curve use one scale.
        for r in [0, 100, 500, 999] {
            let f = curve.frame_at_rank(r).unwrap();
            let h = curve.height(r);
            let q = curve.relative(f).unwrap();
            assert!((h - (0.08 + 0.92 * q)).abs() < 1e-5, "{h} vs {q}");
        }
    }

    #[test]
    fn displayed_quality_of_identical_scores_is_full() {
        let curve = Curve::new(&[0.5; 4]);
        assert_eq!(curve.relative(2), Some(1.0));
    }

    #[test]
    fn truncate_counts_characters() {
        assert_eq!(truncate("abcdef", 6), "abcdef");
        assert_eq!(truncate("abcdef", 4), "abc\u{2026}");
        assert_eq!(truncate("\u{e9}\u{e9}\u{e9}", 2), "\u{e9}\u{2026}");
        // A cut that lands on a space does not leave it before the ellipsis.
        assert_eq!(truncate("NVIDIA A100 80GB", 13), "NVIDIA A100\u{2026}");
    }
}
