// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `SharpCap` `*.CameraSettings.txt` sidecar files (INI-style `Key=Value`).
//!
//! Only used as a hint for things the AVI container cannot say, chiefly
//! whether 8-bit video is raw Bayer data and which pattern.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::frame::BayerPattern;

/// Parsed sidecar key/value pairs. Keys are matched case-insensitively.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CameraSettings {
    values: BTreeMap<String, String>,
    /// Section headings seen, normally the camera model.
    pub sections: Vec<String>,
}

impl CameraSettings {
    /// Parse sidecar text. Malformed lines are ignored.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut out = Self::default();
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
                continue;
            }
            if let Some(sec) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                out.sections.push(sec.to_string());
            } else if let Some((k, v)) = line.split_once('=') {
                out.values
                    .entry(k.trim().to_ascii_lowercase())
                    .or_insert_with(|| v.trim().to_string());
            }
        }
        out
    }

    /// Path `SharpCap` uses for the sidecar of `capture` (`x.avi` -> `x.CameraSettings.txt`).
    #[must_use]
    pub fn sidecar_path(capture: &Path) -> PathBuf {
        capture.with_extension("CameraSettings.txt")
    }

    /// Load the sidecar next to `capture`, if there is a readable one.
    #[must_use]
    pub fn for_capture(capture: &Path) -> Option<Self> {
        // Bound the read so a hostile neighbour file cannot exhaust memory.
        let meta = std::fs::metadata(Self::sidecar_path(capture)).ok()?;
        if meta.len() > 1 << 20 {
            return None;
        }
        std::fs::read_to_string(Self::sidecar_path(capture))
            .ok()
            .map(|t| Self::parse(&t))
    }

    /// Value for `key` (any case).
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values
            .get(&key.to_ascii_lowercase())
            .map(String::as_str)
    }

    /// True when `Colour Space` names a raw format (`RAW8`, `RAW16`), i.e. a
    /// colour camera delivering undemosaiced data. `None` when unknown.
    #[must_use]
    pub fn is_raw_colour(&self) -> Option<bool> {
        self.get("Colour Space")
            .map(|v| v.to_ascii_uppercase().starts_with("RAW"))
    }

    /// The pattern `Debayer Preview=Force XXXX` names, if it does.
    #[must_use]
    pub fn forced_pattern(&self) -> Option<BayerPattern> {
        let up = self.get("Debayer Preview")?.to_ascii_uppercase();
        [
            ("RGGB", BayerPattern::Rggb),
            ("GRBG", BayerPattern::Grbg),
            ("GBRG", BayerPattern::Gbrg),
            ("BGGR", BayerPattern::Bggr),
        ]
        .into_iter()
        .find_map(|(name, p)| up.contains(name).then_some(p))
    }

    /// Bayer pattern hint: `Debayer Preview=Force RGGB` wins, otherwise a
    /// raw colour space implies `RGGB`, the layout of the ZWO colour sensors.
    #[must_use]
    pub fn bayer_hint(&self) -> Option<BayerPattern> {
        self.forced_pattern()
            .or_else(|| (self.is_raw_colour() == Some(true)).then_some(BayerPattern::Rggb))
    }

    /// Frames `SharpCap` reports having written.
    #[must_use]
    pub fn frame_count(&self) -> Option<usize> {
        self.get("FrameCount").and_then(|v| v.parse().ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sharpcap_text() {
        let s = CameraSettings::parse(
            "[ZWO ASI482MC]\nColour Space=RAW8\nDebayer Preview=On\nFrameCount=630\n; c\nbad line\nGain=17\n",
        );
        assert_eq!(s.sections, vec!["ZWO ASI482MC"]);
        assert_eq!(s.get("gain"), Some("17"));
        assert_eq!(s.is_raw_colour(), Some(true));
        assert_eq!(s.bayer_hint(), Some(BayerPattern::Rggb));
        assert_eq!(s.frame_count(), Some(630));
    }

    #[test]
    fn forced_pattern_and_mono() {
        let s = CameraSettings::parse("Colour Space=RAW8\nDebayer Preview=Force GBRG\n");
        assert_eq!(s.bayer_hint(), Some(BayerPattern::Gbrg));
        let m = CameraSettings::parse("Colour Space=MON8\n");
        assert_eq!(m.is_raw_colour(), Some(false));
        assert_eq!(m.bayer_hint(), None);
        assert_eq!(CameraSettings::parse("").bayer_hint(), None);
    }

    #[test]
    fn sidecar_path_swaps_extension() {
        assert_eq!(
            CameraSettings::sidecar_path(Path::new("a/15_32_09.avi")),
            PathBuf::from("a/15_32_09.CameraSettings.txt")
        );
    }
}
