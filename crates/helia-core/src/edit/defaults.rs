// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Recommended sharpening settings: a method, and per-filter values for it.
//!
//! The values live beside the colour looks (`PresetInfo::sharpen` in
//! [`crate::colour`]) and follow the same rule as the rest of the
//! recommendations: a starting point that is safe, never the last word. Every
//! number is an ordinary option that overrides it.
//!
//! The reasoning, in short. Faint lines (Ca K and H, He D3, H-beta) are noisy,
//! so they get higher noise thresholds, fewer deconvolution iterations and
//! gentler gains: sharpening noise helps nobody. White light carries the most
//! signal and the finest structure (granulation), so it takes the strongest
//! settings. H-alpha sits between: plenty of signal, fine fibrils on the
//! scale of layers 2 and 3.

use serde::{Deserialize, Serialize};

use super::deconv::{Deconvolve, Psf, heuristic_sigma};
use super::wavelet::{Layer, Unsharp, Wavelet};
use super::{EditList, Op};
use crate::colour::Preset;
use crate::error::{Error, Result};

/// How the stack is sharpened.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharpenMethod {
    /// Leave the stack as it is.
    None,
    /// Unsharp mask.
    Unsharp,
    /// A-trous wavelet layers (the usual first choice).
    #[default]
    Wavelet,
    /// Richardson-Lucy deconvolution.
    RichardsonLucy,
}

impl SharpenMethod {
    /// Every method, in menu order.
    pub const ALL: [SharpenMethod; 4] = [
        Self::None,
        Self::Wavelet,
        Self::Unsharp,
        Self::RichardsonLucy,
    ];

    /// Id used on the command line (`none`, `unsharp`, `wavelet`, `rl`).
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Unsharp => "unsharp",
            Self::Wavelet => "wavelet",
            Self::RichardsonLucy => "rl",
        }
    }

    /// Plain name.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Unsharp => "Unsharp mask",
            Self::Wavelet => "Wavelets",
            Self::RichardsonLucy => "Richardson-Lucy",
        }
    }

    /// One sentence on what it does and when to reach for it.
    #[must_use]
    pub fn summary(self) -> &'static str {
        match self {
            Self::None => "The stack is left as it is.",
            Self::Unsharp => {
                "Adds back some of the fine detail a blur removes. Quick and predictable; best for a light touch."
            }
            Self::Wavelet => {
                "Boosts detail one size at a time and removes noise at the same time. The usual choice for a solar stack."
            }
            Self::RichardsonLucy => {
                "Undoes a known blur. The most powerful and the most likely to leave artefacts; needs a good stack and a sensible blur size."
            }
        }
    }
}

impl std::str::FromStr for SharpenMethod {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" | "off" | "no" => Ok(Self::None),
            "unsharp" | "usm" | "unsharp-mask" => Ok(Self::Unsharp),
            "wavelet" | "wavelets" | "atrous" => Ok(Self::Wavelet),
            "rl" | "richardson-lucy" | "deconvolution" | "deconv" => Ok(Self::RichardsonLucy),
            other => Err(Error::Invalid(format!(
                "unknown sharpening {other:?}; use wavelet, unsharp, rl or none"
            ))),
        }
    }
}

/// Recommended sharpening values for one filter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SharpenTuning {
    /// Wavelet gain of layers 1 to 6.
    pub gains: [f32; 6],
    /// Wavelet noise threshold of layers 1 to 6, in noise sigmas.
    pub thresholds: [f32; 6],
    /// Unsharp amount.
    pub unsharp_amount: f32,
    /// Unsharp threshold (image units).
    pub unsharp_threshold: f32,
    /// Richardson-Lucy iterations.
    pub rl_iterations: u32,
    /// Richardson-Lucy total-variation weight.
    pub rl_tv: f32,
    /// Wavelet denoise strength for the Adjust step.
    pub denoise: f32,
}

impl SharpenTuning {
    /// For a capture whose filter is not known.
    pub const GENERIC: SharpenTuning = SharpenTuning {
        gains: [1.5, 2.6, 2.0, 1.4, 1.0, 1.0],
        thresholds: [3.0, 1.2, 0.6, 0.0, 0.0, 0.0],
        unsharp_amount: 0.6,
        unsharp_threshold: 0.003,
        rl_iterations: 12,
        rl_tv: 0.0,
        denoise: 1.5,
    };
}

impl Preset {
    /// The recommended sharpening values of this filter.
    #[must_use]
    pub fn sharpen_tuning(self) -> &'static SharpenTuning {
        &self.info().sharpen
    }
}

/// The values for `preset`, or the generic ones.
#[must_use]
pub fn tuning_for(preset: Option<Preset>) -> SharpenTuning {
    preset.map_or(SharpenTuning::GENERIC, |p| *p.sharpen_tuning())
}

/// The edit list that sharpens with `method` using the recommended values of
/// `preset` for an image of `width` x `height`. A deconvolution leaves its
/// PSF at [`Psf::Limb`]: measured from the solar limb when the edit runs, or
/// sized from the image scale when there is no clear limb.
#[must_use]
pub fn sharpen_edits(
    method: SharpenMethod,
    preset: Option<Preset>,
    width: usize,
    height: usize,
) -> EditList {
    let t = tuning_for(preset);
    let op = match method {
        SharpenMethod::None => return EditList::default(),
        SharpenMethod::Unsharp => Op::Unsharp(Unsharp {
            radius: (1.4 * heuristic_sigma(width, height)).clamp(1.0, 3.0),
            amount: t.unsharp_amount,
            threshold: t.unsharp_threshold,
        }),
        SharpenMethod::Wavelet => Op::Wavelet(Wavelet::from_layers(
            t.gains
                .iter()
                .zip(&t.thresholds)
                .map(|(&gain, &threshold)| Layer::from_gain(gain, threshold))
                .collect(),
        )),
        SharpenMethod::RichardsonLucy => Op::Deconvolve(Deconvolve {
            psf: Psf::Limb,
            iterations: t.rl_iterations,
            tv: t.rl_tv,
            protect: 1.0,
        }),
    };
    EditList::new(vec![op])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn methods_parse_by_their_ids_and_aliases() {
        for m in SharpenMethod::ALL {
            assert_eq!(m.id().parse::<SharpenMethod>().ok(), Some(m));
        }
        assert_eq!(
            "USM".parse::<SharpenMethod>().ok(),
            Some(SharpenMethod::Unsharp)
        );
        assert!("wobble".parse::<SharpenMethod>().is_err());
    }

    #[test]
    fn every_filter_has_valid_recommendations_for_every_method() {
        for p in Preset::ALL.into_iter().map(Some).chain([None]) {
            for m in SharpenMethod::ALL {
                let list = sharpen_edits(m, p, 1280, 960);
                assert_eq!(list.is_empty(), m == SharpenMethod::None);
                list.validate(1280, 960)
                    .unwrap_or_else(|e| panic!("{p:?} {m:?}: {e}"));
            }
        }
    }

    #[test]
    fn faint_lines_are_treated_more_gently_than_white_light() {
        let white = Preset::White.sharpen_tuning();
        for faint in [Preset::CaK, Preset::CaH, Preset::HeD3, Preset::Hbeta] {
            let t = faint.sharpen_tuning();
            assert!(t.thresholds[0] > white.thresholds[0], "{faint:?}");
            assert!(t.rl_iterations < white.rl_iterations, "{faint:?}");
            assert!(t.gains[0] < white.gains[0], "{faint:?}");
        }
    }

    #[test]
    fn recommended_gains_never_cut_detail() {
        for p in Preset::ALL {
            let t = p.sharpen_tuning();
            assert!(t.gains.iter().all(|&g| g >= 1.0));
            assert!(t.thresholds.iter().all(|&g| g >= 0.0));
        }
    }
}
