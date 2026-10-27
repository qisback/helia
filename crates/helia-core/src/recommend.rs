// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Recommended settings for a capture: the "just press Stack" defaults.
//!
//! The filter is read from the clues a capture carries (file name, folder,
//! `SharpCap` sidecar, SER header), or given by the user as a preset. The
//! preset then picks the Bayer channel and the share of frames to keep, and
//! the image scale picks the alignment point size. Nothing here is hidden
//! magic: every value is also an ordinary option that overrides it.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use crate::colour::{Detected, Preset, detect_filter, granulation_ap_size, recommended_ap_size};
use crate::debayer::Channel;
use crate::edit::{EditList, SharpenMethod, sharpen_edits};
use crate::io::ser::SerReader;
use crate::io::sidecar::CameraSettings;
use crate::quality::Keep;

/// Sidecar keys that may name the filter.
const SIDECAR_KEYS: [&str; 6] = [
    "filter",
    "filter name",
    "filter wheel",
    "target",
    "description",
    "notes",
];

/// Texts of a capture that may name its filter, most trusted first: the file
/// name, the folder it is in, notes in the `SharpCap` sidecar, the SER header
/// strings, and the folder above.
#[must_use]
pub fn capture_texts(path: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let name = |p: &Path| p.file_stem().map(|s| s.to_string_lossy().into_owned());
    out.extend(name(path));
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    out.extend(
        parent
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned()),
    );
    if let Some(s) = CameraSettings::for_capture(path) {
        out.extend(
            SIDECAR_KEYS
                .iter()
                .filter_map(|k| s.get(k).map(str::to_owned)),
        );
    }
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("ser"))
        && let Ok(f) = File::open(path)
        && let Ok(r) = SerReader::new(BufReader::new(f))
    {
        let h = r.header();
        out.extend(
            [&h.instrument, &h.telescope, &h.observer]
                .into_iter()
                .filter(|s| !s.trim().is_empty())
                .cloned(),
        );
    }
    out.extend(
        parent
            .and_then(Path::parent)
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned()),
    );
    out
}

/// What Helia would do with a capture, and why.
#[derive(Clone, Debug, PartialEq)]
pub struct Recommendation {
    /// The look and settings in force, if a filter is known.
    pub preset: Option<Preset>,
    /// How the preset was chosen: `None` when none was, "chosen" when the user
    /// named it, or the text it was detected in.
    pub detected_from: Option<String>,
    /// The frames are raw colour, so the channel matters.
    pub is_bayer: bool,
    /// Bayer channel to stack.
    pub channel: Channel,
    /// Frames to keep.
    pub keep: Keep,
    /// Alignment point size in pixels.
    pub ap_size: usize,
    /// How the stack is sharpened by default.
    pub sharpen: SharpenMethod,
    /// The edit list that method stands for, with this filter's values.
    pub sharpen_edits: EditList,
}

impl Recommendation {
    /// One line saying what was chosen, for a log or a status line.
    #[must_use]
    pub fn summary(&self) -> String {
        let filter = match (&self.preset, &self.detected_from) {
            (Some(p), Some(src)) if src == "chosen" => format!("{} (chosen)", p.name()),
            (Some(p), Some(src)) => format!("{} (from \"{src}\")", p.name()),
            (Some(p), None) => p.name().to_string(),
            (None, _) => "filter not recognised".to_string(),
        };
        let channel = match self.channel {
            Channel::Red => "red",
            Channel::Green => "green",
            Channel::Blue => "blue",
            Channel::Luminance => "luminance",
        };
        let keep = match self.keep {
            Keep::Fraction(f) => format!("{:.0}%", f * 100.0),
            Keep::Count(n) => format!("{n} frames"),
        };
        // A mono camera has no channel to speak of.
        let channel = if self.is_bayer {
            format!("{channel} channel, ")
        } else {
            String::new()
        };
        format!(
            "{filter}; {channel}keep {keep}, alignment points {} px",
            self.ap_size
        )
    }
}

impl Recommendation {
    /// One line on the default sharpening, for a log or a status line.
    #[must_use]
    pub fn sharpen_summary(&self) -> String {
        match self.preset {
            Some(p) => format!(
                "{} sharpening with the {} values: {}",
                self.sharpen.name().to_lowercase(),
                p.name(),
                p.info().sharpen_reason
            ),
            None => format!(
                "{} sharpening with the generic values",
                self.sharpen.name().to_lowercase()
            ),
        }
    }
}

/// The recommended settings for a capture of `width` x `height` pixels.
/// `explicit` is a preset the user chose; otherwise one is detected from
/// `texts` (see [`capture_texts`]). `is_bayer` says the frames are raw colour,
/// the only case where a channel choice matters.
#[must_use]
pub fn recommend(
    width: usize,
    height: usize,
    is_bayer: bool,
    texts: &[String],
    explicit: Option<Preset>,
) -> Recommendation {
    recommend_with_scale(width, height, None, is_bayer, texts, explicit)
}

/// As [`recommend`], with the image scale in arcseconds per pixel when it is
/// known (for instance from a limb fit). Photospheric filters (white light,
/// Na D) size their alignment points from it in granules; see
/// [`granulation_ap_size`]. Every other filter ignores it.
#[must_use]
pub fn recommend_with_scale(
    width: usize,
    height: usize,
    arcsec_per_px: Option<f32>,
    is_bayer: bool,
    texts: &[String],
    explicit: Option<Preset>,
) -> Recommendation {
    let (preset, detected_from) = match explicit {
        Some(p) => (Some(p), Some("chosen".to_string())),
        None => match detect_filter(texts) {
            Some(Detected { preset, source }) => (Some(preset), Some(source)),
            None => (None, None),
        },
    };
    Recommendation {
        preset,
        detected_from,
        is_bayer,
        channel: match preset {
            Some(p) if is_bayer => p.info().channel,
            _ => Channel::Luminance,
        },
        keep: preset.map_or(Keep::default(), |p| {
            Keep::Fraction(p.info().keep_percent / 100.0)
        }),
        ap_size: match preset {
            Some(p) if p.is_photospheric() => granulation_ap_size(width, height, arcsec_per_px),
            _ => recommended_ap_size(width, height),
        },
        sharpen: SharpenMethod::Wavelet,
        sharpen_edits: sharpen_edits(SharpenMethod::Wavelet, preset, width, height),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_detected_filter_picks_channel_and_keep() {
        let texts = vec!["2026-10-19_Ha_001".to_string()];
        let r = recommend(1936, 1096, true, &texts, None);
        assert_eq!(r.preset, Some(Preset::Ha));
        assert_eq!(r.channel, Channel::Red);
        assert_eq!(r.keep, Keep::Fraction(0.25));
        assert_eq!(r.ap_size, 72);
        assert!(r.summary().contains("H-alpha (from \"2026-10-19_Ha_001\")"));
        assert!(
            r.summary().contains("red channel, keep 25%"),
            "{}",
            r.summary()
        );
    }

    #[test]
    fn photospheric_filters_keep_few_frames_and_small_points() {
        for (text, preset) in [("sun_Na", Preset::Na), ("sun_white_light", Preset::White)] {
            let r = recommend(1936, 1096, true, &[text.to_string()], None);
            assert_eq!(r.preset, Some(preset));
            assert_eq!(r.keep, Keep::Fraction(0.12));
            assert_eq!(r.ap_size, 32);
            // Na D reaches the sensor in its green (and red) pixels, not the blue.
            let channel = if preset == Preset::Na {
                Channel::Green
            } else {
                Channel::Luminance
            };
            assert_eq!(r.channel, channel);
            // The sharpening leaves layers 2 and 3 unthresholded.
            let t = preset.sharpen_tuning();
            assert!(t.thresholds[1] == 0.0 && t.thresholds[2] == 0.0);
            assert!(t.gains[1] > t.gains[0] && t.gains[2] >= 1.5);
        }
        // A known scale sizes the points from the granules.
        let r = recommend_with_scale(1936, 1096, Some(0.1), true, &["Na".to_string()], None);
        assert_eq!(r.ap_size, 40);
    }

    #[test]
    fn chromospheric_filters_keep_the_general_values() {
        for text in ["Ha", "CaK", "CaH", "HeD3", "Hbeta"] {
            let r = recommend_with_scale(1936, 1096, Some(0.1), true, &[text.to_string()], None);
            assert_eq!(r.ap_size, 72, "{text}");
            assert!(matches!(r.keep, Keep::Fraction(f) if f >= 0.25), "{text}");
        }
    }

    #[test]
    fn mono_cameras_ignore_the_channel() {
        let r = recommend(1280, 960, false, &["CaK".to_string()], None);
        assert_eq!(r.preset, Some(Preset::CaK));
        assert_eq!(r.channel, Channel::Luminance);
        assert_eq!(r.keep, Keep::Fraction(0.30));
    }

    #[test]
    fn a_chosen_preset_beats_the_file_name() {
        let r = recommend(1280, 960, true, &["sun_Ha".to_string()], Some(Preset::Mg));
        assert_eq!(r.preset, Some(Preset::Mg));
        assert_eq!(r.channel, Channel::Green);
        assert!(r.summary().contains("(chosen)"));
    }

    #[test]
    fn an_unknown_filter_falls_back_to_plain_defaults() {
        let r = recommend(1280, 960, true, &["capture_0001".to_string()], None);
        assert_eq!(r.preset, None);
        assert_eq!(r.channel, Channel::Luminance);
        assert_eq!(r.keep, Keep::default());
        assert!(r.summary().starts_with("filter not recognised"));
    }

    #[test]
    fn texts_come_from_the_name_the_folder_and_the_sidecar() {
        let dir = std::env::temp_dir().join(format!("helia-rec-{}", std::process::id()));
        let cap_dir = dir.join("Na_session");
        std::fs::create_dir_all(&cap_dir).unwrap_or_else(|e| panic!("{e}"));
        let cap = cap_dir.join("15_32_09.avi");
        std::fs::write(
            CameraSettings::sidecar_path(&cap),
            "[Cam]\nFilter=CaK 0.3nm\nGain=1\n",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let t = capture_texts(&cap);
        assert_eq!(t[0], "15_32_09");
        assert_eq!(t[1], "Na_session");
        assert!(t.contains(&"CaK 0.3nm".to_string()));
        // The folder outranks the sidecar note.
        assert_eq!(detect_filter(&t).map(|d| d.preset), Some(Preset::Na));
    }
}
