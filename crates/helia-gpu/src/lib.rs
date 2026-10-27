// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! wgpu compute backend for Helia.
//!
//! Implements [`helia_core::Backend`] with WGSL kernels for shift-and-add
//! accumulation and the sharpness metrics. The CPU backend in `helia-core` is
//! the specification: the parity tests in `tests/parity.rs` check that both
//! agree within the tolerances stated there.
//!
//! wgpu offers no FFT, so global phase correlation stays on the CPU. The
//! planned GPU work for alignment points uses direct (windowed) correlation,
//! which maps well to compute shaders at the small search radii involved.
#![forbid(unsafe_code)]

mod accumulate;
mod colourise;
mod context;
mod decode;
mod edit;
mod kit;
mod points;
mod registration;
mod resident;
mod robust_accumulate;
mod score;
mod sharpness;
mod stacking;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};

use helia_core::align::multipoint::LocalTable;
use helia_core::debayer::Debayer;
use helia_core::io::raw::SharedFrames;
use helia_core::stack::Deposit;
use helia_core::{
    Accumulator, Backend, ColourLut, CpuBackend, DeviceStage, EditList, Error, Frame,
    MeasureRequest, Metric, Rect, RegisterRequest, Result, RgbImage, ScoreRequest, Shift,
    StackRequest, StackResult,
};

pub use context::{AdapterRequest, GpuContext, GpuError};

/// The wgpu this crate is built on, so code that uses
/// [`GpuContext::device`] names the same types and cannot drift to another
/// version.
pub use wgpu;

/// Version of wgpu this build links (read from `Cargo.lock` at build time).
pub const WGPU_VERSION: &str = env!("HELIA_WGPU_VERSION");

/// Which backend the user asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BackendChoice {
    /// GPU if a real adapter exists, otherwise CPU.
    #[default]
    Auto,
    /// Always the CPU reference.
    Cpu,
    /// GPU only; an error when no adapter (even a software one) exists.
    Gpu,
}

impl std::str::FromStr for BackendChoice {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "cpu" => Ok(Self::Cpu),
            "gpu" => Ok(Self::Gpu),
            other => Err(Error::Invalid(format!(
                "unknown backend {other:?}; use auto, cpu or gpu"
            ))),
        }
    }
}

/// GPU implementation of [`Backend`].
pub struct GpuBackend {
    ctx: Arc<GpuContext>,
    accumulate: accumulate::Kernels,
    robust: robust_accumulate::RobustKernels,
    colourise: colourise::ColouriseKernel,
    /// Compiled on first use: most runs never edit.
    edit: OnceLock<edit::EditKernels>,
    sharpness: sharpness::Sharpness,
    score: score::ScoreKernels,
    points: points::PointKernels,
    stacking: stacking::StackKernels,
    registration: registration::RegisterKernels,
    /// Device memory for raw frames kept between stages, bytes; `u64::MAX`
    /// means "choose from the adapter".
    resident_budget: AtomicU64,
}

impl GpuBackend {
    /// Create a backend on an adapter chosen by `request`.
    ///
    /// # Errors
    /// [`GpuError`] when no adapter or device is available.
    pub fn new(request: AdapterRequest) -> std::result::Result<Self, GpuError> {
        let ctx = Arc::new(GpuContext::new(request)?);
        kit::gpu_trace::attach(&ctx.device, &ctx.queue);
        Ok(Self {
            accumulate: accumulate::Kernels::new(&ctx),
            robust: robust_accumulate::RobustKernels::new(&ctx),
            colourise: colourise::ColouriseKernel::new(&ctx),
            edit: OnceLock::new(),
            sharpness: sharpness::Sharpness::new(&ctx),
            score: score::ScoreKernels::new(&ctx),
            points: points::PointKernels::new(&ctx),
            stacking: stacking::StackKernels::new(&ctx),
            registration: registration::RegisterKernels::new(&ctx),
            resident_budget: AtomicU64::new(u64::MAX),
            ctx,
        })
    }

    /// One backend per adapter request, shared by every caller that holds the
    /// returned `Arc`. A second caller gets the first one's device instead of
    /// creating another.
    ///
    /// A **software** adapter (lavapipe, WARP) is kept for the life of the
    /// process: opening and closing such devices over and over in one process
    /// hangs lavapipe (threads spinning for minutes, API tests that never
    /// finished; issue #37). A **hardware** adapter is held weakly:
    /// when the last holder drops it the device is torn down in order (see
    /// [`GpuContext`]) and the next caller opens a fresh one. Keeping an NVIDIA
    /// device alive to process exit makes the driver kill the process with a
    /// signal on the way out (issue #31), which a command-line run
    /// must not do. A failed open is not remembered, so a later call tries again.
    ///
    /// # Errors
    /// [`GpuError`] when no adapter or device is available.
    pub fn shared(request: AdapterRequest) -> std::result::Result<Arc<Self>, GpuError> {
        type Registry = Mutex<HashMap<AdapterRequest, (Weak<GpuBackend>, Option<Arc<GpuBackend>>)>>;
        static SHARED: std::sync::OnceLock<Registry> = std::sync::OnceLock::new();
        let mut map = SHARED
            .get_or_init(Registry::default)
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(live) = map.get(&request).and_then(|(weak, _)| weak.upgrade()) {
            return Ok(live);
        }
        let backend = Arc::new(Self::new(request)?);
        let keep = backend.is_software().then(|| Arc::clone(&backend));
        map.insert(request, (Arc::downgrade(&backend), keep));
        Ok(backend)
    }

    /// Limit the device memory used to keep raw frames between stages to
    /// `bytes` (0 turns it off, so every stage reads and uploads the frames
    /// itself). Applies to captures first used after the call. The default is
    /// chosen from the adapter, or from `HELIA_GPU_RESIDENT_MB`.
    pub fn set_resident_budget(&self, bytes: Option<u64>) {
        self.resident_budget
            .store(bytes.unwrap_or(u64::MAX), Ordering::Relaxed);
    }

    /// The store of `frames` on this device, for the stages.
    fn resident(&self, frames: &dyn SharedFrames) -> Option<Arc<resident::Resident>> {
        let budget = self.resident_budget.load(Ordering::Relaxed);
        resident::for_source(&self.ctx, frames, (budget != u64::MAX).then_some(budget))
    }

    /// Adapter description: name, API and device type.
    #[must_use]
    pub fn adapter_summary(&self) -> String {
        self.ctx.summary()
    }

    /// Decode frames of `src` on the device and read them back as mono
    /// frames: the stored bytes are converted and demosaiced by the same
    /// kernel the stages use. For tests and tools; the stages keep frames on
    /// the device. `None` when the kernels cannot handle the frame size.
    ///
    /// # Errors
    /// Source or device failures.
    pub fn decode_frames(
        &self,
        src: &dyn SharedFrames,
        indices: &[usize],
        debayer: Debayer,
    ) -> Result<Option<Vec<Frame>>> {
        decode::decode_to_host(&self.ctx, src, indices, debayer)
    }

    /// True when the adapter is a software rasteriser (WARP, lavapipe, ...).
    #[must_use]
    pub fn is_software(&self) -> bool {
        self.ctx.is_software()
    }
}

impl Backend for GpuBackend {
    fn name(&self) -> String {
        format!("GPU ({})", self.ctx.summary())
    }

    fn sharpness(&self, frame: &Frame, metric: Metric, roi: Option<Rect>) -> Result<f32> {
        self.sharpness.score(&self.ctx, frame, metric, roi)
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
        Ok(Box::new(
            self.accumulate.start(&self.ctx, width, height, kernel)?,
        ))
    }

    fn robust_accumulator(
        &self,
        centre: &Frame,
        deposit: Deposit,
        kind: helia_core::stack::RobustKind,
        k: f32,
    ) -> Result<Box<dyn helia_core::RobustAccumulator>> {
        deposit.validate()?;
        let Deposit::Resample(kernel) = deposit else {
            return Err(Error::Unsupported("drizzle".into()));
        };
        Ok(Box::new(
            self.robust.start(&self.ctx, centre, kernel, kind, k)?,
        ))
    }

    fn supports(&self, stage: DeviceStage) -> bool {
        matches!(
            stage,
            DeviceStage::Score | DeviceStage::Register | DeviceStage::Points | DeviceStage::Stack
        )
    }

    fn score_frames(&self, req: &ScoreRequest<'_>) -> Result<Option<Vec<f32>>> {
        self.score.run(&self.ctx, req, self.resident(req.frames))
    }

    fn register_frames(&self, req: &RegisterRequest<'_>) -> Result<Option<Vec<Shift>>> {
        self.registration
            .run(&self.ctx, req, self.resident(req.frames))
    }

    fn measure_points(&self, req: &MeasureRequest<'_>) -> Result<Option<LocalTable>> {
        self.points.run(&self.ctx, req, self.resident(req.frames))
    }

    fn stack_frames(&self, req: &StackRequest<'_>) -> Result<Option<StackResult>> {
        self.stacking.run(&self.ctx, req, self.resident(req.frames))
    }

    fn release_frames(&self, frames: &dyn SharedFrames) {
        if let Some(cache) = frames.device_cache() {
            cache.clear();
        }
    }

    fn colourise(&self, frame: &Frame, lut: &ColourLut) -> Result<RgbImage> {
        match self.colourise.run(&self.ctx, frame, lut)? {
            Some(img) => Ok(img),
            None => Ok(helia_core::colour::apply_lut(frame, lut)),
        }
    }

    fn apply_edits(&self, frame: &Frame, edits: &EditList) -> Result<Frame> {
        let kernels = self.edit.get_or_init(|| edit::EditKernels::new(&self.ctx));
        match kernels.run(&self.ctx, frame, edits)? {
            Some(out) => Ok(out),
            None => helia_core::edit::apply_cpu(frame, edits),
        }
    }
}

/// The backend `auto` picks on an Apple GPU: alignment points on Metal, every
/// other stage on the CPU.
///
/// Measured on an Apple M1 (issue #203): Metal does the alignment point
/// stage in 0.5 to 0.6 of the CPU's time, but scoring, whole-frame alignment and
/// stacking are slower on it than on the eight CPU cores, because the GPU shares
/// the CPU's memory and has no upload to hide. This takes the one stage that wins.
/// `--backend gpu` still runs every stage on Metal.
struct PointsOnGpu {
    cpu: CpuBackend,
    gpu: Arc<GpuBackend>,
}

impl Backend for PointsOnGpu {
    fn name(&self) -> String {
        format!(
            "{}, alignment points on GPU ({})",
            self.cpu.name(),
            self.gpu.adapter_summary()
        )
    }

    fn sharpness(&self, frame: &Frame, metric: Metric, roi: Option<Rect>) -> Result<f32> {
        self.cpu.sharpness(frame, metric, roi)
    }

    fn accumulator(
        &self,
        width: usize,
        height: usize,
        deposit: Deposit,
    ) -> Result<Box<dyn Accumulator>> {
        self.cpu.accumulator(width, height, deposit)
    }

    fn robust_accumulator(
        &self,
        centre: &Frame,
        deposit: Deposit,
        kind: helia_core::stack::RobustKind,
        k: f32,
    ) -> Result<Box<dyn helia_core::RobustAccumulator>> {
        self.cpu.robust_accumulator(centre, deposit, kind, k)
    }

    fn supports(&self, stage: DeviceStage) -> bool {
        stage == DeviceStage::Points
    }

    fn measure_points(&self, req: &MeasureRequest<'_>) -> Result<Option<LocalTable>> {
        self.gpu.measure_points(req)
    }

    fn release_frames(&self, frames: &dyn SharedFrames) {
        self.gpu.release_frames(frames);
    }

    fn colourise(&self, frame: &Frame, lut: &ColourLut) -> Result<RgbImage> {
        self.gpu.colourise(frame, lut)
    }

    fn apply_edits(&self, frame: &Frame, edits: &EditList) -> Result<Frame> {
        self.gpu.apply_edits(frame, edits)
    }
}

/// Pick a backend. `Auto` uses a hardware GPU when there is one and the CPU
/// otherwise; a software adapter is slower than the CPU path, so only
/// `Gpu` will use it. On an Apple GPU `Auto` gives the GPU only the alignment
/// points (`PointsOnGpu`).
///
/// # Errors
/// [`Error::Backend`] for `Gpu` when no adapter at all can be opened.
pub fn select_backend(choice: BackendChoice) -> Result<Arc<dyn Backend>> {
    match choice {
        BackendChoice::Cpu => Ok(Arc::new(CpuBackend)),
        BackendChoice::Auto => match GpuBackend::shared(AdapterRequest::Hardware) {
            // A software adapter is slower than the multi-threaded CPU path.
            Ok(g) if g.ctx.is_apple_integrated() => Ok(Arc::new(PointsOnGpu {
                cpu: CpuBackend,
                gpu: g,
            })),
            Ok(g) if !g.is_software() => Ok(g),
            _ => Ok(Arc::new(CpuBackend)),
        },
        BackendChoice::Gpu => {
            let g = GpuBackend::shared(AdapterRequest::Hardware)
                .or_else(|_| GpuBackend::shared(AdapterRequest::Software))
                .map_err(|e| Error::Backend(e.to_string()))?;
            Ok(g)
        }
    }
}
