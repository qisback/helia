// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! 8-bit PNG output for sharing, greyscale and RGB.

use std::io::Write;

use crate::colour::RgbImage;
use crate::error::{Error, Result};
use crate::frame::Frame;

fn png_err(e: impl std::fmt::Display) -> Error {
    Error::Invalid(format!("PNG: {e}"))
}

fn write<W: Write>(
    w: W,
    width: usize,
    height: usize,
    colour: ::png::ColorType,
    bytes: &[u8],
    description: Option<&str>,
) -> Result<()> {
    let to_u32 =
        |v: usize| u32::try_from(v).map_err(|_| Error::Invalid("image too large for PNG".into()));
    let mut enc = ::png::Encoder::new(w, to_u32(width)?, to_u32(height)?);
    enc.set_color(colour);
    enc.set_depth(::png::BitDepth::Eight);
    if let Some(d) = description {
        enc.add_text_chunk("Software".into(), "Helia".into())
            .map_err(png_err)?;
        enc.add_text_chunk("Description".into(), d.into())
            .map_err(png_err)?;
    }
    let mut writer = enc.write_header().map_err(png_err)?;
    writer.write_image_data(bytes).map_err(png_err)?;
    writer.finish().map_err(png_err)
}

/// Write `img` as an 8-bit RGB PNG. `description` becomes a text chunk.
///
/// # Errors
/// Encoder failure or a dimension beyond 32 bits.
pub fn write_png8_rgb<W: Write>(w: W, img: &RgbImage, description: Option<&str>) -> Result<()> {
    write(
        w,
        img.width,
        img.height,
        ::png::ColorType::Rgb,
        &img.to_u8(),
        description,
    )
}

/// Write `frame` as an 8-bit greyscale PNG (samples clamped to 0..1).
///
/// # Errors
/// Encoder failure or a dimension beyond 32 bits.
pub fn write_png8<W: Write>(w: W, frame: &Frame, description: Option<&str>) -> Result<()> {
    let bytes: Vec<u8> = frame
        .data
        .iter()
        .map(|&v| {
            if v.is_finite() {
                (v.clamp(0.0, 1.0) * 255.0).round() as u8
            } else {
                0
            }
        })
        .collect();
    write(
        w,
        frame.width,
        frame.height,
        ::png::ColorType::Grayscale,
        &bytes,
        description,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(buf: Vec<u8>) -> (::png::Info<'static>, Vec<u8>) {
        let mut reader = ::png::Decoder::new(std::io::Cursor::new(buf))
            .read_info()
            .unwrap_or_else(|e| panic!("{e}"));
        let mut out = vec![0u8; reader.output_buffer_size().unwrap_or(0)];
        reader
            .next_frame(&mut out)
            .unwrap_or_else(|e| panic!("{e}"));
        (reader.info().clone(), out)
    }

    #[test]
    fn rgb_png_decodes_to_the_same_bytes() {
        let img = RgbImage {
            width: 2,
            height: 1,
            data: vec![1.0, 0.5, 0.0, 0.25, 0.75, 1.0],
        };
        let mut buf = Vec::new();
        write_png8_rgb(&mut buf, &img, Some("preset ha")).unwrap_or_else(|e| panic!("{e}"));
        let (info, out) = decode(buf);
        assert_eq!((info.width, info.height), (2, 1));
        assert_eq!(info.color_type, ::png::ColorType::Rgb);
        assert_eq!(&out[..6], &img.to_u8()[..]);
        assert!(
            info.uncompressed_latin1_text
                .iter()
                .any(|t| t.keyword == "Description" && t.text == "preset ha")
        );
    }

    #[test]
    fn grey_png_is_one_byte_per_pixel() {
        let f = Frame::new(3, 1, vec![0.0, 0.5, 2.0]).unwrap_or_else(|e| panic!("{e}"));
        let mut buf = Vec::new();
        write_png8(&mut buf, &f, None).unwrap_or_else(|e| panic!("{e}"));
        let (_, out) = decode(buf);
        assert_eq!(&out[..3], &[0, 128, 255]);
    }
}
