// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Colourisation presets: a declared look for each solar line.
//!
//! Colour here is presentation, not measurement. A narrow-band solar capture
//! is monochrome; the orange of H-alpha or the violet of Ca K is a choice made
//! when the picture is shared. Helia keeps the stack mono (FITS output is never
//! altered) and applies a [`ColourLut`] only when a colour file is written. The
//! preset that was used is recorded in the file.
//!
//! The mapping is the classic amateur one: levels and gamma on the mono value,
//! then a gain and an exponent per channel. For a preset with tone gamma `g`,
//! gains `k` and exponents `e`, a mono value `v` in `0..=1` becomes
//!
//! ```text
//! t   = v ^ g                    (after invert: v -> 1 - v)
//! rgb = lerp(t, min(1, k * t^e), strength)
//! ```
//!
//! per channel. Every exponent is positive, so each curve rises monotonically
//! and a darker input never gets a brighter output. The result is sampled once
//! into a [`ColourLut`] of [`LUT_SIZE`] entries, which is the specification for
//! the CPU path ([`apply_lut`]) and for the GPU kernel.
//!
//! Colour conventions: there is no formal standard. Amateur practice is
//! greyscale, pseudo-colour, and inverted variants per filter, with H-alpha
//! commonly red-orange (`docs/research/state-of-the-art-2026.md`, section 6.2).
//! The hues below follow the colour of the line's wavelength where the eye can
//! see it, and a conventional violet for the two calcium lines, which sit at
//! the edge of vision.

use rayon::prelude::*;

use crate::debayer::Channel;
use crate::edit::SharpenTuning;
use crate::error::{Error, Result};
use crate::frame::Frame;

/// Entries in a [`ColourLut`].
pub const LUT_SIZE: usize = 4096;

/// A solar line (or white light) with a recommended look.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Preset {
    /// H-alpha, 656.3 nm.
    Ha,
    /// Ca II K, 393.4 nm.
    CaK,
    /// Ca II H, 396.8 nm.
    CaH,
    /// Mg I b, 517.3 nm.
    Mg,
    /// Na I D, 589.0 and 589.6 nm.
    Na,
    /// He I D3, 587.6 nm.
    HeD3,
    /// H-beta, 486.1 nm.
    Hbeta,
    /// White light / continuum.
    White,
}

/// Recommended settings, colour parameters and the words that explain them.
#[derive(Clone, Copy, Debug)]
pub struct PresetInfo {
    /// The preset.
    pub preset: Preset,
    /// Short id used on the command line and in the API: `ha`, `cak`, ...
    pub id: &'static str,
    /// Plain name.
    pub name: &'static str,
    /// Wavelength as printed in the docs.
    pub wavelength: &'static str,
    /// Hue in words.
    pub hue: &'static str,
    /// One-line description for lists.
    pub summary: &'static str,
    /// Why the recommendations are what they are.
    pub reason: &'static str,
    /// Channel to stack from a Bayer capture.
    pub channel: Channel,
    /// Share of frames to keep, percent.
    pub keep_percent: f32,
    /// Tone exponent applied before colouring (below 1 brightens).
    pub gamma: f32,
    /// Per-channel gain.
    pub gains: [f32; 3],
    /// Per-channel exponent.
    pub exponents: [f32; 3],
    /// Recommended sharpening values (see [`crate::edit::defaults`]).
    pub sharpen: SharpenTuning,
    /// Why the sharpening values are what they are.
    pub sharpen_reason: &'static str,
}

const INFO: [PresetInfo; 8] = [
    PresetInfo {
        preset: Preset::Ha,
        id: "ha",
        name: "H-alpha",
        wavelength: "656.3 nm",
        hue: "orange-red",
        summary: "H-alpha chromosphere: red-orange, stacked from the red channel",
        reason: "H-alpha is deep red, so a colour camera records it almost only in the red pixels; the green and blue pixels add noise, not detail.",
        channel: Channel::Red,
        keep_percent: 25.0,
        gamma: 0.85,
        gains: [1.0, 0.45, 0.10],
        exponents: [0.70, 0.90, 1.40],
        sharpen: SharpenTuning {
            gains: [1.4, 3.2, 2.4, 1.6, 1.0, 1.0],
            thresholds: [3.0, 1.0, 0.5, 0.0, 0.0, 0.0],
            unsharp_amount: 0.6,
            unsharp_threshold: 0.003,
            rl_iterations: 12,
            rl_tv: 0.0,
            denoise: 1.5,
        },
        sharpen_reason: "H-alpha has plenty of signal and its fibrils, plage and filaments sit on layers 2 to 4, so those layers are lifted most; the finest layer is lifted least and keeps a high noise threshold. Measured on four real captures, this adds 3 to 5 points more fine detail and 10 points more fibril-scale contrast than the old 1.5, 1.45, 1.25 for 14 points less pixel noise.",
    },
    PresetInfo {
        preset: Preset::CaK,
        id: "cak",
        name: "Ca II K",
        wavelength: "393.4 nm",
        hue: "violet",
        summary: "Calcium K network and plage: violet, stacked from the blue channel",
        reason: "Ca K is at the violet edge of vision and a sensor sees it in its blue pixels. The line is faint, so a few more frames (30%) are kept to average the noise down.",
        channel: Channel::Blue,
        keep_percent: 30.0,
        gamma: 0.90,
        gains: [0.85, 0.78, 1.0],
        exponents: [1.35, 1.90, 0.80],
        sharpen: SharpenTuning {
            gains: [1.7, 2.8, 2.0, 1.4, 1.0, 1.0],
            thresholds: [3.0, 1.5, 0.8, 0.2, 0.0, 0.0],
            unsharp_amount: 0.45,
            unsharp_threshold: 0.004,
            rl_iterations: 8,
            rl_tv: 0.0,
            denoise: 2.0,
        },
        sharpen_reason: "Ca K is faint and noisy, so the gains are gentler, the noise thresholds higher and the deconvolution short: sharpening noise only makes a prettier mess.",
    },
    PresetInfo {
        preset: Preset::CaH,
        id: "cah",
        name: "Ca II H",
        wavelength: "396.8 nm",
        hue: "violet",
        summary: "Calcium H, next to Ca K: violet with a little more red, from the blue channel",
        reason: "Ca H sits 3.4 nm from Ca K, so it takes the same channel and frame share. It is drawn a touch redder so the two can be told apart in a pair.",
        channel: Channel::Blue,
        keep_percent: 30.0,
        gamma: 0.90,
        gains: [0.90, 0.70, 1.0],
        exponents: [1.15, 2.10, 0.85],
        sharpen: SharpenTuning {
            gains: [1.7, 2.8, 2.0, 1.4, 1.0, 1.0],
            thresholds: [3.0, 1.5, 0.8, 0.2, 0.0, 0.0],
            unsharp_amount: 0.45,
            unsharp_threshold: 0.004,
            rl_iterations: 8,
            rl_tv: 0.0,
            denoise: 2.0,
        },
        sharpen_reason: "Ca H is as faint as Ca K and takes the same careful settings.",
    },
    PresetInfo {
        preset: Preset::Mg,
        id: "mg",
        name: "Mg I b",
        wavelength: "517.3 nm",
        hue: "green",
        summary: "Magnesium b2 photosphere and low chromosphere: green, from the green channel",
        reason: "517.3 nm is green, which is where the green pixels of a Bayer sensor respond. The line is dim, so 30% of the frames are kept.",
        channel: Channel::Green,
        keep_percent: 30.0,
        gamma: 0.90,
        gains: [0.80, 1.0, 0.75],
        exponents: [1.80, 0.80, 1.70],
        sharpen: SharpenTuning {
            gains: [1.6, 2.8, 2.0, 1.4, 1.0, 1.0],
            thresholds: [2.5, 1.2, 0.6, 0.0, 0.0, 0.0],
            unsharp_amount: 0.5,
            unsharp_threshold: 0.004,
            rl_iterations: 10,
            rl_tv: 0.0,
            denoise: 1.8,
        },
        sharpen_reason: "Mg b2 is dim, so the noise thresholds are a little higher than for H-alpha and the deconvolution shorter.",
    },
    PresetInfo {
        preset: Preset::Na,
        id: "na",
        name: "Na I D",
        wavelength: "589.0 / 589.6 nm",
        hue: "yellow-orange",
        summary: "Sodium D lines: yellow-orange, stacked from the green channel",
        reason: "589 nm is yellow, which a Bayer sensor splits between its red and green pixels, and the sodium filter passes almost nothing to the blue ones. Averaging all three channels mixes those empty blue pixels into the stack and prints the mosaic as a fine diagonal hatch over the granulation, so the green channel (two pixels in four) is stacked. No real sodium capture has been measured yet: the captures this was first tuned on are H-alpha. It shows granulation, so only the best 12% of frames are kept and the alignment points are a few granules wide.",
        channel: Channel::Green,
        keep_percent: 12.0,
        gamma: 0.90,
        gains: [1.0, 0.74, 0.40],
        exponents: [0.75, 1.05, 2.80],
        sharpen: SharpenTuning {
            gains: [1.5, 2.6, 2.0, 1.3, 1.0, 1.0],
            thresholds: [2.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            unsharp_amount: 0.5,
            unsharp_threshold: 0.003,
            rl_iterations: 10,
            rl_tv: 0.0,
            denoise: 1.5,
        },
        sharpen_reason: "Sodium D is photospheric: granulation is its finest low-contrast detail. Layers 2 and 3 (granule size) are lifted most and carry no noise threshold, because a threshold there removes granulation with the noise; the finest layer keeps its threshold.",
    },
    PresetInfo {
        preset: Preset::HeD3,
        id: "he-d3",
        name: "He I D3",
        wavelength: "587.6 nm",
        hue: "yellow",
        summary: "Helium D3, seen only in flares and prominences: yellow, from all channels combined",
        reason: "587.6 nm is yellow-green, shared by the red and green pixels, so the luminance of all three is used. It is drawn yellower than sodium.",
        channel: Channel::Luminance,
        keep_percent: 25.0,
        gamma: 0.90,
        gains: [1.0, 0.92, 0.45],
        exponents: [0.80, 0.90, 2.60],
        sharpen: SharpenTuning {
            gains: [1.5, 2.4, 1.8, 1.3, 1.0, 1.0],
            thresholds: [3.0, 1.5, 0.8, 0.2, 0.0, 0.0],
            unsharp_amount: 0.4,
            unsharp_threshold: 0.004,
            rl_iterations: 8,
            rl_tv: 0.0,
            denoise: 2.0,
        },
        sharpen_reason: "He D3 shows only in faint prominences and flares, so it is sharpened with the most restraint.",
    },
    PresetInfo {
        preset: Preset::Hbeta,
        id: "hbeta",
        name: "H-beta",
        wavelength: "486.1 nm",
        hue: "blue-cyan",
        summary: "H-beta: blue-cyan, from the blue channel",
        reason: "486.1 nm is blue-cyan and a sensor sees it in its blue pixels. It is faint, so 30% of the frames are kept.",
        channel: Channel::Blue,
        keep_percent: 30.0,
        gamma: 0.90,
        gains: [0.55, 0.85, 1.0],
        exponents: [2.20, 1.00, 0.75],
        sharpen: SharpenTuning {
            gains: [1.5, 2.4, 1.8, 1.3, 1.0, 1.0],
            thresholds: [3.0, 1.5, 0.8, 0.3, 0.0, 0.0],
            unsharp_amount: 0.45,
            unsharp_threshold: 0.004,
            rl_iterations: 8,
            rl_tv: 0.0,
            denoise: 2.0,
        },
        sharpen_reason: "H-beta is faint, like the calcium lines, and takes the same gentle settings.",
    },
    PresetInfo {
        preset: Preset::White,
        id: "white",
        name: "White light",
        wavelength: "continuum",
        hue: "warm neutral",
        summary: "White light (sunspots, granulation): warm neutral, from all channels combined",
        reason: "A white-light capture already carries all three colours, so the luminance is stacked, and a slight warm tint stands in for the solar spectrum. It shows granulation, so only the best 12% of frames are kept and the alignment points are a few granules wide.",
        channel: Channel::Luminance,
        keep_percent: 12.0,
        gamma: 1.0,
        gains: [1.0, 0.96, 0.88],
        exponents: [1.0, 1.0, 1.0],
        sharpen: SharpenTuning {
            gains: [2.0, 2.8, 2.2, 1.3, 1.0, 1.0],
            thresholds: [1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            unsharp_amount: 0.8,
            unsharp_threshold: 0.002,
            rl_iterations: 20,
            rl_tv: 0.0,
            denoise: 1.0,
        },
        sharpen_reason: "White light has the most signal and the finest structure (granulation), so it takes the strongest gains and the longest deconvolution. Layers 2 and 3 (granule size) are lifted most and carry no noise threshold, because a threshold there removes granulation with the noise.",
    },
];

impl Preset {
    /// Every preset, in the order they are listed.
    pub const ALL: [Preset; 8] = [
        Preset::Ha,
        Preset::CaK,
        Preset::CaH,
        Preset::Mg,
        Preset::Na,
        Preset::HeD3,
        Preset::Hbeta,
        Preset::White,
    ];

    /// The recommendations and colour parameters of this preset.
    #[must_use]
    pub fn info(self) -> &'static PresetInfo {
        INFO.iter().find(|i| i.preset == self).unwrap_or(&INFO[0])
    }

    /// Short id (`ha`, `cak`, `he-d3`, ...).
    #[must_use]
    pub fn id(self) -> &'static str {
        self.info().id
    }

    /// Plain name (`H-alpha`).
    #[must_use]
    pub fn name(self) -> &'static str {
        self.info().name
    }

    /// Look a preset up by id. Accepts the ids in any case and a few spellings
    /// (`halpha`, `ca-k`, `hed3`, `d3`, `h-beta`, `wl`).
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        let key: String = id
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .map(|c| c.to_ascii_lowercase())
            .collect();
        match key.as_str() {
            "ha" | "halpha" => Some(Self::Ha),
            "cak" | "caiik" => Some(Self::CaK),
            "cah" | "caiih" => Some(Self::CaH),
            "mg" | "mgb" | "mgi" | "mgib" => Some(Self::Mg),
            "na" | "nad" | "naid" => Some(Self::Na),
            "hed3" | "d3" | "hei" => Some(Self::HeD3),
            "hbeta" | "hb" => Some(Self::Hbeta),
            "white" | "wl" | "whitelight" | "continuum" => Some(Self::White),
            _ => None,
        }
    }

    /// The RGB the preset gives a mono value, with default options.
    #[must_use]
    pub fn colour_of(self, v: f32) -> [f32; 3] {
        ColourParams::preset(self).map_value(v)
    }
}

impl std::str::FromStr for Preset {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::from_id(s).ok_or_else(|| {
            let ids: Vec<&str> = Self::ALL.iter().map(|p| p.id()).collect();
            Error::Invalid(format!(
                "unknown preset {s:?}; use one of {}",
                ids.join(", ")
            ))
        })
    }
}

/// How a mono image is coloured.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColourParams {
    /// The look; `None` is plain mono (still honouring `invert`).
    pub preset: Option<Preset>,
    /// Invert before colouring (disc-inverted H-alpha shows filaments well).
    pub invert: bool,
    /// Mix of colour and grey, `0..=1`. 1 is the full preset.
    pub strength: f32,
    /// The user's adjustments to the look (see [`crate::grade`]).
    pub grade: crate::grade::Grade,
}

impl Default for ColourParams {
    fn default() -> Self {
        Self {
            preset: None,
            invert: false,
            strength: 1.0,
            grade: crate::grade::Grade::default(),
        }
    }
}

impl ColourParams {
    /// The full look of `preset`, not inverted.
    #[must_use]
    pub fn preset(preset: Preset) -> Self {
        Self {
            preset: Some(preset),
            ..Self::default()
        }
    }

    /// True when the output differs from the grey input in hue.
    #[must_use]
    pub fn is_colour(&self) -> bool {
        self.preset.is_some() && self.strength > 0.0
    }

    /// Colour of one mono value (the formula in the module docs).
    #[must_use]
    pub fn map_value(&self, value: f32) -> [f32; 3] {
        let value = if value.is_finite() {
            value.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let value = if self.invert { 1.0 - value } else { value };
        let value = self.grade.tone(value);
        let Some(preset) = self.preset else {
            return self.grade.colour([value; 3]);
        };
        let look = preset.info();
        let tone = value.powf(look.gamma);
        let mix = self.strength.clamp(0.0, 1.0);
        let mut out = [0.0; 3];
        for (c, o) in out.iter_mut().enumerate() {
            let coloured = (look.gains[c] * tone.powf(look.exponents[c])).min(1.0);
            *o = tone + (coloured - tone) * mix;
        }
        self.grade.colour(out)
    }

    /// Sample the mapping into a table.
    #[must_use]
    pub fn lut(&self) -> ColourLut {
        let entries = (0..LUT_SIZE)
            .map(|i| self.map_value(i as f32 / (LUT_SIZE - 1) as f32))
            .collect();
        ColourLut { entries }
    }

    /// One line for file metadata: which look made the colours.
    #[must_use]
    pub fn describe(&self) -> String {
        match self.preset {
            Some(p) => {
                let i = p.info();
                format!(
                    "Colour is presentation, not measurement: preset {} ({} {}, {}), invert {}, strength {:.2}",
                    i.id,
                    i.name,
                    i.wavelength,
                    i.hue,
                    if self.invert { "on" } else { "off" },
                    self.strength.clamp(0.0, 1.0)
                ) + &self.grade_note()
            }
            None => {
                format!(
                    "Mono (no colour preset), invert {}",
                    if self.invert { "on" } else { "off" }
                ) + &self.grade_note()
            }
        }
    }

    /// The grade, for the file history; empty when there is none.
    fn grade_note(&self) -> String {
        if self.grade.is_identity() {
            String::new()
        } else {
            format!(", grade {}", self.grade.to_text())
        }
    }
}

/// A sampled colour mapping: `entries[i]` is the colour of mono value
/// `i / (len - 1)`.
#[derive(Clone, Debug, PartialEq)]
pub struct ColourLut {
    /// Red, green, blue in `0..=1`.
    pub entries: Vec<[f32; 3]>,
}

impl ColourLut {
    /// Colour of `v`, interpolating linearly between entries. This is the
    /// exact arithmetic the GPU kernel repeats.
    #[must_use]
    pub fn lookup(&self, value: f32) -> [f32; 3] {
        let last = self.entries.len() - 1;
        let value = if value.is_finite() {
            value.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let pos = value * last as f32;
        let lower = (pos as usize).min(last.saturating_sub(1));
        let frac = pos - lower as f32;
        let (near, far) = (self.entries[lower], self.entries[(lower + 1).min(last)]);
        [
            near[0] + (far[0] - near[0]) * frac,
            near[1] + (far[1] - near[1]) * frac,
            near[2] + (far[2] - near[2]) * frac,
        ]
    }
}

/// A colour image: interleaved red, green, blue samples in `0..=1`.
#[derive(Clone, Debug, PartialEq)]
pub struct RgbImage {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// `3 * width * height` samples, row-major.
    pub data: Vec<f32>,
}

impl RgbImage {
    /// Sample (red, green, blue) at (`x`, `y`).
    #[must_use]
    pub fn pixel(&self, x: usize, y: usize) -> [f32; 3] {
        let o = 3 * (y * self.width + x);
        [self.data[o], self.data[o + 1], self.data[o + 2]]
    }

    /// 16-bit samples, interleaved.
    #[must_use]
    pub fn to_u16(&self) -> Vec<u16> {
        self.data
            .iter()
            .map(|&v| {
                if v.is_finite() {
                    (v.clamp(0.0, 1.0) * 65535.0).round() as u16
                } else {
                    0
                }
            })
            .collect()
    }

    /// 8-bit samples, interleaved.
    #[must_use]
    pub fn to_u8(&self) -> Vec<u8> {
        self.data
            .iter()
            .map(|&v| {
                if v.is_finite() {
                    (v.clamp(0.0, 1.0) * 255.0).round() as u8
                } else {
                    0
                }
            })
            .collect()
    }
}

/// The CPU reference: colour `frame` through `lut`.
#[must_use]
pub fn apply_lut(frame: &Frame, lut: &ColourLut) -> RgbImage {
    let mut data = vec![0.0f32; frame.data.len() * 3];
    data.par_chunks_mut(3 * 4096)
        .zip(frame.data.par_chunks(4096))
        .for_each(|(out, src)| {
            for (o, &v) in out.as_chunks_mut::<3>().0.iter_mut().zip(src) {
                o.copy_from_slice(&lut.lookup(v));
            }
        });
    RgbImage {
        width: frame.width,
        height: frame.height,
        data,
    }
}

/// Colour `frame` as `params` say (build the table, then [`apply_lut`]).
#[must_use]
pub fn colourise(frame: &Frame, params: &ColourParams) -> RgbImage {
    apply_lut(frame, &params.lut())
}

/// Where a filter guess came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detected {
    /// The guess.
    pub preset: Preset,
    /// The text that gave it away, e.g. the file name.
    pub source: String,
}

/// Split `text` into lower-case words at punctuation, at a lower-to-upper case
/// change (`SunHa` is `sun`, `ha`) and between letters and digits (`cak3934`).
fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev: Option<char> = None;
    for c in text.chars() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev = None;
            continue;
        }
        let split = prev.is_some_and(|p| {
            (p.is_lowercase() && c.is_uppercase()) || (p.is_alphabetic() != c.is_alphabetic())
        });
        if split && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        cur.extend(c.to_lowercase());
        prev = Some(c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Guess the filter named in `text` (a file name, a folder, a camera note).
#[must_use]
pub fn detect_in_text(text: &str) -> Option<Preset> {
    let w = words(text);
    // Single words, then runs of two and three joined ("ca" "ii" "k" -> "caiik").
    for n in 1..=3usize {
        for run in w.windows(n) {
            let key = run.concat();
            // Bare numbers (wavelengths) count only on their own, not as the
            // joined digits of a date.
            if n > 1 && key.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            let hit = match key.as_str() {
                "ha" | "halpha" | "hydrogenalpha" | "656" | "6563" | "h\u{3b1}" => Some(Preset::Ha),
                "cak" | "caiik" | "calciumk" | "393" | "3934" => Some(Preset::CaK),
                "cah" | "caiih" | "calciumh" | "396" | "3968" => Some(Preset::CaH),
                "mg" | "mgb" | "mgi" | "mgib" | "magnesium" | "517" | "5173" => Some(Preset::Mg),
                "na" | "nad" | "naid" | "sodium" | "589" | "5890" => Some(Preset::Na),
                "d3" | "hed3" | "heid3" | "helium" | "587" | "5876" => Some(Preset::HeD3),
                "hb" | "hbeta" | "h\u{3b2}" | "486" | "4861" => Some(Preset::Hbeta),
                "white" | "whitelight" | "wl" | "continuum" | "cont" => Some(Preset::White),
                _ => None,
            };
            if hit.is_some() {
                return hit;
            }
        }
    }
    None
}

/// Guess the filter from several texts, most trusted first (the file name,
/// then its folder, then camera or sidecar notes). Returns the first match and
/// the text it came from.
#[must_use]
pub fn detect_filter<S: AsRef<str>>(texts: &[S]) -> Option<Detected> {
    texts.iter().find_map(|t| {
        detect_in_text(t.as_ref()).map(|preset| Detected {
            preset,
            source: t.as_ref().to_string(),
        })
    })
}

/// Size of a solar granule, arcseconds.
pub const GRANULE_ARCSEC: f32 = 1.5;

/// Alignment point size for granulation, in granules.
pub const GRANULES_PER_AP: f32 = 2.5;

impl Preset {
    /// The filter shows the photosphere, where granulation is the detail that
    /// matters. The chromospheric lines (H-alpha, Ca K and H, He D3, H-beta)
    /// and Mg b2 (high photosphere, low contrast) are not tuned for it.
    #[must_use]
    pub fn is_photospheric(self) -> bool {
        matches!(self, Preset::Na | Preset::White)
    }
}

/// Alignment point size for a photospheric capture: `GRANULES_PER_AP` (2.5)
/// granules of `GRANULE_ARCSEC` (1.5 arcsec), so each point is small enough
/// to follow the local seeing distortion over the granulation but large enough
/// to correlate on it. Rounded to a multiple of 8 and kept within 32 to 96
/// pixels, because a window under 32 pixels has too little to correlate on.
///
/// With `arcsec_per_px` unknown the size follows the frame: 32 pixels for a
/// full-HD-class capture (a short side of 1080 pixels), in proportion to the
/// short side for others.
#[must_use]
pub fn granulation_ap_size(width: usize, height: usize, arcsec_per_px: Option<f32>) -> usize {
    let px = match arcsec_per_px.filter(|s| s.is_finite() && *s > 0.0) {
        Some(scale) => GRANULES_PER_AP * GRANULE_ARCSEC / scale,
        None => 32.0 * width.min(height) as f32 / 1080.0,
    };
    ((px / 8.0).round() as usize * 8).clamp(32, 96)
}

/// Alignment point size suited to the image scale: about a sixteenth of the
/// short side, rounded to a multiple of 8 and kept within 32 to 96 pixels.
#[must_use]
pub fn recommended_ap_size(width: usize, height: usize) -> usize {
    let side = width.min(height);
    ((side as f32 / 16.0 / 8.0).round() as usize * 8).clamp(32, 96)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::float_cmp,
        clippy::needless_range_loop,
        reason = "tests compare table values exactly and index channels"
    )]
    use super::*;

    fn u8s(c: [f32; 3]) -> [u8; 3] {
        c.map(|v| (v * 255.0).round() as u8)
    }

    /// Mid-grey (0.5) through each preset, worked out by hand from the
    /// parameters in the module docs and rounded to 8 bits.
    #[test]
    fn mid_grey_maps_to_the_documented_colour() {
        let expected = [
            (Preset::Ha, [169, 68, 11]),
            (Preset::CaK, [93, 61, 155]),
            (Preset::CaH, [112, 48, 150]),
            (Preset::Mg, [66, 155, 66]),
            (Preset::Na, [160, 98, 18]),
            (Preset::HeD3, [155, 134, 23]),
            (Preset::Hbeta, [36, 116, 160]),
            (Preset::White, [128, 122, 112]),
        ];
        for (p, want) in expected {
            let got = u8s(p.colour_of(0.5));
            for c in 0..3 {
                assert!(
                    (i32::from(got[c]) - want[c]).abs() <= 1,
                    "{p:?}: got {got:?}, documented {want:?}"
                );
            }
        }
    }

    #[test]
    fn black_stays_black_and_white_is_the_gain() {
        for p in Preset::ALL {
            assert_eq!(p.colour_of(0.0), [0.0; 3], "{p:?}");
            let top = p.colour_of(1.0);
            for c in 0..3 {
                assert!((top[c] - p.info().gains[c].min(1.0)).abs() < 1e-6, "{p:?}");
            }
        }
    }

    #[test]
    fn every_curve_rises_monotonically() {
        for p in Preset::ALL {
            for invert in [false, true] {
                let lut = ColourParams {
                    preset: Some(p),
                    invert,
                    strength: 1.0,
                    grade: crate::grade::Grade::default(),
                }
                .lut();
                for c in 0..3 {
                    let rising = |a: [f32; 3], b: [f32; 3]| {
                        if invert {
                            b[c] <= a[c] + 1e-7
                        } else {
                            b[c] >= a[c] - 1e-7
                        }
                    };
                    assert!(
                        lut.entries.windows(2).all(|w| rising(w[0], w[1])),
                        "{p:?} channel {c} invert {invert}"
                    );
                }
            }
        }
    }

    #[test]
    fn invert_is_the_mirror_image() {
        for p in Preset::ALL {
            let a = ColourParams::preset(p);
            let b = ColourParams { invert: true, ..a };
            for i in 0..=20 {
                let v = i as f32 / 20.0;
                assert_eq!(a.map_value(1.0 - v), b.map_value(v), "{p:?} at {v}");
            }
        }
        let mono = ColourParams {
            invert: true,
            ..ColourParams::default()
        };
        assert_eq!(mono.map_value(0.25), [0.75; 3]);
    }

    #[test]
    fn strength_blends_from_grey_to_the_preset() {
        let full = ColourParams::preset(Preset::Ha);
        let none = ColourParams {
            strength: 0.0,
            ..full
        };
        let half = ColourParams {
            strength: 0.5,
            ..full
        };
        let g = none.map_value(0.5);
        assert_eq!(g[0], g[1]);
        assert_eq!(g[1], g[2]);
        assert!(!none.is_colour());
        for c in 0..3 {
            let mid = half.map_value(0.5)[c];
            let want = f32::midpoint(g[c], full.map_value(0.5)[c]);
            assert!((mid - want).abs() < 1e-6);
        }
    }

    #[test]
    fn mono_is_the_identity() {
        let lut = ColourParams::default().lut();
        for v in [0.0, 0.1, 0.5, 0.999, 1.0] {
            let c = lut.lookup(v);
            assert!((c[0] - v).abs() < 1e-6 && c[0] == c[1] && c[1] == c[2]);
        }
    }

    #[test]
    fn the_table_follows_the_formula_closely() {
        for p in Preset::ALL {
            let params = ColourParams::preset(p);
            let lut = params.lut();
            for i in 0..=1000 {
                let v = i as f32 / 1000.0;
                let (a, b) = (lut.lookup(v), params.map_value(v));
                for c in 0..3 {
                    assert!((a[c] - b[c]).abs() < 1.5e-3, "{p:?} v {v} channel {c}");
                }
            }
        }
    }

    #[test]
    fn bad_samples_become_black() {
        let lut = ColourParams::preset(Preset::Ha).lut();
        assert_eq!(lut.lookup(f32::NAN), [0.0; 3]);
        assert_eq!(lut.lookup(-3.0), [0.0; 3]);
        assert_eq!(lut.lookup(9.0), lut.entries[LUT_SIZE - 1]);
    }

    #[test]
    fn colourising_a_frame_keeps_its_shape() {
        let f = Frame::new(3, 2, vec![0.0, 0.2, 0.4, 0.6, 0.8, 1.0]).expect("frame");
        let img = colourise(&f, &ColourParams::preset(Preset::Mg));
        assert_eq!((img.width, img.height, img.data.len()), (3, 2, 18));
        assert_eq!(img.pixel(0, 0), [0.0; 3]);
        assert_eq!(img.to_u8().len(), 18);
        assert_eq!(img.to_u16()[15], 52428); // Mg red gain 0.8 at full scale
    }

    #[test]
    fn ids_round_trip_and_unknown_ones_list_the_choices() {
        for p in Preset::ALL {
            assert_eq!(Preset::from_id(p.id()), Some(p));
            assert_eq!(p.id().parse::<Preset>().ok(), Some(p));
        }
        assert_eq!(Preset::from_id("H-Alpha"), Some(Preset::Ha));
        assert_eq!(Preset::from_id("Ca-K"), Some(Preset::CaK));
        let err = "xyz".parse::<Preset>().expect_err("unknown").to_string();
        assert!(err.contains("ha") && err.contains("he-d3"), "{err}");
    }

    #[test]
    fn file_names_give_the_filter_away() {
        let cases = [
            ("2026-10-19_Sun_Ha_001.ser", Some(Preset::Ha)),
            ("sun-halpha-pst.avi", Some(Preset::Ha)),
            ("SunHa001.ser", Some(Preset::Ha)),
            ("H-alpha 2026.ser", Some(Preset::Ha)),
            ("CaK_2026.ser", Some(Preset::CaK)),
            ("sun cak3934.ser", Some(Preset::CaK)),
            ("Ca II K line.avi", Some(Preset::CaK)),
            ("sun_CaH.ser", Some(Preset::CaH)),
            ("Mg_b2.ser", Some(Preset::Mg)),
            ("sodium_d.ser", Some(Preset::Na)),
            ("Na_589.ser", Some(Preset::Na)),
            ("sun_HeD3.ser", Some(Preset::HeD3)),
            ("He-D3.ser", Some(Preset::HeD3)),
            ("Hbeta_x.ser", Some(Preset::Hbeta)),
            ("H-beta.ser", Some(Preset::Hbeta)),
            ("sun_white_light.ser", Some(Preset::White)),
            ("WL_spots.ser", Some(Preset::White)),
            ("Hampshire_morning.ser", None),
            ("capture_0001.ser", None),
            ("Sha_nal.ser", None),
        ];
        for (name, want) in cases {
            assert_eq!(detect_in_text(name), want, "{name}");
        }
    }

    #[test]
    fn detection_prefers_the_earlier_text() {
        let d = detect_filter(&["capture_1.ser", "/data/CaK/", "Ha notes"]).expect("found");
        assert_eq!(d.preset, Preset::CaK);
        assert_eq!(d.source, "/data/CaK/");
        assert!(detect_filter::<&str>(&[]).is_none());
    }

    #[test]
    fn granulation_alignment_points_follow_the_granule_size() {
        // Unknown scale: 32 px at full HD, in proportion beyond.
        assert_eq!(granulation_ap_size(1920, 1080, None), 32);
        assert_eq!(granulation_ap_size(1936, 1096, None), 32);
        assert_eq!(granulation_ap_size(1280, 960, None), 32);
        assert_eq!(granulation_ap_size(3856, 2180, None), 64);
        // Known scale: 2.5 granules of 1.5 arcsec, not under 32 px.
        assert_eq!(granulation_ap_size(1920, 1080, Some(1.0)), 32);
        assert_eq!(granulation_ap_size(1920, 1080, Some(0.1)), 40);
        assert_eq!(granulation_ap_size(1920, 1080, Some(0.02)), 96);
        assert_eq!(granulation_ap_size(1920, 1080, Some(f32::NAN)), 32);
        assert!(Preset::Na.is_photospheric() && Preset::White.is_photospheric());
        assert!(!Preset::Ha.is_photospheric() && !Preset::CaK.is_photospheric());
    }

    #[test]
    fn alignment_point_size_follows_the_scale() {
        assert_eq!(recommended_ap_size(1280, 960), 64);
        assert_eq!(recommended_ap_size(1936, 1096), 72);
        assert_eq!(recommended_ap_size(1920, 1080), 64);
        assert_eq!(recommended_ap_size(3856, 2180), 96);
        assert_eq!(recommended_ap_size(384, 288), 32);
        assert_eq!(recommended_ap_size(6248, 4176), 96);
    }

    #[test]
    fn metadata_line_names_the_preset_and_the_caveat() {
        let d = ColourParams::preset(Preset::CaK).describe();
        assert!(d.contains("cak") && d.contains("presentation"), "{d}");
        assert!(ColourParams::default().describe().contains("Mono"));
    }
}
