// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Read back an image Helia (or anything else) wrote: TIFF, PNG or FITS,
//! mono or RGB. This is what `helia edit` starts from.

use std::fs::File;
use std::io::{BufReader, Cursor, Read};
use std::path::Path;

use tiff::decoder::{Decoder, DecodingResult};
use tiff::{ColorType, tags::Tag};

use crate::colour::RgbImage;
use crate::edit::geometry::MAX_SIDE;
use crate::error::{Error, Result};
use crate::frame::Frame;
use crate::io::{ImageFormat, fits};

/// Largest image file read (bytes).
const MAX_FILE: u64 = 2 << 30;

/// An image read from disk.
#[derive(Clone, Debug, PartialEq)]
pub enum Loaded {
    /// One channel.
    Mono(Frame),
    /// Red, green and blue.
    Rgb(RgbImage),
}

impl Loaded {
    /// Width and height.
    #[must_use]
    pub fn size(&self) -> (usize, usize) {
        match self {
            Self::Mono(f) => (f.width, f.height),
            Self::Rgb(i) => (i.width, i.height),
        }
    }
}

fn check_size(w: usize, h: usize) -> Result<()> {
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
        return Err(Error::Invalid(format!(
            "image {w}x{h} is outside 1..={MAX_SIDE} pixels a side"
        )));
    }
    Ok(())
}

fn tiff_err(e: impl std::fmt::Display) -> Error {
    Error::Tiff(e.to_string())
}

/// Read a TIFF: 8, 16 or 32-bit float grey, or 8 or 16-bit RGB (an alpha
/// channel is dropped). Returns the image and its `ImageDescription`, if any.
///
/// # Errors
/// [`Error::Tiff`] for a layout that is not understood, [`Error::Invalid`]
/// for an absurd size.
pub fn read_tiff<R: Read + std::io::Seek>(r: R) -> Result<(Loaded, Option<String>)> {
    let mut dec = Decoder::new(r).map_err(tiff_err)?;
    let (w, h) = dec.dimensions().map_err(tiff_err)?;
    let (w, h) = (w as usize, h as usize);
    check_size(w, h)?;
    let colour = dec.colortype().map_err(tiff_err)?;
    let description = dec.get_tag_ascii_string(Tag::ImageDescription).ok();
    let (channels, grey) = match colour {
        ColorType::Gray(8 | 16 | 32) => (1, true),
        ColorType::RGB(8 | 16) => (3, false),
        ColorType::RGBA(8 | 16) => (4, false),
        other => {
            return Err(Error::Unsupported(format!(
                "TIFF colour type {other:?}; use 8 or 16-bit grey or RGB"
            )));
        }
    };
    let samples: Vec<f32> = match dec.read_image().map_err(tiff_err)? {
        DecodingResult::U8(v) => v.iter().map(|&x| f32::from(x) / 255.0).collect(),
        DecodingResult::U16(v) => v.iter().map(|&x| f32::from(x) / 65535.0).collect(),
        DecodingResult::F32(v) => v,
        _ => return Err(Error::Unsupported("TIFF sample format".into())),
    };
    if samples.len() != w * h * channels {
        return Err(Error::Tiff("TIFF data does not match its size".into()));
    }
    Ok((assemble(w, h, channels, grey, &samples)?, description))
}

fn assemble(w: usize, h: usize, channels: usize, grey: bool, samples: &[f32]) -> Result<Loaded> {
    if grey {
        return Ok(Loaded::Mono(Frame::new(w, h, samples.to_vec())?));
    }
    let mut data = Vec::with_capacity(w * h * 3);
    for px in samples.chunks_exact(channels) {
        data.extend_from_slice(&px[..3]);
    }
    Ok(Loaded::Rgb(RgbImage {
        width: w,
        height: h,
        data,
    }))
}

/// Read a PNG: 8 or 16-bit grey or RGB (alpha dropped; palettes expanded).
/// Returns the image and its `Description` text, if any.
///
/// # Errors
/// [`Error::Invalid`] for a file that does not decode or has an absurd size.
pub fn read_png<R: std::io::BufRead + std::io::Seek>(r: R) -> Result<(Loaded, Option<String>)> {
    let png_err = |e: ::png::DecodingError| Error::Invalid(format!("PNG: {e}"));
    let mut dec = ::png::Decoder::new(r);
    dec.set_transformations(::png::Transformations::EXPAND);
    let mut reader = dec.read_info().map_err(png_err)?;
    let info = reader.info();
    let (w, h) = (info.width as usize, info.height as usize);
    check_size(w, h)?;
    let description = info
        .uncompressed_latin1_text
        .iter()
        .find(|t| t.keyword == "Description")
        .map(|t| t.text.clone());
    let mut buf = vec![
        0u8;
        reader.output_buffer_size().ok_or_else(|| {
            Error::Invalid("PNG output size is not representable".into())
        })?
    ];
    let frame = reader.next_frame(&mut buf).map_err(png_err)?;
    let (colour, depth) = (frame.color_type, frame.bit_depth);
    let (channels, grey) = match colour {
        ::png::ColorType::Grayscale => (1, true),
        ::png::ColorType::GrayscaleAlpha => (2, true),
        ::png::ColorType::Rgb => (3, false),
        ::png::ColorType::Rgba => (4, false),
        ::png::ColorType::Indexed => {
            return Err(Error::Unsupported("indexed PNG".into()));
        }
    };
    let bytes = &buf[..frame.buffer_size()];
    let samples: Vec<f32> = match depth {
        ::png::BitDepth::Eight => bytes.iter().map(|&x| f32::from(x) / 255.0).collect(),
        ::png::BitDepth::Sixteen => bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| f32::from(u16::from_be_bytes(*c)) / 65535.0)
            .collect(),
        other => {
            return Err(Error::Unsupported(format!("PNG bit depth {other:?}")));
        }
    };
    if samples.len() != w * h * channels {
        return Err(Error::Invalid("PNG data does not match its size".into()));
    }
    let loaded = if grey && channels == 2 {
        let data: Vec<f32> = samples.as_chunks::<2>().0.iter().map(|p| p[0]).collect();
        Loaded::Mono(Frame::new(w, h, data)?)
    } else {
        assemble(w, h, channels, grey, &samples)?
    };
    Ok((loaded, description))
}

/// Read a TIFF, PNG or FITS file chosen by its extension. The second value is
/// the description recorded in the file, when it keeps one.
///
/// # Errors
/// I/O failure, an unknown extension or a file that does not decode.
pub fn read_any(path: &Path) -> Result<(Loaded, Option<String>)> {
    let meta = std::fs::metadata(path)?;
    if meta.len() > MAX_FILE {
        return Err(Error::Invalid(format!("{} is too large", path.display())));
    }
    match ImageFormat::from_path(path)? {
        ImageFormat::Tiff => {
            // The TIFF decoder wants Seek; a buffered file has it.
            read_tiff(BufReader::new(File::open(path)?))
        }
        ImageFormat::Png => read_png(BufReader::new(File::open(path)?)),
        ImageFormat::Fits => {
            let (planes, _) = fits::read_fits_planes(BufReader::new(File::open(path)?))?;
            if let Some(p) = planes.first() {
                check_size(p.width, p.height)?;
            }
            if let [r, g, b] = planes.as_slice() {
                let data = (0..r.data.len())
                    .flat_map(|i| [r.data[i], g.data[i], b.data[i]])
                    .collect();
                let rgb = RgbImage {
                    width: r.width,
                    height: r.height,
                    data,
                };
                return Ok((Loaded::Rgb(rgb), None));
            }
            let first = planes
                .into_iter()
                .next()
                .ok_or_else(|| Error::Invalid("FITS file holds no image".into()))?;
            Ok((Loaded::Mono(first.to_mono()), None))
        }
    }
}

/// Decode a TIFF held in memory (for tests and the API).
///
/// # Errors
/// As [`read_tiff`].
pub fn read_tiff_bytes(bytes: &[u8]) -> Result<(Loaded, Option<String>)> {
    read_tiff(Cursor::new(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::{ImageNotes, png, save_image_with, save_rgb, tiff as tiffw};

    fn dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("helia-any-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap_or_else(|e| panic!("{e}"));
        d
    }

    fn ramp() -> Frame {
        Frame::new(7, 5, (0..35).map(|i| i as f32 / 34.0).collect())
            .unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn grey_tiff_round_trips_within_one_level() {
        let f = ramp();
        let d = dir("tif");
        let p = d.join("a.tif");
        save_image_with(&p, &f, &ImageNotes::line("hello")).unwrap_or_else(|e| panic!("{e}"));
        let (got, desc) = read_any(&p).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(desc.as_deref(), Some("hello"));
        let Loaded::Mono(g) = got else {
            panic!("expected mono")
        };
        assert_eq!((g.width, g.height), (7, 5));
        for (a, b) in g.data.iter().zip(&f.data) {
            assert!((a - b).abs() <= 0.5 / 65535.0 + 1e-6);
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn rgb_tiff_and_png_round_trip() {
        let img = RgbImage {
            width: 3,
            height: 2,
            data: (0..18).map(|i| i as f32 / 17.0).collect(),
        };
        let d = dir("rgb");
        for name in ["c.tif", "c.png"] {
            let p = d.join(name);
            save_rgb(&p, &img, &ImageNotes::line("x")).unwrap_or_else(|e| panic!("{e}"));
            let (got, _) = read_any(&p).unwrap_or_else(|e| panic!("{e}"));
            let Loaded::Rgb(g) = got else {
                panic!("expected rgb")
            };
            assert_eq!((g.width, g.height), (3, 2));
            for (a, b) in g.data.iter().zip(&img.data) {
                assert!((a - b).abs() <= 1.0 / 255.0, "{name}: {a} vs {b}");
            }
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn grey_png_and_fits_read_back() {
        let f = ramp();
        let d = dir("grey");
        let mut buf = Vec::new();
        png::write_png8(&mut buf, &f, Some("note")).unwrap_or_else(|e| panic!("{e}"));
        let (got, desc) = read_png(Cursor::new(buf)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(desc.as_deref(), Some("note"));
        assert!(matches!(got, Loaded::Mono(_)));
        let p = d.join("g.fits");
        save_image_with(&p, &f, &ImageNotes::default()).unwrap_or_else(|e| panic!("{e}"));
        let (got, _) = read_any(&p).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(got, Loaded::Mono(f));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn unknown_or_odd_files_are_refused_with_a_message() {
        let d = dir("bad");
        let p = d.join("a.jpg");
        std::fs::write(&p, b"x").unwrap_or_else(|e| panic!("{e}"));
        assert!(read_any(&p).is_err());
        let t = d.join("short.tif");
        std::fs::write(&t, b"II*\0nothing").unwrap_or_else(|e| panic!("{e}"));
        assert!(read_any(&t).is_err());
        let mut buf = std::io::Cursor::new(Vec::new());
        tiffw::write_tiff16(&mut buf, &ramp()).unwrap_or_else(|e| panic!("{e}"));
        assert!(read_tiff_bytes(buf.get_ref()).is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }
}
