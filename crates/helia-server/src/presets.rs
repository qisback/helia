// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `GET /api/v1/presets`: the filter presets and what they recommend.

use helia_core::Preset;
use helia_core::debayer::Channel;
use serde::Serialize;
use utoipa::ToSchema;

/// The colour parameters of a preset.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct PresetColour {
    /// Tone exponent applied before colouring (below 1 brightens).
    pub gamma: f32,
    /// Per-channel gain (red, green, blue).
    pub gains: [f32; 3],
    /// Per-channel exponent (red, green, blue).
    pub exponents: [f32; 3],
    /// What a mid-grey (0.5) becomes, 8-bit red, green, blue.
    pub mid_grey_rgb: [u8; 3],
}

/// The recommended sharpening values of a preset (what `sharpen` in a job
/// uses for this filter).
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct PresetSharpen {
    /// Wavelet gain of layers 1 to 6.
    pub wavelet_gains: [f32; 6],
    /// Wavelet noise threshold of layers 1 to 6, in noise sigmas.
    pub wavelet_noise: [f32; 6],
    /// Unsharp mask amount.
    pub unsharp_amount: f32,
    /// Unsharp mask threshold (image units, 0 to 1).
    pub unsharp_threshold: f32,
    /// Richardson-Lucy iterations.
    pub rl_iterations: u32,
    /// Richardson-Lucy total-variation weight.
    pub rl_tv: f32,
    /// Wavelet denoise strength.
    pub denoise: f32,
    /// Why they are what they are.
    pub reason: &'static str,
}

/// One filter preset.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct PresetView {
    /// Id to give as `preset` or `colour` in a job: `ha`, `cak`, ...
    pub id: &'static str,
    /// Plain name.
    pub name: &'static str,
    /// Wavelength as printed in the docs.
    pub wavelength: &'static str,
    /// Hue in words.
    pub hue: &'static str,
    /// One line on what it is for.
    pub summary: &'static str,
    /// Why the recommendations are what they are.
    pub reason: &'static str,
    /// Bayer channel stacked (`r`, `g`, `b` or `lum`).
    pub channel: &'static str,
    /// Share of frames kept, percent.
    pub keep_percent: f32,
    /// Colour parameters. Colour is presentation, not measurement.
    pub colour: PresetColour,
    /// Recommended sharpening values.
    pub sharpen: PresetSharpen,
}

/// `GET /presets` body.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct PresetList {
    /// Every preset.
    pub presets: Vec<PresetView>,
}

/// Name of a channel as the API spells it.
#[must_use]
pub fn channel_name(c: Channel) -> &'static str {
    match c {
        Channel::Red => "r",
        Channel::Green => "g",
        Channel::Blue => "b",
        Channel::Luminance => "lum",
    }
}

/// All presets.
#[must_use]
pub fn list() -> PresetList {
    PresetList {
        presets: Preset::ALL
            .into_iter()
            .map(|p| {
                let i = p.info();
                PresetView {
                    id: i.id,
                    name: i.name,
                    wavelength: i.wavelength,
                    hue: i.hue,
                    summary: i.summary,
                    reason: i.reason,
                    channel: channel_name(i.channel),
                    keep_percent: i.keep_percent,
                    colour: PresetColour {
                        gamma: i.gamma,
                        gains: i.gains,
                        exponents: i.exponents,
                        mid_grey_rgb: p.colour_of(0.5).map(|v| (v * 255.0).round() as u8),
                    },
                    sharpen: PresetSharpen {
                        wavelet_gains: i.sharpen.gains,
                        wavelet_noise: i.sharpen.thresholds,
                        unsharp_amount: i.sharpen.unsharp_amount,
                        unsharp_threshold: i.sharpen.unsharp_threshold,
                        rl_iterations: i.sharpen.rl_iterations,
                        rl_tv: i.sharpen.rl_tv,
                        denoise: i.sharpen.denoise,
                        reason: i.sharpen_reason,
                    },
                }
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_is_listed_once_with_a_mid_grey() {
        let l = list();
        assert_eq!(l.presets.len(), Preset::ALL.len());
        let ha = &l.presets[0];
        assert_eq!((ha.id, ha.channel), ("ha", "r"));
        assert!(ha.colour.mid_grey_rgb[0] > ha.colour.mid_grey_rgb[2]);
        assert!(ha.sharpen.wavelet_gains[0] > 1.0 && ha.sharpen.rl_iterations > 0);
    }
}
