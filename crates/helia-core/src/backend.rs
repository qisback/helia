// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Compute backend abstraction.
//!
//! The trait lives here so the core stays GPU-free. [`CpuBackend`] is the
//! specification: any other backend must agree with it within a stated
//! tolerance (see the parity tests in `helia-gpu`).

use std::sync::atomic::AtomicBool;

use rayon::prelude::*;

use crate::align::Shift;
use crate::align::global::PhaseOptions;
use crate::align::multipoint::{ApGrid, ApTemplate, LocalTable, MultipointConfig};
use crate::colour::{ColourLut, RgbImage, apply_lut};
use crate::debayer::Debayer;
use crate::edit::{EditList, apply_cpu};
use crate::error::{Error, Result};
use crate::frame::{Frame, Rect};
use crate::io::raw::SharedFrames;
use crate::profile::Profile;
use crate::quality::{self, Metric};
use crate::resample::{Kernel, resample_region};
use crate::stack::{Deposit, RobustKind, TileStack};

/// Sum and weight maps produced by an [`Accumulator`].
#[derive(Clone, Debug)]
pub struct Accumulated {
    /// Width.
    pub width: usize,
    /// Height.
    pub height: usize,
    /// Sum of `weight * sample` over valid samples.
    pub sum: Vec<f32>,
    /// Sum of weights over valid samples.
    pub weight: Vec<f32>,
}

impl Accumulated {
    /// Weighted mean; pixels that never received a sample take `fallback`.
    ///
    /// # Panics
    /// If `fallback` differs in size (caller bug).
    #[must_use]
    pub fn mean(&self, fallback: &Frame) -> Frame {
        assert_eq!(fallback.data.len(), self.sum.len());
        let data = self
            .sum
            .iter()
            .zip(&self.weight)
            .zip(&fallback.data)
            .map(|((&s, &w), &f)| if w > 0.0 { s / w } else { f })
            .collect();
        Frame {
            width: self.width,
            height: self.height,
            data,
            bayer: None,
        }
    }
}

/// Running shift-and-add of whole frames: the splat interface. Every whole-frame
/// deposit (interpolating resample today, drizzle later) goes through it.
pub trait Accumulator: Send {
    /// Add `frame` resampled by `shift` with the given weight. Pixels whose
    /// source position is outside the frame contribute nothing.
    ///
    /// # Errors
    /// Backend failure or a frame of the wrong size.
    fn add(&mut self, frame: &Frame, shift: Shift, weight: f32) -> Result<()>;

    /// Download the result.
    ///
    /// # Errors
    /// Backend failure.
    fn finish(self: Box<Self>) -> Result<Accumulated>;
}

/// Two-pass robust shift-and-add of whole frames (Huber, winsorised or hard
/// clip, see [`crate::stack::RobustKind`]): the splat interface with a second
/// streaming pass. Memory is O(pixels) whatever the number of frames.
///
/// Call [`Self::add_stats`] for every frame, then [`Self::begin_reject`], then
/// [`Self::add_robust`] for the same frames with the same shifts and weights.
pub trait RobustAccumulator: Send {
    /// Pass one: gather the per-pixel weighted mean and spread.
    ///
    /// # Errors
    /// Backend failure or a frame of the wrong size.
    fn add_stats(&mut self, frame: &Frame, shift: Shift, weight: f32) -> Result<()>;

    /// Fix the per-pixel centre and limit from pass one.
    ///
    /// # Errors
    /// Backend failure.
    fn begin_reject(&mut self) -> Result<()>;

    /// Pass two: add the frame under the robust rule.
    ///
    /// # Errors
    /// Backend failure or a frame of the wrong size.
    fn add_robust(&mut self, frame: &Frame, shift: Shift, weight: f32) -> Result<()>;

    /// Download the result. Pixels no sample reached keep the centre image.
    ///
    /// # Errors
    /// Backend failure.
    fn finish(self: Box<Self>) -> Result<Frame>;
}

/// A pipeline stage a backend can run itself, see [`Backend::supports`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceStage {
    /// [`Backend::score_frames`].
    Score,
    /// [`Backend::register_frames`].
    Register,
    /// [`Backend::measure_points`].
    Points,
    /// [`Backend::stack_frames`].
    Stack,
}

/// Progress within one stage, `0.0..=1.0`. Backends call it from the thread
/// that called them, never from a worker.
pub type StageProgress<'a> = &'a dyn Fn(f32);

/// Score frames: [`Backend::score_frames`].
pub struct ScoreRequest<'a> {
    /// Where the stored frames come from.
    pub frames: &'a dyn SharedFrames,
    /// Frames to score, in the order the result must follow.
    pub indices: &'a [usize],
    /// Bayer to mono conversion.
    pub debayer: Debayer,
    /// Sharpness metric.
    pub metric: Metric,
    /// Scored region, whole frame when `None`.
    pub roi: Option<Rect>,
    /// Progress callback.
    pub progress: StageProgress<'a>,
    /// Set to abort with [`Error::Cancelled`].
    pub cancel: &'a AtomicBool,
    /// Busy-time accounting.
    pub profile: &'a Profile,
}

/// Global registration: [`Backend::register_frames`].
pub struct RegisterRequest<'a> {
    /// Where the stored frames come from.
    pub frames: &'a dyn SharedFrames,
    /// Frames to register.
    pub indices: &'a [usize],
    /// Bayer to mono conversion.
    pub debayer: Debayer,
    /// Image the frames are registered to.
    pub reference: &'a Frame,
    /// Phase correlation tuning.
    pub phase: PhaseOptions,
    /// Progress callback.
    pub progress: StageProgress<'a>,
    /// Set to abort with [`Error::Cancelled`].
    pub cancel: &'a AtomicBool,
    /// Busy-time accounting.
    pub profile: &'a Profile,
}

/// Alignment point measurement: [`Backend::measure_points`].
pub struct MeasureRequest<'a> {
    /// Where the stored frames come from.
    pub frames: &'a dyn SharedFrames,
    /// Frames to measure.
    pub indices: &'a [usize],
    /// Bayer to mono conversion.
    pub debayer: Debayer,
    /// Global shift of each frame in `indices`.
    pub shifts: &'a [Shift],
    /// Alignment point layout.
    pub grid: &'a ApGrid,
    /// Reference patches of the active points.
    pub templates: &'a [Option<ApTemplate>],
    /// Alignment point settings.
    pub config: &'a MultipointConfig,
    /// Progress callback.
    pub progress: StageProgress<'a>,
    /// Set to abort with [`Error::Cancelled`].
    pub cancel: &'a AtomicBool,
    /// Busy-time accounting.
    pub profile: &'a Profile,
}

/// Shift and weight with which a frame enters a stack, or `None` to skip it.
pub type FrameDeposit = Option<(Shift, f32)>;

/// Mean stacking of tiles and of the whole frame: [`Backend::stack_frames`].
pub struct StackRequest<'a> {
    /// Where the stored frames come from.
    pub frames: &'a dyn SharedFrames,
    /// Frames to process, in order; the callbacks index into this list.
    pub order: &'a [usize],
    /// Bayer to mono conversion.
    pub debayer: Debayer,
    /// Output size.
    pub width: usize,
    /// Output size.
    pub height: usize,
    /// Interpolation kernel.
    pub kernel: Kernel,
    /// Output rectangles (alignment point tiles).
    pub tiles: &'a [Rect],
    /// Reference image over each tile; sums are kept relative to it.
    pub centres: &'a [Vec<f32>],
    /// Shift and weight of `order[k]` in tile `t`.
    pub tile_deposit: &'a (dyn Fn(usize, usize) -> FrameDeposit + Sync),
    /// Shift and weight of `order[k]` in the whole-frame accumulation.
    pub frame_deposit: &'a (dyn Fn(usize) -> FrameDeposit + Sync),
    /// Progress callback.
    pub progress: StageProgress<'a>,
    /// Set to abort with [`Error::Cancelled`].
    pub cancel: &'a AtomicBool,
    /// Busy-time accounting.
    pub profile: &'a Profile,
}

/// Result of [`Backend::stack_frames`].
pub struct StackResult {
    /// Finished tiles: reference plus weighted mean offset, `NaN` where no
    /// sample landed (as [`crate::stack::TileStack::finish`]).
    pub tiles: Vec<Vec<f32>>,
    /// Whole-frame sums and weights.
    pub frame: Accumulated,
}

/// A place to run the heavy image maths.
pub trait Backend: Send + Sync {
    /// Human-readable name, e.g. the GPU adapter.
    fn name(&self) -> String;

    /// Sharpness score as defined in [`quality`]. May return
    /// [`Error::Unsupported`] for metrics it lacks; callers then use the CPU.
    ///
    /// # Errors
    /// Backend failure or unsupported metric.
    fn sharpness(&self, frame: &Frame, metric: Metric, roi: Option<Rect>) -> Result<f32>;

    /// Start an accumulation of `width` x `height` frames. May return
    /// [`Error::Unsupported`] for deposits it lacks (all backends, for drizzle).
    ///
    /// # Errors
    /// Backend failure or unsupported deposit.
    fn accumulator(
        &self,
        width: usize,
        height: usize,
        deposit: Deposit,
    ) -> Result<Box<dyn Accumulator>>;

    /// Start a two-pass robust accumulation of `centre`-sized frames, sums kept
    /// relative to `centre`. The default is the CPU reference
    /// ([`cpu_robust_accumulator`]); a device may override it but must
    /// agree within the tolerance of the parity tests. May return
    /// [`Error::Unsupported`] for deposits it lacks.
    ///
    /// # Errors
    /// Backend failure or unsupported deposit.
    fn robust_accumulator(
        &self,
        centre: &Frame,
        deposit: Deposit,
        kind: RobustKind,
        k: f32,
    ) -> Result<Box<dyn RobustAccumulator>> {
        cpu_robust_accumulator(centre, deposit, kind, k)
    }

    /// Whether the backend implements `stage` (its method may still decline a
    /// particular request). The pipeline uses this to choose the cheapest
    /// order of work: the CPU runs registration and alignment points in one
    /// pass over the frames, a backend that takes the points needs the global
    /// shifts first.
    fn supports(&self, _stage: DeviceStage) -> bool {
        false
    }

    /// Score many frames at once, reading them itself so it can overlap
    /// reading with computing and keep the frames on the device. `Ok(None)`
    /// means "not handled": the caller scores on the CPU.
    ///
    /// # Errors
    /// Source or backend failures, or cancellation.
    fn score_frames(&self, _req: &ScoreRequest<'_>) -> Result<Option<Vec<f32>>> {
        Ok(None)
    }

    /// Global shifts of many frames against a reference, as
    /// [`crate::align::global::PhaseCorrelator`] would estimate them.
    /// `Ok(None)` means "not handled".
    ///
    /// # Errors
    /// Source or backend failures, or cancellation.
    fn register_frames(&self, _req: &RegisterRequest<'_>) -> Result<Option<Vec<Shift>>> {
        Ok(None)
    }

    /// Measure every active alignment point in many frames, as
    /// [`crate::align::multipoint::measure`] and
    /// [`crate::align::multipoint::fill_missing`] would. `Ok(None)` means
    /// "not handled".
    ///
    /// # Errors
    /// Source or backend failures, or cancellation.
    fn measure_points(&self, _req: &MeasureRequest<'_>) -> Result<Option<LocalTable>> {
        Ok(None)
    }

    /// A run (or a scoring pass) on `frames` has finished, successfully or
    /// not: release anything the backend kept on the device for them (raw
    /// frames resident between stages). The default does nothing.
    fn release_frames(&self, _frames: &dyn SharedFrames) {}

    /// Weighted-mean stacking of tiles and of the whole frame. `Ok(None)`
    /// means "not handled".
    ///
    /// # Errors
    /// Source or backend failures, or cancellation.
    fn stack_frames(&self, _req: &StackRequest<'_>) -> Result<Option<StackResult>> {
        Ok(None)
    }

    /// Colour a mono image through `lut`. The default is the CPU reference,
    /// [`crate::colour::apply_lut`]; a device may override it but must agree
    /// with that within a rounding error.
    ///
    /// # Errors
    /// Device failures.
    fn colourise(&self, frame: &Frame, lut: &ColourLut) -> Result<RgbImage> {
        Ok(apply_lut(frame, lut))
    }

    /// Apply an edit list to a mono image. The default is the CPU reference,
    /// [`crate::edit::apply_cpu`]; a device may run the filter operations
    /// itself but must agree with it within the tolerances of the parity
    /// tests. Geometry edits always run on the CPU.
    ///
    /// # Errors
    /// [`Error::Invalid`] for a bad list; device failures.
    fn apply_edits(&self, frame: &Frame, edits: &EditList) -> Result<Frame> {
        apply_cpu(frame, edits)
    }
}

/// Multi-threaded CPU reference implementation.
#[derive(Clone, Copy, Debug, Default)]
pub struct CpuBackend;

impl Backend for CpuBackend {
    fn name(&self) -> String {
        format!("CPU ({} threads)", rayon::current_num_threads())
    }

    fn sharpness(&self, frame: &Frame, metric: Metric, roi: Option<Rect>) -> Result<f32> {
        Ok(quality::score(frame, metric, roi))
    }

    fn accumulator(
        &self,
        width: usize,
        height: usize,
        deposit: Deposit,
    ) -> Result<Box<dyn Accumulator>> {
        deposit.validate()?;
        let Deposit::Resample(kernel) = deposit else {
            return Err(Error::Unsupported("drizzle".into()));
        };
        Ok(Box::new(CpuAccumulator {
            width,
            height,
            kernel,
            sum: vec![0.0; width * height],
            weight: vec![0.0; width * height],
            scratch: vec![0.0; width * height],
        }))
    }
}

/// The CPU robust accumulator, the specification for device ones.
///
/// # Errors
/// [`Error::Unsupported`] for a drizzle deposit.
pub fn cpu_robust_accumulator(
    centre: &Frame,
    deposit: Deposit,
    kind: RobustKind,
    k: f32,
) -> Result<Box<dyn RobustAccumulator>> {
    deposit.validate()?;
    let Deposit::Resample(kernel) = deposit else {
        return Err(Error::Unsupported("drizzle".into()));
    };
    let (w, h) = (centre.width, centre.height);
    Ok(Box::new(CpuRobustAccumulator {
        width: w,
        height: h,
        kernel,
        kind,
        k,
        tile: TileStack::new(Rect::new(0, 0, w, h), centre.data.clone(), true),
        centre: centre.data.clone(),
        scratch: vec![0.0; w * h],
    }))
}

struct CpuRobustAccumulator {
    width: usize,
    height: usize,
    kernel: crate::resample::Kernel,
    kind: RobustKind,
    k: f32,
    tile: TileStack,
    centre: Vec<f32>,
    scratch: Vec<f32>,
}

impl CpuRobustAccumulator {
    fn resample(&mut self, frame: &Frame, shift: Shift) -> Result<()> {
        if frame.width != self.width || frame.height != self.height {
            return Err(Error::Invalid(
                "accumulated frame has a different size".into(),
            ));
        }
        let (w, kernel) = (self.width, self.kernel);
        self.scratch
            .par_chunks_mut(w * BAND)
            .enumerate()
            .for_each(|(i, chunk)| {
                let rect = Rect::new(0, i * BAND, w, chunk.len() / w);
                resample_region(frame, shift, rect, kernel, chunk);
            });
        Ok(())
    }
}

impl RobustAccumulator for CpuRobustAccumulator {
    fn add_stats(&mut self, frame: &Frame, shift: Shift, weight: f32) -> Result<()> {
        self.resample(frame, shift)?;
        self.tile.add(&self.scratch, weight);
        Ok(())
    }

    fn begin_reject(&mut self) -> Result<()> {
        self.tile.begin_robust(self.kind, self.k);
        Ok(())
    }

    fn add_robust(&mut self, frame: &Frame, shift: Shift, weight: f32) -> Result<()> {
        self.resample(frame, shift)?;
        self.tile.add_clipped(&self.scratch, weight);
        Ok(())
    }

    fn finish(self: Box<Self>) -> Result<Frame> {
        let data = self
            .tile
            .finish()
            .into_iter()
            .zip(&self.centre)
            .map(|(v, &c)| if v.is_nan() { c } else { v })
            .collect();
        Ok(Frame {
            width: self.width,
            height: self.height,
            data,
            bayer: None,
        })
    }
}

struct CpuAccumulator {
    width: usize,
    height: usize,
    kernel: crate::resample::Kernel,
    sum: Vec<f32>,
    weight: Vec<f32>,
    scratch: Vec<f32>,
}

/// Rows per parallel work item.
const BAND: usize = 16;

impl Accumulator for CpuAccumulator {
    fn add(&mut self, frame: &Frame, shift: Shift, weight: f32) -> Result<()> {
        if frame.width != self.width || frame.height != self.height {
            return Err(Error::Invalid(
                "accumulated frame has a different size".into(),
            ));
        }
        let (w, kernel) = (self.width, self.kernel);
        self.scratch
            .par_chunks_mut(w * BAND)
            .enumerate()
            .for_each(|(i, chunk)| {
                let y0 = i * BAND;
                let rect = Rect::new(0, y0, w, chunk.len() / w);
                resample_region(frame, shift, rect, kernel, chunk);
            });
        self.sum
            .par_iter_mut()
            .zip(self.weight.par_iter_mut())
            .zip(self.scratch.par_iter())
            .for_each(|((s, wt), &v)| {
                if !v.is_nan() {
                    *s += weight * v;
                    *wt += weight;
                }
            });
        Ok(())
    }

    fn finish(self: Box<Self>) -> Result<Accumulated> {
        Ok(Accumulated {
            width: self.width,
            height: self.height,
            sum: self.sum,
            weight: self.weight,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resample::Kernel;
    use crate::testutil::{SyntheticConfig, SyntheticSun};

    #[test]
    fn accumulating_a_shifted_copy_restores_the_original() {
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let truth = sun.render_clean(Shift::default(), None);
        let s = Shift::new(2.5, -1.25);
        let moved = sun.render_clean(s, None);
        let mut acc = CpuBackend
            .accumulator(
                truth.width,
                truth.height,
                Deposit::Resample(Kernel::Lanczos3),
            )
            .expect("acc");
        acc.add(&moved, s, 1.0).expect("add");
        let out = acc.finish().expect("finish").mean(&truth);
        assert!(crate::testutil::rmse(&out, &truth, 8) < 5e-3);
    }

    #[test]
    fn sharpness_matches_quality_module() {
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let f = sun.render_clean(Shift::default(), None);
        let a = CpuBackend
            .sharpness(&f, Metric::GradientEnergy, None)
            .expect("score");
        assert_eq!(a, quality::score(&f, Metric::GradientEnergy, None));
    }
}
