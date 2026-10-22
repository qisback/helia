// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The seeing estimate on the real captures (set `HELIA_REAL_DATA` to the
//! folder holding them): the best frames of a capture, ranked by sharpness,
//! should read as better seeing than its worst frames.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use helia_core::debayer::{Channel, Debayer};
use helia_core::io::source::{FrameSource, LimitedSource, OpenOptions, open_capture};
use helia_core::pipeline::{Pipeline, StackConfig};
use helia_core::seeing::SeeingSettings;
use helia_core::{Frame, Keep, Result};

const FRAMES: usize = 240;
const APERTURE_MM: f32 = 80.0;

fn find(name: &str) -> Option<PathBuf> {
    let base = PathBuf::from(std::env::var_os("HELIA_REAL_DATA")?);
    [base.join(name), base.join("solar-2022-07-17").join(name)]
        .into_iter()
        .find(|p| p.exists())
}

/// Some of the frames of another source, in the order given.
struct Subset<S> {
    inner: S,
    pick: Vec<usize>,
}

impl<S: FrameSource> FrameSource for Subset<S> {
    fn dimensions(&self) -> (usize, usize) {
        self.inner.dimensions()
    }
    fn frame_count(&self) -> usize {
        self.pick.len()
    }
    fn read(&mut self, index: usize) -> Result<Frame> {
        self.inner.read(self.pick[index])
    }
    fn describe(&self) -> String {
        format!("{} frames of {}", self.pick.len(), self.inner.describe())
    }
}

fn open(path: &Path) -> LimitedSource<Box<dyn FrameSource>> {
    let inner = open_capture(path, &OpenOptions::default()).unwrap_or_else(|e| panic!("{e}"));
    LimitedSource::new(inner, 0, FRAMES)
}

/// The sodium preset stacks the green channel: the filter leaves the blue
/// pixels empty, and averaging them in prints the mosaic over the picture.
fn base() -> StackConfig {
    StackConfig {
        debayer: Debayer {
            channel: Channel::Green,
            ..Debayer::default()
        },
        ..StackConfig::default()
    }
}

fn seeing_of(path: &Path, pick: &[usize]) -> Option<helia_core::seeing::SeeingReport> {
    let mut src = Subset {
        inner: open(path),
        pick: pick.to_vec(),
    };
    let cfg = StackConfig {
        keep: Keep::Fraction(0.5),
        multipoint: None,
        seeing: Some(SeeingSettings {
            aperture_mm: Some(APERTURE_MM),
            wavelength_nm: Some(589.3),
        }),
        ..base()
    };
    Pipeline::new(cfg)
        .run(&mut src, &|_, _| {}, &AtomicBool::new(false))
        .unwrap_or_else(|e| panic!("{e}"))
        .seeing
}

#[test]
fn the_best_frames_read_as_better_seeing_than_the_worst() {
    if std::env::var_os("HELIA_REAL_DATA").is_none() {
        eprintln!("HELIA_REAL_DATA is not set: skipping real-data tests");
        return;
    }
    for name in ["15_20_57.ser", "Sun_Test.ser", "15_32_09.avi"] {
        let Some(path) = find(name) else {
            eprintln!("{name} not found: skipped");
            continue;
        };
        let mut src = open(&path);
        let scores = Pipeline::new(base())
            .score(&mut src, &|_, _| {}, &AtomicBool::new(false))
            .unwrap_or_else(|e| panic!("{e}"));
        let rank = helia_core::quality::rank(&scores);
        // A quarter of the frames, or a half when the capture is short: the
        // spectral ratio wants about fifty frames.
        let q = if rank.len() >= 200 {
            rank.len() / 4
        } else {
            rank.len() / 2
        };
        let (best, worst) = (&rank[..q], &rank[rank.len() - q..]);
        let (b, w) = (seeing_of(&path, best), seeing_of(&path, worst));
        let (Some(b), Some(w)) = (b, w) else {
            panic!("{name}: no seeing report");
        };
        let (bm, wm) = (
            b.motion.expect("best motion").rms_px,
            w.motion.expect("worst motion").rms_px,
        );
        let (br, wr) = (
            b.spectral.map(|s| s.d_over_r0),
            w.spectral.map(|s| s.d_over_r0),
        );
        eprintln!(
            "{name}: motion index best {bm:.3} px, worst {wm:.3} px; D/r0 best {br:?}, worst {wr:?}"
        );
        assert!(bm < wm, "{name}: motion index {bm} vs {wm}");
        if let (Some(br), Some(wr)) = (br, wr) {
            assert!(br < wr, "{name}: D/r0 {br} vs {wr}");
        }
    }
}
