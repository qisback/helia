// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! 16-bit TIFF output, greyscale and RGB.

use std::io::{Seek, Write};

use tiff::encoder::{TiffEncoder, colortype};
use tiff::tags::Tag;

use crate::colour::RgbImage;
use crate::error::{Error, Result};
use crate::frame::Frame;

fn tiff_err(e: impl std::fmt::Display) -> Error {
    Error::Tiff(e.to_string())
}

fn dims(width: usize, height: usize) -> Result<(u32, u32)> {
    let to_u32 =
        |v: usize| u32::try_from(v).map_err(|_| Error::Invalid("image too large for TIFF".into()));
    Ok((to_u32(width)?, to_u32(height)?))
}

/// Write `img` as an uncompressed 16-bit RGB TIFF. `description` goes into the
/// `ImageDescription` tag.
///
/// # Errors
/// [`Error::Tiff`] if the encoder fails, or [`Error::Invalid`] when a
/// dimension exceeds the 32-bit TIFF limit.
pub fn write_tiff16_rgb<W: Write + Seek>(
    w: &mut W,
    img: &RgbImage,
    description: Option<&str>,
) -> Result<()> {
    let (width, height) = dims(img.width, img.height)?;
    let mut enc = TiffEncoder::new(w).map_err(tiff_err)?;
    let mut image = enc
        .new_image::<colortype::RGB16>(width, height)
        .map_err(tiff_err)?;
    if let Some(d) = description {
        image
            .encoder()
            .write_tag(Tag::ImageDescription, d)
            .map_err(tiff_err)?;
    }
    image.write_data(&img.to_u16()).map_err(tiff_err)
}

/// Write `frame` as an uncompressed 16-bit greyscale TIFF. Samples are
/// clamped to 0..1 and scaled to 0..65535.
///
/// # Errors
/// [`Error::Tiff`] if the encoder fails, or [`Error::Invalid`] when a
/// dimension exceeds the 32-bit TIFF limit.
pub fn write_tiff16<W: Write + Seek>(w: &mut W, frame: &Frame) -> Result<()> {
    write_tiff16_described(w, frame, None)
}

/// As [`write_tiff16`], with `description` in the `ImageDescription` tag.
///
/// # Errors
/// As [`write_tiff16`].
pub fn write_tiff16_described<W: Write + Seek>(
    w: &mut W,
    frame: &Frame,
    description: Option<&str>,
) -> Result<()> {
    let (width, height) = dims(frame.width, frame.height)?;
    let data: Vec<u16> = frame
        .data
        .iter()
        .map(|&v| {
            if v.is_finite() {
                (v.clamp(0.0, 1.0) * 65535.0).round() as u16
            } else {
                0
            }
        })
        .collect();
    let mut enc = TiffEncoder::new(w).map_err(tiff_err)?;
    let mut image = enc
        .new_image::<colortype::Gray16>(width, height)
        .map_err(tiff_err)?;
    if let Some(d) = description {
        image
            .encoder()
            .write_tag(Tag::ImageDescription, d)
            .map_err(tiff_err)?;
    }
    image.write_data(&data).map_err(tiff_err)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use tiff::decoder::{Decoder, DecodingResult};

    use super::*;

    #[test]
    fn round_trip_through_decoder() {
        let data: Vec<f32> = (0..15 * 10).map(|i| i as f32 / 150.0).collect();
        let f = Frame {
            width: 15,
            height: 10,
            data,
            bayer: None,
        };
        let mut buf = Cursor::new(Vec::new());
        write_tiff16(&mut buf, &f).unwrap_or_else(|e| panic!("{e}"));
        buf.set_position(0);
        let mut dec = Decoder::new(buf).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(dec.dimensions().ok(), Some((15, 10)));
        let Ok(DecodingResult::U16(px)) = dec.read_image() else {
            panic!("expected u16 image")
        };
        for (a, b) in px.iter().zip(&f.data) {
            assert!((f32::from(*a) / 65535.0 - b).abs() <= 0.5 / 65535.0 + 1e-6);
        }
    }

    #[test]
    fn rgb_round_trips_with_its_description() {
        let img = RgbImage {
            width: 2,
            height: 1,
            data: vec![1.0, 0.5, 0.0, 0.25, 0.75, 1.0],
        };
        let mut buf = Cursor::new(Vec::new());
        write_tiff16_rgb(&mut buf, &img, Some("preset ha")).unwrap_or_else(|e| panic!("{e}"));
        buf.set_position(0);
        let mut dec = Decoder::new(buf).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(dec.dimensions().ok(), Some((2, 1)));
        assert!(matches!(dec.colortype(), Ok(tiff::ColorType::RGB(16))));
        assert_eq!(
            dec.get_tag_ascii_string(Tag::ImageDescription)
                .ok()
                .as_deref(),
            Some("preset ha")
        );
        let Ok(DecodingResult::U16(px)) = dec.read_image() else {
            panic!("expected u16 image")
        };
        assert_eq!(px, img.to_u16());
    }

    #[test]
    fn grey_keeps_its_description_too() {
        let f = Frame::new(2, 2, vec![0.0, 0.5, 1.0, 0.25]).unwrap_or_else(|e| panic!("{e}"));
        let mut buf = Cursor::new(Vec::new());
        write_tiff16_described(&mut buf, &f, Some("mono")).unwrap_or_else(|e| panic!("{e}"));
        buf.set_position(0);
        let mut dec = Decoder::new(buf).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            dec.get_tag_ascii_string(Tag::ImageDescription)
                .ok()
                .as_deref(),
            Some("mono")
        );
    }
}
