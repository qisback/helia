// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Busy-time accounting per kind of work.
//!
//! [`crate::pipeline::StackOutput::timings`] gives wall time per pipeline
//! stage. Within a stage the work is spread over threads and mixed (decoding
//! overlaps scoring), so [`Profile`] also sums the time every thread spends in
//! each [`Category`]. Busy seconds divided by the thread count approximate the
//! wall time a category would take on its own, which is what to optimise.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// A kind of work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    /// Reading and converting stored samples to `f32`.
    Decode,
    /// Demosaicing to mono.
    Debayer,
    /// Sharpness scoring and ranking.
    Quality,
    /// Reference image and global (phase correlation) registration.
    GlobalAlign,
    /// Alignment point preparation, correlation and window scoring.
    ApAlign,
    /// Resampling and accumulating frames.
    Stack,
    /// Blending, normalising and downloading the result.
    Finish,
}

impl Category {
    /// Every category, in display order.
    pub const ALL: [Self; 7] = [
        Self::Decode,
        Self::Debayer,
        Self::Quality,
        Self::GlobalAlign,
        Self::ApAlign,
        Self::Stack,
        Self::Finish,
    ];

    /// Short label for tables.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Decode => "decode",
            Self::Debayer => "debayer",
            Self::Quality => "quality",
            Self::GlobalAlign => "global align",
            Self::ApAlign => "AP align",
            Self::Stack => "stack",
            Self::Finish => "finish",
        }
    }
}

/// Worker threads available to the pipeline.
#[must_use]
pub fn threads() -> usize {
    rayon::current_num_threads().max(1)
}

/// Thread-safe accumulator of busy time per [`Category`].
#[derive(Debug, Default)]
pub struct Profile {
    nanos: [AtomicU64; Category::ALL.len()],
}

impl Profile {
    /// All-zero profile.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add `d` to `cat`.
    pub fn add(&self, cat: Category, d: Duration) {
        let n = u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
        self.nanos[cat as usize].fetch_add(n, Ordering::Relaxed);
    }

    /// Run `f` and charge its duration to `cat`.
    pub fn time<T>(&self, cat: Category, f: impl FnOnce() -> T) -> T {
        let t = Instant::now();
        let v = f();
        self.add(cat, t.elapsed());
        v
    }

    /// Busy time per category, in display order.
    #[must_use]
    pub fn snapshot(&self) -> Vec<(Category, Duration)> {
        Category::ALL
            .iter()
            .map(|&c| {
                (
                    c,
                    Duration::from_nanos(self.nanos[c as usize].load(Ordering::Relaxed)),
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_charges_the_right_category() {
        let p = Profile::new();
        p.time(Category::Stack, || {
            std::thread::sleep(Duration::from_millis(5));
        });
        p.add(Category::Decode, Duration::from_millis(2));
        let s = p.snapshot();
        assert_eq!(s.len(), Category::ALL.len());
        let get = |c| {
            s.iter()
                .find(|(k, _)| *k == c)
                .map(|x| x.1)
                .unwrap_or_default()
        };
        assert!(get(Category::Stack) >= Duration::from_millis(5));
        assert_eq!(get(Category::Decode), Duration::from_millis(2));
        assert_eq!(get(Category::Quality), Duration::ZERO);
    }
}
