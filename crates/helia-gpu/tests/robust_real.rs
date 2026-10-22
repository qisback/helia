// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Robust stacking on real frames with artefacts written into the raw sensor
//! data before alignment (issue #150).
//!
//! A dark line is written across the disc of two frames and hot pixels into
//! two others, in sensor coordinates, which is how a satellite, a cosmic ray
//! or a hot pixel arrives; the alignment then moves each frame's copy of the
//! artefact with the Sun, so at a stack pixel it is one outlier in twenty.
//! `--robust sigma|huber|winsor` must take it out of the whole-frame stack and
//! the alignment point stack, on the CPU and on the GPU, with the same result
//! on both. Most tests rank frames on a region the damage does not touch so the
//! damaged and clean runs share a reference and weights; one ranks on the whole
//! frame and checks that the damage does not move the reference (#172).
//!
//! Needs `HELIA_REAL_DATA` (the 2022-07-17 captures) and, for the GPU half,
//! `HELIA_GPU_TESTS=software|hardware`; each part is skipped with a note when
//! its input is absent.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock};

use helia_core::align::global::{PhaseCorrelator, PhaseOptions};
use helia_core::align::method::AlignMethod;
use helia_core::debayer::{Channel, Debayer};
use helia_core::io::source::{FrameSource, LimitedSource, OpenOptions, open_capture};
use helia_core::pipeline::{Pipeline, StackConfig};
use helia_core::stack::StackMode;
use helia_core::testutil::Rng;
use helia_core::{Backend, Frame, Keep, Rect};
use helia_gpu::{AdapterRequest, GpuBackend};

const FRAMES: usize = 20;
/// Frames carrying the dark line, and the hot pixels.
const LINE_FRAMES: [usize; 2] = [3, 12];
const HOT_FRAMES: [usize; 2] = [7, 15];
const LINE_X: std::ops::Range<usize> = 1000..1003;
const LINE_DEPTH: f32 = 20000.0 / 65535.0;
const HOT_HEIGHT: f32 = 30000.0 / 65535.0;
const HOT_PIXELS: usize = 300;

fn capture() -> Option<PathBuf> {
    let Some(base) = std::env::var_os("HELIA_REAL_DATA").map(PathBuf::from) else {
        eprintln!("HELIA_REAL_DATA is not set: skipping the real-frame robust test");
        return None;
    };
    let name = "15_20_57.ser";
    [base.join(name), base.join("solar-2022-07-17").join(name)]
        .into_iter()
        .find(|p| p.exists())
        .or_else(|| {
            eprintln!("{name} not found under {}: skipping", base.display());
            None
        })
}

fn gpu() -> Option<Arc<GpuBackend>> {
    let request = match std::env::var("HELIA_GPU_TESTS")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "software" => AdapterRequest::Software,
        "hardware" => AdapterRequest::Hardware,
        _ => {
            eprintln!("HELIA_GPU_TESTS is unset or 'none': skipping the GPU half");
            return None;
        }
    };
    Some(GpuBackend::shared(request).expect("GPU adapter"))
}

/// What is written into the raw frames.
#[derive(Clone, Copy, PartialEq)]
enum Damage {
    None,
    /// Line and hot pixels in a few frames: the outliers robust stacking is for.
    Transient,
    /// The same hot pixels in every frame: a sensor defect.
    Fixed,
}

/// Hot pixel positions, all right of the line and outside the ranking region.
fn hot_positions(seed: u64) -> Vec<(usize, usize)> {
    let mut rng = Rng::new(seed);
    (0..HOT_PIXELS)
        .map(|_| {
            (
                1020 + (rng.next_u64() % 680) as usize,
                100 + (rng.next_u64() % 900) as usize,
            )
        })
        .collect()
}

struct Damaged {
    inner: Box<dyn FrameSource>,
    damage: Damage,
}

impl FrameSource for Damaged {
    fn dimensions(&self) -> (usize, usize) {
        self.inner.dimensions()
    }
    fn frame_count(&self) -> usize {
        self.inner.frame_count()
    }
    fn read(&mut self, index: usize) -> helia_core::Result<Frame> {
        let mut f = self.inner.read(index)?;
        let w = f.width;
        let hot = |f: &mut Frame, seed: u64| {
            for (x, y) in hot_positions(seed) {
                let v = &mut f.data[y * w + x];
                *v = (*v + HOT_HEIGHT).min(1.0);
            }
        };
        match self.damage {
            Damage::None => {}
            Damage::Transient => {
                if LINE_FRAMES.contains(&index) {
                    for y in 0..f.height {
                        for x in LINE_X {
                            let v = &mut f.data[y * w + x];
                            *v = (*v - LINE_DEPTH).max(0.0);
                        }
                    }
                }
                if HOT_FRAMES.contains(&index) {
                    // Different pixels in each frame.
                    hot(&mut f, 1 + index as u64);
                }
            }
            Damage::Fixed => hot(&mut f, 99),
        }
        Ok(f)
    }
    fn describe(&self) -> String {
        "damaged".into()
    }
}

fn config(multipoint: bool, mode: StackMode, rank_clean: bool) -> StackConfig {
    let mut cfg = StackConfig {
        keep: Keep::Fraction(1.0),
        mode,
        normalise: false,
        align: AlignMethod::Phase,
        debayer: Debayer {
            channel: Channel::Green,
            ..Debayer::default()
        },
        // Rank on the left part of the disc, away from every artefact, or on
        // the whole frame, where the damage raises a frame's score.
        roi: rank_clean.then(|| Rect::new(200, 100, 780, 880)),
        ..StackConfig::default()
    };
    if !multipoint {
        cfg.multipoint = None;
    }
    cfg
}

struct Stacked {
    image: Frame,
    reference: Frame,
    ranking: Vec<usize>,
    scores: Vec<f32>,
    outliers: Vec<(usize, f32)>,
    shifts: Vec<(f32, f32)>,
}

fn stack(
    path: &Path,
    damage: Damage,
    multipoint: bool,
    mode: StackMode,
    backend: Option<Arc<dyn Backend>>,
    rank_clean: bool,
) -> Stacked {
    let inner = open_capture(path, &OpenOptions::default()).expect("open");
    let limited = LimitedSource::new(inner, 0, FRAMES);
    let mut src = Damaged {
        inner: Box::new(limited),
        damage,
    };
    let mut p = Pipeline::new(config(multipoint, mode, rank_clean));
    if let Some(b) = backend {
        p = p.with_backend(b);
    }
    let out = p
        .run(&mut src, &|_, _| {}, &AtomicBool::new(false))
        .expect("stack");
    Stacked {
        image: out.image,
        reference: out.reference,
        ranking: out.ranking,
        scores: out.scores,
        outliers: out.score_outliers,
        shifts: out.shifts.iter().map(|(_, s)| (s.dx, s.dy)).collect(),
    }
}

fn l1(a: &Frame, b: &Frame, r: Rect) -> f32 {
    let mut sum = 0.0f64;
    for y in r.y0..r.y1 {
        for x in r.x0..r.x1 {
            sum += f64::from((a.data[y * a.width + x] - b.data[y * a.width + x]).abs());
        }
    }
    (sum / r.area() as f64) as f32
}

fn max_abs(a: &Frame, b: &Frame) -> f32 {
    a.data
        .iter()
        .zip(&b.data)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max)
}

/// Pixels of `r` that differ by more than `tol`.
fn count_over(a: &Frame, b: &Frame, r: Rect, tol: f32) -> f32 {
    let mut n = 0u32;
    for y in r.y0..r.y1 {
        for x in r.x0..r.x1 {
            if (a.data[y * a.width + x] - b.data[y * a.width + x]).abs() > tol {
                n += 1;
            }
        }
    }
    n as f32
}

/// Error of a damaged stack against the clean one of the same mode: the mean
/// difference across the line, and the number of pixels over the hot pixel
/// region that the hot pixels moved by more than 0.01.
fn errors(clean: &Frame, dirty: &Frame) -> (f32, f32) {
    (
        l1(clean, dirty, Rect::new(985, 250, 45, 600)),
        count_over(clean, dirty, Rect::new(1020, 100, 680, 900), 0.01),
    )
}

/// One run at a time: each stack holds several full-size frames and a device.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Name, mode, and the share of the plain mean's error that may remain, for
/// the line and for the hot pixels. The soft forms (Huber, winsor) only pull a
/// gross outlier part of the way back, the hard clip removes it; what is left
/// in the alignment point stack is the shift the artefact itself put into the
/// point that holds it, which no combining rule can undo.
const MODES: [(&str, StackMode, f32, f32); 3] = [
    ("sigma", StackMode::SigmaClip { kappa: 2.5 }, 0.25, 0.35),
    ("huber", StackMode::Huber { k: 1.345 }, 0.75, 0.35),
    ("winsor", StackMode::Winsor { k: 2.0 }, 0.75, 0.35),
];

fn check(multipoint: bool, backend: Option<&Arc<dyn Backend>>, label: &str) {
    let Some(path) = capture() else { return };
    let _guard = serial();
    let run = |damage, mode| stack(&path, damage, multipoint, mode, backend.cloned(), true).image;
    let (mean_line, mean_hot) = errors(
        &run(Damage::None, StackMode::Mean),
        &run(Damage::Transient, StackMode::Mean),
    );
    eprintln!("{label}: mean line {mean_line:.5} hot {mean_hot}");
    assert!(mean_line > 0.001 && mean_hot > 50.0, "damage too weak");
    for (name, mode, keep_line, keep_hot) in MODES {
        let (line, hot) = errors(&run(Damage::None, mode), &run(Damage::Transient, mode));
        eprintln!(
            "{label} {name}: line {line:.5} ({:.1}% of mean) hot {hot} ({:.1}%)",
            100.0 * line / mean_line,
            100.0 * hot / mean_hot
        );
        assert!(line < keep_line * mean_line, "{label} {name}: line {line}");
        assert!(hot < keep_hot * mean_hot, "{label} {name}: hot {hot}");
    }
}

#[test]
fn whole_frame_cpu_removes_artefacts_written_into_raw_frames() {
    check(false, None, "whole-frame cpu");
}

#[test]
fn alignment_point_cpu_removes_artefacts_written_into_raw_frames() {
    check(true, None, "points cpu");
}

#[test]
fn whole_frame_gpu_removes_artefacts_and_matches_the_cpu() {
    gpu_case(false);
}

#[test]
fn alignment_point_gpu_removes_artefacts_and_matches_the_cpu() {
    gpu_case(true);
}

fn gpu_case(multipoint: bool) {
    let (Some(path), Some(g)) = (capture(), gpu()) else {
        return;
    };
    let g: Arc<dyn Backend> = g;
    let label = if multipoint {
        "points gpu"
    } else {
        "whole-frame gpu"
    };
    check(multipoint, Some(&g), label);
    let _guard = serial();
    for (name, mode, ..) in MODES {
        let c = stack(&path, Damage::Transient, multipoint, mode, None, true).image;
        let d = stack(
            &path,
            Damage::Transient,
            multipoint,
            mode,
            Some(Arc::clone(&g)),
            true,
        )
        .image;
        let diff = max_abs(&c, &d);
        let mean_diff = l1(&c, &d, c.rect());
        eprintln!("{label} {name}: cpu vs gpu max {diff:.5} mean {mean_diff:.7}");
        assert!(mean_diff < 2e-5, "{label} {name}: mean {mean_diff}");
        assert!(diff < 0.005, "{label} {name}: max {diff}");
    }
}

/// A defect fixed on the sensor is in the same raw pixel of every frame. The
/// alignment moves it only as far as the frames drift, so unless they drift by
/// several pixels it is not an outlier and robust stacking leaves it: that is
/// what calibration (a dark frame, a bad-pixel map) is for.
#[test]
fn a_defect_fixed_on_the_sensor_is_not_an_outlier() {
    let Some(path) = capture() else { return };
    let _guard = serial();
    let clean = stack(&path, Damage::None, false, StackMode::Mean, None, true);
    let fixed = stack(&path, Damage::Fixed, false, StackMode::Mean, None, true);
    let (xs, ys): (Vec<f32>, Vec<f32>) = fixed.shifts.iter().copied().unzip();
    let spread = |v: &[f32]| {
        v.iter().copied().fold(f32::MIN, f32::max) - v.iter().copied().fold(f32::MAX, f32::min)
    };
    eprintln!(
        "drift over {FRAMES} frames: {:.2} px x, {:.2} px y",
        spread(&xs),
        spread(&ys)
    );
    let region = Rect::new(1020, 100, 680, 900);
    let mean_err = l1(&clean.image, &fixed.image, region);
    let sigma = StackMode::SigmaClip { kappa: 2.5 };
    let clean_r = stack(&path, Damage::None, false, sigma, None, true);
    let fixed_r = stack(&path, Damage::Fixed, false, sigma, None, true);
    let robust_err = l1(&clean_r.image, &fixed_r.image, region);
    eprintln!("fixed hot pixels: mean error {mean_err:.6}, sigma error {robust_err:.6}");
    // Not removed: robust stacking is no substitute for a bad-pixel map.
    if spread(&xs).max(spread(&ys)) < 3.0 {
        assert!(robust_err > 0.5 * mean_err);
    }
}

/// Deepest column mean of `dirty - clean` across the line, over the disc rows,
/// as measured in the issue. Sub-pixel shifts of the whole stack average out
/// along a column; a line does not.
fn line_dip(clean: &Frame, dirty: &Frame) -> f32 {
    (996..1008)
        .map(|x| {
            let sum: f32 = (300..800)
                .map(|y| dirty.data[y * dirty.width + x] - clean.data[y * dirty.width + x])
                .sum();
            (sum / 500.0).abs()
        })
        .fold(0.0, f32::max)
}

/// The case of the issue: nothing is ranked away from the damage, so the frames
/// carrying the dark line score best (its edges are the sharpest detail) and
/// get the highest weight. The rejection must not be measured with those
/// weights. Only the line is checked: the reference, and so the whole stack,
/// moves a little when it is built from damaged frames.
#[test]
fn frames_that_the_damage_makes_score_best_are_still_clipped() {
    let Some(path) = capture() else { return };
    let _guard = serial();
    let mut backends: Vec<Option<Arc<dyn Backend>>> = vec![None];
    if let Some(g) = gpu() {
        backends.push(Some(g));
    }
    for multipoint in [false, true] {
        for backend in &backends {
            let run =
                |damage, mode| stack(&path, damage, multipoint, mode, backend.clone(), false).image;
            let mean = line_dip(
                &run(Damage::None, StackMode::Mean),
                &run(Damage::Transient, StackMode::Mean),
            );
            let sigma = StackMode::SigmaClip { kappa: 2.5 };
            let clipped = line_dip(&run(Damage::None, sigma), &run(Damage::Transient, sigma));
            eprintln!(
                "scored on the whole frame, points {multipoint}, gpu {}: mean {mean:.5}, sigma {clipped:.5}",
                backend.is_some()
            );
            assert!(mean > 0.03, "damage too weak: {mean}");
            assert!(clipped < 0.25 * mean, "points {multipoint}: {clipped}");
        }
    }
}

/// Sub-pixel offset of `b` against `a` (phase correlation), pixels.
fn offset(a: &Frame, b: &Frame) -> (f32, f32) {
    let est = PhaseCorrelator::new(a, PhaseOptions::default())
        .expect("correlator")
        .estimate(b)
        .expect("estimate");
    (est.shift.dx, est.shift.dy)
}

fn length((dx, dy): (f32, f32)) -> f32 {
    dx.hypot(dy)
}

/// Damage in frames that nothing is ranked away from must not move the
/// reference or the stack (issue #172). Before the fix, four of the
/// five frames the reference was built from were damaged and the stack moved
/// by 0.14 px; the two frames with the line are now put back by their
/// neighbours in time, and the median reference ignores the hot-pixel frames
/// that still rank high (their score is within the seeing).
#[test]
fn damaged_frames_do_not_move_the_reference_or_the_stack() {
    let Some(path) = capture() else { return };
    let _guard = serial();
    let sigma = StackMode::SigmaClip { kappa: 2.5 };
    for multipoint in [false, true] {
        let clean = stack(&path, Damage::None, multipoint, sigma, None, false);
        let dirty = stack(&path, Damage::Transient, multipoint, sigma, None, false);
        let top: Vec<usize> = dirty.ranking.iter().take(5).copied().collect();
        let reference = length(offset(&clean.reference, &dirty.reference));
        let image = length(offset(&clean.image, &dirty.image));
        eprintln!(
            "points {multipoint}: top five {top:?}, reference moved {reference:.3} px, stack moved {image:.3} px"
        );
        assert!(
            !top.iter().any(|i| LINE_FRAMES.contains(i)),
            "{top:?} holds a frame with the line"
        );
        assert!(reference < 0.05, "reference moved {reference} px");
        assert!(image < 0.05, "stack moved {image} px");
    }
}

/// The figures behind the test above, with the scores, for the changelog;
/// run with `--ignored --nocapture`.
#[test]
#[ignore = "a measurement report"]
fn report_reference_and_stack_shift_with_damage() {
    let Some(path) = capture() else { return };
    let _guard = serial();
    let sigma = StackMode::SigmaClip { kappa: 2.5 };
    for multipoint in [false, true] {
        let clean = stack(&path, Damage::None, multipoint, sigma, None, false);
        let dirty = stack(&path, Damage::Transient, multipoint, sigma, None, false);
        let round = |v: &[f32]| {
            v.iter()
                .map(|s| (s * 1e5).round() / 1e5)
                .collect::<Vec<_>>()
        };
        eprintln!(
            "points {multipoint}: scores clean {:?}
  damaged {:?}
  put back {:?}",
            round(&clean.scores),
            round(&dirty.scores),
            dirty.outliers
        );
        let top = |r: &[usize]| r.iter().take(5).copied().collect::<Vec<_>>();
        eprintln!(
            "points {multipoint}: top five clean {:?} damaged {:?}; reference moved {:.3} px, stack moved {:.3} px",
            top(&clean.ranking),
            top(&dirty.ranking),
            length(offset(&clean.reference, &dirty.reference)),
            length(offset(&clean.image, &dirty.image)),
        );
    }
}
