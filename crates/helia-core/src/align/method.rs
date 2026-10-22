// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Which method aligns whole frames, and what it reports.
//!
//! Two methods exist: fitting the solar limb ([`crate::align::limb`]) and FFT
//! phase correlation ([`crate::align::global`]). With [`AlignMethod::Auto`] the
//! limb is used when the whole disc is visible and its fit is confident, and
//! phase correlation otherwise (a cropped region, a disc cut by the frame, no
//! disc at all). Either way every frame carries a confidence from 0 to 1, and a
//! frame whose limb fit is not confident falls back to phase correlation.

use std::str::FromStr;

use crate::align::Shift;
use crate::align::limb::{LimbFit, LimbReference};
use crate::error::{Error, Result};
use crate::frame::Frame;

/// Reference limb confidence needed for [`AlignMethod::Auto`] to choose the limb.
pub const AUTO_MIN_CONFIDENCE: f32 = 0.8;

/// What the user asks for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AlignMethod {
    /// The limb when the full disc is visible and the fit is confident, else
    /// phase correlation.
    #[default]
    Auto,
    /// Always phase correlation.
    Phase,
    /// The limb fit; frames without a usable limb still fall back to phase
    /// correlation, and a reference without one is an error.
    Limb,
}

impl AlignMethod {
    /// The name used on the command line and in the API.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Phase => "phase",
            Self::Limb => "limb",
        }
    }
}

impl FromStr for AlignMethod {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "phase" | "phase-correlation" => Ok(Self::Phase),
            "limb" | "limb-fit" => Ok(Self::Limb),
            other => Err(Error::Invalid(format!(
                "unknown alignment method {other:?}; use auto, phase or limb"
            ))),
        }
    }
}

/// The method in force for a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlobalMethod {
    /// Limb fit.
    Limb,
    /// Phase correlation.
    Phase,
}

impl GlobalMethod {
    /// Plain name for messages.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Limb => "limb fit",
            Self::Phase => "phase correlation",
        }
    }
}

/// Confidence of a phase-correlation lock from its peak-to-sidelobe ratio:
/// 0 at 4 (not trustworthy), 1 at 10 (a solid lock), linear between.
#[must_use]
pub fn phase_confidence(psr: f32) -> f32 {
    if psr.is_finite() {
        ((psr - 4.0) / 6.0).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// One frame's global alignment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameAlign {
    /// Shift relative to the reference.
    pub shift: Shift,
    /// 0 to 1.
    pub confidence: f32,
    /// What produced the shift.
    pub used: GlobalMethod,
    /// The limb was wanted but this frame fell back to phase correlation.
    pub fell_back: bool,
}

/// What a run did for global alignment, for the command line, the app and the API.
#[derive(Clone, Debug, PartialEq)]
pub struct AlignReport {
    /// What was asked for.
    pub requested: AlignMethod,
    /// What ran.
    pub method: GlobalMethod,
    /// Why, in a sentence.
    pub reason: String,
    /// The limb fit of the reference, when one was made.
    pub limb: Option<LimbFit>,
    /// Frames aligned.
    pub frames: usize,
    /// Mean confidence; `None` when not measured.
    pub confidence_mean: Option<f32>,
    /// Least confidence.
    pub confidence_min: Option<f32>,
    /// Frames aligned by the limb fit.
    pub limb_frames: usize,
    /// Frames aligned by phase correlation.
    pub phase_frames: usize,
    /// Frames that wanted the limb but used phase correlation.
    pub fallbacks: usize,
    /// The confidence comes from a sample of the frames (the GPU registers
    /// without one), not from every frame.
    pub sampled: bool,
}

impl AlignReport {
    /// One line for a log or a status line.
    #[must_use]
    pub fn summary(&self) -> String {
        let conf = match (self.confidence_mean, self.confidence_min) {
            (Some(m), Some(lo)) => format!(
                "; confidence mean {m:.2}, least {lo:.2}{}",
                if self.sampled { " (sampled)" } else { "" }
            ),
            _ => String::new(),
        };
        let fb = if self.fallbacks > 0 {
            format!(
                "; {} frame(s) fell back to phase correlation",
                self.fallbacks
            )
        } else {
            String::new()
        };
        format!("{} ({}){conf}{fb}", self.method.name(), self.reason)
    }

    fn totals(&mut self, frames: &[FrameAlign]) {
        self.frames = frames.len();
        self.limb_frames = frames
            .iter()
            .filter(|f| f.used == GlobalMethod::Limb)
            .count();
        self.phase_frames = frames.len() - self.limb_frames;
        self.fallbacks = frames.iter().filter(|f| f.fell_back).count();
        if !frames.is_empty() {
            self.confidence_mean =
                Some(frames.iter().map(|f| f.confidence).sum::<f32>() / frames.len() as f32);
            self.confidence_min = frames.iter().map(|f| f.confidence).reduce(f32::min);
        }
    }

    /// Report for `frames`, every frame aligned.
    #[must_use]
    pub fn measured(plan: &AlignPlan, frames: &[FrameAlign]) -> Self {
        let mut r = Self::empty(plan);
        r.totals(frames);
        r
    }

    /// Report for a run whose shifts came from the GPU: `sample` is a few of
    /// the frames re-measured on the CPU, `all` the number registered.
    #[must_use]
    pub fn sampled(plan: &AlignPlan, sample: &[FrameAlign], all: usize) -> Self {
        let mut r = Self::empty(plan);
        r.totals(sample);
        r.sampled = true;
        r.limb_frames = 0;
        r.phase_frames = all;
        r.fallbacks = 0;
        r.frames = all;
        r
    }

    fn empty(plan: &AlignPlan) -> Self {
        Self {
            requested: plan.requested,
            method: plan.method,
            reason: plan.reason.clone(),
            limb: plan.limb.as_ref().map(|l| l.fit),
            frames: 0,
            confidence_mean: None,
            confidence_min: None,
            limb_frames: 0,
            phase_frames: 0,
            fallbacks: 0,
            sampled: false,
        }
    }
}

/// The decision, made once from the reference.
#[derive(Clone, Debug)]
pub struct AlignPlan {
    /// What was asked for.
    pub requested: AlignMethod,
    /// What will run.
    pub method: GlobalMethod,
    /// Why.
    pub reason: String,
    /// The reference limb, when one was fitted (also when phase was chosen).
    pub limb: Option<LimbReference>,
}

impl AlignPlan {
    /// Decide how to align frames to `reference`.
    ///
    /// # Errors
    /// [`Error::Invalid`] when the limb is asked for and the reference has none.
    pub fn choose(requested: AlignMethod, reference: &Frame) -> Result<Self> {
        if requested == AlignMethod::Phase {
            return Ok(Self {
                requested,
                method: GlobalMethod::Phase,
                reason: "chosen".into(),
                limb: None,
            });
        }
        let limb = LimbReference::new(reference);
        let (method, reason) = match (&limb, requested) {
            (None, AlignMethod::Limb) => {
                return Err(Error::Invalid(
                    "no solar limb found in the reference image; use --align phase (or auto)"
                        .into(),
                ));
            }
            (None, _) => (GlobalMethod::Phase, "no solar disc found".to_owned()),
            (Some(l), AlignMethod::Limb) => (
                GlobalMethod::Limb,
                format!(
                    "chosen, {:.0}% of the limb in the frame, confidence {:.2}",
                    l.fit.in_frame * 100.0,
                    l.fit.confidence
                ),
            ),
            (Some(l), _) if !l.fit.is_full_disc() => (
                GlobalMethod::Phase,
                format!(
                    "the frame cuts the limb, {:.0}% of it visible",
                    l.fit.in_frame * 100.0
                ),
            ),
            (Some(l), _) if l.fit.confidence < AUTO_MIN_CONFIDENCE => (
                GlobalMethod::Phase,
                format!("limb fit not confident enough, {:.2}", l.fit.confidence),
            ),
            (Some(l), _) => (
                GlobalMethod::Limb,
                format!(
                    "full disc visible, reference limb confidence {:.2}",
                    l.fit.confidence
                ),
            ),
        };
        Ok(Self {
            requested,
            method,
            reason,
            limb,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_confidence_runs_from_four_to_ten() {
        assert_eq!(phase_confidence(3.0), 0.0);
        assert_eq!(phase_confidence(4.0), 0.0);
        assert!((phase_confidence(7.0) - 0.5).abs() < 1e-6);
        assert_eq!(phase_confidence(10.0), 1.0);
        assert_eq!(phase_confidence(50.0), 1.0);
        assert_eq!(phase_confidence(f32::NAN), 0.0);
    }

    #[test]
    fn the_report_counts_frames_and_says_what_it_did() {
        let plan = AlignPlan {
            requested: AlignMethod::Limb,
            method: GlobalMethod::Limb,
            reason: "chosen".into(),
            limb: None,
        };
        let frame = |c: f32, used, fell_back| FrameAlign {
            shift: Shift::new(0.0, 0.0),
            confidence: c,
            used,
            fell_back,
        };
        let frames = [
            frame(0.9, GlobalMethod::Limb, false),
            frame(0.7, GlobalMethod::Limb, false),
            frame(0.4, GlobalMethod::Phase, true),
        ];
        let r = AlignReport::measured(&plan, &frames);
        assert_eq!(
            (r.frames, r.limb_frames, r.phase_frames, r.fallbacks),
            (3, 2, 1, 1)
        );
        assert!((r.confidence_mean.unwrap_or(0.0) - 2.0 / 3.0).abs() < 1e-6);
        assert_eq!(r.confidence_min, Some(0.4));
        assert!(!r.sampled);
        let text = r.summary();
        assert!(text.starts_with("limb fit (chosen)"), "{text}");
        assert!(text.contains("1 frame(s) fell back"), "{text}");
        let s = AlignReport::sampled(&plan, &frames[..2], 40);
        assert!(s.sampled && s.frames == 40);
        assert!(s.summary().contains("(sampled)"));
        let none = AlignReport::measured(&plan, &[]);
        assert_eq!(none.confidence_mean, None);
    }
}
