// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Split-half check of a self-calibrated flat on a real capture.
//!
//! `cargo run --release -p helia-core --example selfflat_split -- <capture> [frames]`
//!
//! Reads `frames` (default 120) evenly spread over the capture, solves a flat
//! from the even ones and from the odd ones, and compares them. Structure that
//! is really on the sensor comes out of both halves the same; a solution that is
//! only noise does not.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use helia_core::calibrate::master::CalInput;
use helia_core::calibrate::selfflat::{SelfFlatOptions, solve};
use helia_core::io::source::OpenOptions;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(args.next().expect("capture path"));
    let want: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(120);
    let mut input = CalInput::open(&[path], &OpenOptions::default()).expect("open");
    let n = input.frame_count();
    let pick: Vec<usize> = (0..want.min(n)).map(|i| i * (n - 1) / (want - 1)).collect();
    let frames: Vec<_> = pick.iter().map(|&i| input.read(i).expect("read")).collect();
    let half = |parity: usize| {
        let sub: Vec<_> = frames
            .iter()
            .enumerate()
            .filter(|(k, _)| k % 2 == parity)
            .map(|(_, f)| f.clone())
            .collect();
        solve(
            &sub,
            None,
            &SelfFlatOptions::default(),
            &AtomicBool::new(false),
            &|_| {},
        )
        .expect("solve")
    };
    let (a, b) = (half(0), half(1));
    let (ga, gb) = (&a.master.frame.data, &b.master.frame.data);
    // The pixels both solved (a pixel left at 1 is not covered), leaving out the
    // wild ones (hot pixels, a burned-in timestamp) that are further than 0.5
    // from 1 in either.
    let solved = |i: usize| (ga[i] - 1.0).abs() > 1e-6 && (gb[i] - 1.0).abs() > 1e-6;
    let all: Vec<usize> = (0..ga.len()).filter(|&i| solved(i)).collect();
    let both: Vec<usize> = all
        .iter()
        .copied()
        .filter(|&i| (ga[i] - 1.0).abs() < 0.5 && (gb[i] - 1.0).abs() < 0.5)
        .collect();
    println!(
        "{} of {} solved pixels are wild and left out",
        all.len() - both.len(),
        all.len()
    );
    let rms = |f: &dyn Fn(usize) -> f32| {
        (both.iter().map(|&i| f64::from(f(i)).powi(2)).sum::<f64>() / both.len() as f64).sqrt()
    };
    let (sa, sb) = (rms(&|i| ga[i] - 1.0), rms(&|i| gb[i] - 1.0));
    let diff = rms(&|i| ga[i] - gb[i]);
    let cov = both
        .iter()
        .map(|&i| f64::from(ga[i] - 1.0) * f64::from(gb[i] - 1.0))
        .sum::<f64>()
        / both.len() as f64;
    println!(
        "{} frames per half, {} pixels solved in both, misfit {:.2}% and {:.2}%",
        a.report.frames,
        both.len(),
        a.report.residual * 100.0,
        b.report.residual * 100.0
    );
    println!(
        "RMS of the flat: {:.3}% and {:.3}%; RMS of the difference {:.3}%; correlation {:.3}",
        sa * 100.0,
        sb * 100.0,
        diff * 100.0,
        cov / (sa * sb)
    );
    println!(
        "span {:.0} px, high-pass {:?} px",
        a.report.span_px, a.report.high_pass_px
    );
}
