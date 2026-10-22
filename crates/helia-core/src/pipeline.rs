// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! End-to-end stacking: load, quality, global align, multipoint, stack.
//!
//! Frames are never all held in memory. The capture is streamed several
//! times in batches (about two frames per thread): once to score every frame,
//! once to register the candidates, and once or twice to stack. Memory is
//! bounded by the batch plus the accumulators.
//!
//! Samples combine as a quality-weighted mean ([`quality::soft_weights`]: a
//! smooth curve over the selected frames, not a cutoff); sigma clipping, a Huber step
//! and a winsorised mean are optional two-pass modes.
//!
//! Selection works in two tiers. The best `keep` frames by global quality are
//! the "kept" set used by the global-only path and as the multipoint fallback.
//! With multipoint enabled, the best `keep * candidate_factor` frames are
//! registered and each alignment point then stacks its own best `keep` of them.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rayon::prelude::*;

use crate::align::Shift;
use crate::align::apstats;
use crate::align::global::{PhaseCorrelator, PhaseOptions, build_reference};
use crate::align::limb::LimbReference;
use crate::align::method::{
    AlignMethod, AlignPlan, AlignReport, FrameAlign, GlobalMethod, phase_confidence,
};
use crate::align::multipoint::{
    ApGrid, ApMask, ApMeasure, ApTemplate, LocalTable, MultipointConfig, blend, fill_missing,
    measure,
};
use crate::align::refine::{self, SmoothConfig};
use crate::autoap::{self, ApChoice, AutoAp, PROBE_FRAMES, PROBE_SIZE, Seeing};
use crate::autocrop::{self, CropReport};
use crate::backend::{
    Accumulated, Accumulator, Backend, CpuBackend, DeviceStage, FrameDeposit, MeasureRequest,
    RegisterRequest, RobustAccumulator, ScoreRequest, StackRequest,
};
use crate::calibrate::{CalibratedSource, Calibration, Plan};
use crate::debayer::{Channel, Debayer};
use crate::error::{Error, Result};
use crate::field_rotation::{DerotatedSource, FieldRotation, Plan as FieldRotationPlan};
use crate::frame::{Frame, Rect};
use crate::io::solarnet::{ProvStep, pipeline_steps};
use crate::io::source::FrameSource;
use crate::limbdark::{self, Law, LimbFit};
use crate::mask::{self, Disc, MaskConfig, MaskedSource};
use crate::profile::{Category, Profile};
use crate::quality::{self, Keep, Metric};
use crate::resample::{Kernel, resample_region};
use crate::seeing::probe::BurstProbe;
use crate::seeing::{SAMPLE_FRAMES, SeeingReport, SeeingSettings};
use crate::stack::{Deposit, RobustKind, StackMode, TileStack};
use crate::truecolour::{self, ColourStack, TrueColour};

/// Pipeline phase, reported through the progress callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stage {
    /// Scoring every frame.
    Quality,
    /// Building the reference image.
    Reference,
    /// Registering candidate frames globally.
    GlobalAlign,
    /// Measuring alignment points.
    LocalAlign,
    /// Accumulating the stack.
    Stack,
    /// Blending and normalising.
    Finish,
}

impl Stage {
    /// Short label for progress displays.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Quality => "scoring frames",
            Self::Reference => "building reference",
            Self::GlobalAlign => "aligning frames",
            Self::LocalAlign => "measuring alignment points",
            Self::Stack => "stacking",
            Self::Finish => "finishing",
        }
    }
}

/// Everything configurable about a run.
#[derive(Clone, Debug)]
pub struct StackConfig {
    /// Frames to stack (globally, and per alignment point).
    pub keep: Keep,
    /// Multipoint registers `keep * candidate_factor` frames (at least 1).
    pub candidate_factor: f32,
    /// Frame ranking metric.
    pub metric: Metric,
    /// Region used for ranking, whole frame when `None`.
    pub roi: Option<Rect>,
    /// Best frames combined into the reference (their per-pixel median when
    /// there are three or more, so an artefact in one or two does not reach
    /// it).
    pub reference_frames: usize,
    /// Put back the scores that an artefact (a satellite trail, a line, hot
    /// pixels) has raised, judged against the frames either side in time, before
    /// ranking (see [`quality::robust_scores`]). On by default.
    pub robust_ranking: bool,
    /// Move the stack to another colour plane when the one named is clipped
    /// (see [`crate::clip`]). Off by default: set it only when the plane came
    /// from a preset, not from a choice the user made.
    pub clip_fallback: bool,
    /// Alignment point settings; `None` for a global-only stack.
    pub multipoint: Option<MultipointConfig>,
    /// How samples combine.
    pub mode: StackMode,
    /// Resampling kernel.
    pub kernel: Kernel,
    /// Drizzle scale; currently always an error (the deposit interface exists).
    pub drizzle: Option<f32>,
    /// Scale the result so its 99.99th percentile is 1.
    pub normalise: bool,
    /// Phase correlation tuning.
    pub phase: PhaseOptions,
    /// Bayer to mono conversion.
    pub debayer: Debayer,
    /// What the alignment keeps away from: burned-in text, exclusions, sky.
    pub mask: MaskConfig,
    /// How whole frames are aligned: the solar limb, phase correlation, or
    /// whichever suits the capture.
    pub align: AlignMethod,
    /// Dark, flat and bad-pixel masters applied to every frame as it is read,
    /// before it is demosaiced, scored or aligned. `None` for no calibration.
    pub calibration: Option<Arc<Calibration>>,
    /// Choose the alignment point size, spacing and search range from the
    /// image scale and the seeing instead of using `multipoint` as given.
    /// Ignored without multipoint.
    pub auto_ap: Option<AutoAp>,
    /// Crop the result to the solar disc plus this margin, in radii.
    pub autocrop: Option<f32>,
    /// Fit this limb-darkening law to the stack and report it (the stack
    /// itself is not changed). `None` skips the fit.
    pub limb_law: Option<Law>,
    /// Second alignment point pass and shift field smoothing. Ignored without
    /// multipoint.
    pub ap_refine: ApRefine,
    /// Stack the red, green and blue planes of a raw colour capture on one
    /// alignment and make a colour picture ([`crate::truecolour`]). The
    /// alignment then runs on the luminance whatever `debayer.channel` says;
    /// `image` of the result is the luminance of the colour picture.
    pub true_colour: Option<TrueColour>,
    /// Estimate the seeing from the capture (spectral-ratio r0 and an
    /// alignment point motion index) once the stack is done; the settings say
    /// what is known of the optics.
    pub seeing: Option<SeeingSettings>,
    /// Turn every frame to one orientation before anything else sees it: the
    /// field rotation of an altazimuth mount ([`crate::field_rotation`]).
    pub field_rotation: Option<FieldRotation>,
}

/// Refinement of the alignment point shifts (issue #4).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ApRefine {
    /// 1 measures the points against the reference; 2 stacks once, then
    /// measures them again against that stack, which has far less noise.
    pub passes: u8,
    /// Replace shifts that disagree with their neighbours; `None` keeps all.
    pub smooth: Option<SmoothConfig>,
    /// Measure the points once more against the finished stack and report
    /// how far the shifts that were used are from that ([`RefineReport::residual_rms`]).
    pub residual: bool,
}

impl Default for ApRefine {
    fn default() -> Self {
        Self {
            passes: 1,
            smooth: None,
            residual: false,
        }
    }
}

/// What the refinement did, see [`StackOutput::refine`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RefineReport {
    /// Passes run.
    pub passes: u8,
    /// Shifts replaced by the smoothing, summed over the passes.
    pub smoothed: usize,
    /// Shifts of the table that is stacked (frame, point pairs).
    pub measurements: usize,
    /// RMS change, in pixels, of the shifts the second pass made against the first.
    pub moved_rms: Option<f32>,
    /// RMS distance, in pixels, between the shifts used for the stack and the
    /// shifts measured against the finished stack; 0 would mean the stack
    /// agrees with every frame it was made from. Only with `residual`.
    pub residual_rms: Option<f32>,
}

impl RefineReport {
    /// One line for the command line and the log.
    #[must_use]
    pub fn summary(&self) -> String {
        use std::fmt::Write;
        let mut s = format!(
            "{} pass{}, {} shifts replaced by the smoothing of {} measured",
            self.passes,
            if self.passes == 1 { "" } else { "es" },
            self.smoothed,
            self.measurements
        );
        if let Some(m) = self.moved_rms {
            let _ = write!(s, "; the second pass moved the shifts {m:.3} px RMS");
        }
        if let Some(r) = self.residual_rms {
            let _ = write!(s, "; residual against the stack {r:.3} px RMS");
        }
        s
    }
}

impl Default for StackConfig {
    fn default() -> Self {
        Self {
            keep: Keep::default(),
            candidate_factor: 2.0,
            metric: Metric::default(),
            roi: None,
            reference_frames: 5,
            robust_ranking: true,
            clip_fallback: false,
            multipoint: Some(MultipointConfig::default()),
            mode: StackMode::Mean,
            kernel: Kernel::default(),
            drizzle: None,
            normalise: true,
            phase: PhaseOptions::default(),
            debayer: Debayer::default(),
            mask: MaskConfig::default(),
            align: AlignMethod::default(),
            calibration: None,
            auto_ap: None,
            autocrop: None,
            limb_law: Some(Law::default()),
            ap_refine: ApRefine::default(),
            true_colour: None,
            seeing: None,
            field_rotation: None,
        }
    }
}

/// Result of a run.
#[derive(Clone, Debug)]
pub struct StackOutput {
    /// The stacked image.
    pub image: Frame,
    /// Reference image the stack is registered to.
    pub reference: Frame,
    /// Quality score of every frame, as ranked: with `robust_ranking`, a score
    /// an artefact raised is replaced by its neighbours' level.
    pub scores: Vec<f32>,
    /// Frames whose score was replaced, with the score they measured.
    pub score_outliers: Vec<(usize, f32)>,
    /// All frame indices, best first.
    pub ranking: Vec<usize>,
    /// Frames in the kept set, best first.
    pub kept: Vec<usize>,
    /// Global shift of each registered candidate (source index, shift).
    pub shifts: Vec<(usize, Shift)>,
    /// Alignment point grid when multipoint ran.
    pub grid: Option<ApGrid>,
    /// Rectangles that were flattened in every frame: the user's exclusions
    /// and any burned-in text that was detected.
    pub masked: Vec<Rect>,
    /// The fitted solar disc, when the limb fit ran and found one. It is in the
    /// coordinates of `image`, so after a crop it is the disc fitted to the
    /// stack, moved with the crop.
    pub disc: Option<Disc>,
    /// What the automatic crop did; `None` when it was off or skipped.
    pub crop: Option<CropReport>,
    /// Why the automatic crop left the image alone, when it was on.
    pub crop_skipped: Option<autocrop::Skipped>,
    /// What the automatic alignment points chose, when they were on.
    pub auto_ap: Option<ApChoice>,
    /// The limb-darkening law fitted to the stack, when there is a disc. Its
    /// centre is in the coordinates of `image`, so after a crop it moves with
    /// the crop.
    pub limb: Option<LimbFit>,
    /// What the second pass and smoothing did, when multipoint ran.
    pub refine: Option<RefineReport>,
    /// What each alignment point measured over the registered frames (lock
    /// rate, sharpness, displacement from the global shift and, with
    /// `ap_refine.residual`, the residual against the stack), when multipoint ran.
    pub ap_stats: Option<apstats::ApStats>,
    /// The colour picture, when true colour was asked for. `image` is its
    /// luminance.
    pub colour: Option<ColourStack>,
    /// The seeing estimated from the capture, when asked for and measurable.
    pub seeing: Option<SeeingReport>,
    /// The colour plane that was stacked.
    pub channel: Channel,
    /// Why the plane differs from the one asked for (it was clipped), or that
    /// no plane is usable; `None` when nothing needs saying.
    pub channel_note: Option<String>,
    /// What the field rotation correction did, when it ran.
    pub field_rotation: Option<FieldRotationPlan>,
    /// The processing steps that ran, in order, for FITS provenance.
    pub provenance: Vec<ProvStep>,
    /// One line per calibration master applied (and any caveat), for the
    /// history of a TIFF or PNG and the `HISTORY` cards of a FITS file.
    pub calibration_notes: Vec<String>,
    /// How whole frames were aligned, and how confident each method was.
    pub alignment: AlignReport,
    /// Backend name.
    pub backend: String,
    /// Wall time per stage.
    pub timings: Vec<(Stage, Duration)>,
    /// Busy time summed over all threads, per kind of work.
    pub busy: Vec<(Category, Duration)>,
}

/// Progress reporting and cancellation shared by the stages.
struct Ctx<'a> {
    progress: &'a dyn Fn(Stage, f32),
    cancel: &'a AtomicBool,
    profile: &'a Profile,
}

impl Ctx<'_> {
    fn check(&self) -> Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }

    fn report(&self, stage: Stage, fraction: f32) {
        (self.progress)(stage, fraction.clamp(0.0, 1.0));
    }
}

/// Wall time per stage. Stages that interleave per batch add their own parts.
struct Timeline {
    timings: Vec<(Stage, Duration)>,
    clock: Instant,
}

impl Timeline {
    fn new() -> Self {
        Self {
            timings: Vec::new(),
            clock: Instant::now(),
        }
    }

    /// Close the stage that has been running since the last lap.
    fn lap(&mut self, stage: Stage) {
        self.timings.push((stage, self.clock.elapsed()));
        self.clock = Instant::now();
    }

    /// Record a stage whose time was summed from parts, and restart the clock.
    fn record(&mut self, stage: Stage, d: Duration) {
        self.timings.push((stage, d));
        self.clock = Instant::now();
    }
}

/// What stacking a plane needs from the alignment: shared by every plane of a
/// true colour run.
struct StackStage<'a> {
    cand: &'a [usize],
    keep_n: usize,
    shifts: &'a [Shift],
    scores: &'a [f32],
    reference: &'a Frame,
    points: Option<&'a PointSet>,
    table: Option<&'a LocalTable>,
}

/// Alignment point layout and the reference patches, built once per run.
struct PointSet {
    grid: ApGrid,
    templates: Vec<Option<ApTemplate>>,
    config: MultipointConfig,
}

/// What `register` needs besides the frames: the alignment points to measure
/// (if any) and how whole frames are aligned.
struct RegisterPlan<'a> {
    points: Option<&'a PointSet>,
    align: &'a AlignPlan,
}

/// Aligns one frame to the reference: the limb fit when the plan says so, with
/// phase correlation as the fallback and the only method otherwise.
struct Aligner<'a> {
    phase: PhaseCorrelator,
    limb: Option<&'a LimbReference>,
}

impl Aligner<'_> {
    fn estimate(&self, f: &Frame) -> Result<FrameAlign> {
        if let Some(l) = self.limb
            && let Some((shift, fit)) = l.estimate(f)
        {
            return Ok(FrameAlign {
                shift,
                confidence: fit.confidence,
                used: GlobalMethod::Limb,
                fell_back: false,
            });
        }
        let e = self.phase.estimate(f)?;
        Ok(FrameAlign {
            shift: e.shift,
            confidence: phase_confidence(e.psr),
            used: GlobalMethod::Phase,
            fell_back: self.limb.is_some(),
        })
    }
}

/// What the CPU registration pass produced.
struct CpuRegistration {
    shifts: Vec<Shift>,
    table: Option<LocalTable>,
    global_wall: Duration,
    local_wall: Duration,
    /// Per-frame alignment details, when the global shifts were found here.
    aligns: Option<Vec<FrameAlign>>,
}

/// Configured stacker.
pub struct Pipeline {
    cfg: StackConfig,
    backend: Arc<dyn Backend>,
}

fn batch_size() -> usize {
    (rayon::current_num_threads() * 2).max(4)
}

impl Pipeline {
    /// Pipeline on the CPU backend.
    #[must_use]
    pub fn new(cfg: StackConfig) -> Self {
        Self {
            cfg,
            backend: Arc::new(CpuBackend),
        }
    }

    /// Use another backend for the stages it implements (scoring,
    /// registration, alignment points, stacking); the CPU does the rest.
    #[must_use]
    pub fn with_backend(mut self, backend: Arc<dyn Backend>) -> Self {
        self.backend = backend;
        self
    }

    /// Configuration in use.
    #[must_use]
    pub fn config(&self) -> &StackConfig {
        &self.cfg
    }

    /// Read one frame as mono, for previews.
    ///
    /// # Errors
    /// Source errors.
    pub fn read_mono(&self, src: &mut dyn FrameSource, index: usize) -> Result<Frame> {
        Ok(self.cfg.debayer.apply_owned(src.read(index)?))
    }

    /// Load frames as mono: concurrently when the source allows it, otherwise
    /// read sequentially and demosaic in parallel.
    fn load(&self, src: &mut dyn FrameSource, indices: &[usize], ctx: &Ctx) -> Result<Vec<Frame>> {
        let (w, h) = src.dimensions();
        if let Some(shared) = src.shared() {
            // Every thread reads and demosaics its own frame straight from the file.
            let (d, prof) = (self.cfg.debayer, ctx.profile);
            return indices
                .par_iter()
                .map(|&i| shared.read_mono(i, &d, prof))
                .collect();
        }
        let raw = ctx.profile.time(Category::Decode, || {
            indices
                .iter()
                .map(|&i| src.read(i))
                .collect::<Result<Vec<_>>>()
        })?;
        if raw.iter().any(|f| f.width != w || f.height != h) {
            return Err(Error::Invalid(
                "frames in the capture differ in size".into(),
            ));
        }
        let d = self.cfg.debayer;
        let prof = ctx.profile;
        Ok(raw
            .into_par_iter()
            .map(|f| prof.time(Category::Debayer, || d.apply_owned(f)))
            .collect())
    }

    fn sharpness(&self, frame: &Frame, prof: &Profile) -> Result<f32> {
        prof.time(Category::Quality, || self.sharpness_inner(frame))
    }

    fn sharpness_inner(&self, frame: &Frame) -> Result<f32> {
        match self.backend.sharpness(frame, self.cfg.metric, self.cfg.roi) {
            Err(Error::Unsupported(_)) => Ok(quality::score(frame, self.cfg.metric, self.cfg.roi)),
            other => other,
        }
    }

    /// Score every frame of `src`.
    ///
    /// # Errors
    /// Source or backend failures, or cancellation.
    pub fn score(
        &self,
        src: &mut dyn FrameSource,
        progress: &dyn Fn(Stage, f32),
        cancel: &AtomicBool,
    ) -> Result<Vec<f32>> {
        let profile = Profile::new();
        let ctx = Ctx {
            progress,
            cancel,
            profile: &profile,
        };
        let plan = self.calibration_plan(src)?;
        let mut calibrated;
        let src: &mut dyn FrameSource = match plan {
            Some(plan) => {
                calibrated = CalibratedSource::new(src, plan)?;
                &mut calibrated
            }
            None => src,
        };
        let (rects, level) = self.plan_mask(src, &ctx)?;
        let mut masked;
        let src: &mut dyn FrameSource = if rects.is_empty() {
            src
        } else {
            masked = MaskedSource::new(src, rects, level);
            &mut masked
        };
        let rotation_plan = self
            .cfg
            .field_rotation
            .as_ref()
            .map(FieldRotation::plan)
            .transpose()?;
        let mut derotated;
        let src: &mut dyn FrameSource = match &rotation_plan {
            Some(plan) => {
                derotated = DerotatedSource::new(src, plan, self.cfg.debayer)?;
                &mut derotated
            }
            None => src,
        };
        let scores = self.score_ctx(src, &ctx);
        self.release_device_frames(src);
        scores
    }

    /// Tell the backend the frames of `src` are no longer in use, so device
    /// copies of them (and the wrappers' over them, which die with the
    /// wrappers) are freed rather than held while the caller keeps the
    /// source open.
    fn release_device_frames(&self, src: &mut dyn FrameSource) {
        if let Some(shared) = src.shared() {
            self.backend.release_frames(shared);
        }
    }

    /// The calibration plan for `src`, when the configuration has masters.
    fn calibration_plan(&self, src: &mut dyn FrameSource) -> Result<Option<Plan>> {
        let Some(cal) = self.cfg.calibration.as_ref().filter(|c| !c.is_empty()) else {
            return Ok(None);
        };
        let (w, h) = src.dimensions();
        let pattern = src.read(0).ok().and_then(|f| f.bayer);
        cal.plan(w, h, pattern).map(Some)
    }

    /// Rectangles to flatten (the user's exclusions plus detected burned-in
    /// text) and the level to flatten them to.
    fn plan_mask(&self, src: &mut dyn FrameSource, ctx: &Ctx) -> Result<(Vec<Rect>, f32)> {
        let (w, h) = src.dimensions();
        let n = src.frame_count();
        let mut rects: Vec<Rect> = self
            .cfg
            .mask
            .exclude
            .iter()
            .map(|r| r.clamp(w, h))
            .filter(|r| !r.is_empty())
            .collect();
        if !self.cfg.mask.overlay && rects.is_empty() || n == 0 {
            return Ok((rects, 0.0));
        }
        let want = if self.cfg.mask.overlay {
            mask::OVERLAY_SAMPLES.min(n)
        } else {
            n.min(3)
        };
        let idx = mask::sample_indices(n, want);
        let frames = self.load(src, &idx, ctx)?;
        if self.cfg.mask.overlay {
            rects.extend(mask::detect_overlay(&frames));
        }
        let level = mask::fill_level(&frames, &rects);
        Ok((rects, level))
    }

    fn score_ctx(&self, src: &mut dyn FrameSource, ctx: &Ctx) -> Result<Vec<f32>> {
        let n = src.frame_count();
        let all: Vec<usize> = (0..n).collect();
        if let Some(shared) = src
            .shared()
            .filter(|_| self.backend.supports(DeviceStage::Score))
        {
            let report = |f: f32| ctx.report(Stage::Quality, f);
            let req = ScoreRequest {
                frames: shared,
                indices: &all,
                debayer: self.cfg.debayer,
                metric: self.cfg.metric,
                roi: self.cfg.roi,
                progress: &report,
                cancel: ctx.cancel,
                profile: ctx.profile,
            };
            if let Some(scores) = self.backend.score_frames(&req)? {
                ctx.report(Stage::Quality, 1.0);
                return Ok(scores);
            }
        }
        let mut scores = Vec::with_capacity(n);
        for chunk in all.chunks(batch_size()) {
            ctx.check()?;
            let frames = self.load(src, chunk, ctx)?;
            let prof = ctx.profile;
            let part: Vec<f32> = frames
                .par_iter()
                .map(|f| self.sharpness(f, prof))
                .collect::<Result<_>>()?;
            scores.extend(part);
            ctx.report(Stage::Quality, scores.len() as f32 / n as f32);
        }
        Ok(scores)
    }

    /// Run the whole pipeline.
    ///
    /// # Errors
    /// Source, backend or configuration failures, or cancellation.
    pub fn run(
        &self,
        src: &mut dyn FrameSource,
        progress: &dyn Fn(Stage, f32),
        cancel: &AtomicBool,
    ) -> Result<StackOutput> {
        if self.cfg.true_colour.is_some() && self.cfg.field_rotation.is_some() {
            return Err(Error::Invalid(
                "field rotation cannot be corrected in true colour: the mosaic cannot be turned before it is demosaiced; stack one colour plane instead".into(),
            ));
        }
        // True colour aligns on the luminance, whatever channel was named.
        if self.cfg.true_colour.is_some() && self.cfg.debayer.channel != Channel::Luminance {
            return self
                .with_channel(Channel::Luminance)
                .run_impl(src, progress, cancel);
        }
        if self.cfg.clip_fallback && self.cfg.true_colour.is_none() {
            let choice = crate::clip::choose_channel(self.cfg.debayer.channel, src);
            let mut out = if choice.channel == self.cfg.debayer.channel {
                self.run_impl(src, progress, cancel)?
            } else {
                self.with_channel(choice.channel)
                    .run_impl(src, progress, cancel)?
            };
            out.channel_note = choice.note;
            return Ok(out);
        }
        self.run_impl(src, progress, cancel)
    }

    /// This pipeline with another colour plane to stack.
    fn with_channel(&self, channel: Channel) -> Pipeline {
        Pipeline {
            cfg: StackConfig {
                debayer: Debayer {
                    channel,
                    ..self.cfg.debayer
                },
                ..self.cfg.clone()
            },
            backend: Arc::clone(&self.backend),
        }
    }

    /// [`Self::run_stages`], then free what the backend kept on the device.
    fn run_impl(
        &self,
        src: &mut dyn FrameSource,
        progress: &dyn Fn(Stage, f32),
        cancel: &AtomicBool,
    ) -> Result<StackOutput> {
        let out = self.run_stages(&mut *src, progress, cancel);
        self.release_device_frames(src);
        out
    }

    #[allow(clippy::too_many_lines, reason = "one flat sequence of stages")]
    fn run_stages(
        &self,
        src: &mut dyn FrameSource,
        progress: &dyn Fn(Stage, f32),
        cancel: &AtomicBool,
    ) -> Result<StackOutput> {
        let profile = Profile::new();
        let ctx = Ctx {
            progress,
            cancel,
            profile: &profile,
        };
        let cfg = &self.cfg;
        self.deposit().validate()?;
        let n = src.frame_count();
        if n == 0 {
            return Err(Error::Invalid("capture has no frames".into()));
        }
        let (w, h) = src.dimensions();
        let bayer = src.read(0).is_ok_and(|f| f.bayer.is_some());
        if cfg.true_colour.is_some() && !bayer {
            return Err(Error::Invalid(
                "true colour needs a raw colour capture (a Bayer mosaic); this one is mono or already demosaiced".into(),
            ));
        }
        let mut tl = Timeline::new();

        // Calibration comes first: everything below sees calibrated frames.
        let plan = self.calibration_plan(src)?;
        let mut calibration_notes = Vec::new();
        if let Some(c) = self.cfg.calibration.as_ref().filter(|_| plan.is_some()) {
            calibration_notes = c.note_lines();
        }
        if let Some(p) = &plan {
            calibration_notes.extend(p.notes.iter().cloned());
        }
        let mut calibrated;
        let src: &mut dyn FrameSource = match plan {
            Some(plan) => {
                calibrated = CalibratedSource::new(src, plan)?;
                &mut calibrated
            }
            None => src,
        };

        // Burned-in text and user exclusions are flattened in every frame the
        // stages below read.
        let (masked_rects, level) = self.plan_mask(src, &ctx)?;
        let mut masked_src;
        let src: &mut dyn FrameSource = if masked_rects.is_empty() {
            src
        } else {
            masked_src = MaskedSource::new(src, masked_rects.clone(), level);
            &mut masked_src
        };

        // The field rotation of an altazimuth mount is undone last of the
        // per-frame corrections: the mask rectangles are in the capture's
        // coordinates, and everything below wants one orientation.
        let rotation_plan = cfg
            .field_rotation
            .as_ref()
            .map(FieldRotation::plan)
            .transpose()?;
        let mut derotated;
        let src: &mut dyn FrameSource = match &rotation_plan {
            Some(plan) => {
                derotated = DerotatedSource::new(src, plan, cfg.debayer)?;
                &mut derotated
            }
            None => src,
        };

        // Quality.
        let measured = self.score_ctx(src, &ctx)?;
        let (scores, outliers) = if cfg.robust_ranking {
            quality::robust_scores(&measured)
        } else {
            (measured.clone(), Vec::new())
        };
        let score_outliers: Vec<(usize, f32)> =
            outliers.iter().map(|&i| (i, measured[i])).collect();
        let ranking = quality::rank(&scores);
        tl.lap(Stage::Quality);
        let keep_n = cfg.keep.count(n);
        let cand_n = if cfg.multipoint.is_some() {
            ((keep_n as f32 * cfg.candidate_factor.max(1.0)).ceil() as usize).clamp(keep_n, n)
        } else {
            keep_n
        };
        let cand: Vec<usize> = ranking[..cand_n].to_vec();

        // Reference.
        ctx.report(Stage::Reference, 0.0);
        let ref_n = cfg.reference_frames.clamp(1, keep_n);
        let ref_frames = self.load(src, &cand[..ref_n], &ctx)?;
        let refs: Vec<&Frame> = ref_frames.iter().collect();
        let reference = ctx.profile.time(Category::GlobalAlign, || {
            build_reference(&refs, cfg.phase, cfg.kernel)
        })?;
        drop(ref_frames);
        ctx.report(Stage::Reference, 1.0);
        tl.lap(Stage::Reference);

        // Global registration of the candidates and, with multipoint, the
        // local shifts and sharpness of every alignment point in them.
        let disc = cfg.mask.disc.then(|| mask::fit_disc(&reference)).flatten();
        let ap_mask = ApMask {
            disc,
            band: cfg.mask.prominence_band as f32,
            exclude: masked_rects.clone(),
        };
        let mut auto_ap: Option<ApChoice> = None;
        let multipoint = match (cfg.multipoint, cfg.auto_ap) {
            (Some(mp), Some(auto)) => {
                let scale = disc
                    .or_else(|| mask::fit_disc(&reference))
                    .and_then(|d| autoap::arcsec_per_px(d.r));
                let seeing = self.probe_seeing(src, &cand, &reference, &ap_mask, &ctx)?;
                let choice = autoap::choose(auto, scale, seeing);
                auto_ap = Some(choice);
                Some(choice.apply(mp))
            }
            (mp, _) => mp,
        };
        let points = multipoint
            .map(|mp| -> Result<PointSet> {
                let grid = ApGrid::build_masked(&reference, &mp, &ap_mask)?;
                let templates = grid.templates(&reference);
                Ok(PointSet {
                    grid,
                    templates,
                    config: mp,
                })
            })
            .transpose()?;
        let plan = AlignPlan::choose(cfg.align, &reference)?;
        let reg = RegisterPlan {
            points: points.as_ref(),
            align: &plan,
        };
        let (shifts, mut table, alignment) =
            self.register(src, &cand, &reference, &reg, None, &ctx, &mut tl)?;
        let mut refine_report = None;

        let (image, grid, mut colour, ap_stats) =
            if let (Some(ps), Some(first)) = (&points, table.as_mut()) {
                let mut report = RefineReport {
                    passes: 1,
                    ..RefineReport::default()
                };
                if let Some(sc) = cfg.ap_refine.smooth {
                    report.smoothed += refine::smooth_table(&ps.grid, first, &sc);
                }
                let mut second: Option<(PointSet, LocalTable)> = None;
                if cfg.ap_refine.passes >= 2 {
                    // Stack once, and measure the points again against that stack.
                    let (stack, _) = self.stack_multipoint(
                        src, &cand, keep_n, &shifts, &scores, &reference, ps, first, &ctx,
                    )?;
                    let ps2 = PointSet {
                        grid: ps.grid.clone(),
                        templates: ps.grid.templates(&stack),
                        config: ps.config,
                    };
                    let reg2 = RegisterPlan {
                        points: Some(&ps2),
                        align: &plan,
                    };
                    let (_, t2, _) =
                        self.register(src, &cand, &stack, &reg2, Some(&shifts), &ctx, &mut tl)?;
                    if let Some(mut t2) = t2 {
                        if let Some(sc) = cfg.ap_refine.smooth {
                            report.smoothed += refine::smooth_table(&ps2.grid, &mut t2, &sc);
                        }
                        report.passes = 2;
                        report.moved_rms = moved_rms(first, &t2);
                        second = Some((ps2, t2));
                    }
                }
                report.measurements = first.quality.iter().filter(|q| q.is_finite()).count();
                refine_report = Some(report);
                let (ps, table) = second.as_ref().map_or((ps, &*first), |(p, t)| (p, t));
                let stage = StackStage {
                    cand: &cand,
                    keep_n,
                    shifts: &shifts,
                    scores: &scores,
                    reference: &reference,
                    points: Some(ps),
                    table: Some(table),
                };
                let (img, grid, colour) = self.stack_planes(src, &stage, &masked_rects, &ctx)?;
                let mut stats = apstats::ap_stats(&ps.grid, table, &shifts, keep_n);
                if cfg.ap_refine.residual {
                    let ps3 = PointSet {
                        grid: ps.grid.clone(),
                        templates: ps.grid.templates(&img),
                        config: ps.config,
                    };
                    let reg3 = RegisterPlan {
                        points: Some(&ps3),
                        align: &plan,
                    };
                    let (_, t3, _) =
                        self.register(src, &cand, &img, &reg3, Some(&shifts), &ctx, &mut tl)?;
                    report.residual_rms = t3.as_ref().and_then(|t3| moved_rms(table, t3));
                    if let Some(t3) = &t3 {
                        stats.add_residual(table, t3);
                    }
                    refine_report = Some(report);
                }
                (img, grid, colour, Some(stats))
            } else {
                let stage = StackStage {
                    cand: &cand,
                    keep_n,
                    shifts: &shifts,
                    scores: &scores,
                    reference: &reference,
                    points: None,
                    table: None,
                };
                let (img, grid, colour) = self.stack_planes(src, &stage, &masked_rects, &ctx)?;
                (img, grid, colour, None)
            };
        tl.lap(Stage::Stack);

        let seeing = match &cfg.seeing {
            Some(settings) => {
                self.probe_burst(src, &reference, &ap_mask, disc.map(|d| d.r), settings, &ctx)?
            }
            None => None,
        };

        ctx.report(Stage::Finish, 0.0);
        let mut image = image;
        if cfg.normalise && colour.is_none() {
            ctx.profile.time(Category::Finish, || normalise(&mut image));
        }
        let mut limb = cfg
            .limb_law
            .and_then(|law| limbdark::fit_image(&image, law));
        let (mut disc, mut crop, mut crop_skipped) = (disc, None, None);
        if let Some(margin) = cfg.autocrop {
            match autocrop::autocrop(&image, margin, disc) {
                Ok((cropped, report)) => {
                    image = cropped;
                    colour = colour.map(|c| c.cut(report.rect));
                    disc = Some(report.disc);
                    // The fit's centre is in the uncropped frame; move it with the crop.
                    if let Some(l) = &mut limb {
                        l.cx -= report.rect.x0 as f32;
                        l.cy -= report.rect.y0 as f32;
                    }
                    crop = Some(report);
                }
                Err(why) => crop_skipped = Some(why),
            }
        }
        ctx.report(Stage::Finish, 1.0);
        tl.lap(Stage::Finish);
        debug_assert!(crop.is_some() || (image.width, image.height) == (w, h));
        // The provenance names the settings that ran, not the ones asked for.
        let mut ran = cfg.clone();
        ran.multipoint = multipoint;
        let mut provenance = pipeline_steps(
            &ran,
            bayer,
            n,
            keep_n,
            masked_rects.len(),
            disc.is_some(),
            &alignment,
        );
        if let Some(p) = &rotation_plan {
            provenance.insert(
                provenance.len().min(1),
                ProvStep::new(
                    "FIELD-ROTATION",
                    "helia-core field_rotation",
                    format!(
                        "altazimuth parallactic angle, {:.2} deg over the capture, frames turned by up to {:.2} deg to {}",
                        p.span_deg,
                        p.max_turn_deg,
                        crate::io::solarnet::iso_from_unix(p.reference_unix)
                    ),
                ),
            );
        }
        if let Some(c) = &colour {
            provenance.push(ProvStep::new(
                "WHITE-BALANCE",
                "helia-core truecolour",
                format!(
                    "gains {:.3}, {:.3}, {:.3}",
                    c.gains[0], c.gains[1], c.gains[2]
                ),
            ));
            if let Some([cr, cb]) = c.channels {
                provenance.push(ProvStep::new(
                    "CHANNEL-ALIGNMENT",
                    "helia-core truecolour",
                    format!(
                        "red ({:+.2}, {:+.2}) px{}, blue ({:+.2}, {:+.2}) px{} onto green",
                        cr.shift.dx,
                        cr.shift.dy,
                        if cr.applied { "" } else { " (not applied)" },
                        cb.shift.dx,
                        cb.shift.dy,
                        if cb.applied { "" } else { " (not applied)" },
                    ),
                ));
            }
        }
        if let Some(l) = &limb {
            provenance.push(ProvStep::new(
                "LIMB-DARKENING",
                "helia-core limbdark",
                format!("{} law; {}", l.law.id(), l.coefficients_text()),
            ));
        }
        if let Some(c) = &crop {
            provenance.push(ProvStep::new(
                "CROPPING",
                "helia-core autocrop",
                format!(
                    "disc plus {:.0}% margin; {}x{} from ({}, {})",
                    c.margin * 100.0,
                    c.rect.width(),
                    c.rect.height(),
                    c.rect.x0,
                    c.rect.y0
                ),
            ));
        }
        Ok(StackOutput {
            image,
            reference,
            scores,
            score_outliers,
            kept: ranking[..keep_n].to_vec(),
            ranking,
            shifts: cand.iter().copied().zip(shifts).collect(),
            grid,
            provenance,
            calibration_notes,
            masked: masked_rects,
            disc,
            crop,
            crop_skipped,
            auto_ap,
            limb,
            refine: refine_report,
            ap_stats,
            colour,
            seeing,
            channel: self.cfg.debayer.channel,
            channel_note: None,
            field_rotation: rotation_plan,
            alignment,
            backend: self.backend.name(),
            timings: tl.timings,
            busy: ctx.profile.snapshot(),
        })
    }

    /// Follow tiles of the disc through a sample of every frame of the capture,
    /// spread evenly, and estimate the seeing from them. `None` when the disc
    /// has too few tiles with enough detail.
    fn probe_burst(
        &self,
        src: &mut dyn FrameSource,
        reference: &Frame,
        ap_mask: &ApMask,
        disc_radius: Option<f32>,
        settings: &SeeingSettings,
        ctx: &Ctx,
    ) -> Result<Option<SeeingReport>> {
        let Some(mut probe) = BurstProbe::new(reference, ap_mask, self.cfg.phase)? else {
            return Ok(None);
        };
        let n = src.frame_count();
        let ids = mask::sample_indices(n, SAMPLE_FRAMES.min(n));
        for chunk in ids.chunks(batch_size()) {
            ctx.check()?;
            let frames = self.load(src, chunk, ctx)?;
            probe.add(&frames)?;
        }
        let scale = disc_radius.and_then(autoap::arcsec_per_px);
        let probed = probe.finish();
        let mut report = crate::seeing::report(&probed, scale, settings);
        report.series = ids
            .iter()
            .zip(&probed.series)
            .filter_map(|(&frame, s)| s.map(|rms_px| crate::seeing::MotionSample { frame, rms_px }))
            .collect();
        Ok(Some(report))
    }

    /// Measure the local warp of the seeing on a sample of the candidates, to
    /// choose alignment points from. `None` when too few points lock.
    fn probe_seeing(
        &self,
        src: &mut dyn FrameSource,
        cand: &[usize],
        reference: &Frame,
        ap_mask: &ApMask,
        ctx: &Ctx,
    ) -> Result<Option<Seeing>> {
        let ids: Vec<usize> = mask::sample_indices(cand.len(), PROBE_FRAMES)
            .into_iter()
            .map(|k| cand[k])
            .collect();
        let probe = MultipointConfig {
            ap_size: PROBE_SIZE,
            search_radius: autoap::MAX_SEARCH,
            ..MultipointConfig::default()
        };
        let grid = ApGrid::build_masked(reference, &probe, ap_mask)?;
        let templates = grid.templates(reference);
        let corr = PhaseCorrelator::new(reference, self.cfg.phase)?;
        let frames = self.load(src, &ids, ctx)?;
        let rows: Vec<Vec<ApMeasure>> = frames
            .par_iter()
            .map(|f| -> Result<Vec<ApMeasure>> {
                let g = corr.estimate(f)?.shift;
                let prep = grid.prepare(f);
                Ok(templates
                    .iter()
                    .filter_map(|t| t.as_ref().and_then(|t| measure(t, &prep, g, &probe)))
                    .collect())
            })
            .collect::<Result<_>>()?;
        Ok(Seeing::from_rows(&rows))
    }

    /// The stack of a run: one plane, or in true colour the red, green and
    /// blue planes on the shared alignment, composed into a colour picture
    /// (`masked` rectangles made grey). Returns the image (the luminance for
    /// colour), the alignment point grid and the colour picture.
    fn stack_planes(
        &self,
        src: &mut dyn FrameSource,
        st: &StackStage,
        masked: &[Rect],
        ctx: &Ctx,
    ) -> Result<(Frame, Option<ApGrid>, Option<ColourStack>)> {
        let Some(tc) = self.cfg.true_colour else {
            let (image, grid) = self.stack_plane(src, st, ctx)?;
            return Ok((image, grid, None));
        };
        let mut planes = Vec::with_capacity(3);
        let mut grid = None;
        for (k, channel) in [Channel::Red, Channel::Green, Channel::Blue]
            .into_iter()
            .enumerate()
        {
            let third = |stage: Stage, f: f32| {
                let f = if stage == Stage::Stack {
                    (k as f32 + f) / 3.0
                } else {
                    f
                };
                ctx.report(stage, f);
            };
            let sub = Ctx {
                progress: &third,
                cancel: ctx.cancel,
                profile: ctx.profile,
            };
            let (plane, g) = self.with_channel(channel).stack_plane(src, st, &sub)?;
            grid = g;
            planes.push(plane);
        }
        let planes: [Frame; 3] = planes.try_into().expect("three planes");
        ctx.report(Stage::Finish, 0.0);
        let mut stack = ctx.profile.time(Category::Finish, || {
            truecolour::compose(planes, &tc, self.cfg.normalise)
        })?;
        stack.neutralise(masked);
        let lum = truecolour::rgb_luminance(&stack.rgb)?;
        Ok((lum, grid, Some(stack)))
    }

    /// Stack the plane this pipeline is set to (`debayer.channel`) with an
    /// alignment already worked out: the multipoint stack when there is a
    /// point table, else the global one.
    fn stack_plane(
        &self,
        src: &mut dyn FrameSource,
        st: &StackStage,
        ctx: &Ctx,
    ) -> Result<(Frame, Option<ApGrid>)> {
        if let (Some(ps), Some(table)) = (st.points, st.table) {
            let (img, grid) = self.stack_multipoint(
                src,
                st.cand,
                st.keep_n,
                st.shifts,
                st.scores,
                st.reference,
                ps,
                table,
                ctx,
            )?;
            Ok((img, Some(grid)))
        } else {
            let kept_scores: Vec<f32> =
                st.cand[..st.keep_n].iter().map(|&i| st.scores[i]).collect();
            let weights = quality::soft_weights(&kept_scores);
            Ok((
                self.stack_global(
                    src,
                    &st.cand[..st.keep_n],
                    st.shifts,
                    &weights,
                    st.reference,
                    ctx,
                )?,
                None,
            ))
        }
    }

    /// Global shifts of `cand` against `reference` and, when `plan.points` is
    /// set, the alignment point table, plus a report of how the global shifts
    /// were found. The backend takes the parts it implements; the CPU does the
    /// rest in one pass over the frames. The limb fit always runs on the CPU.
    /// `fixed` gives the global shifts, which are then not estimated again.
    #[allow(clippy::too_many_arguments)] // internal stage; a context struct would only rename the arguments
    fn register(
        &self,
        src: &mut dyn FrameSource,
        cand: &[usize],
        reference: &Frame,
        plan: &RegisterPlan,
        fixed: Option<&[Shift]>,
        ctx: &Ctx,
        tl: &mut Timeline,
    ) -> Result<(Vec<Shift>, Option<LocalTable>, AlignReport)> {
        let cfg = &self.cfg;
        let (points, align) = (plan.points, plan.align);
        let mut shifts: Option<Vec<Shift>> = fixed.map(<[Shift]>::to_vec);
        let mut table: Option<LocalTable> = None;
        let mut measured: Option<Vec<FrameAlign>> = None;
        let (mut global_wall, mut local_wall) = (Duration::ZERO, Duration::ZERO);
        let device_points = points.is_some()
            && src.shared().is_some()
            && self.backend.supports(DeviceStage::Points);
        if shifts.is_none()
            && align.method == GlobalMethod::Phase
            && self.backend.supports(DeviceStage::Register)
            && let Some(shared) = src.shared()
        {
            let t = Instant::now();
            let report = |f: f32| ctx.report(Stage::GlobalAlign, f);
            shifts = self.backend.register_frames(&RegisterRequest {
                frames: shared,
                indices: cand,
                debayer: cfg.debayer,
                reference,
                phase: cfg.phase,
                progress: &report,
                cancel: ctx.cancel,
                profile: ctx.profile,
            })?;
            if shifts.is_some() {
                global_wall = t.elapsed();
            }
        }
        if shifts.is_none() && device_points {
            // The device needs the global shifts of the candidates first.
            let done = self.register_on_cpu(src, cand, reference, None, None, align, ctx)?;
            shifts = Some(done.shifts);
            measured = done.aligns;
            global_wall += done.global_wall;
        }
        if let (Some(s), Some(ps), true) = (shifts.as_deref(), points, device_points)
            && let Some(shared) = src.shared()
        {
            let t = Instant::now();
            let report = |f: f32| ctx.report(Stage::LocalAlign, f);
            table = self.backend.measure_points(&MeasureRequest {
                frames: shared,
                indices: cand,
                debayer: cfg.debayer,
                shifts: s,
                grid: &ps.grid,
                templates: &ps.templates,
                config: &ps.config,
                progress: &report,
                cancel: ctx.cancel,
                profile: ctx.profile,
            })?;
            if table.is_some() {
                local_wall = t.elapsed();
            }
        }

        let need_global = shifts.is_none();
        let need_local = points.is_some() && table.is_none();
        if need_global || need_local {
            let done = self.register_on_cpu(
                src,
                cand,
                reference,
                points.filter(|_| need_local),
                shifts.take(),
                align,
                ctx,
            )?;
            shifts = Some(done.shifts);
            if table.is_none() {
                table = done.table;
            }
            if done.aligns.is_some() {
                measured = done.aligns;
            }
            global_wall += done.global_wall;
            local_wall += done.local_wall;
        }
        tl.record(Stage::GlobalAlign, global_wall);
        if points.is_some() {
            tl.record(Stage::LocalAlign, local_wall);
        }
        let shifts = shifts.unwrap_or_default();
        let report = if let Some(m) = measured {
            AlignReport::measured(align, &m)
        } else if fixed.is_some() {
            // The shifts were given: nothing was aligned here.
            AlignReport::sampled(align, &[], cand.len())
        } else {
            let sample = self.sample_phase(src, cand, reference, &shifts, ctx)?;
            AlignReport::sampled(align, &sample, cand.len())
        };
        Ok((shifts, table, report))
    }

    /// Re-measure a few evenly spread candidates with the CPU phase
    /// correlator, to give a confidence for shifts the GPU found without one.
    fn sample_phase(
        &self,
        src: &mut dyn FrameSource,
        cand: &[usize],
        reference: &Frame,
        gpu: &[Shift],
        ctx: &Ctx,
    ) -> Result<Vec<FrameAlign>> {
        const SAMPLE: usize = 8;
        let picks: Vec<usize> = mask::sample_indices(cand.len(), SAMPLE);
        if picks.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<usize> = picks.iter().map(|&k| cand[k]).collect();
        let frames = self.load(src, &ids, ctx)?;
        let corr = PhaseCorrelator::new(reference, self.cfg.phase)?;
        frames
            .iter()
            .zip(&picks)
            .map(|(f, &k)| {
                let e = corr.estimate(f)?;
                // The GPU's own answer is the shift in use; the CPU one only
                // supplies the lock quality. A disagreement lowers it.
                let miss = (e.shift - gpu[k]).length();
                let agree = (1.0 - miss / 0.5).clamp(0.0, 1.0);
                Ok(FrameAlign {
                    shift: gpu[k],
                    confidence: phase_confidence(e.psr) * agree,
                    used: GlobalMethod::Phase,
                    fell_back: false,
                })
            })
            .collect()
    }

    /// The CPU part of [`Self::register`]: global shifts unless `given`, and the
    /// alignment point table when `points` is set, in one pass over the frames.
    /// Returns the wall time spent on each.
    #[allow(
        clippy::too_many_arguments,
        reason = "the stage inputs; bundling them would only move the arguments"
    )]
    fn register_on_cpu(
        &self,
        src: &mut dyn FrameSource,
        cand: &[usize],
        reference: &Frame,
        points: Option<&PointSet>,
        given: Option<Vec<Shift>>,
        align: &AlignPlan,
        ctx: &Ctx,
    ) -> Result<CpuRegistration> {
        let cand_n = cand.len();
        let aligner = if given.is_none() {
            Some(Aligner {
                phase: PhaseCorrelator::new(reference, self.cfg.phase)?,
                limb: align
                    .limb
                    .as_ref()
                    .filter(|_| align.method == GlobalMethod::Limb),
            })
        } else {
            None
        };
        let (mut global_wall, mut local_wall) = (Duration::ZERO, Duration::ZERO);
        let mut got: Vec<Shift> = Vec::with_capacity(cand_n);
        let mut aligns: Vec<FrameAlign> = Vec::with_capacity(cand_n);
        let mut table = points.map(|ps| LocalTable::new(cand_n, ps.grid.aps.len()));
        let bs = batch_size();
        let prof = ctx.profile;
        for (ci, chunk) in cand.chunks(bs).enumerate() {
            ctx.check()?;
            let frames = self.load(src, chunk, ctx)?;
            let batch: Vec<Shift> = if let Some(al) = &aligner {
                let t = Instant::now();
                let est: Vec<FrameAlign> = frames
                    .par_iter()
                    .map(|f| prof.time(Category::GlobalAlign, || al.estimate(f)))
                    .collect::<Result<_>>()?;
                global_wall += t.elapsed();
                let shifts: Vec<Shift> = est.iter().map(|e| e.shift).collect();
                got.extend_from_slice(&shifts);
                aligns.extend(est);
                ctx.report(Stage::GlobalAlign, got.len() as f32 / cand_n as f32);
                shifts
            } else {
                given
                    .as_deref()
                    .map_or_else(Vec::new, |g| g[ci * bs..ci * bs + chunk.len()].to_vec())
            };
            if let (Some(table), Some(ps)) = (table.as_mut(), points) {
                let t = Instant::now();
                let rows: Vec<Vec<Option<ApMeasure>>> = frames
                    .par_iter()
                    .zip(&batch)
                    .map(|(f, &g)| prof.time(Category::ApAlign, || measure_row(ps, f, g)))
                    .collect();
                local_wall += t.elapsed();
                for (j, row) in rows.iter().enumerate() {
                    table.set_row(ci * bs + j, row);
                }
                ctx.report(
                    Stage::LocalAlign,
                    ((ci * bs + chunk.len()) as f32) / cand_n as f32,
                );
            }
        }
        let measured = given.is_none();
        Ok(CpuRegistration {
            shifts: given.unwrap_or(got),
            table,
            global_wall,
            local_wall,
            aligns: measured.then_some(aligns),
        })
    }

    fn deposit(&self) -> Deposit {
        match self.cfg.drizzle {
            Some(scale) => Deposit::Drizzle {
                scale,
                pixfrac: 0.8,
            },
            None => Deposit::Resample(self.cfg.kernel),
        }
    }

    fn accumulator(&self, w: usize, h: usize) -> Result<Box<dyn Accumulator>> {
        let deposit = self.deposit();
        match self.backend.accumulator(w, h, deposit) {
            // Drizzle is unsupported everywhere; only fall back for backend limits.
            Err(Error::Unsupported(_)) if matches!(deposit, Deposit::Resample(_)) => {
                CpuBackend.accumulator(w, h, deposit)
            }
            other => other,
        }
    }

    fn robust_accumulator(
        &self,
        reference: &Frame,
        kind: RobustKind,
        k: f32,
    ) -> Result<Box<dyn RobustAccumulator>> {
        let deposit = self.deposit();
        match self.backend.robust_accumulator(reference, deposit, kind, k) {
            Err(Error::Unsupported(_)) if matches!(deposit, Deposit::Resample(_)) => {
                CpuBackend.robust_accumulator(reference, deposit, kind, k)
            }
            other => other,
        }
    }

    fn stack_global(
        &self,
        src: &mut dyn FrameSource,
        kept: &[usize],
        shifts: &[Shift],
        weights: &[f32],
        reference: &Frame,
        ctx: &Ctx,
    ) -> Result<Frame> {
        let (w, h) = (reference.width, reference.height);
        match self.cfg.mode {
            StackMode::Mean => {
                if let Some(shared) = src
                    .shared()
                    .filter(|_| self.backend.supports(DeviceStage::Stack))
                {
                    let report = |f: f32| ctx.report(Stage::Stack, f);
                    let frame_deposit = |k: usize| Some((shifts[k], weights[k]));
                    let none = |_: usize, _: usize| None;
                    let res = self.backend.stack_frames(&StackRequest {
                        frames: shared,
                        order: kept,
                        debayer: self.cfg.debayer,
                        width: w,
                        height: h,
                        kernel: self.cfg.kernel,
                        tiles: &[],
                        centres: &[],
                        tile_deposit: &none,
                        frame_deposit: &frame_deposit,
                        progress: &report,
                        cancel: ctx.cancel,
                        profile: ctx.profile,
                    })?;
                    if let Some(res) = res {
                        return Ok(res.frame.mean(reference));
                    }
                }
                let mut acc = self.accumulator(w, h)?;
                let mut done = 0;
                for (ci, chunk) in kept.chunks(batch_size()).enumerate() {
                    ctx.check()?;
                    let frames = self.load(src, chunk, ctx)?;
                    ctx.profile.time(Category::Stack, || {
                        for (j, f) in frames.iter().enumerate() {
                            let k = ci * batch_size() + j;
                            acc.add(f, shifts[k], weights[k])?;
                        }
                        Ok::<(), Error>(())
                    })?;
                    done += chunk.len();
                    ctx.report(Stage::Stack, done as f32 / kept.len() as f32);
                }
                Ok(acc.finish()?.mean(reference))
            }
            mode => {
                let (kind, k) = mode.second_pass().expect("robust mode");
                let mut acc = self.robust_accumulator(reference, kind, k)?;
                let total = (kept.len() * 2).max(1);
                let mut done = 0;
                for pass in 0..2 {
                    if pass == 1 {
                        acc.begin_reject()?;
                    }
                    for (ci, chunk) in kept.chunks(batch_size()).enumerate() {
                        ctx.check()?;
                        let frames = self.load(src, chunk, ctx)?;
                        ctx.profile.time(Category::Stack, || {
                            for (j, f) in frames.iter().enumerate() {
                                let i = ci * batch_size() + j;
                                if pass == 0 {
                                    acc.add_stats(f, shifts[i], weights[i])?;
                                } else {
                                    acc.add_robust(f, shifts[i], weights[i])?;
                                }
                            }
                            Ok::<(), Error>(())
                        })?;
                        done += chunk.len();
                        ctx.report(Stage::Stack, done as f32 / total as f32);
                    }
                }
                acc.finish()
            }
        }
    }

    #[allow(clippy::too_many_arguments)] // internal stage; a context struct would only rename the arguments
    fn stack_multipoint(
        &self,
        src: &mut dyn FrameSource,
        cand: &[usize],
        keep_n: usize,
        shifts: &[Shift],
        scores: &[f32],
        reference: &Frame,
        ps: &PointSet,
        table: &LocalTable,
        ctx: &Ctx,
    ) -> Result<(Frame, ApGrid)> {
        let cand_n = cand.len();
        let grid = &ps.grid;
        let templates = &ps.templates;
        let n_aps = grid.aps.len();

        // Per-point selection.
        let mut selected = vec![false; cand_n * n_aps];
        let mut weight = vec![0.0f32; cand_n * n_aps];
        let mut tile_ap: Vec<usize> = Vec::new();
        for (a, t) in templates.iter().enumerate() {
            if t.is_none() {
                continue;
            }
            let best = table.best_frames(cand_n, a, keep_n);
            if best.is_empty() {
                continue;
            }
            let q: Vec<f32> = best.iter().map(|&f| table.quality[f * n_aps + a]).collect();
            for (&f, w) in best.iter().zip(quality::soft_weights(&q)) {
                selected[f * n_aps + a] = true;
                weight[f * n_aps + a] = w;
            }
            tile_ap.push(a);
        }
        let clip = self.cfg.mode.second_pass();

        // Frames anyone needs: selected somewhere, or in the global kept set.
        let needed: Vec<usize> = (0..cand_n)
            .filter(|&f| f < keep_n || tile_ap.iter().any(|&a| selected[f * n_aps + a]))
            .collect();
        let order: Vec<usize> = needed.iter().map(|&f| cand[f]).collect();
        let sf = |k: usize, t: usize| {
            let (f, a) = (needed[k], tile_ap[t]);
            selected[f * n_aps + a].then(|| (table.shifts[f * n_aps + a], weight[f * n_aps + a]))
        };
        let kept_w = quality::soft_weights(
            &cand[..keep_n]
                .iter()
                .map(|&i| scores[i])
                .collect::<Vec<_>>(),
        );
        let frame_deposit = |k: usize| {
            let f = needed[k];
            (f < keep_n).then(|| (shifts[f], kept_w[f]))
        };

        let on_device = self.stack_points_device(
            src,
            &order,
            reference,
            grid,
            &tile_ap,
            clip,
            &sf,
            &frame_deposit,
            ctx,
        )?;
        let (by_tile, fallback) = if let Some((tiles, acc)) = on_device {
            (tiles, acc.mean(reference))
        } else {
            let mut tiles: Vec<TileStack> = tile_ap
                .iter()
                .map(|&a| {
                    let rect = grid.aps[a].rect;
                    TileStack::new(rect, tile_of(reference, rect), clip.is_some())
                })
                .collect();
            let fallback = self.stack_tiles_and_fill(
                src,
                &order,
                &mut tiles,
                &sf,
                clip,
                &frame_deposit,
                reference,
                ctx,
            )?;
            (tiles.iter().map(TileStack::finish).collect(), fallback)
        };

        ctx.report(Stage::Finish, 0.0);
        let img = ctx.profile.time(Category::Finish, || -> Frame {
            let mut by_ap: Vec<Option<Vec<f32>>> = vec![None; n_aps];
            for (t, &a) in by_tile.into_iter().zip(&tile_ap) {
                by_ap[a] = Some(t);
            }
            blend(grid, &by_ap, &fallback)
        });
        Ok((img, ps.grid.clone()))
    }

    /// Mean stacking of the alignment point tiles and the whole-frame fallback
    /// on the backend, or `None` when it does not implement it (or the mode is
    /// a robust mode, which needs two passes).
    #[allow(clippy::too_many_arguments)] // internal stage; a context struct would only rename the arguments
    fn stack_points_device(
        &self,
        src: &mut dyn FrameSource,
        order: &[usize],
        reference: &Frame,
        grid: &ApGrid,
        tile_ap: &[usize],
        clip: Option<(RobustKind, f32)>,
        tile_deposit: &(dyn Fn(usize, usize) -> FrameDeposit + Sync),
        frame_deposit: &(dyn Fn(usize) -> FrameDeposit + Sync),
        ctx: &Ctx,
    ) -> Result<Option<(Vec<Vec<f32>>, Accumulated)>> {
        let (Some(shared), None) = (
            src.shared()
                .filter(|_| self.backend.supports(DeviceStage::Stack)),
            clip,
        ) else {
            return Ok(None);
        };
        let rects: Vec<Rect> = tile_ap.iter().map(|&a| grid.aps[a].rect).collect();
        let centres: Vec<Vec<f32>> = rects.iter().map(|&r| tile_of(reference, r)).collect();
        let report = |f: f32| ctx.report(Stage::Stack, f);
        let res = self.backend.stack_frames(&StackRequest {
            frames: shared,
            order,
            debayer: self.cfg.debayer,
            width: reference.width,
            height: reference.height,
            kernel: self.cfg.kernel,
            tiles: &rects,
            centres: &centres,
            tile_deposit,
            frame_deposit,
            progress: &report,
            cancel: ctx.cancel,
            profile: ctx.profile,
        })?;
        Ok(res.map(|r| (r.tiles, r.frame)))
    }

    /// Stream `order` through `tiles` and, alongside, through the whole-frame
    /// image that fills where no point has a tile. The fill gets the same
    /// robust rule as the tiles (issue #150).
    #[allow(clippy::too_many_arguments)] // internal helper of stack_multipoint
    fn stack_tiles_and_fill(
        &self,
        src: &mut dyn FrameSource,
        order: &[usize],
        tiles: &mut [TileStack],
        shift_for: &(dyn Fn(usize, usize) -> Option<(Shift, f32)> + Sync),
        clip: Option<(RobustKind, f32)>,
        frame_deposit: &(dyn Fn(usize) -> FrameDeposit + Sync),
        reference: &Frame,
        ctx: &Ctx,
    ) -> Result<Frame> {
        let (w, h) = (reference.width, reference.height);
        let mut plain = if clip.is_none() {
            Some(self.accumulator(w, h)?)
        } else {
            None
        };
        let mut robust = match clip {
            Some((kind, k)) => Some(self.robust_accumulator(reference, kind, k)?),
            None => None,
        };
        let mut begun = false;
        let mut each_frame = |k: usize, frame: &Frame, pass: usize| -> Result<()> {
            if let (1, Some(acc), false) = (pass, robust.as_mut(), begun) {
                acc.begin_reject()?;
                begun = true;
            }
            let Some((s, wt)) = frame_deposit(k) else {
                return Ok(());
            };
            if let Some(acc) = plain.as_mut() {
                acc.add(frame, s, wt)?;
            } else if let Some(acc) = robust.as_mut() {
                if pass == 0 {
                    acc.add_stats(frame, s, wt)?;
                } else {
                    acc.add_robust(frame, s, wt)?;
                }
            }
            Ok(())
        };
        self.tile_passes(src, order, tiles, shift_for, clip, ctx, &mut each_frame)?;
        match (plain, robust) {
            (Some(acc), _) => Ok(acc.finish()?.mean(reference)),
            (None, Some(acc)) => acc.finish(),
            (None, None) => unreachable!("one accumulator is always made"),
        }
    }

    /// Stream `order` through the tiles. Pass one gathers statistics; with
    /// `clip`, pass two sums the surviving samples. `each_frame` sees every
    /// frame in every pass (the whole-frame stack that fills between tiles).
    #[allow(clippy::too_many_arguments)] // internal helper shared by two stackers
    fn tile_passes(
        &self,
        src: &mut dyn FrameSource,
        order: &[usize],
        tiles: &mut [TileStack],
        shift_for: &(dyn Fn(usize, usize) -> Option<(Shift, f32)> + Sync),
        clip: Option<(RobustKind, f32)>,
        ctx: &Ctx,
        each_frame: &mut dyn FnMut(usize, &Frame, usize) -> Result<()>,
    ) -> Result<()> {
        let passes = if clip.is_some() { 2 } else { 1 };
        let kernel = self.cfg.kernel;
        let total = (order.len() * passes).max(1);
        let mut done = 0;
        for pass in 0..passes {
            if let (1, Some((kind, k))) = (pass, clip) {
                for t in tiles.iter_mut() {
                    t.begin_robust(kind, k);
                }
            }
            let mut k0 = 0;
            for chunk in order.chunks(batch_size()) {
                ctx.check()?;
                let frames = self.load(src, chunk, ctx)?;
                ctx.profile.time(Category::Stack, || {
                    for (j, f) in frames.iter().enumerate() {
                        each_frame(k0 + j, f, pass)?;
                    }
                    Ok::<(), Error>(())
                })?;
                let prof = ctx.profile;
                tiles.par_iter_mut().enumerate().for_each(|(t, tile)| {
                    prof.time(Category::Stack, || {
                        let mut buf = vec![0.0f32; tile.rect().area()];
                        for (j, f) in frames.iter().enumerate() {
                            if let Some((s, wt)) = shift_for(k0 + j, t) {
                                resample_region(f, s, tile.rect(), kernel, &mut buf);
                                if pass == 0 {
                                    tile.add(&buf, wt);
                                } else {
                                    tile.add_clipped(&buf, wt);
                                }
                            }
                        }
                    });
                });
                k0 += chunk.len();
                done += chunk.len();
                ctx.report(Stage::Stack, done as f32 / total as f32);
            }
        }
        Ok(())
    }
}

/// Measure every alignment point in one frame given its global shift `g`.
fn measure_row(ps: &PointSet, frame: &Frame, g: Shift) -> Vec<Option<ApMeasure>> {
    let prep = ps.grid.prepare(frame);
    let mut row: Vec<Option<ApMeasure>> = ps
        .templates
        .iter()
        .map(|t| t.as_ref().and_then(|t| measure(t, &prep, g, &ps.config)))
        .collect();
    fill_missing(&ps.grid, &ps.templates, &mut row, &prep, g);
    row
}

/// RMS distance, in pixels, between two tables over the entries both measured.
fn moved_rms(a: &LocalTable, b: &LocalTable) -> Option<f32> {
    let (mut sum, mut n) = (0.0f64, 0u64);
    for (i, (qa, qb)) in a.quality.iter().zip(&b.quality).enumerate() {
        if qa.is_finite() && qb.is_finite() {
            sum += f64::from((a.shifts[i] - b.shifts[i]).length()).powi(2);
            n += 1;
        }
    }
    (n > 0).then(|| (sum / n as f64).sqrt() as f32)
}

/// Copy `rect` out of `frame`.
fn tile_of(frame: &Frame, rect: Rect) -> Vec<f32> {
    let mut v = Vec::with_capacity(rect.area());
    for y in rect.y0..rect.y1 {
        v.extend_from_slice(&frame.row(y)[rect.x0..rect.x1]);
    }
    v
}

/// Scale so the 99.99th percentile of finite samples becomes 1.
fn normalise(img: &mut Frame) {
    let mut v: Vec<f32> = img.data.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return;
    }
    let k = (((v.len() as f64) * 0.9999) as usize).min(v.len() - 1);
    let p = *v.select_nth_unstable_by(k, f32::total_cmp).1;
    if p > 0.0 {
        let inv = 1.0 / p;
        img.data.iter_mut().for_each(|x| *x *= inv);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::*;
    use crate::io::source::MemorySource;
    use crate::mask::MaskConfig;
    use crate::testutil::{CaptureConfig, SyntheticCapture, SyntheticConfig, SyntheticSun, rmse};

    fn run(
        cap: &SyntheticCapture,
        cfg: StackConfig,
        progress: &dyn Fn(Stage, f32),
    ) -> Result<StackOutput> {
        let mut src = MemorySource(cap.frames.clone());
        Pipeline::new(cfg).run(&mut src, progress, &AtomicBool::new(false))
    }

    fn quiet(_: Stage, _: f32) {}

    /// Gradient-energy sharpness measured after a light blur so that pixel
    /// noise (which stacking removes) does not read as detail.
    fn sharp(f: &Frame) -> f32 {
        quality::score(
            &crate::filter::gaussian_blur(f, 1.5),
            Metric::GradientEnergy,
            None,
        )
    }

    fn config(multipoint: bool) -> StackConfig {
        StackConfig {
            keep: Keep::Fraction(0.4),
            normalise: false,
            multipoint: multipoint.then_some(MultipointConfig {
                ap_size: 32,
                min_contrast: 0.01,
                ..MultipointConfig::default()
            }),
            ..StackConfig::default()
        }
    }

    #[test]
    fn quality_ranking_puts_least_blurred_first() {
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let cap = SyntheticCapture::generate(&sun, &CaptureConfig::default());
        let out = run(&cap, config(false), &quiet).expect("run");
        let n = cap.frames.len();
        // Rank order must follow blur order for the sharpest quarter.
        let mut by_sigma: Vec<usize> = (0..n).collect();
        by_sigma.sort_by(|&a, &b| cap.sigmas[a].total_cmp(&cap.sigmas[b]));
        let top: std::collections::HashSet<_> = by_sigma[..n / 4].iter().copied().collect();
        let got: std::collections::HashSet<_> = out.ranking[..n / 4].iter().copied().collect();
        assert_eq!(top, got, "sharpest quarter must rank first");
        assert_eq!(out.ranking[0], cap.sharpest());
    }

    #[test]
    fn global_stack_beats_every_single_frame() {
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let cap = SyntheticCapture::generate(
            &sun,
            &CaptureConfig {
                noise: 0.03,
                ..CaptureConfig::default()
            },
        );
        let cfg = StackConfig {
            keep: Keep::Fraction(0.15),
            ..config(false)
        };
        let out = run(&cap, cfg, &quiet).expect("run");
        let best = cap.sharpest();
        let truth = sun.render_clean(cap.shifts[best], None);
        let stack_err = rmse(&out.image, &truth, 14);
        // Best single frame after perfect registration, so position cannot count against it.
        let aligned: Vec<Frame> = cap
            .frames
            .iter()
            .zip(&cap.shifts)
            .map(|(f, s)| crate::resample::shifted(f, *s - cap.shifts[best], Kernel::Lanczos3))
            .collect();
        let best_single_err = aligned
            .iter()
            .map(|f| rmse(f, &truth, 14))
            .fold(f32::INFINITY, f32::min);
        let best_single_sharp = aligned.iter().map(sharp).fold(0.0, f32::max);
        eprintln!(
            "global: stack rmse {stack_err:.5} vs best single {best_single_err:.5}; sharpness {:.5} vs {best_single_sharp:.5}",
            sharp(&out.image)
        );
        assert!(
            stack_err < 0.85 * best_single_err,
            "stack {stack_err} vs best single {best_single_err}"
        );
        assert!(sharp(&out.image) > best_single_sharp);
    }

    #[test]
    fn recovered_global_shifts_match_ground_truth() {
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let cap = SyntheticCapture::generate(&sun, &CaptureConfig::default());
        let out = run(&cap, config(false), &quiet).expect("run");
        let ref_shift = cap.shifts[out.ranking[0]];
        let mut worst = 0.0f32;
        for &(idx, s) in &out.shifts {
            worst = worst.max((s - (cap.shifts[idx] - ref_shift)).length());
        }
        eprintln!(
            "pipeline: worst registered shift error {worst:.4} px over {} frames",
            out.shifts.len()
        );
        assert!(worst < 0.1, "{worst}");
    }

    #[test]
    fn multipoint_beats_global_on_locally_warped_data() {
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let cc = CaptureConfig {
            warp_amp: 2.0,
            warp_wavelength: 90.0,
            warp_floor: 1.0,
            warp_free: 0,
            frames: 48,
            noise: 0.02,
            ..CaptureConfig::default()
        };
        let cap = SyntheticCapture::generate(&sun, &cc);
        let best = cap.sharpest();
        // Every frame carries a warp of equal amplitude and random phase. The
        // reference is the single sharpest frame, so the truth is the clean sun
        // in that frame's geometry.
        let truth = sun.render_clean(cap.shifts[best], Some(&cap.warps[best]));
        let cfg = |mp| StackConfig {
            keep: Keep::Fraction(0.3),
            reference_frames: 1,
            // Windows that only touch the limb still measure the seeing.
            mask: MaskConfig {
                prominence_band: 16,
                ..MaskConfig::default()
            },
            ..config(mp)
        };
        let g = run(&cap, cfg(false), &quiet).expect("global");
        let m = run(&cap, cfg(true), &quiet).expect("multipoint");
        let (eg, em) = (rmse(&g.image, &truth, 14), rmse(&m.image, &truth, 14));
        eprintln!(
            "warped: global rmse {eg:.5} sharp {:.5}; multipoint rmse {em:.5} sharp {:.5}; active APs {}",
            sharp(&g.image),
            sharp(&m.image),
            m.grid.as_ref().map_or(0, ApGrid::active_count)
        );
        assert!(em < 0.9 * eg, "multipoint {em} vs global {eg}");
        assert!(sharp(&m.image) > sharp(&g.image));
    }

    /// The warped, noisy capture the refinement tests share.
    fn noisy_warped(noise: f32) -> (SyntheticSun, SyntheticCapture) {
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let cc = CaptureConfig {
            warp_amp: 2.0,
            warp_wavelength: 90.0,
            warp_floor: 1.0,
            warp_free: 0,
            frames: 48,
            noise,
            ..CaptureConfig::default()
        };
        let cap = SyntheticCapture::generate(&sun, &cc);
        (sun, cap)
    }

    fn refined(passes: u8, smooth: bool) -> StackConfig {
        StackConfig {
            keep: Keep::Fraction(0.3),
            reference_frames: 1,
            mask: MaskConfig {
                prominence_band: 16,
                ..MaskConfig::default()
            },
            ap_refine: ApRefine {
                passes,
                smooth: smooth.then(SmoothConfig::default),
                residual: false,
            },
            ..config(true)
        }
    }

    #[test]
    fn refinement_does_not_hurt_on_noisy_warped_data() {
        // The truth is the clean sun in the geometry of the frame the run
        // took as its reference, so every variant is judged the same way.
        for noise in [0.02, 0.2] {
            let (sun, cap) = noisy_warped(noise);
            let mut got = Vec::new();
            for (passes, smooth) in [(1, false), (1, true), (2, false), (2, true)] {
                let out = run(&cap, refined(passes, smooth), &quiet).expect("run");
                let r = out.refine.expect("report");
                let b = out.ranking[0];
                let truth = sun.render_clean(cap.shifts[b], Some(&cap.warps[b]));
                let (e, s) = (rmse(&out.image, &truth, 14), sharp(&out.image));
                eprintln!(
                    "noise {noise} passes {passes} smooth {smooth}: rmse {e:.5} sharp {s:.5}                      replaced {} moved {:?}",
                    r.smoothed, r.moved_rms
                );
                assert_eq!(r.passes, passes);
                assert_eq!(r.moved_rms.is_some(), passes == 2);
                got.push((e, s));
            }
            let (base, refined) = (got[0], got[3]);
            assert!(
                refined.0 <= base.0 * 1.15,
                "rmse {} vs {}",
                refined.0,
                base.0
            );
            assert!(
                refined.1 >= base.1 * 0.95,
                "sharp {} vs {}",
                refined.1,
                base.1
            );
        }
    }

    #[test]
    fn a_single_pass_without_smoothing_is_unchanged_by_the_option_existing() {
        let (_, cap) = noisy_warped(0.02);
        let a = run(&cap, refined(1, false), &quiet).expect("a");
        let b = run(
            &cap,
            StackConfig {
                ap_refine: ApRefine::default(),
                ..refined(1, false)
            },
            &quiet,
        )
        .expect("b");
        assert_eq!(a.image.data, b.image.data);
        assert_eq!(a.refine.map(|r| (r.passes, r.smoothed)), Some((1, 0)));
    }

    #[test]
    fn the_seeing_probe_ranks_synthetic_warps_and_is_off_by_default() {
        // A disc big enough for several 64 px tiles.
        let sun = SyntheticSun::new(&SyntheticConfig {
            width: 512,
            height: 420,
            radius_frac: 0.45,
            granule_sigma: 2.5,
            ..SyntheticConfig::default()
        });
        let mut index = Vec::new();
        for amp in [0.4f32, 1.2, 3.0] {
            let cap = SyntheticCapture::generate(
                &sun,
                &CaptureConfig {
                    warp_amp: amp,
                    warp_wavelength: 70.0,
                    warp_floor: 1.0,
                    frames: 40,
                    noise: 0.02,
                    ..CaptureConfig::default()
                },
            );
            let cfg = StackConfig {
                seeing: Some(SeeingSettings::default()),
                ..config(false)
            };
            let out = run(&cap, cfg, &quiet).expect("run");
            let s = out.seeing.expect("report");
            let m = s.motion.expect("motion index");
            eprintln!(
                "warp {amp}: {:?}, tiles {}, frames {}",
                m, s.tiles, s.frames
            );
            index.push(m.rms_px);
            assert!(
                s.spectral.is_none() && s.grade.is_none(),
                "no aperture, no r0"
            );
            assert!(s.lines().iter().any(|l| l.contains("--aperture-mm")));
        }
        assert!(index[0] < index[1] && index[1] < index[2], "{index:?}");
        let cap = SyntheticCapture::generate(&sun, &CaptureConfig::default());
        assert!(
            run(&cap, config(false), &quiet)
                .expect("run")
                .seeing
                .is_none()
        );
    }

    #[test]
    fn sigma_clip_runs_and_matches_mean_without_outliers() {
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let cap = SyntheticCapture::generate(
            &sun,
            &CaptureConfig {
                frames: 24,
                ..CaptureConfig::default()
            },
        );
        let mean = run(&cap, config(false), &quiet).expect("mean");
        let mut c = config(false);
        c.mode = StackMode::SigmaClip { kappa: 2.5 };
        let clipped = run(&cap, c, &quiet).expect("clip");
        assert!(rmse(&mean.image, &clipped.image, 14) < 0.02);
        let mut c = config(true);
        c.mode = StackMode::SigmaClip { kappa: 2.5 };
        assert!(run(&cap, c, &quiet).is_ok());
    }

    /// Damage written into the raw frames, before any alignment, the way a
    /// hot pixel, a cosmic ray or a satellite trail arrives: a dark vertical
    /// line over the disc in two frames, hot pixels in the sky in two others
    /// (issue #150).
    fn damaged(cap: &SyntheticCapture) -> SyntheticCapture {
        let mut cap = cap.clone();
        for &i in &[3usize, 11] {
            let f = &mut cap.frames[i];
            for y in 0..f.height {
                for x in 90..93 {
                    f.data[y * f.width + x] = (f.data[y * f.width + x] - 0.3).max(0.0);
                }
            }
        }
        let mut rng = crate::testutil::Rng::new(5);
        for &i in &[6usize, 17] {
            let f = &mut cap.frames[i];
            for _ in 0..60 {
                let x = 20 + (rng.next_u64() % 120) as usize;
                let y = 4 + (rng.next_u64() % 16) as usize;
                f.data[y * f.width + x] = 0.7;
            }
        }
        cap
    }

    /// Mean absolute difference of two images over `rect`.
    fn l1(a: &Frame, b: &Frame, rect: Rect) -> f32 {
        let mut sum = 0.0f64;
        for y in rect.y0..rect.y1 {
            for x in rect.x0..rect.x1 {
                sum += f64::from((a.data[y * a.width + x] - b.data[y * a.width + x]).abs());
            }
        }
        (sum / rect.area() as f64) as f32
    }

    #[test]
    fn robust_modes_remove_artefacts_written_into_raw_frames() {
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let clean = SyntheticCapture::generate(
            &sun,
            &CaptureConfig {
                frames: 24,
                ..CaptureConfig::default()
            },
        );
        let dirty = damaged(&clean);
        // Sky strip with the hot pixels, and a strip across the dark line.
        let sky = Rect::new(30, 4, 100, 16);
        let line = Rect::new(70, 50, 50, 60);
        for multipoint in [false, true] {
            let stack = |cap: &SyntheticCapture, mode: StackMode| {
                let mut c = config(multipoint);
                c.keep = Keep::Fraction(1.0);
                // Rank on a region the damage does not touch, so the damaged
                // and clean runs choose the same reference and weights.
                c.roi = Some(Rect::new(100, 30, 60, 100));
                c.mode = mode;
                run(cap, c, &quiet).expect("stack").image
            };
            let err = |mode: StackMode| {
                let (a, b) = (stack(&clean, mode), stack(&dirty, mode));
                (l1(&a, &b, sky), l1(&a, &b, line))
            };
            let (mean_sky, mean_line) = err(StackMode::Mean);
            assert!(
                mean_sky > 0.002 && mean_line > 0.002,
                "{mean_sky} {mean_line}"
            );
            for (name, mode, bound) in [
                ("sigma", StackMode::SigmaClip { kappa: 2.5 }, 0.5),
                ("huber", StackMode::Huber { k: 1.345 }, 0.7),
                ("winsor", StackMode::Winsor { k: 2.0 }, 0.7),
            ] {
                let (sky_err, line_err) = err(mode);
                eprintln!(
                    "multipoint {multipoint} {name}: sky {sky_err:.5} ({mean_sky:.5}) line {line_err:.5} ({mean_line:.5})"
                );
                assert!(
                    sky_err < bound * mean_sky,
                    "{name} sky, multipoint {multipoint}"
                );
                assert!(
                    line_err < bound * mean_line,
                    "{name} line, multipoint {multipoint}"
                );
            }
        }
    }

    #[test]
    fn huber_and_winsor_run_and_match_mean_without_outliers() {
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let cap = SyntheticCapture::generate(
            &sun,
            &CaptureConfig {
                frames: 24,
                ..CaptureConfig::default()
            },
        );
        let mean = run(&cap, config(false), &quiet).expect("mean");
        for mode in [StackMode::Huber { k: 1.345 }, StackMode::Winsor { k: 2.0 }] {
            for multipoint in [false, true] {
                let mut c = config(multipoint);
                c.mode = mode;
                let r = run(&cap, c, &quiet).expect("robust");
                if !multipoint {
                    assert!(rmse(&mean.image, &r.image, 14) < 0.02, "{mode:?}");
                }
            }
        }
    }

    #[test]
    fn progress_is_monotone_per_stage_and_cancellation_works() {
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let cap = SyntheticCapture::generate(
            &sun,
            &CaptureConfig {
                frames: 16,
                ..CaptureConfig::default()
            },
        );
        let seen = std::sync::Mutex::new(Vec::<(Stage, f32)>::new());
        let cb = |s, f| seen.lock().expect("lock").push((s, f));
        run(&cap, config(true), &cb).expect("run");
        let seen = seen.into_inner().expect("lock");
        for stage in [
            Stage::Quality,
            Stage::GlobalAlign,
            Stage::LocalAlign,
            Stage::Stack,
            Stage::Finish,
        ] {
            let v: Vec<f32> = seen
                .iter()
                .filter(|(s, _)| *s == stage)
                .map(|p| p.1)
                .collect();
            assert!(
                !v.is_empty() && v.windows(2).all(|p| p[0] <= p[1]),
                "{stage:?}: {v:?}"
            );
            assert!(
                (v[v.len() - 1] - 1.0).abs() < 1e-6,
                "{stage:?} ends at {}",
                v[v.len() - 1]
            );
        }
        let mut src = MemorySource(cap.frames.clone());
        let cancel = AtomicBool::new(true);
        let r = Pipeline::new(config(false)).run(&mut src, &quiet, &cancel);
        assert!(matches!(r, Err(Error::Cancelled)));
    }

    /// A capture with a changing burned-in timestamp top-left and a disc.
    fn stamped_capture() -> (SyntheticCapture, Rect) {
        let sun = SyntheticSun::new(&SyntheticConfig {
            width: 320,
            height: 240,
            ..SyntheticConfig::default()
        });
        let mut cap = SyntheticCapture::generate(&sun, &CaptureConfig::default());
        let mut text = Rect::new(0, 0, 0, 0);
        for (i, f) in cap.frames.iter_mut().enumerate() {
            text = crate::testutil::burn_timestamp(f, i);
        }
        (cap, text)
    }

    fn worst_shift_error(cap: &SyntheticCapture, out: &StackOutput) -> f32 {
        let ref_shift = cap.shifts[out.ranking[0]];
        out.shifts
            .iter()
            .map(|&(idx, s)| (s - (cap.shifts[idx] - ref_shift)).length())
            .fold(0.0, f32::max)
    }

    #[test]
    fn timestamp_and_sky_get_no_alignment_points_and_shifts_stay_exact() {
        let (cap, text) = stamped_capture();
        let out = run(&cap, config(true), &quiet).expect("run");

        // The text was found, covers the whole stamp and is not much bigger.
        assert_eq!(out.masked.len(), 1, "{:?}", out.masked);
        let m = out.masked[0];
        assert!(
            m.x0 <= text.x0 && m.y0 <= text.y0 && m.x1 >= text.x1 && m.y1 >= text.y1,
            "{m:?} does not cover {text:?}"
        );
        assert!(
            m.area() < 3 * text.area(),
            "{m:?} is far larger than {text:?}"
        );

        // The limb fit is close to the truth.
        let disc = out.disc.expect("disc fit");
        // The reference sits in the geometry of the best frame.
        let rs = cap.shifts[out.ranking[0]];
        let (cx, cy) = (cap.sun.centre.0 + rs.dx, cap.sun.centre.1 + rs.dy);
        assert!(
            (disc.cx - cx).abs() < 1.0 && (disc.cy - cy).abs() < 1.0,
            "{disc:?}"
        );
        assert!(
            (disc.r - cap.sun.radius).abs() < 1.5,
            "{disc:?} vs {}",
            cap.sun.radius
        );

        // No point overlaps the text and none has its centre outside the disc.
        let grid = out.grid.as_ref().expect("grid");
        let active: Vec<_> = grid.aps.iter().filter(|a| a.active).collect();
        assert!(active.len() > 10, "{} points", active.len());
        for ap in &active {
            assert!(
                !crate::mask::overlaps(ap.rect, text),
                "point {ap:?} on the text"
            );
            let d = (ap.cx as f32 - cx).hypot(ap.cy as f32 - cy);
            assert!(
                d <= cap.sun.radius + 1.5,
                "point {ap:?} outside the disc ({d})"
            );
        }

        // The changing text does not pull the registration.
        let worst = worst_shift_error(&cap, &out);
        eprintln!(
            "stamped capture: worst shift error {worst:.4} px, {} points",
            active.len()
        );
        assert!(worst < 0.1, "{worst}");
    }

    #[test]
    fn without_the_mask_the_timestamp_does_get_points() {
        // Guards the test above: it only means something if the stamp really
        // attracts points when masking is off.
        let (cap, text) = stamped_capture();
        let mut cfg = config(true);
        cfg.mask = MaskConfig::off();
        let out = run(&cap, cfg, &quiet).expect("run");
        let grid = out.grid.expect("grid");
        let on_text = grid
            .aps
            .iter()
            .filter(|a| a.active && crate::mask::overlaps(a.rect, text))
            .count();
        assert!(on_text > 0, "no point on the text even without the mask");
    }

    #[test]
    fn manual_exclusions_and_the_prominence_band_are_honoured() {
        let (cap, _) = stamped_capture();
        let (cx, cy) = cap.sun.centre;
        // A block in the middle of the disc.
        let hole = Rect::new(cx as usize - 20, cy as usize - 20, 40, 40);
        let mut cfg = config(true);
        cfg.mask.exclude = vec![hole];
        let tight = run(&cap, cfg.clone(), &quiet).expect("run");
        let g = tight.grid.as_ref().expect("grid");
        assert!(
            g.aps
                .iter()
                .all(|a| !(a.active && crate::mask::overlaps(a.rect, hole)))
        );
        assert!(tight.masked.contains(&hole));
        // A band lets points sit outside the limb again.
        cfg.mask.prominence_band = 24;
        let wide = run(&cap, cfg, &quiet).expect("run");
        let (a, b) = (
            tight.grid.as_ref().map_or(0, ApGrid::active_count),
            wide.grid.as_ref().map_or(0, ApGrid::active_count),
        );
        assert!(b >= a, "{b} < {a}");
    }

    #[test]
    fn drizzle_and_empty_captures_are_typed_errors() {
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let cap = SyntheticCapture::generate(
            &sun,
            &CaptureConfig {
                frames: 4,
                ..CaptureConfig::default()
            },
        );
        let mut c = config(false);
        c.drizzle = Some(2.0);
        assert!(matches!(run(&cap, c, &quiet), Err(Error::Unsupported(_))));
        let mut empty = MemorySource(Vec::new());
        let r = Pipeline::new(config(false)).run(&mut empty, &quiet, &AtomicBool::new(false));
        assert!(matches!(r, Err(Error::Invalid(_))));
    }

    #[test]
    fn autocrop_cuts_the_stack_to_the_disc_and_nothing_else() {
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let cap = SyntheticCapture::generate(&sun, &CaptureConfig::default());
        let whole = run(&cap, config(true), &quiet).expect("run");
        let cropped = run(
            &cap,
            StackConfig {
                autocrop: Some(0.1),
                ..config(true)
            },
            &quiet,
        )
        .expect("run");
        let c = cropped.crop.expect("cropped");
        assert_eq!(
            (cropped.image.width, cropped.image.height),
            (c.rect.width(), c.rect.height())
        );
        // The disc has radius 56: a square of about 2 * 1.1 * 56 pixels.
        assert!((118..=130).contains(&cropped.image.width), "{c:?}");
        assert!(cropped.image.width < whole.image.width);
        // The same pixels as cutting the uncropped stack.
        assert_eq!(cropped.image, crate::autocrop::cut(&whole.image, c.rect));
        // The disc is reported in the cropped image's coordinates.
        assert_eq!(cropped.disc, Some(c.disc));
        assert!((c.disc.cx - cropped.image.width as f32 / 2.0).abs() < 3.0);
        // The limb fit follows the crop too, so a profile taken from it is on the disc.
        let (lw, lc) = (whole.limb.expect("limb"), cropped.limb.expect("limb"));
        assert!((lc.cx - (lw.cx - c.rect.x0 as f32)).abs() < 1e-3);
        assert!((lc.cy - (lw.cy - c.rect.y0 as f32)).abs() < 1e-3);
        assert!(cropped.provenance.iter().any(|p| p.step == "CROPPING"));
        assert!(whole.crop.is_none() && whole.crop_skipped.is_none());
    }

    #[test]
    fn autocrop_without_a_disc_leaves_the_image_whole() {
        let flat: Vec<Frame> = (0..6)
            .map(|_| Frame::new(96, 96, vec![0.4; 96 * 96]).expect("frame"))
            .collect();
        let mut src = MemorySource(flat);
        let cfg = StackConfig {
            autocrop: Some(0.1),
            multipoint: None,
            ..StackConfig::default()
        };
        let out = Pipeline::new(cfg)
            .run(&mut src, &quiet, &AtomicBool::new(false))
            .expect("run");
        assert_eq!((out.image.width, out.image.height), (96, 96));
        assert!(out.crop.is_none());
        assert_eq!(out.crop_skipped, Some(crate::autocrop::Skipped::NoDisc));
    }

    #[test]
    fn automatic_alignment_points_are_chosen_and_used() {
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let cap = SyntheticCapture::generate(&sun, &CaptureConfig::default());
        let cfg = StackConfig {
            auto_ap: Some(AutoAp::photosphere(24)),
            ..config(true)
        };
        let out = run(&cap, cfg, &quiet).expect("run");
        let choice = out.auto_ap.expect("choice");
        let grid = out.grid.expect("grid");
        assert_eq!(grid.size, choice.size);
        assert!(choice.scale.is_some(), "the limb was fitted");
        assert!(choice.seeing.is_some(), "the probe found points");
        assert!(
            out.provenance
                .iter()
                .any(|p| p.params.contains(&format!("{} px", choice.size)))
        );
        // Off: the settings are the caller's.
        let off = run(&cap, config(true), &quiet).expect("run");
        assert!(off.auto_ap.is_none());
        assert_eq!(off.grid.expect("grid").size, 32);
    }

    /// A raw colour capture of the synthetic Sun: red, green and blue see the
    /// scene at the given brightness and, for dispersion, displaced.
    fn colour_capture(colours: [f32; 3], dispersion: [Shift; 3]) -> SyntheticCapture {
        let sun = SyntheticSun::new(&SyntheticConfig {
            width: 192,
            height: 160,
            ..SyntheticConfig::default()
        });
        let mut cap = SyntheticCapture::generate(
            &sun,
            &CaptureConfig {
                noise: 0.01,
                ..CaptureConfig::default()
            },
        );
        cap.frames = crate::testutil::colour_mosaic(
            &cap.frames,
            colours,
            dispersion,
            crate::frame::BayerPattern::Rggb,
        );
        cap
    }

    /// Mean (red, green and blue) in the middle of the disc.
    fn disc_colour(rgb: &crate::colour::RgbImage) -> [f32; 3] {
        let (cx, cy) = (rgb.width / 2, rgb.height / 2);
        let mut sum = [0.0f64; 3];
        let mut n = 0.0;
        for y in cy - 15..cy + 15 {
            for x in cx - 15..cx + 15 {
                let p = rgb.pixel(x, y);
                for c in 0..3 {
                    sum[c] += f64::from(p[c]);
                }
                n += 1.0;
            }
        }
        sum.map(|s| (s / n) as f32)
    }

    fn true_colour(balance: crate::truecolour::WhiteBalance, align: bool) -> StackConfig {
        StackConfig {
            true_colour: Some(TrueColour {
                white_balance: balance,
                align_channels: align,
            }),
            ..config(true)
        }
    }

    #[test]
    fn true_colour_recovers_the_colour_of_a_synthetic_disc() {
        use crate::truecolour::WhiteBalance;
        let cap = colour_capture([0.5, 1.0, 0.7], [Shift::default(); 3]);
        // Without a balance the planes keep the sensor's ratios.
        let raw = run(&cap, true_colour(WhiteBalance::Off, false), &quiet).expect("run");
        let c = raw.colour.as_ref().expect("a colour picture");
        let m = disc_colour(&c.rgb);
        assert!((m[0] / m[1] - 0.5).abs() < 0.03, "{m:?}");
        assert!((m[2] / m[1] - 0.7).abs() < 0.03, "{m:?}");
        assert!(c.gains[0] > 0.0 && (c.gains[0] - c.gains[1]).abs() < 1e-6);
        // The luminance is the mean of the three planes.
        let lum = crate::truecolour::rgb_luminance(&c.rgb).expect("luminance");
        assert_eq!(raw.image, lum);
        // Automatic balance makes the same disc neutral.
        let auto = run(&cap, true_colour(WhiteBalance::Auto, false), &quiet).expect("run");
        let rgb = &auto.colour.as_ref().expect("colour").rgb;
        let m = disc_colour(rgb);
        assert!(
            (m[0] / m[1] - 1.0).abs() < 0.03 && (m[2] / m[1] - 1.0).abs() < 0.03,
            "{m:?}"
        );
        // The 99.99th percentile of the brightest channel is one.
        let top = rgb.data.iter().copied().fold(0.0f32, f32::max);
        assert!((0.95..1.3).contains(&top), "{top}");
        assert!(auto.provenance.iter().any(|p| p.step == "WHITE-BALANCE"));
    }

    #[test]
    fn true_colour_measures_and_removes_dispersion() {
        use crate::truecolour::WhiteBalance;
        let red = Shift::new(-0.6, -0.8);
        let blue = Shift::new(0.5, 0.7);
        let cap = colour_capture([1.0; 3], [red, Shift::default(), blue]);
        let out = run(&cap, true_colour(WhiteBalance::Off, true), &quiet).expect("run");
        let c = out.colour.as_ref().expect("colour");
        let [mr, mb] = c.channels.expect("channels");
        let (mr, mb) = (mr.shift, mb.shift);
        // Demosaicing puts a small offset of its own between the planes.
        assert!(
            (mr.dx - red.dx).abs() < 0.3 && (mr.dy - red.dy).abs() < 0.3,
            "{mr:?}"
        );
        assert!(
            (mb.dx - blue.dx).abs() < 0.3 && (mb.dy - blue.dy).abs() < 0.3,
            "{mb:?}"
        );
        assert!(out.provenance.iter().any(|p| p.step == "CHANNEL-ALIGNMENT"));
        // The planes agree better after the move than before.
        let off = run(&cap, true_colour(WhiteBalance::Off, false), &quiet).expect("run");
        let spread = |o: &StackOutput| {
            let rgb = &o.colour.as_ref().expect("colour").rgb;
            let mut s = 0.0f64;
            for y in 50..110 {
                for x in 60..130 {
                    let p = rgb.pixel(x, y);
                    s += f64::from((p[0] - p[1]).abs() + (p[2] - p[1]).abs());
                }
            }
            s
        };
        assert!(
            spread(&out) < 0.6 * spread(&off),
            "{} against {}",
            spread(&out),
            spread(&off)
        );
    }

    #[test]
    fn true_colour_follows_a_crop() {
        use crate::truecolour::WhiteBalance;
        let cap = colour_capture([0.8, 1.0, 0.9], [Shift::default(); 3]);
        let whole = run(&cap, true_colour(WhiteBalance::Auto, false), &quiet).expect("run");
        let cropped = run(
            &cap,
            StackConfig {
                autocrop: Some(0.1),
                ..true_colour(WhiteBalance::Auto, false)
            },
            &quiet,
        )
        .expect("run");
        let rect = cropped.crop.expect("cropped").rect;
        let (a, b) = (
            cropped.colour.as_ref().expect("colour"),
            whole.colour.as_ref().expect("colour"),
        );
        assert_eq!((a.rgb.width, a.rgb.height), (rect.width(), rect.height()));
        assert_eq!(a.rgb, b.cut(rect).rgb);
        assert_eq!(cropped.image.width, rect.width());
        assert!(whole.limb.is_some(), "the limb fit runs on the luminance");
        assert!(
            whole.grid.is_some(),
            "one alignment point grid for all three planes"
        );
    }

    #[test]
    fn true_colour_needs_a_raw_colour_capture() {
        use crate::truecolour::WhiteBalance;
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let mono = SyntheticCapture::generate(&sun, &CaptureConfig::default());
        let e = run(&mono, true_colour(WhiteBalance::Auto, true), &quiet).err();
        assert!(
            matches!(&e, Some(Error::Invalid(m)) if m.contains("raw colour")),
            "{e:?}"
        );
    }

    #[test]
    fn true_colour_finds_no_dispersion_where_there_is_none() {
        use crate::truecolour::WhiteBalance;
        let cap = colour_capture([0.7, 1.0, 0.8], [Shift::default(); 3]);
        let out = run(&cap, true_colour(WhiteBalance::Auto, true), &quiet).expect("run");
        let ch = out
            .colour
            .as_ref()
            .expect("colour")
            .channels
            .expect("channels");
        // Planes of the same scene, even with the mosaic's own sampling, agree.
        for c in ch {
            assert!(c.shift.length() < 0.12, "{c:?}");
        }
    }

    #[test]
    fn true_colour_uses_the_refined_alignment_points() {
        use crate::truecolour::WhiteBalance;
        let cap = colour_capture([0.6, 1.0, 0.8], [Shift::default(); 3]);
        let mut cfg = true_colour(WhiteBalance::Auto, false);
        cfg.ap_refine = ApRefine {
            passes: 2,
            smooth: None,
            residual: true,
        };
        let out = run(&cap, cfg, &quiet).expect("run");
        let refine = out.refine.expect("the refinement ran");
        assert_eq!(refine.passes, 2);
        assert!(refine.residual_rms.is_some());
        assert!(out.colour.is_some() && out.grid.is_some());
    }
}
