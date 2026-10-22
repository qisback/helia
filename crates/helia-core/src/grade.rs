// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Colour grading: the user's adjustments on top of a band's look.
//!
//! The look of a band (see [`crate::colour`]) is a starting point. A [`Grade`]
//! is applied in the same table, so the CPU and the GPU agree and a saved
//! `.cube` or `.lut` carries it:
//!
//! ```text
//! v   = clamp((v - black) / (white - black), 0, 1) ^ (1 / gamma)    (before the look)
//! rgb = look(v)
//! rgb = rgb ^ (1 / curve)            per channel
//! rgb = hue-rotate(rgb, hue); saturate(rgb, saturation)
//! ```
//!
//! Hue rotates the colour about the grey axis (Rodrigues rotation, in degrees);
//! saturation scales the distance from the luma of the colour (Rec. 709
//! weights), so 0 is grey and 1 leaves it. The default grade does nothing, and
//! `rgb` is returned bit for bit.
//!
//! Text form for the command line and the API, `key=value` separated by commas:
//! `hue=10,sat=1.2,gamma=1.1,black=0.02,white=0.98,r=1.0,g=1.0,b=1.1`.

#![allow(
    clippy::float_cmp,
    reason = "exactly the neutral setting, not nearly it"
)]

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Luma weights (Rec. 709).
const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// The user's adjustments to a look. The default changes nothing.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Grade {
    /// Hue rotation in degrees, -180 to 180.
    pub hue: f32,
    /// Saturation, 0 (grey) to 2; 1 leaves it.
    pub saturation: f32,
    /// Brightness gamma of the mono value, 0.3 to 3: above 1 brightens the
    /// mid-tones.
    pub gamma: f32,
    /// Mono value that becomes black, 0 to 0.9.
    pub black: f32,
    /// Mono value that becomes full, 0.1 to 1.
    pub white: f32,
    /// Per-channel curve (red, green, blue), 0.3 to 3: above 1 lifts that
    /// channel's mid-tones.
    pub curves: [f32; 3],
}

impl Default for Grade {
    fn default() -> Self {
        Self {
            hue: 0.0,
            saturation: 1.0,
            gamma: 1.0,
            black: 0.0,
            white: 1.0,
            curves: [1.0; 3],
        }
    }
}

impl Grade {
    /// True when the grade changes nothing.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        *self == Self::default()
    }

    /// Check the ranges.
    ///
    /// # Errors
    /// [`Error::Invalid`] naming the ranges.
    pub fn validate(&self) -> Result<()> {
        let ok = |v: f32, lo: f32, hi: f32| v.is_finite() && (lo..=hi).contains(&v);
        if ok(self.hue, -180.0, 180.0)
            && ok(self.saturation, 0.0, 2.0)
            && ok(self.gamma, 0.3, 3.0)
            && ok(self.black, 0.0, 0.9)
            && ok(self.white, 0.1, 1.0)
            && self.white - self.black >= 0.05
            && self.curves.iter().all(|&c| ok(c, 0.3, 3.0))
        {
            Ok(())
        } else {
            Err(Error::Invalid(
                "grade: hue -180 to 180, sat 0 to 2, gamma 0.3 to 3, black 0 to 0.9, white 0.1 to 1 (at least 0.05 above black), r, g, b 0.3 to 3".into(),
            ))
        }
    }

    /// The mono value after black, white and gamma, before the look.
    #[must_use]
    pub fn tone(&self, v: f32) -> f32 {
        if self.black == 0.0 && self.white == 1.0 && self.gamma == 1.0 {
            return v;
        }
        ((v - self.black) / (self.white - self.black))
            .clamp(0.0, 1.0)
            .powf(1.0 / self.gamma)
    }

    /// The colour after the look: channel curves, hue and saturation.
    #[must_use]
    pub fn colour(&self, mut rgb: [f32; 3]) -> [f32; 3] {
        for (c, k) in rgb.iter_mut().zip(self.curves) {
            if k != 1.0 {
                *c = c.clamp(0.0, 1.0).powf(1.0 / k);
            }
        }
        if self.hue != 0.0 {
            let (s, c) = self.hue.to_radians().sin_cos();
            // Rotation about the grey axis (1,1,1)/sqrt 3.
            let k = (1.0 - c) / 3.0;
            let r3 = 3.0f32.sqrt() / 3.0 * s;
            let m = [
                [c + k, k - r3, k + r3],
                [k + r3, c + k, k - r3],
                [k - r3, k + r3, c + k],
            ];
            let src = rgb;
            for (o, row) in rgb.iter_mut().zip(m) {
                *o = row[0] * src[0] + row[1] * src[1] + row[2] * src[2];
            }
        }
        if self.saturation != 1.0 {
            let luma = LUMA[0] * rgb[0] + LUMA[1] * rgb[1] + LUMA[2] * rgb[2];
            for c in &mut rgb {
                *c = luma + (*c - luma) * self.saturation;
            }
        }
        if !self.is_identity() {
            for c in &mut rgb {
                *c = c.clamp(0.0, 1.0);
            }
        }
        rgb
    }

    /// Parse the text form (see the module documentation). Keys not given keep
    /// their default.
    ///
    /// # Errors
    /// [`Error::Invalid`] for an unknown key, a bad number or a value out of
    /// range.
    pub fn parse(text: &str) -> Result<Self> {
        let mut g = Self::default();
        for item in text.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let (k, v) = item
                .split_once('=')
                .ok_or_else(|| Error::Invalid(format!("grade: {item:?} is not key=value")))?;
            let n: f32 = v
                .trim()
                .parse()
                .map_err(|_| Error::Invalid(format!("grade: {v:?} is not a number")))?;
            match k.trim().to_ascii_lowercase().as_str() {
                "hue" => g.hue = n,
                "sat" | "saturation" => g.saturation = n,
                "gamma" => g.gamma = n,
                "black" => g.black = n,
                "white" => g.white = n,
                "r" | "red" => g.curves[0] = n,
                "g" | "green" => g.curves[1] = n,
                "b" | "blue" => g.curves[2] = n,
                other => {
                    return Err(Error::Invalid(format!(
                        "grade: unknown key {other:?} (hue, sat, gamma, black, white, r, g, b)"
                    )));
                }
            }
        }
        g.validate()?;
        Ok(g)
    }

    /// The text form of the values that differ from the default.
    #[must_use]
    pub fn to_text(&self) -> String {
        let d = Self::default();
        let mut v = Vec::new();
        let mut add = |k: &str, a: f32, b: f32| {
            if a != b {
                v.push(format!("{k}={a}"));
            }
        };
        add("hue", self.hue, d.hue);
        add("sat", self.saturation, d.saturation);
        add("gamma", self.gamma, d.gamma);
        add("black", self.black, d.black);
        add("white", self.white, d.white);
        add("r", self.curves[0], 1.0);
        add("g", self.curves[1], 1.0);
        add("b", self.curves[2], 1.0);
        v.join(",")
    }
}

/// A named, saved grade (see [`crate::edit::store`] for the folder logic).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GradePreset {
    /// Name shown in lists.
    pub name: String,
    /// Filter id the grade was made for, if any.
    pub filter: Option<String>,
    /// The grade.
    pub grade: Grade,
}

/// The folder grade presets are kept in: `$HELIA_COLOUR_PRESETS`, else
/// `colour-presets` beside the edit presets.
#[must_use]
pub fn presets_dir() -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("HELIA_COLOUR_PRESETS").filter(|v| !v.is_empty()) {
        return Some(p.into());
    }
    crate::edit::store::presets_dir().map(|d| d.with_file_name("colour-presets"))
}

/// Write `preset` into `dir`.
///
/// # Errors
/// An unusable name, an invalid grade or an I/O failure.
pub fn save(dir: &std::path::Path, preset: &GradePreset) -> Result<std::path::PathBuf> {
    preset.grade.validate()?;
    let stem = crate::edit::store::file_stem(&preset.name)?;
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{stem}.json"));
    let text = serde_json::to_string_pretty(preset)
        .map_err(|e| Error::Invalid(format!("colour preset: {e}")))?;
    std::fs::write(&path, text + "\n")?;
    Ok(path)
}

/// Every readable grade preset in `dir`, by name.
#[must_use]
pub fn list(dir: &std::path::Path) -> Vec<GradePreset> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<GradePreset> = rd
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("json"))
        })
        .filter_map(|p| {
            let t = std::fs::read_to_string(&p).ok()?;
            let mut g: GradePreset = serde_json::from_str(&t).ok()?;
            g.grade.validate().ok()?;
            if g.name.is_empty() {
                g.name = p.file_stem()?.to_string_lossy().into_owned();
            }
            Some(g)
        })
        .collect();
    out.sort_by_key(|a| a.name.to_lowercase());
    out
}

/// Delete the preset called `name` from `dir` (a missing file is fine).
///
/// # Errors
/// An unusable name or an I/O failure.
pub fn remove(dir: &std::path::Path, name: &str) -> Result<()> {
    let path = dir.join(format!("{}.json", crate::edit::store::file_stem(name)?));
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Find a preset by file path or by name.
///
/// # Errors
/// No such preset.
pub fn find(reference: &str, dir: Option<&std::path::Path>) -> Result<GradePreset> {
    let direct = std::path::Path::new(reference);
    if direct.is_file() {
        let t = std::fs::read_to_string(direct)?;
        let g: GradePreset = serde_json::from_str(&t)
            .map_err(|e| Error::Invalid(format!("{}: {e}", direct.display())))?;
        g.grade.validate()?;
        return Ok(g);
    }
    dir.and_then(|d| {
        list(d)
            .into_iter()
            .find(|p| p.name.eq_ignore_ascii_case(reference))
    })
    .ok_or_else(|| {
        Error::Invalid(format!(
            "no colour preset {reference:?}: give a .json file or the name of a saved preset"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_grade_changes_nothing() {
        let g = Grade::default();
        assert!(g.is_identity());
        let c = [0.3, 0.6, 0.1];
        assert_eq!(g.colour(c), c);
        assert_eq!(g.tone(0.37), 0.37);
    }

    #[test]
    fn saturation_zero_is_grey_and_hue_keeps_luma_grey() {
        let g = Grade {
            saturation: 0.0,
            ..Grade::default()
        };
        let c = g.colour([0.9, 0.3, 0.1]);
        assert!((c[0] - c[1]).abs() < 1e-6 && (c[1] - c[2]).abs() < 1e-6);
        // A grey stays grey under any hue rotation.
        let h = Grade {
            hue: 70.0,
            ..Grade::default()
        };
        let c = h.colour([0.5, 0.5, 0.5]);
        assert!(c.iter().all(|v| (v - 0.5).abs() < 1e-5));
        // Rotating by 120 degrees sends red to green.
        let r = Grade {
            hue: 120.0,
            ..Grade::default()
        }
        .colour([1.0, 0.0, 0.0]);
        assert!(r[1] > 0.99 && r[0] < 0.01 && r[2] < 0.01, "{r:?}");
    }

    #[test]
    fn tone_and_curves_move_the_midtones() {
        let g = Grade {
            gamma: 2.0,
            black: 0.1,
            white: 0.9,
            curves: [1.0, 1.0, 2.0],
            ..Grade::default()
        };
        assert_eq!(g.tone(0.05), 0.0);
        assert_eq!(g.tone(0.95), 1.0);
        assert!(g.tone(0.5) > 0.5);
        let c = g.colour([0.25, 0.25, 0.25]);
        assert!(c[2] > c[0]);
    }

    #[test]
    fn text_parses_checks_and_round_trips() {
        let g =
            Grade::parse("hue=10,sat=1.2,gamma=1.1,black=0.02,white=0.98,b=1.1").expect("parse");
        assert_eq!(Grade::parse(&g.to_text()).expect("again"), g);
        assert!(Grade::parse("hue=200").is_err());
        assert!(Grade::parse("wobble=1").is_err());
        assert!(Grade::parse("black=0.8,white=0.82").is_err());
        assert!(Grade::parse("").expect("empty").is_identity());
    }

    #[test]
    fn presets_save_list_find_and_remove() {
        let dir = std::env::temp_dir().join(format!("helia-grades-{}", std::process::id()));
        let p = GradePreset {
            name: "Warm Ha".into(),
            filter: Some("ha".into()),
            grade: Grade::parse("hue=-8,sat=1.1").expect("grade"),
        };
        save(&dir, &p).expect("save");
        assert_eq!(list(&dir), vec![p.clone()]);
        assert_eq!(find("warm ha", Some(&dir)).expect("find"), p);
        remove(&dir, "Warm Ha").expect("remove");
        assert_eq!(list(&dir), Vec::new());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
