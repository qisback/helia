// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The mosaic on panels cut from a real full-disc stack.
//!
//! There are no real mosaic captures. What can be measured is how well the panels
//! of a mosaic go back together when they are cut from a real stack of the whole
//! disc with known offsets, rotations and gains, as the real ones would differ.
//! The stack is never stored in the repository: `HELIA_MOSAIC_STACK` names a
//! full-disc stacked image (a TIFF, PNG or FITS file); without it this test prints a
//! note and passes. `HELIA_MOSAIC_OUT` names a directory to write the panels and
//! the mosaic to, as PNG, for the documentation.
#![allow(
    clippy::many_single_char_names,
    reason = "x, y, w, h are the natural names in image maths"
)]

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use helia_core::Frame;
use helia_core::io::any::read_any;
use helia_core::mosaic::{Options, build, summary_lines};
use helia_core::picture::{self, Picture};
use helia_core::resample::{Kernel, sample};

/// Panel size and the shifts, turns and gains the panels are cut with: a 2 x 3
/// grid with about a fifth of each panel shared with its neighbours.
const PANEL: (usize, usize) = (330, 330);
/// Where a panel is cut, how far it is turned, and its gain and offset.
type Cut = ((f64, f64), f64, (f32, f32));

const CUTS: [Cut; 6] = [
    ((0.0, 0.0), 0.0, (1.00, 0.000)),
    ((190.7, 3.4), 1.1, (0.93, 0.012)),
    ((380.2, -2.6), -0.7, (1.07, -0.015)),
    ((4.3, 205.6), -1.0, (0.96, 0.010)),
    ((193.4, 204.1), 0.6, (1.04, -0.008)),
    ((377.9, 206.8), 0.9, (0.91, 0.020)),
];

/// A small deterministic Gaussian generator (the test helpers are not built for
/// integration tests).
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn uniform(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 40) as f32 + 0.5) / 16_777_216.0
    }

    fn gauss(&mut self) -> f32 {
        let (a, b) = (self.uniform(), self.uniform());
        (-2.0 * a.ln()).sqrt() * (std::f32::consts::TAU * b).cos()
    }
}

fn cut(truth: &Frame, spec: &Cut, seed: u64, noise: f32) -> Frame {
    let ((x0, y0), deg, (gain, offset)) = *spec;
    let (w, h) = PANEL;
    let c = ((w as f64 - 1.0) / 2.0, (h as f64 - 1.0) / 2.0);
    let (s, co) = deg.to_radians().sin_cos();
    let mut rng = Rng::new(seed);
    let mut f = Frame::zeros(w, h);
    for y in 0..h {
        for x in 0..w {
            let (u, v) = (x as f64 - c.0, y as f64 - c.1);
            let (qx, qy) = (co * u - s * v + c.0 + x0, s * u + co * v + c.1 + y0);
            let v = sample(truth, qx as f32, qy as f32, Kernel::Lanczos3);
            f.data[y * w + x] = v * gain + offset + noise * rng.gauss();
        }
    }
    f
}

fn write_png(path: &std::path::Path, f: &Frame) {
    let file = std::fs::File::create(path).expect("create");
    helia_core::io::png::write_png8(std::io::BufWriter::new(file), f, None).expect("png");
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one sequence: cut, join, measure, report"
)]
fn panels_cut_from_a_real_stack_go_back_together() {
    let Some(path) = std::env::var_os("HELIA_MOSAIC_STACK").map(PathBuf::from) else {
        eprintln!("skipping the real-stack mosaic test: set HELIA_MOSAIC_STACK");
        return;
    };
    let (loaded, _) = read_any(&path).unwrap_or_else(|e| panic!("{e}"));
    let truth = picture::luma(&loaded);
    assert!(
        truth.width >= 700 && truth.height >= 540,
        "the stack must be at least 700 x 540"
    );
    let noise = 0.004;
    let panels: Vec<Frame> = CUTS
        .iter()
        .enumerate()
        .map(|(i, c)| cut(&truth, c, 31 + i as u64, noise))
        .collect();
    let pictures: Vec<Picture> = panels.iter().map(|f| Picture::Mono(f.clone())).collect();
    let go = |opts: &Options| {
        build(&pictures, &[], opts, &|_| {}, &AtomicBool::new(false))
            .unwrap_or_else(|e| panic!("{e}"))
    };
    let mosaic = go(&Options::default());
    for l in summary_lines(&mosaic) {
        eprintln!("mosaic: {l}");
    }
    // Registration against what the panels were cut with.
    let mut worst_shift = 0.0f64;
    let mut worst_angle = 0.0f64;
    for (p, c) in mosaic.panels.iter().zip(&CUTS) {
        let (ex, ey) = (c.0.0 - CUTS[0].0.0, c.0.1 - CUTS[0].0.1);
        let e = (p.x - ex).hypot(p.y - ey);
        worst_shift = worst_shift.max(e);
        worst_angle = worst_angle.max((p.angle_deg - c.1).abs());
        eprintln!(
            "panel {}: placed ({:.2}, {:.2}) want ({ex:.2}, {ey:.2}), error {e:.3} px; angle {:.3} want {:.1}; gain {:.3} offset {:.4}",
            p.index + 1,
            p.x,
            p.y,
            p.angle_deg,
            c.1,
            p.gain,
            p.offset
        );
    }
    // The mosaic against the truth, inside the disc and where a panel reached.
    let Picture::Mono(img) = &mosaic.image else {
        panic!("a mono mosaic")
    };
    let score = |m: &Frame, origin: (f64, f64)| {
        // The mosaic is in panel 0's brightness: compare with the truth scaled to it.
        let (g0, o0) = CUTS[0].2;
        let disc = helia_core::mask::fit_disc(&truth).expect("the stack has a disc");
        let (mut e, mut n, mut seam_e, mut seam_n) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
        for y in 0..m.height {
            for x in 0..m.width {
                let v = m.data[y * m.width + x];
                let (tx, ty) = (
                    x as f64 + origin.0 + CUTS[0].0.0,
                    y as f64 + origin.1 + CUTS[0].0.1,
                );
                if v == 0.0
                    || (tx - f64::from(disc.cx)).hypot(ty - f64::from(disc.cy))
                        > 0.95 * f64::from(disc.r)
                {
                    continue;
                }
                let t = f64::from(sample(&truth, tx as f32, ty as f32, Kernel::Lanczos3) * g0 + o0);
                let d = f64::from(v) - t;
                e += d * d;
                n += 1.0;
                // Pixels within 6 px of a place where two panels meet (a panel border).
                if CUTS.iter().any(|c| {
                    let near = |a: f64, lo: f64, len: f64| {
                        (a - lo).abs() < 6.0 || (a - (lo + len)).abs() < 6.0
                    };
                    near(tx, c.0.0, PANEL.0 as f64) || near(ty, c.0.1, PANEL.1 as f64)
                }) {
                    seam_e += d * d;
                    seam_n += 1.0;
                }
            }
        }
        ((e / n).sqrt(), (seam_e / seam_n.max(1.0)).sqrt(), n)
    };
    let (rms, near_rms, n) = score(img, mosaic.origin);
    eprintln!(
        "against the truth: RMS {rms:.4} over {n} px, {near_rms:.4} within 6 px of a panel border; panel noise {noise}"
    );
    // The disc's centre-to-limb range is about 0.6, so these are a percent or two of it.
    let no_levels = go(&Options {
        match_levels: false,
        ..Options::default()
    });
    let no_seams = go(&Options {
        seams: false,
        ..Options::default()
    });
    for (what, m) in [
        ("without gain matching", &no_levels),
        ("without seams", &no_seams),
    ] {
        let Picture::Mono(f) = &m.image else {
            panic!("mono")
        };
        let (r, nr, _) = score(f, m.origin);
        eprintln!(
            "{what}: RMS {r:.4}, {nr:.4} near borders; seam RMS {:.4}% (default {:.4}%)",
            100.0 * m.seam_rms,
            100.0 * mosaic.seam_rms
        );
    }
    // The same with no noise: what is left is the registration and the resampling.
    let clean: Vec<Picture> = CUTS
        .iter()
        .enumerate()
        .map(|(i, c)| Picture::Mono(cut(&truth, c, 31 + i as u64, 0.0)))
        .collect();
    let exact = build(
        &clean,
        &[],
        &Options::default(),
        &|_| {},
        &AtomicBool::new(false),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let Picture::Mono(exact_img) = &exact.image else {
        panic!("mono")
    };
    let (erms, enear, _) = score(exact_img, exact.origin);
    eprintln!(
        "without noise: RMS {erms:.5} ({enear:.5} within 6 px of a border); seam RMS {:.4}% and overlap RMS {:.4}% of the level",
        100.0 * exact.seam_rms,
        100.0 * exact.overlap_rms
    );
    assert!(erms < 0.004, "noise-free mosaic RMS {erms}");
    // The absolute difference from the truth, brightened eightfold, for the pictures.
    let diff_image = |m: &Frame, origin: (f64, f64)| {
        let (g0, o0) = CUTS[0].2;
        let mut out = Frame::zeros(m.width, m.height);
        for y in 0..m.height {
            for x in 0..m.width {
                let v = m.data[y * m.width + x];
                if v == 0.0 {
                    continue;
                }
                let (tx, ty) = (
                    x as f64 + origin.0 + CUTS[0].0.0,
                    y as f64 + origin.1 + CUTS[0].0.1,
                );
                let t = sample(&truth, tx as f32, ty as f32, Kernel::Lanczos3) * g0 + o0;
                out.data[y * m.width + x] = ((v - t).abs() * 8.0).min(1.0);
            }
        }
        out
    };
    if let Some(dir) = std::env::var_os("HELIA_MOSAIC_OUT").map(PathBuf::from) {
        std::fs::create_dir_all(&dir).expect("dir");
        for (i, f) in panels.iter().enumerate() {
            write_png(&dir.join(format!("panel-{}.png", i + 1)), f);
        }
        write_png(&dir.join("mosaic.png"), img);
        write_png(&dir.join("truth.png"), &truth);
        write_png(
            &dir.join("diff-default.png"),
            &diff_image(img, mosaic.origin),
        );
        if let Picture::Mono(f) = &no_levels.image {
            write_png(
                &dir.join("diff-no-gain.png"),
                &diff_image(f, no_levels.origin),
            );
        }
        if let Picture::Mono(f) = &no_levels.image {
            write_png(&dir.join("mosaic-no-gain.png"), f);
        }
        if let Picture::Mono(f) = &no_seams.image {
            write_png(&dir.join("mosaic-no-seams.png"), f);
        }
    }
    assert!(worst_shift < 0.3, "worst placement error {worst_shift} px");
    assert!(
        worst_angle < 0.08,
        "worst angle error {worst_angle} degrees"
    );
    assert!(rms < 0.02, "mosaic RMS against the truth {rms}");
}
