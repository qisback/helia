// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! A screenshot as RGBA pixels: PNG in and out, highlight burn-in, and
//! shrinking to fit the relay's size limit.

use std::io::Cursor;

use crate::protocol::{Highlight, MAX_SCREENSHOT_BYTES};

/// The colour highlights are burned in with.
pub const HIGHLIGHT_RGBA: [u8; 4] = [255, 48, 48, 255];

/// Why a screenshot could not be handled.
#[derive(Debug, thiserror::Error)]
pub enum ShotError {
    /// The pixel buffer does not match the size.
    #[error("the screenshot is {0} bytes, which does not match its size")]
    Size(usize),
    /// The PNG could not be read or written.
    #[error("PNG: {0}")]
    Png(String),
    /// Even a heavily shrunk copy is over the limit.
    #[error("the screenshot cannot be made smaller than the {0} byte limit")]
    TooLarge(usize),
}

/// An 8-bit RGBA image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screenshot {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes, row by row.
    pub rgba: Vec<u8>,
}

impl Screenshot {
    /// Wrap pixels.
    ///
    /// # Errors
    /// [`ShotError::Size`] when `rgba` is not `width * height * 4` bytes.
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Result<Self, ShotError> {
        if rgba.len() != width as usize * height as usize * 4 || width == 0 || height == 0 {
            return Err(ShotError::Size(rgba.len()));
        }
        Ok(Self {
            width,
            height,
            rgba,
        })
    }

    /// Encode as PNG.
    ///
    /// # Errors
    /// [`ShotError::Png`] if the encoder fails.
    pub fn to_png(&self) -> Result<Vec<u8>, ShotError> {
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, self.width, self.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc
            .write_header()
            .map_err(|e| ShotError::Png(e.to_string()))?;
        w.write_image_data(&self.rgba)
            .map_err(|e| ShotError::Png(e.to_string()))?;
        w.finish().map_err(|e| ShotError::Png(e.to_string()))?;
        Ok(out)
    }

    /// Decode a PNG of any colour type to RGBA.
    ///
    /// # Errors
    /// [`ShotError::Png`] when the bytes are not a readable PNG.
    pub fn from_png(bytes: &[u8]) -> Result<Self, ShotError> {
        let mut dec = png::Decoder::new(Cursor::new(bytes));
        dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = dec.read_info().map_err(|e| ShotError::Png(e.to_string()))?;
        let mut buf = vec![0; reader.output_buffer_size().unwrap_or(0)];
        let info = reader
            .next_frame(&mut buf)
            .map_err(|e| ShotError::Png(e.to_string()))?;
        buf.truncate(info.buffer_size());
        let rgba = match info.color_type {
            png::ColorType::Rgba => buf,
            png::ColorType::Rgb => buf
                .as_chunks::<3>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2], 255])
                .collect(),
            png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
            png::ColorType::GrayscaleAlpha => buf
                .as_chunks::<2>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[0], p[0], p[1]])
                .collect(),
            png::ColorType::Indexed => {
                return Err(ShotError::Png("palette not expanded".into()));
            }
        };
        Self::new(info.width, info.height, rgba)
    }

    /// Halve the size with a 2 by 2 average.
    #[must_use]
    pub fn halved(&self) -> Self {
        let (w, h) = ((self.width / 2).max(1), (self.height / 2).max(1));
        let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
        for y in 0..h {
            for x in 0..w {
                for c in 0..4 {
                    let mut sum = 0u32;
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let sx = (x * 2 + dx).min(self.width - 1);
                        let sy = (y * 2 + dy).min(self.height - 1);
                        sum += u32::from(
                            self.rgba[(sy as usize * self.width as usize + sx as usize) * 4 + c],
                        );
                    }
                    rgba.push(((sum + 2) / 4) as u8);
                }
            }
        }
        Self {
            width: w,
            height: h,
            rgba,
        }
    }

    /// Outline each highlight in [`HIGHLIGHT_RGBA`]. The outline lies inside
    /// the rectangle, is at least 3 pixels thick, and is clipped to the image.
    pub fn burn_in(&mut self, highlights: &[Highlight]) {
        let t = (self.width.min(self.height) / 200).max(3);
        for h in highlights {
            let x1 = (h.x + h.w).min(self.width);
            let y1 = (h.y + h.h).min(self.height);
            for y in h.y.min(self.height)..y1 {
                for x in h.x.min(self.width)..x1 {
                    let edge =
                        x < h.x + t || x + t >= h.x + h.w || y < h.y + t || y + t >= h.y + h.h;
                    if edge {
                        let i = (y as usize * self.width as usize + x as usize) * 4;
                        self.rgba[i..i + 4].copy_from_slice(&HIGHLIGHT_RGBA);
                    }
                }
            }
        }
    }

    /// A copy with the highlights burned in.
    #[must_use]
    pub fn with_highlights(&self, highlights: &[Highlight]) -> Self {
        let mut c = self.clone();
        c.burn_in(highlights);
        c
    }

    /// PNG bytes no larger than `max_bytes`, halving the image until they fit.
    /// The highlights are returned scaled to the image that was encoded.
    ///
    /// # Errors
    /// [`ShotError::TooLarge`] when it still does not fit at 64 pixels wide.
    pub fn encode_within(
        &self,
        highlights: &[Highlight],
        max_bytes: usize,
    ) -> Result<(Vec<u8>, Vec<Highlight>), ShotError> {
        let mut shot = self.clone();
        let mut hs = highlights.to_vec();
        loop {
            let png = shot.to_png()?;
            if png.len() <= max_bytes {
                return Ok((png, hs));
            }
            if shot.width < 128 || shot.height < 128 {
                return Err(ShotError::TooLarge(max_bytes));
            }
            shot = shot.halved();
            hs = hs.into_iter().map(|h| h.shrunk(2)).collect();
        }
    }

    /// [`Self::encode_within`] at the relay's limit.
    ///
    /// # Errors
    /// As [`Self::encode_within`].
    pub fn encode_for_relay(
        &self,
        highlights: &[Highlight],
    ) -> Result<(Vec<u8>, Vec<Highlight>), ShotError> {
        self.encode_within(highlights, MAX_SCREENSHOT_BYTES)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use super::*;

    fn grey(w: u32, h: u32, v: u8) -> Screenshot {
        Screenshot::new(w, h, [v, v, v, 255].repeat((w * h) as usize)).unwrap()
    }

    fn px(s: &Screenshot, x: u32, y: u32) -> [u8; 4] {
        let i = (y * s.width + x) as usize * 4;
        s.rgba[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn burn_in_draws_an_outline_and_leaves_the_inside_alone() {
        let mut s = grey(100, 80, 10);
        s.burn_in(&[Highlight {
            x: 20,
            y: 10,
            w: 40,
            h: 30,
        }]);
        for (x, y) in [
            (20, 10),
            (59, 10),
            (20, 39),
            (59, 39),
            (40, 10),
            (40, 12),
            (21, 25),
        ] {
            assert_eq!(px(&s, x, y), HIGHLIGHT_RGBA, "({x},{y}) is on the outline");
        }
        for (x, y) in [
            (40, 25),
            (24, 14),
            (19, 25),
            (60, 25),
            (40, 9),
            (40, 40),
            (0, 0),
        ] {
            assert_eq!(px(&s, x, y), [10, 10, 10, 255], "({x},{y}) is untouched");
        }
    }

    #[test]
    fn burn_in_clips_to_the_image() {
        let mut s = grey(30, 30, 0);
        s.burn_in(&[Highlight {
            x: 20,
            y: 20,
            w: 100,
            h: 100,
        }]);
        assert_eq!(px(&s, 20, 25), HIGHLIGHT_RGBA);
        assert_eq!(
            px(&s, 29, 29),
            [0, 0, 0, 255],
            "the clipped far edge is off the image"
        );
        assert_eq!(px(&s, 10, 10), [0, 0, 0, 255]);
        s.burn_in(&[Highlight {
            x: 500,
            y: 500,
            w: 10,
            h: 10,
        }]);
    }

    #[test]
    fn png_round_trips() {
        let mut s = grey(17, 9, 40);
        s.rgba[0..4].copy_from_slice(&[1, 2, 3, 4]);
        let back = Screenshot::from_png(&s.to_png().unwrap()).unwrap();
        assert_eq!(back, s);
        assert!(Screenshot::from_png(b"not a png").is_err());
    }

    #[test]
    fn a_wrong_buffer_is_refused() {
        assert!(Screenshot::new(2, 2, vec![0; 15]).is_err());
        assert!(Screenshot::new(0, 2, vec![]).is_err());
    }

    #[test]
    fn halving_averages_and_scales_highlights() {
        let mut s = grey(8, 8, 0);
        for x in 0..8 {
            for y in 0..8 {
                let i = (y * 8 + x) as usize * 4;
                s.rgba[i] = if x % 2 == 0 { 100 } else { 200 };
            }
        }
        let h = s.halved();
        assert_eq!((h.width, h.height), (4, 4));
        assert_eq!(px(&h, 1, 1)[0], 150);
        let hl = Highlight {
            x: 4,
            y: 6,
            w: 20,
            h: 1,
        };
        assert_eq!(
            hl.shrunk(2),
            Highlight {
                x: 2,
                y: 3,
                w: 10,
                h: 1
            }
        );
    }

    #[test]
    fn oversize_screenshots_are_shrunk_until_they_fit() {
        // Noise does not compress, so the first encode is over the limit.
        let mut seed = 12345u32;
        let rgba: Vec<u8> = (0..256 * 256 * 4)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 24) as u8
            })
            .collect();
        let s = Screenshot::new(256, 256, rgba).unwrap();
        let full = s.to_png().unwrap().len();
        let hl = [Highlight {
            x: 64,
            y: 64,
            w: 100,
            h: 100,
        }];
        let (png, hs) = s.encode_within(&hl, full / 2).unwrap();
        assert!(png.len() <= full / 2);
        let back = Screenshot::from_png(&png).unwrap();
        assert_eq!((back.width, back.height), (128, 128));
        assert_eq!(hs[0], hl[0].shrunk(2));
        assert!(matches!(
            s.encode_within(&hl, 10),
            Err(ShotError::TooLarge(10))
        ));
        let (png, hs) = s.encode_for_relay(&hl).unwrap();
        assert_eq!(png.len(), full);
        assert_eq!(hs, hl);
    }
}
