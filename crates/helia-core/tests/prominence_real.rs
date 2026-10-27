// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The prominence composite on the real H-alpha captures (set
//! `HELIA_REAL_DATA` to the folder holding them). Prints what the edit did
//! to the brightness of the prominences, the glow and the noise, and checks
//! that it lifted the prominences, left the disc alone and did not raise the
//! noise of the empty sky.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use helia_core::edit::{Prominence, apply_cpu, sharpen_edits};
use helia_core::io::source::{OpenOptions, open_capture};
use helia_core::mask::fit_disc;
use helia_core::pipeline::{Pipeline, StackConfig};
use helia_core::{EditList, Frame, Keep, Op, SharpenMethod};

fn find(name: &str) -> Option<PathBuf> {
    let base = PathBuf::from(std::env::var_os("HELIA_REAL_DATA")?);
    [
        base.join(name),
        base.join("owner-2026-10-20").join(name),
        base.join("solar-2022-07-17").join(name),
    ]
    .into_iter()
    .find(|p| p.exists())
}

fn median(mut v: Vec<f32>) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let k = v.len() / 2;
    *v.select_nth_unstable_by(k, f32::total_cmp).1
}

/// What the prominences look like: brightnesses as shares of the disc's.
#[derive(Debug)]
struct Look {
    /// The brightest 1% of the ring just outside the limb.
    brightest: f32,
    /// The median of that ring (glow, mostly).
    ring_median: f32,
    /// Share of the ring above 5% of the disc.
    visible: f32,
    /// Standard deviation of the far field, which holds no prominence.
    far_noise: f32,
}

fn look(f: &Frame) -> Look {
    let d = fit_disc(f).expect("a disc");
    let (mut disc, mut ring, mut far) = (Vec::new(), Vec::new(), Vec::new());
    for y in 0..f.height {
        for x in 0..f.width {
            let r = (x as f32 - d.cx).hypot(y as f32 - d.cy) / d.r;
            let v = f.get(x, y);
            if (0.3..0.9).contains(&r) {
                disc.push(v);
            } else if (1.02..1.15).contains(&r) {
                ring.push(v);
            } else if r > 1.35 {
                far.push(v);
            }
        }
    }
    let sky = median(far.clone());
    let disc = median(disc) - sky;
    let mut ring: Vec<f32> = ring.iter().map(|v| (v - sky) / disc).collect();
    ring.sort_by(f32::total_cmp);
    let n = ring.len();
    let top = &ring[n - n / 100..];
    let mean = far.iter().sum::<f32>() / far.len() as f32;
    let var = far.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / far.len() as f32;
    Look {
        brightest: top.iter().sum::<f32>() / top.len() as f32,
        ring_median: ring[n / 2],
        visible: ring.iter().filter(|&&v| v > 0.05).count() as f32 / n as f32,
        far_noise: var.sqrt() / disc,
    }
}

/// How rough the layer is just outside the limb, where the join is: the
/// largest differences between a pixel and the mean of its four neighbours
/// (99.5th percentile over the bright pixels of the band), as a share of the
/// disc. A comb of teeth along the limb (issue #263) raises it above
/// the plain image's own value; a smooth layer stays within 0.02 of it.
fn limb_roughness(f: &Frame) -> f32 {
    let d = fit_disc(f).expect("a disc");
    let (mut disc, mut far) = (Vec::new(), Vec::new());
    for y in 0..f.height {
        for x in 0..f.width {
            let r = (x as f32 - d.cx).hypot(y as f32 - d.cy) / d.r;
            if (0.3..0.9).contains(&r) {
                disc.push(f.get(x, y));
            } else if r > 1.35 {
                far.push(f.get(x, y));
            }
        }
    }
    let sky = median(far);
    let level = median(disc) - sky;
    let mut diffs = Vec::new();
    for y in 1..f.height - 1 {
        for x in 1..f.width - 1 {
            let r = (x as f32 - d.cx).hypot(y as f32 - d.cy) / d.r;
            let v = (f.get(x, y) - sky) / level;
            if (1.005..1.05).contains(&r) && v > 0.15 {
                let around =
                    (f.get(x - 1, y) + f.get(x + 1, y) + f.get(x, y - 1) + f.get(x, y + 1)) / 4.0;
                diffs.push(((f.get(x, y) - around) / level).abs());
            }
        }
    }
    if diffs.len() < 50 {
        return 0.0;
    }
    diffs.sort_by(f32::total_cmp);
    diffs[(diffs.len() - 1) * 995 / 1000]
}

#[test]
fn the_prominence_layer_on_the_h_alpha_captures() {
    let mut ran = false;
    for name in ["15_32_09.avi", "15_37_19.avi", "Sun_Test.ser"] {
        let Some(path) = find(name) else { continue };
        ran = true;
        let mut src = open_capture(&path, &OpenOptions::default()).expect("open");
        let cfg = StackConfig {
            keep: Keep::Fraction(0.25),
            ..StackConfig::default()
        };
        let out = Pipeline::new(cfg)
            .run(&mut src, &|_, _| {}, &AtomicBool::new(false))
            .expect("stack");
        // The usual H-alpha edit: wavelet sharpening; then the same with the layer.
        let preset = Some(helia_core::Preset::Ha);
        let sharpen = sharpen_edits(
            SharpenMethod::Wavelet,
            preset,
            out.image.width,
            out.image.height,
        );
        let plain = apply_cpu(&out.image, &sharpen).expect("sharpen");
        let mut ops = sharpen.ops.clone();
        ops.push(Op::Prominence(Prominence::default()));
        let composite = apply_cpu(&out.image, &EditList::new(ops)).expect("composite");

        let (before, after) = (look(&plain), look(&composite));
        eprintln!("{name}\n  before {before:?}\n  after  {after:?}");
        // The disc is untouched well inside the limb.
        let d = fit_disc(&plain).expect("a disc");
        let inner = d.r * 0.9;
        for y in 0..plain.height {
            for x in 0..plain.width {
                if (x as f32 - d.cx).hypot(y as f32 - d.cy) < inner {
                    assert_eq!(plain.get(x, y), composite.get(x, y));
                }
            }
        }
        assert!(
            after.brightest > 1.5 * before.brightest || after.brightest > 0.5,
            "{before:?} {after:?}"
        );
        // The glow is gone, so what is left bright in the ring is prominence.
        assert!(
            after.visible <= before.visible + 0.05,
            "{before:?} {after:?}"
        );
        assert!(
            after.ring_median < before.ring_median,
            "{before:?} {after:?}"
        );
        assert!(after.far_noise <= before.far_noise * 1.05 + 1e-4);
        // No comb of teeth along the limb under a prominence.
        // The plain image's own roughness (the steep limb) is the floor; the
        // old nearest-pixel fill measured 0.075 and 0.090 against 0.054 and
        // 0.057 plain on the two 2022 captures, the fix 0.056 and 0.070.
        let (rough, floor) = (limb_roughness(&composite), limb_roughness(&plain));
        eprintln!("  limb roughness {rough:.3} (plain {floor:.3})");
        assert!(
            rough < floor + 0.02,
            "{name}: teeth along the limb, roughness {rough} against {floor} plain"
        );
    }
    if !ran {
        eprintln!("HELIA_REAL_DATA is not set or holds no H-alpha capture: skipped");
    }
}
