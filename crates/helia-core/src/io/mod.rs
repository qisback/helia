// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Image and capture file formats.

pub mod any;
pub mod avi;
pub mod capture;
pub mod detect;
pub mod ffmpeg;
pub mod fits;
pub mod fitsseq;
pub mod png;
pub mod raw;
pub mod ser;
pub mod sidecar;
pub mod solarnet;
pub mod source;
pub mod tiff;

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;

use crate::colour::RgbImage;
use crate::error::{Error, Result};
use crate::frame::Frame;

/// Output container chosen from a file extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFormat {
    /// 16-bit greyscale TIFF.
    Tiff,
    /// FITS, 32-bit float.
    Fits,
    /// 8-bit PNG, for sharing.
    Png,
}

impl ImageFormat {
    /// Infer from `.tif`, `.tiff`, `.fit`, `.fits` or `.fts` (any case).
    ///
    /// # Errors
    /// [`Error::Unsupported`] for any other extension.
    pub fn from_path(path: &Path) -> Result<Self> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        match ext.as_deref() {
            Some("tif" | "tiff") => Ok(Self::Tiff),
            Some("fit" | "fits" | "fts") => Ok(Self::Fits),
            Some("png") => Ok(Self::Png),
            _ => Err(Error::Unsupported(format!(
                "cannot choose an output format from {}; use .tif, .png or .fits",
                path.display()
            ))),
        }
    }
}

/// Words recorded in an output file about how it was made.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImageNotes {
    /// One line per fact, e.g. the colour preset. TIFF keeps them in
    /// `ImageDescription`, PNG in a text chunk, FITS as `HISTORY` cards.
    pub lines: Vec<String>,
    /// Observation metadata for the FITS header (SOLARNET keywords); the other
    /// formats ignore it.
    pub fits: Option<solarnet::FitsMeta>,
}

impl ImageNotes {
    /// Notes with a single line.
    #[must_use]
    pub fn line(text: impl Into<String>) -> Self {
        Self {
            lines: vec![text.into()],
            fits: None,
        }
    }

    /// These notes plus SOLARNET metadata for a FITS file.
    #[must_use]
    pub fn with_fits(mut self, meta: solarnet::FitsMeta) -> Self {
        self.fits = Some(meta);
        self
    }

    fn joined(&self) -> Option<String> {
        (!self.lines.is_empty()).then(|| self.lines.join("; "))
    }
}

/// Write a stacked image; the format follows the file extension.
///
/// # Errors
/// I/O or encoding failures, or an unrecognised extension.
pub fn save_image(path: &Path, frame: &Frame) -> Result<()> {
    save_image_with(path, frame, &ImageNotes::default())
}

/// As [`save_image`], recording `notes` in the file. The pixels are never
/// changed by the notes; a FITS file stays mono and unaltered.
///
/// # Errors
/// I/O or encoding failures, or an unrecognised extension.
pub fn save_image_with(path: &Path, frame: &Frame, notes: &ImageNotes) -> Result<()> {
    let format = ImageFormat::from_path(path)?;
    let mut w = BufWriter::new(File::create(path)?);
    let description = notes.joined();
    match format {
        ImageFormat::Tiff => tiff::write_tiff16_described(&mut w, frame, description.as_deref())?,
        ImageFormat::Fits => {
            let cards = notes.fits.as_ref().map(solarnet::FitsMeta::cards);
            let mut history = notes.lines.clone();
            if let Some(m) = &notes.fits {
                history.insert(0, format!("Stacked by Helia {}", m.version));
            }
            fits::write_fits_cards(
                &mut w,
                frame,
                fits::FitsFormat::Float32,
                cards.as_deref().unwrap_or(&[]),
                &history,
            )?;
        }
        ImageFormat::Png => png::write_png8(&mut w, frame, description.as_deref())?,
    }
    std::io::Write::flush(&mut w)?;
    Ok(())
}

/// Write a colour image as a 16-bit RGB TIFF, an 8-bit PNG, or a three-plane
/// 32-bit float FITS cube (red, green, blue), chosen from the extension,
/// recording `notes`.
///
/// # Errors
/// [`Error::Unsupported`] for an unknown extension; I/O or encoding failures.
pub fn save_rgb(path: &Path, img: &RgbImage, notes: &ImageNotes) -> Result<()> {
    let format = ImageFormat::from_path(path)?;
    let mut w = BufWriter::new(File::create(path)?);
    let description = notes.joined();
    match format {
        ImageFormat::Png => png::write_png8_rgb(&mut w, img, description.as_deref())?,
        ImageFormat::Fits => {
            let planes: Vec<Frame> = (0..3)
                .map(|c| {
                    Frame::new(
                        img.width,
                        img.height,
                        img.data.as_chunks::<3>().0.iter().map(|p| p[c]).collect(),
                    )
                })
                .collect::<Result<_>>()?;
            let cards = notes.fits.as_ref().map(solarnet::FitsMeta::cards);
            let mut history = notes.lines.clone();
            if let Some(m) = &notes.fits {
                history.insert(0, format!("Stacked by Helia {}", m.version));
            }
            fits::write_fits_planes(
                &mut w,
                &[&planes[0], &planes[1], &planes[2]],
                fits::FitsFormat::Float32,
                cards.as_deref().unwrap_or(&[]),
                &history,
            )?;
        }
        ImageFormat::Tiff => tiff::write_tiff16_rgb(&mut w, img, description.as_deref())?,
    }
    std::io::Write::flush(&mut w)?;
    Ok(())
}

/// Read a FITS image or the first frame of a SER file as a mono frame.
///
/// # Errors
/// I/O or format failures.
pub fn load_image(path: &Path) -> Result<Frame> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    let f = BufReader::new(File::open(path)?);
    match ext.as_deref() {
        Some("ser") => {
            let mut r = ser::SerReader::new(f)?;
            Ok(r.read_frame(0)?.to_mono())
        }
        _ => Ok(fits::read_fits(f)?.to_mono()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("helia-io-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap_or_else(|e| panic!("{e}"));
        d
    }

    #[test]
    fn png_is_a_known_format() {
        assert_eq!(
            ImageFormat::from_path(Path::new("a.PNG")).ok(),
            Some(ImageFormat::Png)
        );
        assert!(ImageFormat::from_path(Path::new("a.jpg")).is_err());
    }

    #[test]
    fn fits_notes_become_history_and_leave_the_pixels_alone() {
        let f = Frame::new(4, 2, (0..8).map(|i| i as f32 / 8.0).collect())
            .unwrap_or_else(|e| panic!("{e}"));
        let d = dir();
        let (plain, noted) = (d.join("plain.fits"), d.join("noted.fits"));
        save_image(&plain, &f).unwrap_or_else(|e| panic!("{e}"));
        save_image_with(
            &noted,
            &f,
            &ImageNotes::line("Colour preset ha (presentation)"),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let (a, b) = (
            std::fs::read(&plain).unwrap_or_default(),
            std::fs::read(&noted).unwrap_or_default(),
        );
        assert!(String::from_utf8_lossy(&b).contains("HISTORY Colour preset ha"));
        assert!(!String::from_utf8_lossy(&a).contains("HISTORY"));
        // Same data blocks: the header is one block long in both.
        assert_eq!(a.len(), b.len());
        assert_eq!(a[2880..], b[2880..]);
        let back = load_image(&noted).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back.data, f.data);
    }

    #[test]
    fn colour_is_written_to_fits_as_a_cube_and_an_unknown_extension_is_refused() {
        let img = RgbImage {
            width: 2,
            height: 1,
            data: vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6],
        };
        let path = dir().join("c.fits");
        save_rgb(&path, &img, &ImageNotes::default()).unwrap_or_else(|e| panic!("{e}"));
        let (planes, _) = fits::read_fits_planes(BufReader::new(
            File::open(&path).unwrap_or_else(|e| panic!("{e}")),
        ))
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(planes.len(), 3);
        assert_eq!(planes[1].data, [0.2, 0.5]);
        let err = save_rgb(&dir().join("c.jpg"), &img, &ImageNotes::default());
        assert!(matches!(err, Err(Error::Unsupported(_))));
    }
}
