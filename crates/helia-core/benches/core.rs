// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Throughput benchmarks for the hot paths.
// criterion_group! generates an undocumented public function.
#![allow(missing_docs)]
#![allow(
    clippy::many_single_char_names,
    reason = "width, height and pixel coordinates are the natural names"
)]

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use helia_core::align::Shift;
use helia_core::align::global::{PhaseCorrelator, PhaseOptions};
use helia_core::align::multipoint::{ApGrid, MultipointConfig, fill_missing, measure};
use helia_core::debayer::{Channel, Debayer, Method};
use helia_core::resample::resample_region;
use helia_core::stack::Deposit;
use helia_core::{Backend, BayerPattern, CpuBackend, Frame, Kernel, Metric, Rect, filter, quality};
use std::hint::black_box;

/// Smooth disc with texture; deterministic and dependency-free.
fn scene(w: usize, h: usize, dx: f32, dy: f32) -> Frame {
    let data = (0..w * h)
        .map(|i| {
            let (x, y) = ((i % w) as f32 - dx, (i / w) as f32 - dy);
            let (cx, cy) = (x - w as f32 / 2.0, y - h as f32 / 2.0);
            let r = (cx * cx + cy * cy).sqrt() / (w.min(h) as f32 * 0.4);
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

fn phase_correlation(c: &mut Criterion) {
    let mut g = c.benchmark_group("phase_correlation");
    for &(w, h) in &[(512usize, 512usize), (1024, 768)] {
        let reference = scene(w, h, 0.0, 0.0);
        let frame = scene(w, h, 3.3, -2.1);
        let corr = PhaseCorrelator::new(&reference, PhaseOptions::default()).expect("correlator");
        g.throughput(Throughput::Elements((w * h) as u64));
        g.bench_function(format!("estimate_{w}x{h}"), |b| {
            b.iter(|| black_box(corr.estimate(black_box(&frame)).expect("estimate")));
        });
    }
    g.finish();
}

fn stacking(c: &mut Criterion) {
    let mut g = c.benchmark_group("stacking");
    let (w, h) = (1024, 768);
    let frame = scene(w, h, 1.0, 1.0);
    g.throughput(Throughput::Elements((w * h) as u64));
    for (name, kernel) in [
        ("bilinear", Kernel::Bilinear),
        ("lanczos3", Kernel::Lanczos3),
    ] {
        g.bench_function(format!("accumulate_{name}_{w}x{h}"), |b| {
            let mut acc = CpuBackend
                .accumulator(w, h, Deposit::Resample(kernel))
                .expect("acc");
            b.iter(|| {
                acc.add(black_box(&frame), Shift::new(0.37, -0.61), 1.0)
                    .expect("add");
            });
        });
    }
    g.bench_function(format!("sharpness_{w}x{h}"), |b| {
        b.iter(|| {
            black_box(quality::score(
                black_box(&frame),
                Metric::GradientEnergy,
                None,
            ))
        });
    });
    g.finish();
}

/// Full-HD frame, the size of the real captures.
fn hd(dx: f32, dy: f32) -> Frame {
    scene(1920, 1080, dx, dy)
}

fn decode_and_debayer(c: &mut Criterion) {
    let mut g = c.benchmark_group("debayer");
    let (w, h) = (1920usize, 1080usize);
    let mosaic = Frame {
        bayer: Some(BayerPattern::Rggb),
        ..hd(0.0, 0.0)
    };
    g.throughput(Throughput::Elements((w * h) as u64));
    for (name, method, channel) in [
        ("mhc_luminance", Method::Mhc, Channel::Luminance),
        ("mhc_red", Method::Mhc, Channel::Red),
        ("bilinear_luminance", Method::Bilinear, Channel::Luminance),
    ] {
        let d = Debayer { method, channel };
        g.bench_function(format!("{name}_{w}x{h}"), |b| {
            b.iter(|| black_box(d.apply(black_box(&mosaic))));
        });
    }
    g.finish();
}

fn alignment_points(c: &mut Criterion) {
    let mut g = c.benchmark_group("alignment_points");
    let cfg = MultipointConfig::default();
    let reference = hd(0.0, 0.0);
    let frame = hd(1.7, -0.9);
    let grid = ApGrid::build(&reference, &cfg).expect("grid");
    let templates = grid.templates(&reference);
    let guess = Shift::new(2.0, -1.0);
    g.throughput(Throughput::Elements(1920 * 1080));
    g.bench_function("gaussian_blur_sigma1_1920x1080", |b| {
        b.iter(|| black_box(filter::gaussian_blur(black_box(&frame), 1.0)));
    });
    g.bench_function("gaussian_blur_sigma4_1920x1080", |b| {
        b.iter(|| black_box(filter::gaussian_blur(black_box(&frame), 4.0)));
    });
    g.bench_function("prepare_1920x1080", |b| {
        b.iter(|| black_box(grid.prepare(black_box(&frame))));
    });
    let prep = grid.prepare(&frame);
    let floor = prep.floor;
    g.bench_function("window_scorer_build_1920x1080", |b| {
        b.iter(|| black_box(quality::WindowScorer::new(black_box(&prep.scored))));
    });
    g.bench_function("measure_all_points", |b| {
        b.iter(|| {
            let row: Vec<_> = templates
                .iter()
                .map(|t| t.as_ref().and_then(|t| measure(t, &prep, guess, &cfg)))
                .collect();
            black_box(row)
        });
    });
    g.bench_function("measure_and_fill_all_points", |b| {
        b.iter(|| {
            let mut row: Vec<_> = templates
                .iter()
                .map(|t| t.as_ref().and_then(|t| measure(t, &prep, guess, &cfg)))
                .collect();
            fill_missing(&grid, &templates, &mut row, &prep, guess);
            black_box(row)
        });
    });
    g.bench_function("window_score_64", |b| {
        let win = Rect::new(700, 400, 64, 64);
        b.iter(|| {
            black_box(quality::score_with_floor(
                black_box(&prep.scored),
                Metric::GradientEnergy,
                Some(win),
                floor,
            ))
        });
    });
    g.finish();
}

fn resampling(c: &mut Criterion) {
    let mut g = c.benchmark_group("resample");
    let frame = hd(0.0, 0.0);
    let shift = Shift::new(0.37, -0.61);
    for (name, kernel) in [
        ("bilinear", Kernel::Bilinear),
        ("lanczos3", Kernel::Lanczos3),
    ] {
        let tile = Rect::new(600, 400, 64, 64);
        let mut out = vec![0.0f32; tile.area()];
        g.throughput(Throughput::Elements(tile.area() as u64));
        g.bench_function(format!("tile_64_{name}"), |b| {
            b.iter(|| resample_region(black_box(&frame), shift, tile, kernel, &mut out));
        });
    }
    g.finish();
}

fn quality_hd(c: &mut Criterion) {
    let frame = hd(0.0, 0.0);
    let mut g = c.benchmark_group("quality");
    g.throughput(Throughput::Elements(1920 * 1080));
    g.bench_function("score_gradient_1920x1080", |b| {
        b.iter(|| {
            black_box(quality::score(
                black_box(&frame),
                Metric::GradientEnergy,
                None,
            ))
        });
    });
    g.bench_function("noise_floor_1920x1080", |b| {
        b.iter(|| black_box(quality::noise_floor(black_box(&frame))));
    });
    g.finish();
}

fn phase_correlation_hd(c: &mut Criterion) {
    let reference = hd(0.0, 0.0);
    let frame = hd(3.3, -2.1);
    let corr = PhaseCorrelator::new(&reference, PhaseOptions::default()).expect("correlator");
    let mut g = c.benchmark_group("phase_correlation");
    g.sample_size(20);
    g.throughput(Throughput::Elements(1920 * 1080));
    g.bench_function("estimate_1920x1080", |b| {
        b.iter(|| black_box(corr.estimate(black_box(&frame)).expect("estimate")));
    });
    g.finish();
}

criterion_group!(
    benches,
    phase_correlation,
    stacking,
    decode_and_debayer,
    alignment_points,
    resampling,
    quality_hd,
    phase_correlation_hd
);
criterion_main!(benches);
