// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Colour tables as files other programs read.
//!
//! The mapping from a mono value to a colour is a [`ColourLut`]
//! ([`crate::colour`]). Exported, it can colour the same stack in another
//! program, so a picture made elsewhere matches one made here:
//!
//! | Format | File | Read by |
//! |---|---|---|
//! | [`LutFormat::Cube`] | `.cube`, a 1D LUT of `size` rows of red, green, blue | `DaVinci` Resolve, Adobe and others |
//! | [`LutFormat::Csv`] | `.csv`, `value,red,green,blue` | spreadsheets, scripts |
//! | [`LutFormat::ImageJ`] | `.lut`, 768 raw bytes: 256 red, 256 green, 256 blue | `ImageJ` and Fiji |
//! | [`LutFormat::Png`] | `.png`, a strip `size` wide | anything that loads an image |
//!
//! The table is resampled from the 4096-entry original by the same linear
//! interpolation the colouring uses, so an exported table with `size` 4096
//! reproduces it exactly.

use std::str::FromStr;

use crate::colour::{ColourLut, RgbImage};
use crate::error::{Error, Result};

/// Entries of an `ImageJ` table.
const IMAGEJ_SIZE: usize = 256;

/// Fewest and most entries of an exported table.
pub const SIZE_RANGE: std::ops::RangeInclusive<usize> = 2..=4096;

/// Height of a PNG strip, pixels.
const STRIP_HEIGHT: usize = 32;

/// A file format for a colour table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LutFormat {
    /// A 1D `.cube` file.
    Cube,
    /// Comma-separated values.
    Csv,
    /// An `ImageJ` `.lut` file (raw, 256 entries).
    ImageJ,
    /// A PNG strip.
    Png,
}

impl LutFormat {
    /// Every format.
    pub const ALL: [LutFormat; 4] = [Self::Cube, Self::Csv, Self::ImageJ, Self::Png];

    /// Name used on the command line and in the API.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Cube => "cube",
            Self::Csv => "csv",
            Self::ImageJ => "imagej",
            Self::Png => "png",
        }
    }

    /// File extension.
    #[must_use]
    pub fn ext(self) -> &'static str {
        match self {
            Self::Cube => "cube",
            Self::Csv => "csv",
            Self::ImageJ => "lut",
            Self::Png => "png",
        }
    }

    /// Media type, for the API.
    #[must_use]
    pub fn media_type(self) -> &'static str {
        match self {
            Self::Cube | Self::Csv => "text/plain; charset=utf-8",
            Self::ImageJ => "application/octet-stream",
            Self::Png => "image/png",
        }
    }

    /// The format a file name asks for, by its extension.
    #[must_use]
    pub fn from_path(path: &std::path::Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Self::ALL.into_iter().find(|f| f.ext() == ext)
    }
}

impl FromStr for LutFormat {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let key = s.trim().to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|f| f.id() == key || f.ext() == key)
            .ok_or_else(|| {
                Error::Invalid(format!(
                    "unknown LUT format {s:?}; use cube, csv, imagej or png"
                ))
            })
    }
}

/// The table sampled at `size` evenly spaced values from 0 to 1.
#[must_use]
pub fn resample(lut: &ColourLut, size: usize) -> Vec<[f32; 3]> {
    let last = (size.max(2) - 1) as f32;
    (0..size.max(2))
        .map(|i| lut.lookup(i as f32 / last))
        .collect()
}

/// Write `lut` in `format` with `size` entries (an `ImageJ` table always has
/// 256). `title` names it in formats that have a title.
///
/// # Errors
/// [`Error::Invalid`] when `size` is outside [`SIZE_RANGE`].
pub fn export(lut: &ColourLut, title: &str, format: LutFormat, size: usize) -> Result<Vec<u8>> {
    use std::fmt::Write as _;
    if !SIZE_RANGE.contains(&size) {
        return Err(Error::Invalid(format!(
            "a LUT has {} to {} entries",
            SIZE_RANGE.start(),
            SIZE_RANGE.end()
        )));
    }
    Ok(match format {
        LutFormat::Cube => {
            let rows = resample(lut, size);
            let title: String = title
                .chars()
                .filter(|c| c.is_ascii_graphic() || *c == ' ')
                .filter(|c| *c != '"')
                .collect();
            let mut s = format!(
                "# Written by Helia. Colour is presentation, not measurement.\nTITLE \"{title}\"\nDOMAIN_MIN 0.0 0.0 0.0\nDOMAIN_MAX 1.0 1.0 1.0\nLUT_1D_SIZE {size}\n"
            );
            for [r, g, b] in rows {
                let _ = writeln!(s, "{r:.6} {g:.6} {b:.6}");
            }
            s.into_bytes()
        }
        LutFormat::Csv => {
            let rows = resample(lut, size);
            let last = (rows.len() - 1) as f32;
            let mut s = String::from("value,red,green,blue\n");
            for (i, [r, g, b]) in rows.into_iter().enumerate() {
                let _ = writeln!(s, "{:.6},{r:.6},{g:.6},{b:.6}", i as f32 / last);
            }
            s.into_bytes()
        }
        LutFormat::ImageJ => {
            let rows = resample(lut, IMAGEJ_SIZE);
            let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            let mut out = Vec::with_capacity(3 * IMAGEJ_SIZE);
            for c in 0..3 {
                out.extend(rows.iter().map(|p| byte(p[c])));
            }
            out
        }
        LutFormat::Png => {
            let rows = resample(lut, size);
            let mut data = Vec::with_capacity(3 * size * STRIP_HEIGHT);
            for _ in 0..STRIP_HEIGHT {
                data.extend(rows.iter().flatten().copied());
            }
            let img = RgbImage {
                width: size,
                height: STRIP_HEIGHT,
                data,
            };
            let mut out = Vec::new();
            crate::io::png::write_png8_rgb(&mut out, &img, Some(title))?;
            out
        }
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use super::*;
    use crate::colour::{ColourParams, Preset};

    fn ha() -> ColourLut {
        ColourParams::preset(Preset::Ha).lut()
    }

    #[test]
    fn a_cube_file_has_the_header_and_one_row_per_entry() {
        let text = String::from_utf8(export(&ha(), "Helia H-alpha", LutFormat::Cube, 256).unwrap())
            .unwrap();
        assert!(text.contains("TITLE \"Helia H-alpha\""));
        assert!(text.contains("LUT_1D_SIZE 256"));
        let rows: Vec<&str> = text
            .lines()
            .filter(|l| !l.starts_with('#') && l.split_whitespace().count() == 3)
            .filter(|l| l.split_whitespace().all(|t| t.parse::<f32>().is_ok()))
            .collect();
        assert_eq!(rows.len(), 256);
        let first: Vec<f32> = rows[0]
            .split_whitespace()
            .map(|t| t.parse().unwrap())
            .collect();
        assert_eq!(first, vec![0.0, 0.0, 0.0]);
        let last: Vec<f32> = rows[rows.len() - 1]
            .split_whitespace()
            .map(|t| t.parse().unwrap())
            .collect();
        let want = ha().lookup(1.0);
        for c in 0..3 {
            assert!((last[c] - want[c]).abs() < 1e-5);
        }
    }

    #[test]
    fn the_full_size_table_reproduces_the_colouring() {
        let lut = ha();
        let rows = resample(&lut, crate::colour::LUT_SIZE);
        for (a, b) in rows.iter().zip(&lut.entries) {
            for c in 0..3 {
                assert!((a[c] - b[c]).abs() < 1e-5);
            }
        }
        // Values in between match the colouring's own interpolation.
        let small = resample(&lut, 64);
        for (i, p) in small.iter().enumerate() {
            let want = lut.lookup(i as f32 / 63.0);
            assert_eq!(*p, want);
        }
    }

    #[test]
    fn csv_rows_carry_the_value_and_the_colour() {
        let text = String::from_utf8(export(&ha(), "x", LutFormat::Csv, 5).unwrap()).unwrap();
        let rows: Vec<&str> = text.lines().collect();
        assert_eq!(rows[0], "value,red,green,blue");
        assert_eq!(rows.len(), 6);
        assert!(rows[1].starts_with("0.000000,0.000000"));
        assert!(rows[5].starts_with("1.000000,"));
    }

    #[test]
    fn an_imagej_table_is_768_bytes_planar() {
        let bytes = export(&ha(), "x", LutFormat::ImageJ, 256).unwrap();
        assert_eq!(bytes.len(), 768);
        let lut = ha();
        let top = lut.lookup(1.0);
        assert_eq!(bytes[255], (top[0] * 255.0).round() as u8);
        assert_eq!(bytes[256 + 255], (top[1] * 255.0).round() as u8);
        assert_eq!(bytes[512 + 255], (top[2] * 255.0).round() as u8);
        // H-alpha is red-orange: red leads.
        assert!(bytes[200] > bytes[256 + 200] && bytes[256 + 200] > bytes[512 + 200]);
    }

    #[test]
    fn a_png_strip_decodes_to_the_table() {
        let bytes = export(&ha(), "Helia H-alpha", LutFormat::Png, 128).unwrap();
        assert_eq!(&bytes[..4], &[0x89, b'P', b'N', b'G']);
        let (loaded, _) = crate::io::any::read_png(std::io::Cursor::new(&bytes)).unwrap();
        let crate::io::any::Loaded::Rgb(img) = loaded else {
            panic!("an RGB strip")
        };
        assert_eq!((img.width, img.height), (128, STRIP_HEIGHT));
        let want = ha().lookup(100.0 / 127.0);
        let got = img.pixel(100, 7);
        for c in 0..3 {
            assert!(
                (got[c] - want[c]).abs() < 1.0 / 255.0 + 1e-4,
                "{got:?} {want:?}"
            );
        }
    }

    #[test]
    fn formats_come_from_names_and_extensions_and_sizes_are_checked() {
        assert_eq!("CUBE".parse::<LutFormat>().unwrap(), LutFormat::Cube);
        assert_eq!("lut".parse::<LutFormat>().unwrap(), LutFormat::ImageJ);
        assert!("xlsx".parse::<LutFormat>().is_err());
        assert_eq!(
            LutFormat::from_path(std::path::Path::new("a/B.Cube")),
            Some(LutFormat::Cube)
        );
        assert!(export(&ha(), "x", LutFormat::Csv, 1).is_err());
        assert!(export(&ha(), "x", LutFormat::Csv, 5000).is_err());
    }
}
