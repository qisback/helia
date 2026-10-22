// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! CPU-versus-GPU parity.
//!
//! `HELIA_GPU_TESTS` selects the adapter:
//!
//! * `software`: wgpu's fallback adapter (WARP on Windows, lavapipe on Linux);
//!   the tests fail if there is none, so CI cannot silently skip them.
//! * `hardware`: the normal high-performance adapter; also fails if absent.
//! * unset or `none`: skip with a note (the tests still compile).
//!
//! Tolerances: accumulated sums within 1e-4 absolute per unit weight (the CPU
//! and GPU use identical tap weights, so the difference is float contraction
//! only), weight maps exactly equal, sharpness within 2e-3 relative (the GPU
//! sums per workgroup in f32, the CPU in f64).

use std::io::Cursor;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Once};

use helia_core::align::global::{PhaseCorrelator, PhaseOptions};
use helia_core::align::method::{AlignMethod, GlobalMethod};
use helia_core::align::multipoint::{
    ApGrid, ApMeasure, LocalTable, MultipointConfig, fill_missing, measure,
};
use helia_core::debayer::{Channel, Debayer, Method};
use helia_core::io::avi::write_test_avi;
use helia_core::io::ser::{ColorId, SerHeader, SerWriter};
use helia_core::io::source::{FrameSource, MemorySource, OpenOptions, open_capture};
use helia_core::pipeline::{Pipeline, StackConfig, Stage};
use helia_core::profile::Profile;
use helia_core::resample::resample_region;
use helia_core::stack::Deposit;
use helia_core::stack::TileStack;
use helia_core::testutil::{CaptureConfig, SyntheticCapture, SyntheticConfig, SyntheticSun, rmse};
use helia_core::{
    Backend, BayerPattern, CpuBackend, DeviceStage, Frame, Keep, Kernel, MeasureRequest, Metric,
    Rect, RegisterRequest, ScoreRequest, Shift, StackRequest, StackResult, quality,
};
use helia_gpu::{AdapterRequest, BackendChoice, GpuBackend, select_backend};

const SUM_TOL: f32 = 1e-4;
const SHARP_REL_TOL: f32 = 2e-3;

/// The process-wide device, shared by the tests running at once; `GpuBackend::shared`
/// opens it once and keeps it for the life of the process.
fn gpu() -> Option<Arc<GpuBackend>> {
    static ANNOUNCE: Once = Once::new();
    let (mode, request) = test_mode()?;
    let g = GpuBackend::shared(request)
        .unwrap_or_else(|e| panic!("HELIA_GPU_TESTS={mode} but no adapter could be opened: {e}"));
    ANNOUNCE.call_once(|| eprintln!("GPU tests on: {} [{request:?}]", g.adapter_summary()));
    Some(g)
}

/// Delegates to the shared device so a pipeline can own an `Arc<dyn Backend>`.
struct Shared(Arc<GpuBackend>);

impl Backend for Shared {
    fn name(&self) -> String {
        self.0.name()
    }
    fn sharpness(
        &self,
        frame: &Frame,
        metric: Metric,
        roi: Option<Rect>,
    ) -> helia_core::Result<f32> {
        self.0.sharpness(frame, metric, roi)
    }
    fn accumulator(
        &self,
        width: usize,
        height: usize,
        deposit: Deposit,
    ) -> helia_core::Result<Box<dyn helia_core::Accumulator>> {
        self.0.accumulator(width, height, deposit)
    }
    fn score_frames(&self, req: &ScoreRequest<'_>) -> helia_core::Result<Option<Vec<f32>>> {
        self.0.score_frames(req)
    }
    fn measure_points(&self, req: &MeasureRequest<'_>) -> helia_core::Result<Option<LocalTable>> {
        self.0.measure_points(req)
    }
    fn stack_frames(&self, req: &StackRequest<'_>) -> helia_core::Result<Option<StackResult>> {
        self.0.stack_frames(req)
    }
    fn register_frames(&self, req: &RegisterRequest<'_>) -> helia_core::Result<Option<Vec<Shift>>> {
        self.0.register_frames(req)
    }
    fn colourise(
        &self,
        frame: &Frame,
        lut: &helia_core::ColourLut,
    ) -> helia_core::Result<helia_core::RgbImage> {
        self.0.colourise(frame, lut)
    }
    fn apply_edits(
        &self,
        frame: &Frame,
        edits: &helia_core::EditList,
    ) -> helia_core::Result<Frame> {
        self.0.apply_edits(frame, edits)
    }
    fn supports(&self, stage: DeviceStage) -> bool {
        self.0.supports(stage)
    }
}

fn test_mode() -> Option<(String, AdapterRequest)> {
    let mode = std::env::var("HELIA_GPU_TESTS")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let request = match mode.as_str() {
        "" | "none" => {
            eprintln!("HELIA_GPU_TESTS is unset or 'none': skipping GPU tests");
            return None;
        }
        "software" => AdapterRequest::Software,
        "hardware" => AdapterRequest::Hardware,
        other => panic!("HELIA_GPU_TESTS={other:?}: expected software, hardware or none"),
    };
    Some((mode, request))
}

fn scene(w: usize, h: usize) -> (Frame, SyntheticCapture) {
    let cfg = SyntheticConfig {
        width: w,
        height: h,
        ..SyntheticConfig::default()
    };
    let sun = SyntheticSun::new(&cfg);
    let cap = SyntheticCapture::generate(
        &sun,
        &CaptureConfig {
            frames: 12,
            ..CaptureConfig::default()
        },
    );
    (sun.render_clean(Shift::default(), None), cap)
}

#[test]
fn accumulate_matches_cpu() {
    let Some(gpu) = gpu() else { return };
    // Odd size so the last workgroups are partial.
    let (_, cap) = scene(201, 143);
    let shifts = [
        Shift::new(0.0, 0.0),
        Shift::new(2.5, -1.25),
        Shift::new(-3.7, 4.1),
        Shift::new(0.333, 0.667),
        Shift::new(-0.5, 0.0),
    ];
    for kernel in [Kernel::Bilinear, Kernel::Lanczos3] {
        let mut a = CpuBackend
            .accumulator(201, 143, Deposit::Resample(kernel))
            .expect("cpu acc");
        let mut b = gpu
            .accumulator(201, 143, Deposit::Resample(kernel))
            .expect("gpu acc");
        let mut total_w = 0.0;
        for (i, s) in shifts.iter().enumerate() {
            let w = 0.5 + 0.25 * i as f32;
            total_w += w;
            a.add(&cap.frames[i], *s, w).expect("cpu add");
            b.add(&cap.frames[i], *s, w).expect("gpu add");
        }
        let (a, b) = (a.finish().expect("cpu"), b.finish().expect("gpu"));
        let worst_sum = a
            .sum
            .iter()
            .zip(&b.sum)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0f32, f32::max);
        let weight_mismatch = a
            .weight
            .iter()
            .zip(&b.weight)
            .filter(|(x, y)| x.to_bits() != y.to_bits())
            .count();
        eprintln!(
            "accumulate {kernel:?}: worst sum diff {worst_sum:.2e} (total weight {total_w}), weight mismatches {weight_mismatch}"
        );
        assert_eq!(weight_mismatch, 0, "{kernel:?}: validity maps differ");
        assert!(worst_sum <= SUM_TOL * total_w, "{kernel:?}: {worst_sum}");
    }
}

#[test]
fn sharpness_matches_cpu() {
    let Some(gpu) = gpu() else { return };
    let (clean, cap) = scene(201, 143);
    let rois = [
        None,
        Some(Rect::new(30, 20, 100, 80)),
        Some(Rect::new(0, 0, 17, 9)),
        Some(Rect::new(190, 130, 40, 40)),
    ];
    let mut worst = 0.0f32;
    for frame in [&clean, &cap.frames[0], &cap.frames[7]] {
        for metric in [Metric::GradientEnergy, Metric::LaplacianVariance] {
            for roi in rois {
                let c = CpuBackend.sharpness(frame, metric, roi).expect("cpu");
                let g = gpu.sharpness(frame, metric, roi).expect("gpu");
                let rel = (c - g).abs() / c.abs().max(1e-9);
                worst = worst.max(if c.abs() < 1e-12 { (c - g).abs() } else { rel });
                assert!(
                    (c.abs() < 1e-12 && g.abs() < 1e-6) || rel <= SHARP_REL_TOL,
                    "{metric:?} {roi:?}: cpu {c} gpu {g} rel {rel}"
                );
            }
        }
    }
    eprintln!("sharpness: worst relative difference {worst:.2e}");
    // Ranking must agree, which is what the pipeline actually uses.
    let cpu: Vec<f32> = cap
        .frames
        .iter()
        .map(|f| quality::score(f, Metric::GradientEnergy, None))
        .collect();
    let gp: Vec<f32> = cap
        .frames
        .iter()
        .map(|f| gpu.sharpness(f, Metric::GradientEnergy, None).expect("gpu"))
        .collect();
    assert_eq!(quality::rank(&cpu), quality::rank(&gp));
}

/// The metrics that exist only on the CPU are declined by the device, so the
/// pipeline scores those frames with the CPU code.
#[test]
fn cpu_only_metrics_are_declined() {
    let Some(gpu) = gpu() else { return };
    let (clean, _) = scene(64, 64);
    for metric in Metric::ALL.into_iter().filter(|m| !m.on_gpu()) {
        assert!(
            matches!(
                gpu.sharpness(&clean, metric, None),
                Err(helia_core::Error::Unsupported(_))
            ),
            "{metric:?} should be declined"
        );
    }
}

/// Tolerance of the robust two-pass result against the CPU specification, as
/// an absolute difference in image units (the data span 0 to about 1).
const ROBUST_TOL: f32 = 2e-4;

#[test]
fn robust_accumulate_matches_cpu() {
    use helia_core::stack::RobustKind;
    let Some(gpu) = gpu() else { return };
    let (centre, mut cap) = scene(201, 143);
    // Contaminate: hot pixels and a streak in a few frames.
    for (n, f) in cap.frames.iter_mut().enumerate().take(6) {
        for i in 0..40 {
            let x = (i * 37 + n * 11) % 201;
            let y = (i * 53 + n * 7) % 143;
            f.data[y * 201 + x] = 4.0;
        }
        if n == 2 {
            for x in 0..201 {
                f.data[70 * 201 + x] = 3.0;
            }
        }
    }
    let shifts = [
        Shift::new(0.0, 0.0),
        Shift::new(2.5, -1.25),
        Shift::new(-3.7, 4.1),
        Shift::new(0.333, 0.667),
        Shift::new(-0.5, 0.0),
        Shift::new(1.0, 1.0),
        Shift::new(-1.2, 0.4),
        Shift::new(0.1, -0.9),
    ];
    for kind in [RobustKind::Sigma, RobustKind::Huber, RobustKind::Winsor] {
        for kernel in [Kernel::Bilinear, Kernel::Lanczos3] {
            let k = kind.default_k();
            let dep = Deposit::Resample(kernel);
            let mut a = CpuBackend
                .robust_accumulator(&centre, dep, kind, k)
                .expect("cpu");
            let mut b = gpu.robust_accumulator(&centre, dep, kind, k).expect("gpu");
            for pass in 0..2 {
                if pass == 1 {
                    a.begin_reject().expect("cpu begin");
                    b.begin_reject().expect("gpu begin");
                }
                for (i, s) in shifts.iter().enumerate() {
                    let w = 0.5 + 0.25 * i as f32;
                    if pass == 0 {
                        a.add_stats(&cap.frames[i], *s, w).expect("cpu");
                        b.add_stats(&cap.frames[i], *s, w).expect("gpu");
                    } else {
                        a.add_robust(&cap.frames[i], *s, w).expect("cpu");
                        b.add_robust(&cap.frames[i], *s, w).expect("gpu");
                    }
                }
            }
            let (a, b) = (a.finish().expect("cpu"), b.finish().expect("gpu"));
            let worst = worst_diff(&a, &b);
            // The instrument: the rule must have changed the result, else
            // agreement proves nothing.
            let mut plain = CpuBackend.accumulator(201, 143, dep).expect("plain");
            for (i, s) in shifts.iter().enumerate() {
                plain
                    .add(&cap.frames[i], *s, 0.5 + 0.25 * i as f32)
                    .expect("plain add");
            }
            let plain = plain.finish().expect("plain").mean(&centre);
            let effect = worst_diff(&a, &plain);
            assert!(effect > 1e-2, "{kind:?} {kernel:?}: no effect ({effect})");
            eprintln!(
                "robust {kind:?} {kernel:?}: worst diff {worst:.2e}, effect of the rule {effect:.2e}"
            );
            assert!(worst <= ROBUST_TOL, "{kind:?} {kernel:?}: {worst}");
        }
    }
}

#[test]
fn pipeline_on_gpu_matches_cpu() {
    let Some(gpu) = gpu() else { return };
    let (_, cap) = scene(192, 160);
    // Phase correlation, so the device registers the frames.
    let cfg = StackConfig {
        keep: Keep::Fraction(0.5),
        multipoint: None,
        normalise: false,
        kernel: Kernel::Lanczos3,
        align: AlignMethod::Phase,
        ..StackConfig::default()
    };
    let run = |backend: std::sync::Arc<dyn Backend>| {
        let mut src = MemorySource(cap.frames.clone());
        Pipeline::new(cfg.clone())
            .with_backend(backend)
            .run(&mut src, &|_: Stage, _| {}, &AtomicBool::new(false))
            .expect("run")
    };
    let cpu = run(std::sync::Arc::new(CpuBackend));
    let g = run(std::sync::Arc::new(Shared(gpu)));
    let diff = rmse(&cpu.image, &g.image, 0);
    eprintln!(
        "pipeline: cpu vs gpu stack rmse {diff:.2e}; backend {}",
        g.backend
    );
    assert!(diff < 1e-4, "{diff}");
    assert_eq!(cpu.ranking, g.ranking);
    // The device returns no confidence, so a few frames are re-measured on the
    // CPU and the report says so; the CPU run measures every frame.
    assert!(g.alignment.sampled && !cpu.alignment.sampled);
    assert_eq!(g.alignment.method, GlobalMethod::Phase);
    assert!(g.alignment.confidence_mean.is_some_and(|c| c > 0.5));
    assert_eq!(g.alignment.frames, cpu.alignment.frames);
}

#[test]
fn limb_alignment_gives_the_same_answer_on_either_backend() {
    let Some(gpu) = gpu() else { return };
    let (_, cap) = scene(192, 160);
    let cfg = StackConfig {
        keep: Keep::Fraction(0.5),
        multipoint: None,
        normalise: false,
        align: AlignMethod::Limb,
        ..StackConfig::default()
    };
    let run = |backend: std::sync::Arc<dyn Backend>| {
        let mut src = MemorySource(cap.frames.clone());
        Pipeline::new(cfg.clone())
            .with_backend(backend)
            .run(&mut src, &|_: Stage, _| {}, &AtomicBool::new(false))
            .expect("run")
    };
    let cpu = run(std::sync::Arc::new(CpuBackend));
    let g = run(std::sync::Arc::new(Shared(gpu)));
    assert_eq!(g.alignment.method, GlobalMethod::Limb);
    // The limb fit runs on the CPU either way, so the shifts are identical.
    assert_eq!(cpu.shifts, g.shifts);
    assert!(!g.alignment.sampled);
}

#[test]
fn backend_selection_never_fails_for_cpu_and_auto() {
    assert!(select_backend(BackendChoice::Cpu).is_ok());
    // Auto must succeed with or without a GPU.
    let b = select_backend(BackendChoice::Auto).expect("auto");
    eprintln!("auto selected: {}", b.name());
    assert!("gpu".parse::<BackendChoice>().is_ok() && "x".parse::<BackendChoice>().is_err());
}

#[test]
fn drizzle_is_unsupported_on_gpu() {
    let Some(gpu) = gpu() else { return };
    let r = gpu.accumulator(
        8,
        8,
        Deposit::Drizzle {
            scale: 2.0,
            pixfrac: 0.8,
        },
    );
    assert!(matches!(r, Err(helia_core::Error::Unsupported(_))));
}

/// A capture file under the temp dir, removed on drop.
struct TempCapture(std::path::PathBuf);

impl TempCapture {
    fn new(name: &str, bytes: &[u8]) -> Self {
        let path =
            std::env::temp_dir().join(format!("helia-gpu-test-{}-{name}", std::process::id()));
        std::fs::write(&path, bytes).expect("write temp capture");
        Self(path)
    }
}

impl Drop for TempCapture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Textured frames with distinct content so every stencil tap matters.
fn textured(n: usize, w: usize, h: usize) -> Vec<Frame> {
    let sun = SyntheticSun::new(&SyntheticConfig {
        width: w,
        height: h,
        ..SyntheticConfig::default()
    });
    let cap = SyntheticCapture::generate(
        &sun,
        &CaptureConfig {
            frames: n,
            noise: 0.02,
            ..CaptureConfig::default()
        },
    );
    cap.frames
}

fn ser_file(color: ColorId, depth: u32, little: bool, frames: &[Frame]) -> Vec<u8> {
    let mut hdr = SerHeader::new(
        frames[0].width as u32,
        frames[0].height as u32,
        color,
        depth,
    );
    hdr.little_endian = little;
    let mut w = SerWriter::new(Cursor::new(Vec::new()), hdr).expect("header");
    for f in frames {
        w.write_frame(f, None).expect("frame");
    }
    w.finish().expect("finish").into_inner()
}

/// A three-plane SER file (the writer only does mono and Bayer): a mono file's
/// header with the colour id patched, then interleaved planes derived from each frame.
fn rgb_ser(depth: u32, little: bool, frames: &[Frame]) -> Vec<u8> {
    let mut bytes = ser_file(ColorId::Mono, depth, little, &frames[..1])[..178].to_vec();
    bytes[18..22].copy_from_slice(&100i32.to_le_bytes());
    bytes[38..42].copy_from_slice(&i32::try_from(frames.len()).expect("count").to_le_bytes());
    for f in frames {
        for &v in &f.data {
            for scale in [1.0f32, 0.6, 0.3] {
                let q = (v * scale).clamp(0.0, 1.0);
                if depth == 8 {
                    bytes.push((q * 255.0).round() as u8);
                } else {
                    let q = (q * 65535.0).round() as u16;
                    bytes.extend_from_slice(&if little {
                        q.to_le_bytes()
                    } else {
                        q.to_be_bytes()
                    });
                }
            }
        }
    }
    bytes
}

fn worst_diff(a: &Frame, b: &Frame) -> f32 {
    a.data
        .iter()
        .zip(&b.data)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max)
}

/// Decoded frames agree with the CPU decoder to float rounding.
const DECODE_TOL: f32 = 2e-6;

#[test]
fn decode_matches_cpu_for_every_layout() {
    let Some(gpu) = gpu() else { return };
    // Odd size: partial workgroups and an odd Bayer row.
    let frames = textured(5, 67, 45);
    let rggb = ColorId::Bayer(BayerPattern::Rggb);
    let mut cases: Vec<(String, TempCapture, OpenOptions)> = Vec::new();
    for (name, color, depth, little) in [
        ("mono8", ColorId::Mono, 8, true),
        ("mono16le", ColorId::Mono, 16, true),
        ("mono16be", ColorId::Mono, 16, false),
        ("bayer8", rggb, 8, true),
        ("bayer16", rggb, 16, true),
        ("bggr16be", ColorId::Bayer(BayerPattern::Bggr), 16, false),
    ] {
        cases.push((
            name.into(),
            TempCapture::new(
                &format!("{name}.ser"),
                &ser_file(color, depth, little, &frames),
            ),
            OpenOptions::default(),
        ));
    }
    for (name, depth, little) in [
        ("rgb8", 8, true),
        ("rgb16le", 16, true),
        ("rgb16be", 16, false),
    ] {
        cases.push((
            name.into(),
            TempCapture::new(&format!("{name}.ser"), &rgb_ser(depth, little, &frames)),
            OpenOptions::default(),
        ));
    }
    for (fourcc, bits) in [(b"DIB ", 8u16), (b"Y16 ", 16), (b"DIB ", 24)] {
        for bottom_up in [false, true] {
            let bytes = write_test_avi(&frames, fourcc, bits, bottom_up, Some(3));
            cases.push((
                format!("avi{bits}{bottom_up}"),
                TempCapture::new(&format!("{bits}-{bottom_up}.avi"), &bytes),
                OpenOptions {
                    bayer: helia_core::io::source::BayerSetting::Pattern(BayerPattern::Grbg),
                    ..OpenOptions::default()
                },
            ));
        }
    }
    let mut worst = 0.0f32;
    for (name, file, opts) in &cases {
        let src = open_capture(&file.0, opts).expect("open");
        let shared = src.shared().expect("shared");
        for debayer in [
            Debayer::default(),
            Debayer {
                method: Method::Bilinear,
                channel: Channel::Red,
            },
            Debayer {
                method: Method::Mhc,
                channel: Channel::Green,
            },
        ] {
            let idx: Vec<usize> = (0..shared.frame_count()).rev().collect();
            let got = gpu
                .decode_frames(shared, &idx, debayer)
                .expect("gpu decode")
                .expect("supported size");
            for (g, &i) in got.iter().zip(&idx) {
                let want = shared.read_mono(i, &debayer, &Profile::new()).expect("cpu");
                let d = worst_diff(g, &want);
                worst = worst.max(d);
                assert!(d <= DECODE_TOL, "{name} {debayer:?} frame {i}: {d}");
            }
        }
    }
    // In-memory frames: f32 bytes, with and without a Bayer tag.
    for bayer in [None, Some(BayerPattern::Gbrg)] {
        let mem = MemorySource(
            frames
                .iter()
                .cloned()
                .map(|f| f.with_bayer(bayer))
                .collect(),
        );
        let shared = mem.shared().expect("shared");
        let d = Debayer::default();
        let got = gpu
            .decode_frames(shared, &[0, 3, 4], d)
            .expect("gpu")
            .expect("supported");
        for (g, &i) in got.iter().zip(&[0usize, 3, 4]) {
            let want = shared.read_mono(i, &d, &Profile::new()).expect("cpu");
            worst = worst.max(worst_diff(g, &want));
            assert!(worst_diff(g, &want) <= DECODE_TOL, "memory {bayer:?} {i}");
        }
    }
    eprintln!("decode: worst difference from the CPU decoder {worst:.2e}");
}

#[test]
fn score_stage_matches_cpu_over_batches_and_regions() {
    let Some(gpu) = gpu() else { return };
    // More frames than one batch, an odd size, a Bayer capture.
    let frames = textured(40, 129, 97);
    let bytes = ser_file(ColorId::Bayer(BayerPattern::Rggb), 16, true, &frames);
    let file = TempCapture::new("score.ser", &bytes);
    let src = open_capture(&file.0, &OpenOptions::default()).expect("open");
    let shared = src.shared().expect("shared");
    let indices: Vec<usize> = (0..40).rev().collect();
    let debayer = Debayer::default();
    let cpu_frames: Vec<Frame> = indices
        .iter()
        .map(|&i| shared.read_mono(i, &debayer, &Profile::new()).expect("cpu"))
        .collect();
    let rois = [
        None,
        Some(Rect::new(20, 10, 60, 50)),
        Some(Rect::new(100, 80, 60, 60)),
    ];
    let mut worst = 0.0f32;
    for metric in [Metric::GradientEnergy, Metric::LaplacianVariance] {
        for roi in rois {
            let cancel = AtomicBool::new(false);
            let profile = Profile::new();
            let seen = std::cell::RefCell::new(Vec::new());
            let progress = |f: f32| seen.borrow_mut().push(f);
            let req = ScoreRequest {
                frames: shared,
                indices: &indices,
                debayer,
                metric,
                roi,
                progress: &progress,
                cancel: &cancel,
                profile: &profile,
            };
            let got = gpu
                .score_frames(&req)
                .expect("score")
                .expect("handled by the GPU");
            let want: Vec<f32> = cpu_frames
                .iter()
                .map(|f| quality::score(f, metric, roi))
                .collect();
            for (g, c) in got.iter().zip(&want) {
                let rel = (g - c).abs() / c.abs().max(1e-9);
                worst = worst.max(rel);
                assert!(
                    rel <= SHARP_REL_TOL,
                    "{metric:?} {roi:?}: gpu {g} cpu {c} rel {rel}"
                );
            }
            let p = seen.into_inner();
            assert!(p.windows(2).all(|w| w[0] <= w[1]) && (p.last().copied() == Some(1.0)));
            if metric == Metric::GradientEnergy && roi.is_none() {
                assert_eq!(quality::rank(&got), quality::rank(&want));
            }
        }
    }
    eprintln!("score stage: worst relative difference {worst:.2e}");
}

#[test]
fn score_stage_honours_cancellation() {
    let Some(gpu) = gpu() else { return };
    let mem = MemorySource(textured(20, 64, 48));
    let cancel = AtomicBool::new(true);
    let profile = Profile::new();
    let req = ScoreRequest {
        frames: mem.shared().expect("shared"),
        indices: &(0..20).collect::<Vec<_>>(),
        debayer: Debayer::default(),
        metric: Metric::GradientEnergy,
        roi: None,
        progress: &|_| {},
        cancel: &cancel,
        profile: &profile,
    };
    assert!(matches!(
        gpu.score_frames(&req),
        Err(helia_core::Error::Cancelled)
    ));
}

/// Alignment point measurements: the device table against the CPU's rows.
fn compare_points(cfg: MultipointConfig, frames: Vec<Frame>, shifts: &[Shift], reference: &Frame) {
    let gpu = gpu().expect("gpu");
    let grid = ApGrid::build(reference, &cfg).expect("grid");
    let templates = grid.templates(reference);
    let n_aps = grid.aps.len();
    let mem = MemorySource(frames);
    let indices: Vec<usize> = (0..mem.0.len()).collect();
    let cancel = AtomicBool::new(false);
    let profile = Profile::new();
    let req = MeasureRequest {
        frames: mem.shared().expect("shared"),
        indices: &indices,
        debayer: Debayer::default(),
        shifts,
        grid: &grid,
        templates: &templates,
        config: &cfg,
        progress: &|_| {},
        cancel: &cancel,
        profile: &profile,
    };
    let table = gpu
        .measure_points(&req)
        .expect("measure")
        .expect("handled by the GPU");
    let (mut compared, mut flips, mut worst_shift, mut worst_quality) =
        (0usize, 0usize, 0.0f32, 0.0f32);
    let mut measured = 0usize;
    for (i, f) in mem.0.iter().enumerate() {
        let prep = grid.prepare(f);
        let mut row: Vec<Option<ApMeasure>> = templates
            .iter()
            .map(|t| t.as_ref().and_then(|t| measure(t, &prep, shifts[i], &cfg)))
            .collect();
        measured += row.iter().filter(|m| m.is_some()).count();
        fill_missing(&grid, &templates, &mut row, &prep, shifts[i]);
        for (a, cpu) in row.iter().enumerate() {
            let (gq, gs) = (table.quality[i * n_aps + a], table.shifts[i * n_aps + a]);
            match cpu {
                Some(c) if gq.is_finite() => {
                    let dq = (gq - c.quality).abs() / c.quality.abs().max(1e-9);
                    let ds = (gs - c.shift).length();
                    compared += 1;
                    worst_shift = worst_shift.max(ds);
                    worst_quality = worst_quality.max(dq);
                }
                None if !gq.is_finite() => {}
                _ => flips += 1,
            }
        }
    }
    eprintln!(
        "points: {compared} compared ({measured} measured on the CPU), {flips} status flips, worst shift difference {worst_shift:.4} px, worst relative quality difference {worst_quality:.2e}"
    );
    assert!(compared > 100, "too few points compared: {compared}");
    // Surfaces differ by float rounding only, so a point near a threshold
    // may flip; that must be rare, and agreeing points must agree closely.
    assert!(flips * 50 <= compared, "{flips} flips of {compared}");
    assert!(worst_shift < 0.05, "shift {worst_shift}");
    assert!(worst_quality < 5e-3, "quality {worst_quality}");
}

#[test]
fn alignment_points_match_cpu() {
    if gpu().is_none() {
        return;
    }
    let sun = SyntheticSun::new(&SyntheticConfig::default());
    let reference = sun.render_clean(Shift::default(), None);
    let cap = SyntheticCapture::generate(
        &sun,
        &CaptureConfig {
            frames: 11,
            warp_amp: 1.5,
            warp_wavelength: 70.0,
            warp_floor: 0.5,
            warp_free: 0,
            noise: 0.02,
            ..CaptureConfig::default()
        },
    );
    let cfg = MultipointConfig {
        ap_size: 32,
        min_contrast: 0.01,
        ..MultipointConfig::default()
    };
    compare_points(cfg, cap.frames.clone(), &cap.shifts, &reference);
    // No smoothing or background removal, a bigger search and window.
    let cfg = MultipointConfig {
        ap_size: 48,
        min_contrast: 0.01,
        smooth: 0.0,
        highpass: 0.0,
        search_radius: 6,
        ..MultipointConfig::default()
    };
    compare_points(cfg, cap.frames, &cap.shifts, &reference);
}

/// The paths the device now decides: a peak on the border of the search area,
/// a correlation below the threshold, an undefined neighbour, and searches whose
/// side is not a multiple of three. The CPU is the oracle.
#[test]
fn alignment_points_match_cpu_on_rejection_paths() {
    if gpu().is_none() {
        return;
    }
    let sun = SyntheticSun::new(&SyntheticConfig::default());
    let reference = sun.render_clean(Shift::default(), None);
    // Large warps push peaks to the border of a small search; noise lowers the
    // correlation under a high threshold.
    for (warp, noise, radius, min_correlation) in [
        (3.0, 0.02, 2usize, 0.3f32),
        (1.5, 0.08, 4, 0.8),
        (2.0, 0.04, 7, 0.5),
        (1.5, 0.02, 1, 0.3),
    ] {
        let cap = SyntheticCapture::generate(
            &sun,
            &CaptureConfig {
                frames: 9,
                warp_amp: warp,
                warp_wavelength: 60.0,
                warp_floor: 0.5,
                warp_free: 0,
                noise,
                ..CaptureConfig::default()
            },
        );
        let cfg = MultipointConfig {
            ap_size: 32,
            min_contrast: 0.01,
            search_radius: radius,
            min_correlation,
            ..MultipointConfig::default()
        };
        compare_points(cfg, cap.frames, &cap.shifts, &reference);
    }
}

#[test]
#[allow(clippy::too_many_lines, reason = "one scenario with its CPU oracle")]
fn stacking_tiles_and_frame_match_cpu() {
    let Some(gpu) = gpu() else { return };
    let (w, h) = (131, 97);
    let sun = SyntheticSun::new(&SyntheticConfig {
        width: w,
        height: h,
        ..SyntheticConfig::default()
    });
    let reference = sun.render_clean(Shift::default(), None);
    let cap = SyntheticCapture::generate(
        &sun,
        &CaptureConfig {
            frames: 21,
            noise: 0.03,
            ..CaptureConfig::default()
        },
    );
    let grid = ApGrid::build(
        &reference,
        &MultipointConfig {
            ap_size: 32,
            min_contrast: 0.01,
            ..MultipointConfig::default()
        },
    )
    .expect("grid");
    let rects: Vec<Rect> = grid
        .aps
        .iter()
        .filter(|a| a.active)
        .map(|a| a.rect)
        .collect();
    let centres: Vec<Vec<f32>> = rects
        .iter()
        .map(|r| {
            (r.y0..r.y1)
                .flat_map(|y| reference.row(y)[r.x0..r.x1].to_vec())
                .collect()
        })
        .collect();
    let order: Vec<usize> = (0..21).rev().collect();
    // Deterministic shifts and weights, some deposits skipped; shifts large
    // enough that edge samples fall outside the frame.
    let tile_deposit = |k: usize, t: usize| {
        (!(k + 2 * t).is_multiple_of(5)).then(|| {
            (
                Shift::new(
                    0.31 * k as f32 - 0.17 * (t % 7) as f32 - 2.0,
                    0.13 * t as f32 - 0.29 * k as f32 + 1.5,
                ),
                0.2 + 0.07 * ((k * 3 + t) % 11) as f32,
            )
        })
    };
    let frame_deposit = |k: usize| {
        (k % 4 != 3).then(|| {
            (
                Shift::new(0.5 * k as f32 - 3.0, 4.1 - 0.7 * k as f32),
                0.5 + 0.1 * k as f32,
            )
        })
    };
    let mem = MemorySource(cap.frames.clone());
    for kernel in [Kernel::Lanczos3, Kernel::Bilinear] {
        let cancel = AtomicBool::new(false);
        let profile = Profile::new();
        let req = StackRequest {
            frames: mem.shared().expect("shared"),
            order: &order,
            debayer: Debayer::default(),
            width: w,
            height: h,
            kernel,
            tiles: &rects,
            centres: &centres,
            tile_deposit: &tile_deposit,
            frame_deposit: &frame_deposit,
            progress: &|_| {},
            cancel: &cancel,
            profile: &profile,
        };
        let got = gpu
            .stack_frames(&req)
            .expect("stack")
            .expect("handled by the GPU");

        // CPU oracle: the same deposits through TileStack and CpuAccumulator.
        let mut tiles: Vec<TileStack> = rects
            .iter()
            .zip(&centres)
            .map(|(r, c)| TileStack::new(*r, c.clone(), false))
            .collect();
        let mut acc = CpuBackend
            .accumulator(w, h, Deposit::Resample(kernel))
            .expect("acc");
        for (k, &i) in order.iter().enumerate() {
            let f = &cap.frames[i];
            if let Some((s, wt)) = frame_deposit(k) {
                acc.add(f, s, wt).expect("add");
            }
            for (t, tile) in tiles.iter_mut().enumerate() {
                if let Some((s, wt)) = tile_deposit(k, t) {
                    let mut buf = vec![0.0f32; tile.rect().area()];
                    resample_region(f, s, tile.rect(), kernel, &mut buf);
                    tile.add(&buf, wt);
                }
            }
        }
        let want_frame = acc.finish().expect("finish");
        let (mut worst_tile, mut nan_mismatch) = (0.0f32, 0usize);
        for (g, c) in got.tiles.iter().zip(tiles.iter().map(TileStack::finish)) {
            for (a, b) in g.iter().zip(&c) {
                if a.is_nan() != b.is_nan() {
                    nan_mismatch += 1;
                } else if !a.is_nan() {
                    worst_tile = worst_tile.max((a - b).abs());
                }
            }
        }
        let worst_sum = got
            .frame
            .sum
            .iter()
            .zip(&want_frame.sum)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        let weight_mismatch = got
            .frame
            .weight
            .iter()
            .zip(&want_frame.weight)
            .filter(|(a, b)| (**a - **b).abs() > 1e-5)
            .count();
        eprintln!(
            "stacking {kernel:?}: {} tiles, worst tile difference {worst_tile:.2e}, {nan_mismatch} validity mismatches, worst frame sum difference {worst_sum:.2e}, {weight_mismatch} weight mismatches",
            rects.len()
        );
        assert_eq!(nan_mismatch, 0);
        assert_eq!(weight_mismatch, 0);
        assert!(worst_tile < 2e-5, "tile {worst_tile}");
        assert!(worst_sum < 1e-3, "frame {worst_sum}");
    }
}

#[test]
fn pipeline_with_alignment_points_on_gpu_matches_cpu() {
    let Some(gpu) = gpu() else { return };
    let sun = SyntheticSun::new(&SyntheticConfig::default());
    let cap = SyntheticCapture::generate(
        &sun,
        &CaptureConfig {
            frames: 40,
            noise: 0.02,
            warp_amp: 1.5,
            warp_wavelength: 80.0,
            warp_floor: 0.5,
            warp_free: 0,
            ..CaptureConfig::default()
        },
    );
    let cfg = StackConfig {
        keep: Keep::Fraction(0.3),
        multipoint: Some(MultipointConfig {
            ap_size: 32,
            min_contrast: 0.01,
            ..MultipointConfig::default()
        }),
        normalise: false,
        align: AlignMethod::Phase,
        ..StackConfig::default()
    };
    let run = |backend: std::sync::Arc<dyn Backend>| {
        let mut src = MemorySource(cap.frames.clone());
        Pipeline::new(cfg.clone())
            .with_backend(backend)
            .run(&mut src, &|_: Stage, _| {}, &AtomicBool::new(false))
            .expect("run")
    };
    let cpu = run(std::sync::Arc::new(CpuBackend));
    let dev = run(std::sync::Arc::new(Shared(gpu)));
    let diff = rmse(&cpu.image, &dev.image, 0);
    eprintln!("pipeline with points: cpu vs gpu stack rmse {diff:.2e}");
    assert_eq!(cpu.ranking, dev.ranking);
    assert!(diff < 5e-4, "{diff}");
    let stages = |o: &helia_core::pipeline::StackOutput| {
        o.timings.iter().map(|(s, _)| *s).collect::<Vec<_>>()
    };
    assert_eq!(stages(&cpu), stages(&dev));
}

/// Registration shifts: the device's phase correlation against the CPU's.
const REGISTER_TOL: f32 = 5e-3;

#[test]
fn registration_matches_cpu_for_awkward_sizes() {
    let Some(gpu) = gpu() else { return };
    // Transform lengths with every radix: 135 = 5*3*3*3 (odd), 100 = 5*5*4,
    // 200 = 5*5*4*2, 150 = 5*5*3*2, 64 (pure radix 4), 96 = 4*4*3*2.
    for (w, h) in [(129usize, 97usize), (200, 150), (64, 96), (45, 27)] {
        let sun = SyntheticSun::new(&SyntheticConfig {
            width: w,
            height: h,
            ..SyntheticConfig::default()
        });
        let reference = sun.render_clean(Shift::default(), None);
        let cap = SyntheticCapture::generate(
            &sun,
            &CaptureConfig {
                frames: 19,
                noise: 0.02,
                ..CaptureConfig::default()
            },
        );
        let opts = PhaseOptions::default();
        let corr = PhaseCorrelator::new(&reference, opts).expect("cpu correlator");
        let mem = MemorySource(cap.frames.clone());
        let indices: Vec<usize> = (0..19).rev().collect();
        let cancel = AtomicBool::new(false);
        let profile = Profile::new();
        let req = RegisterRequest {
            frames: mem.shared().expect("shared"),
            indices: &indices,
            debayer: Debayer::default(),
            reference: &reference,
            phase: opts,
            progress: &|_| {},
            cancel: &cancel,
            profile: &profile,
        };
        let got = gpu
            .register_frames(&req)
            .expect("register")
            .expect("handled by the GPU");
        let mut worst = 0.0f32;
        for (g, &i) in got.iter().zip(&indices) {
            let want = corr.estimate(&cap.frames[i]).expect("cpu").shift;
            let d = (*g - want).length();
            worst = worst.max(d);
            assert!(
                d < REGISTER_TOL,
                "{w}x{h} frame {i}: gpu {g:?} cpu {want:?}"
            );
        }
        eprintln!("registration {w}x{h}: worst shift difference {worst:.2e} px");
    }
}

/// Every stage's answer, as bits, for one run of the pipeline's device stages
/// over a file-backed capture with the raw-frame store limited to `budget`.
#[allow(
    clippy::too_many_lines,
    reason = "one scenario through all four stages"
)]
fn staged_answers(gpu: &GpuBackend, path: &std::path::Path, budget: Option<u64>) -> Vec<Vec<u32>> {
    gpu.set_resident_budget(budget);
    let src = open_capture(path, &OpenOptions::default()).expect("open");
    let shared = src.shared().expect("shared");
    let n = shared.frame_count();
    let debayer = Debayer::default();
    let cancel = AtomicBool::new(false);
    let profile = Profile::new();
    let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<u32>>();
    let mut out = Vec::new();

    // Scoring sees every frame, in file order.
    let all: Vec<usize> = (0..n).collect();
    let scores = gpu
        .score_frames(&ScoreRequest {
            frames: shared,
            indices: &all,
            debayer,
            metric: Metric::GradientEnergy,
            roi: None,
            progress: &|_| {},
            cancel: &cancel,
            profile: &profile,
        })
        .expect("score")
        .expect("handled by the GPU");
    out.push(bits(&scores));

    // Later stages take a different, reordered subset, as ranking does, so a
    // small store has to evict frames scoring left in it.
    let kept: Vec<usize> = (0..n).rev().filter(|i| i % 3 != 1).collect();
    let reference = shared
        .read_mono(kept[0], &debayer, &Profile::new())
        .expect("reference");
    let shifts = gpu
        .register_frames(&RegisterRequest {
            frames: shared,
            indices: &kept,
            debayer,
            reference: &reference,
            phase: PhaseOptions::default(),
            progress: &|_| {},
            cancel: &cancel,
            profile: &profile,
        })
        .expect("register")
        .expect("handled by the GPU");
    out.push(shifts.iter().flat_map(|s| bits(&[s.dx, s.dy])).collect());

    let cfg = MultipointConfig {
        ap_size: 32,
        min_contrast: 0.01,
        ..MultipointConfig::default()
    };
    let grid = ApGrid::build(&reference, &cfg).expect("grid");
    let templates = grid.templates(&reference);
    let table = gpu
        .measure_points(&MeasureRequest {
            frames: shared,
            indices: &kept,
            debayer,
            shifts: &shifts,
            grid: &grid,
            templates: &templates,
            config: &cfg,
            progress: &|_| {},
            cancel: &cancel,
            profile: &profile,
        })
        .expect("measure")
        .expect("handled by the GPU");
    out.push(bits(&table.quality));
    out.push(
        table
            .shifts
            .iter()
            .flat_map(|s| bits(&[s.dx, s.dy]))
            .collect(),
    );

    let (w, h) = (reference.width, reference.height);
    let rects: Vec<Rect> = grid
        .aps
        .iter()
        .filter(|a| a.active)
        .map(|a| a.rect)
        .collect();
    let centres: Vec<Vec<f32>> = rects
        .iter()
        .map(|r| {
            (r.y0..r.y1)
                .flat_map(|y| reference.row(y)[r.x0..r.x1].to_vec())
                .collect()
        })
        .collect();
    let stack_order: Vec<usize> = kept.iter().copied().step_by(2).collect();
    let tile_deposit = |k: usize, t: usize| {
        Some((
            Shift::new(0.1 * k as f32 - 0.05 * t as f32, 0.2 - 0.03 * k as f32),
            1.0 + 0.1 * (k % 3) as f32,
        ))
    };
    let frame_deposit = |k: usize| Some((Shift::new(0.25 * k as f32 - 1.0, 0.5), 1.0));
    let stacked = gpu
        .stack_frames(&StackRequest {
            frames: shared,
            order: &stack_order,
            debayer,
            width: w,
            height: h,
            kernel: Kernel::Lanczos3,
            tiles: &rects,
            centres: &centres,
            tile_deposit: &tile_deposit,
            frame_deposit: &frame_deposit,
            progress: &|_| {},
            cancel: &cancel,
            profile: &profile,
        })
        .expect("stack")
        .expect("handled by the GPU");
    out.push(bits(&stacked.frame.sum));
    out.push(bits(&stacked.frame.weight));
    for t in &stacked.tiles {
        out.push(bits(t));
    }
    out
}

#[test]
fn frames_kept_on_the_device_change_no_answer() {
    let Some(gpu) = gpu() else { return };
    let frames = textured(40, 129, 97);
    let bytes = ser_file(ColorId::Bayer(BayerPattern::Rggb), 16, true, &frames);
    let file = TempCapture::new("resident.ser", &bytes);
    let frame_bytes = 129 * 97 * 2;
    let page = |k: u64| Some(k * (frame_bytes as u64).next_multiple_of(4));

    // Store off: every stage reads the file and uploads, as before.
    let want = staged_answers(&gpu, &file.0, Some(0));
    // The default store, one that holds everything, then ones that hold a
    // fraction (3, 12 and 1 frames of 40) and must evict and fall back.
    for budget in [None, page(40), page(12), page(3), page(1)] {
        let got = staged_answers(&gpu, &file.0, budget);
        assert_eq!(got.len(), want.len());
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            assert_eq!(g, w, "answer {i} differs with store budget {budget:?}");
        }
    }
    gpu.set_resident_budget(None);
}

/// Colourising: the kernel repeats the table interpolation of
/// `helia_core::colour::apply_lut`, so the two differ by float contraction
/// only. Tolerance 1e-5 per sample (16-bit output steps are 1.5e-5).
#[test]
fn colourise_matches_the_cpu_reference() {
    use helia_core::colour::{ColourParams, Preset, apply_lut};
    let Some(gpu) = gpu() else { return };
    // Every value of a 16-bit ramp, awkward sizes, and the bad samples the
    // host must sanitise.
    let (w, h) = (251, 97);
    let mut data: Vec<f32> = (0..w * h)
        .map(|i| (i % 65_536) as f32 / 65_535.0 * 1.1 - 0.05)
        .collect();
    data[0] = f32::NAN;
    data[1] = f32::INFINITY;
    data[2] = f32::NEG_INFINITY;
    let frame = Frame {
        width: w,
        height: h,
        data,
        bayer: None,
    };
    let mut worst = 0.0f32;
    // Graded looks go through the same table.
    for spec in [
        "hue=40,sat=1.4,gamma=1.3,black=0.05,white=0.9,r=1.2,b=0.8",
        "sat=0",
    ] {
        let grade = helia_core::grade::Grade::parse(spec).expect("grade");
        let lut = ColourParams {
            grade,
            ..ColourParams::preset(Preset::Ha)
        }
        .lut();
        let want = apply_lut(&frame, &lut);
        let got = gpu.colourise(&frame, &lut).expect("gpu colourise");
        assert_eq!(got.data.len(), want.data.len());
        for (a, b) in got.data.iter().zip(&want.data) {
            worst = worst.max((a - b).abs());
        }
    }
    for preset in Preset::ALL.into_iter().map(Some).chain([None]) {
        for invert in [false, true] {
            for strength in [1.0, 0.4] {
                let lut = ColourParams {
                    preset,
                    invert,
                    strength,
                    ..ColourParams::default()
                }
                .lut();
                let want = apply_lut(&frame, &lut);
                let got = gpu.colourise(&frame, &lut).expect("gpu colourise");
                assert_eq!((got.width, got.height), (w, h));
                assert_eq!(got.data.len(), want.data.len());
                for (g, c) in got.data.iter().zip(&want.data) {
                    worst = worst.max((g - c).abs());
                }
            }
        }
    }
    assert!(worst < 1e-5, "worst difference {worst:e}");
    eprintln!("colourise parity: worst difference {worst:e}");
}

/// Parity of one edit list: the device result against the CPU reference.
fn edits_match(gpu: &GpuBackend, what: &str, frame: &Frame, ops: Vec<helia_core::Op>, tol: f32) {
    let list = helia_core::EditList::new(ops);
    let want = CpuBackend.apply_edits(frame, &list).expect("cpu edit");
    let got = gpu.apply_edits(frame, &list).expect("gpu edit");
    assert_eq!(
        (got.width, got.height),
        (want.width, want.height),
        "{what}: size"
    );
    let worst = worst_diff(&got, &want);
    eprintln!("edit parity {what}: worst difference {worst:.2e} (tolerance {tol:.0e})");
    assert!(worst <= tol, "{what}: worst difference {worst:e}");
}

#[test]
fn tone_edits_match_the_cpu_reference() {
    use helia_core::Op;
    use helia_core::edit::{Curve, Denoise, Levels};
    let Some(gpu) = gpu() else { return };
    let (clean, _) = scene(201, 143);
    edits_match(
        &gpu,
        "levels",
        &clean,
        vec![Op::Levels(Levels {
            black: 0.05,
            white: 0.9,
            gamma: 1.7,
            out_black: 0.02,
            out_white: 0.97,
        })],
        2e-6,
    );
    edits_match(
        &gpu,
        "levels without gamma",
        &clean,
        vec![Op::Levels(Levels {
            black: 0.1,
            white: 0.8,
            ..Levels::default()
        })],
        2e-6,
    );
    edits_match(
        &gpu,
        "curve",
        &clean,
        vec![Op::Curve(Curve {
            points: vec![[0.2, 0.1], [0.5, 0.6], [0.8, 0.85]],
        })],
        2e-6,
    );
    let _ = Denoise::default();
}

#[test]
fn limb_darkening_flatten_matches_the_cpu_reference() {
    use helia_core::Op;
    use helia_core::edit::{Flatten, Levels, Unsharp};
    use helia_core::limbdark::{Law, Source};
    let Some(gpu) = gpu() else { return };
    let (clean, cap) = scene(201, 143);
    for (name, frame) in [("clean", &clean), ("noisy", &cap.frames[3])] {
        for (source, law) in [
            (Source::Model, Law::Quadratic),
            (Source::Model, Law::Neckel),
            (Source::Profile, Law::Quadratic),
        ] {
            edits_match(
                &gpu,
                &format!("flatten {} {}, {name}", source.id(), law.id()),
                frame,
                vec![Op::Flatten(Flatten::new(source, law))],
                2e-5,
            );
        }
    }
    // First in a list, then sharpening and levels on the flattened image.
    edits_match(
        &gpu,
        "flatten, unsharp, levels",
        &clean,
        vec![
            Op::Flatten(Flatten::new(Source::Model, Law::Quadratic)),
            Op::Unsharp(Unsharp::default()),
            Op::Levels(Levels {
                gamma: 1.2,
                ..Levels::default()
            }),
        ],
        1e-4,
    );
}

#[test]
fn unsharp_and_wavelet_edits_match_the_cpu_reference() {
    use helia_core::Op;
    use helia_core::edit::{Denoise, Layer, Unsharp, Wavelet};
    let Some(gpu) = gpu() else { return };
    let (clean, cap) = scene(201, 143);
    for (name, frame) in [("clean", &clean), ("noisy", &cap.frames[3])] {
        edits_match(
            &gpu,
            &format!("unsharp, {name}"),
            frame,
            vec![Op::Unsharp(Unsharp {
                radius: 2.0,
                amount: 1.1,
                threshold: 0.004,
            })],
            2e-5,
        );
        let layers = vec![
            Layer::from_gain(1.8, 1.5),
            Layer::from_gain(1.4, 0.8),
            Layer::default(),
            Layer::from_gain(1.2, 0.0),
            Layer::default(),
            Layer::from_gain(0.8, 0.0),
        ];
        edits_match(
            &gpu,
            &format!("wavelet, {name}"),
            frame,
            vec![Op::Wavelet(Wavelet::from_layers(layers))],
            1e-4,
        );
        edits_match(
            &gpu,
            &format!("denoise, {name}"),
            frame,
            vec![Op::Denoise(Denoise::default())],
            1e-4,
        );
    }
    // RegiStax-style schemes, filters and linked layers, with large gains.
    let tut =
        |spec: &str| Wavelet::from_registax_spec(spec).unwrap_or_else(|e| panic!("{spec}: {e}"));
    for spec in [
        "registax:l1=100/0.14/0.2,l2=100/0.13/0.15,l3=50/0.11/0.1,filter=gaussian",
        "registax:l1=100/0.3/0.1,l2=150/0.5/0.2,l3=100/0.2,scheme=linear,initial=2,step=1.5",
        "registax:l1=100/0.14/0.2,l2=100/0.13/0.15,l3=100/0.1,linked=on,filter=gaussian",
        "registax:l2=100/0.4/0.2,l4=80/0.2,scheme=linear,initial=1,step=0",
    ] {
        edits_match(
            &gpu,
            &format!("wavelet {spec}"),
            &cap.frames[3],
            vec![Op::Wavelet(tut(spec))],
            5e-4,
        );
    }
    // The all-default wavelet is exact on the device too.
    let id = helia_core::EditList::new(vec![Op::Wavelet(Wavelet::default())]);
    assert_eq!(gpu.apply_edits(&clean, &id).expect("gpu").data, clean.data);
}

#[test]
fn deconvolution_matches_the_cpu_reference() {
    use helia_core::Op;
    use helia_core::edit::{Deconvolve, Psf};
    let Some(gpu) = gpu() else { return };
    let (clean, cap) = scene(129, 97);
    for (name, psf) in [
        ("gaussian", Psf::Gaussian { sigma: 1.6 }),
        ("airy", Psf::Airy { radius: 2.0 }),
    ] {
        for protect in [0.0, 1.0] {
            edits_match(
                &gpu,
                &format!("richardson-lucy {name}, guard {protect}"),
                &cap.frames[2],
                vec![Op::Deconvolve(Deconvolve {
                    psf,
                    iterations: 12,
                    tv: 0.003,
                    protect,
                })],
                2e-3,
            );
        }
    }
    // Automatic PSF: resolved from the frame before either side runs.
    edits_match(
        &gpu,
        "richardson-lucy auto PSF",
        &clean,
        vec![Op::Deconvolve(Deconvolve {
            iterations: 8,
            ..Deconvolve::default()
        })],
        2e-3,
    );
}

#[test]
fn a_mixed_list_with_geometry_matches_the_cpu_reference() {
    use helia_core::Op;
    use helia_core::edit::{Crop, Flip, Levels, Resize, Rotate, Unsharp, Wavelet};
    let Some(gpu) = gpu() else { return };
    let (clean, _) = scene(201, 143);
    edits_match(
        &gpu,
        "sharpen, crop, rotate, flip, levels, resize",
        &clean,
        vec![
            Op::Wavelet(Wavelet::from_layers(vec![
                helia_core::edit::Layer::from_gain(1.6, 1.0),
            ])),
            Op::Crop(Crop {
                x: 10,
                y: 8,
                width: 150,
                height: 100,
            }),
            Op::Rotate(Rotate { degrees: 90.0 }),
            Op::Flip(Flip::default()),
            Op::Unsharp(Unsharp::default()),
            Op::Levels(Levels {
                gamma: 1.3,
                ..Levels::default()
            }),
            Op::Resize(Resize::by(0.5)),
        ],
        1e-4,
    );
}

/// How long a preview-sized edit takes on the selected adapter and on the CPU.
/// Not a pass/fail test (timings depend on the machine); run it with
/// `HELIA_GPU_TESTS=hardware cargo test -p helia-gpu --test parity preview_latency -- --ignored --nocapture`.
#[test]
#[ignore = "a timing report, not a check"]
fn preview_latency_report() {
    use helia_core::Op;
    use helia_core::edit::{Deconvolve, Denoise, Levels, Psf, SharpenMethod, sharpen_edits};
    let Some(gpu) = gpu() else { return };
    let sun = SyntheticSun::new(&SyntheticConfig {
        width: 1280,
        height: 960,
        ..SyntheticConfig::default()
    });
    let frame = sun.render_clean(Shift::default(), None);
    let lists = [
        (
            "wavelets, 6 layers (recommended)",
            sharpen_edits(SharpenMethod::Wavelet, None, 1280, 960),
        ),
        (
            "unsharp mask",
            sharpen_edits(SharpenMethod::Unsharp, None, 1280, 960),
        ),
        (
            "denoise + levels",
            helia_core::EditList::new(vec![
                Op::Denoise(Denoise::default()),
                Op::Levels(Levels {
                    gamma: 1.2,
                    ..Levels::default()
                }),
            ]),
        ),
        (
            "Richardson-Lucy, 20 iterations, Gaussian",
            helia_core::EditList::new(vec![Op::Deconvolve(Deconvolve {
                psf: Psf::Gaussian { sigma: 1.5 },
                iterations: 20,
                ..Deconvolve::default()
            })]),
        ),
        (
            "Richardson-Lucy, 20 iterations, Airy",
            helia_core::EditList::new(vec![Op::Deconvolve(Deconvolve {
                psf: Psf::Airy { radius: 2.0 },
                iterations: 20,
                ..Deconvolve::default()
            })]),
        ),
    ];
    eprintln!("preview latency, 1280 x 960, best of 5, on {}", gpu.name());
    for (name, list) in &lists {
        let best = |b: &dyn Backend| {
            (0..5)
                .map(|_| {
                    let t = std::time::Instant::now();
                    b.apply_edits(&frame, list).expect("edit");
                    t.elapsed().as_secs_f64() * 1e3
                })
                .fold(f64::MAX, f64::min)
        };
        let cpu = best(&CpuBackend);
        let dev = best(&*gpu);
        eprintln!("  {name:<44} cpu {cpu:>8.1} ms   device {dev:>8.1} ms");
    }
}

/// A calibration with a dark, a flat and a few bad pixels for `w` x `h`
/// frames, made without any capture.
fn synthetic_calibration(
    w: usize,
    h: usize,
    pattern: Option<BayerPattern>,
) -> helia_core::calibrate::Calibration {
    use helia_core::calibrate::master::{Master, MasterInfo, MasterKind};
    use helia_core::calibrate::{BadPixelMap, Calibration};
    let plane = |f: &dyn Fn(usize, usize) -> f32| Frame {
        width: w,
        height: h,
        data: (0..w * h).map(|i| f(i % w, i / w)).collect(),
        bayer: pattern,
    };
    let info = |kind| MasterInfo {
        kind,
        frames_used: 4,
        frames_total: 4,
        combine: "median".into(),
        exposure_s: None,
        bayer: pattern,
        source: "synthetic".into(),
        fingerprint: 0,
    };
    let dark = plane(&|x, y| 0.01 + 0.002 * ((x * 7 + y * 3) % 5) as f32);
    let flat = plane(&|x, y| {
        let (dx, dy) = (x as f32 - w as f32 / 2.0, y as f32 - h as f32 / 2.0);
        1.1 - 0.4 * (dx * dx + dy * dy) / (w * w) as f32
    });
    let mut bad = BadPixelMap::new(w, h, "synthetic");
    bad.indices = vec![
        (3 * w + 4) as u32,
        (h / 2 * w + w / 2) as u32,
        ((h - 3) * w + w - 5) as u32,
    ];
    bad.hot = 3;
    Calibration {
        dark: Some(Master::new(MasterKind::Dark, dark, info(MasterKind::Dark))),
        flat: Some(Master::new(MasterKind::Flat, flat, info(MasterKind::Flat))),
        bad: Some(bad),
        ..Calibration::default()
    }
}

#[test]
fn calibrated_frames_decode_on_the_device_as_on_the_cpu() {
    use helia_core::calibrate::CalibratedSource;
    let Some(gpu) = gpu() else { return };
    let frames = textured(5, 67, 45);
    let mut worst = 0.0f32;
    for (name, color, depth, pattern) in [
        ("mono8", ColorId::Mono, 8, None),
        ("mono16", ColorId::Mono, 16, None),
        (
            "rggb16",
            ColorId::Bayer(BayerPattern::Rggb),
            16,
            Some(BayerPattern::Rggb),
        ),
        (
            "bggr8",
            ColorId::Bayer(BayerPattern::Bggr),
            8,
            Some(BayerPattern::Bggr),
        ),
    ] {
        let file = TempCapture::new(
            &format!("cal-{name}.ser"),
            &ser_file(color, depth, true, &frames),
        );
        let mut inner = open_capture(&file.0, &OpenOptions::default()).expect("open");
        let cal = synthetic_calibration(67, 45, pattern);
        let plan = cal.plan(67, 45, pattern).expect("plan");
        let src = CalibratedSource::new(inner.as_mut(), plan).expect("source");
        let shared = src.shared().expect("shared");
        for debayer in [
            Debayer::default(),
            Debayer {
                method: Method::Bilinear,
                channel: Channel::Red,
            },
        ] {
            let idx: Vec<usize> = (0..shared.frame_count()).rev().collect();
            let got = gpu
                .decode_frames(shared, &idx, debayer)
                .expect("gpu decode")
                .expect("supported size");
            for (g, &i) in got.iter().zip(&idx) {
                let want = shared.read_mono(i, &debayer, &Profile::new()).expect("cpu");
                let d = worst_diff(g, &want);
                worst = worst.max(d);
                assert!(d <= DECODE_TOL, "{name} {debayer:?} frame {i}: {d}");
            }
        }
    }
    eprintln!("calibrated decode: worst difference from the CPU decoder {worst:.2e}");
}

#[test]
fn pipeline_with_calibration_on_gpu_matches_cpu() {
    let Some(gpu) = gpu() else { return };
    let (_, cap) = scene(192, 160);
    let cal = synthetic_calibration(192, 160, None);
    let cfg = StackConfig {
        keep: Keep::Fraction(0.5),
        multipoint: None,
        normalise: false,
        kernel: Kernel::Lanczos3,
        align: AlignMethod::Phase,
        calibration: Some(std::sync::Arc::new(cal)),
        ..StackConfig::default()
    };
    let run = |backend: std::sync::Arc<dyn Backend>, cfg: &StackConfig| {
        let mut src = MemorySource(cap.frames.clone());
        Pipeline::new(cfg.clone())
            .with_backend(backend)
            .run(&mut src, &|_: Stage, _| {}, &AtomicBool::new(false))
            .expect("run")
    };
    let cpu = run(std::sync::Arc::new(CpuBackend), &cfg);
    let dev = run(std::sync::Arc::new(Shared(gpu)), &cfg);
    let diff = rmse(&cpu.image, &dev.image, 0);
    eprintln!("calibrated pipeline: cpu vs gpu stack rmse {diff:.2e}");
    assert!(diff < 1e-4, "{diff}");
    assert_eq!(cpu.ranking, dev.ranking);
    // And the calibration did something.
    let plain = run(
        std::sync::Arc::new(CpuBackend),
        &StackConfig {
            calibration: None,
            ..cfg
        },
    );
    assert!(rmse(&cpu.image, &plain.image, 0) > 1e-3);
}

/// True colour: the three planes stacked on the device against the CPU, with
/// the same alignment, the same gains and the same channel shifts (#72).
#[test]
fn true_colour_stack_matches_the_cpu_reference() {
    use helia_core::truecolour::{TrueColour, WhiteBalance};
    use helia_core::{BayerPattern, Keep, Shift};
    let Some(gpu) = gpu() else { return };
    let sun = SyntheticSun::new(&SyntheticConfig {
        width: 192,
        height: 160,
        ..SyntheticConfig::default()
    });
    let cap = SyntheticCapture::generate(
        &sun,
        &CaptureConfig {
            frames: 24,
            noise: 0.01,
            ..CaptureConfig::default()
        },
    );
    let frames = helia_core::testutil::colour_mosaic(
        &cap.frames,
        [0.6, 1.0, 0.8],
        [
            Shift::new(-0.5, -0.7),
            Shift::default(),
            Shift::new(0.4, 0.6),
        ],
        BayerPattern::Rggb,
    );
    for points in [false, true] {
        let cfg = StackConfig {
            keep: Keep::Fraction(0.4),
            multipoint: points.then_some(MultipointConfig {
                ap_size: 32,
                min_contrast: 0.01,
                ..MultipointConfig::default()
            }),
            true_colour: Some(TrueColour {
                white_balance: WhiteBalance::Auto,
                align_channels: true,
            }),
            ..StackConfig::default()
        };
        let run = |backend: std::sync::Arc<dyn Backend>| {
            let mut src = MemorySource(frames.clone());
            Pipeline::new(cfg.clone())
                .with_backend(backend)
                .run(&mut src, &|_: Stage, _| {}, &AtomicBool::new(false))
                .expect("run")
        };
        let cpu = run(std::sync::Arc::new(CpuBackend));
        let dev = run(std::sync::Arc::new(Shared(gpu.clone())));
        let (c, d) = (
            cpu.colour.as_ref().expect("cpu colour"),
            dev.colour.as_ref().expect("gpu colour"),
        );
        let worst = c
            .rgb
            .data
            .iter()
            .zip(&d.rgb.data)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        let gains = (0..3)
            .map(|k| (c.gains[k] - d.gains[k]).abs() / c.gains[k])
            .fold(0.0f32, f32::max);
        eprintln!(
            "true colour parity (points {points}): worst sample difference {worst:.2e}, gains differ {gains:.2e}; backend {}",
            dev.backend
        );
        assert_eq!(cpu.ranking, dev.ranking);
        assert!(
            worst < 2e-3,
            "points {points}: worst sample difference {worst:e}"
        );
        assert!(gains < 1e-3, "points {points}: gains differ {gains:e}");
        let (cs, ds) = (c.channels.expect("cpu"), d.channels.expect("gpu"));
        for k in 0..2 {
            assert_eq!(cs[k].applied, ds[k].applied, "points {points}");
            assert!(
                (cs[k].shift - ds[k].shift).length() < 0.02,
                "points {points}: {:?} against {:?}",
                cs[k],
                ds[k]
            );
        }
    }
}
