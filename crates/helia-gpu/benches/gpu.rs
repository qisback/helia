// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Throughput of the device stages on full-HD frames. Skipped (with a note)
//! when no hardware adapter is available; `HELIA_GPU_BENCH=software` runs on the
//! software adapter instead.
// criterion_group! generates an undocumented public function.
#![allow(missing_docs)]
#![allow(
    clippy::many_single_char_names,
    reason = "width, height and pixel coordinates are the natural names"
)]

use std::sync::atomic::AtomicBool;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use helia_core::align::global::PhaseOptions;
use helia_core::align::multipoint::{ApGrid, MultipointConfig};
use helia_core::debayer::Debayer;
use helia_core::io::source::{FrameSource, MemorySource};
use helia_core::profile::Profile;
use helia_core::{
    Backend, Frame, Kernel, MeasureRequest, Metric, RegisterRequest, ScoreRequest, Shift,
    StackRequest,
};
use helia_gpu::{AdapterRequest, GpuBackend};

const FRAMES: usize = 16;

/// Textured solar disc, shifted a little per frame.
fn scene(w: usize, h: usize, k: usize) -> Frame {
    let (dx, dy) = (0.7 * k as f32, -0.4 * k as f32);
    let data = (0..w * h)
        .map(|i| {
            let (x, y) = ((i % w) as f32 - dx, (i / w) as f32 - dy);
            let (cx, cy) = (x - w as f32 / 2.0, y - h as f32 / 2.0);
            let r = (cx * cx + cy * cy).sqrt() / (h as f32 * 0.4);
            let disc = if r < 1.0 {
                1.0 - 0.5 * (1.0 - (1.0 - r * r).sqrt())
            } else {
                0.02
            };
            disc * (1.0 + 0.1 * (x * 0.31).sin() * (y * 0.27).cos())
        })
        .collect();
    Frame {
        width: w,
        height: h,
        data,
        bayer: None,
    }
}

fn gpu() -> Option<GpuBackend> {
    let request = if std::env::var("HELIA_GPU_BENCH").as_deref() == Ok("software") {
        AdapterRequest::Software
    } else {
        AdapterRequest::Hardware
    };
    match GpuBackend::new(request) {
        Ok(g) => {
            eprintln!("gpu benches on {}", g.adapter_summary());
            Some(g)
        }
        Err(e) => {
            eprintln!("no GPU adapter ({e}): skipping the GPU benches");
            None
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one benchmark per stage, each with its setup"
)]
fn stages(c: &mut Criterion) {
    let Some(gpu) = gpu() else { return };
    let (w, h) = (1920, 1080);
    let mem = MemorySource((0..FRAMES).map(|k| scene(w, h, k)).collect());
    let frames = mem.shared().expect("shared");
    let indices: Vec<usize> = (0..FRAMES).collect();
    let cancel = AtomicBool::new(false);
    let profile = Profile::new();
    let mut g = c.benchmark_group("gpu_stages");
    g.sample_size(10);
    g.throughput(Throughput::Elements((FRAMES * w * h) as u64));

    g.bench_function("score_16_frames_1920x1080", |b| {
        b.iter(|| {
            let req = ScoreRequest {
                frames,
                indices: &indices,
                debayer: Debayer::default(),
                metric: Metric::GradientEnergy,
                roi: None,
                progress: &|_| {},
                cancel: &cancel,
                profile: &profile,
            };
            gpu.score_frames(&req).expect("score")
        });
    });

    let reference = scene(w, h, 0);
    g.bench_function("register_16_frames_1920x1080", |b| {
        b.iter(|| {
            let req = RegisterRequest {
                frames,
                indices: &indices,
                debayer: Debayer::default(),
                reference: &reference,
                phase: PhaseOptions::default(),
                progress: &|_| {},
                cancel: &cancel,
                profile: &profile,
            };
            gpu.register_frames(&req).expect("register")
        });
    });

    let cfg = MultipointConfig::default();
    let grid = ApGrid::build(&reference, &cfg).expect("grid");
    let templates = grid.templates(&reference);
    let shifts: Vec<Shift> = (0..FRAMES)
        .map(|k| Shift::new(0.7 * k as f32, -0.4 * k as f32))
        .collect();
    g.bench_function("alignment_points_16_frames_1920x1080", |b| {
        b.iter(|| {
            let req = MeasureRequest {
                frames,
                indices: &indices,
                debayer: Debayer::default(),
                shifts: &shifts,
                grid: &grid,
                templates: &templates,
                config: &cfg,
                progress: &|_| {},
                cancel: &cancel,
                profile: &profile,
            };
            gpu.measure_points(&req).expect("points")
        });
    });

    let rects: Vec<_> = grid
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
    let tile_deposit = |k: usize, _t: usize| Some((shifts[k], 1.0));
    let frame_deposit = |k: usize| Some((shifts[k], 1.0));
    g.bench_function("stack_16_frames_1920x1080", |b| {
        b.iter(|| {
            let req = StackRequest {
                frames,
                order: &indices,
                debayer: Debayer::default(),
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
            };
            gpu.stack_frames(&req).expect("stack")
        });
    });
    g.finish();
}

criterion_group!(benches, stages);
criterion_main!(benches);
