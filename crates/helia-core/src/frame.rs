// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Single-channel image frames.

use crate::debayer::Debayer;
use crate::error::{Error, Result};

/// Colour filter array layout, named by the top-left 2x2 block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BayerPattern {
    /// R G / G B.
    Rggb,
    /// G R / B G.
    Grbg,
    /// G B / R G.
    Gbrg,
    /// B G / G R.
    Bggr,
}

impl BayerPattern {
    /// Channel (0 = R, 1 = G, 2 = B) sampled at pixel `(x, y)`.
    #[must_use]
    pub fn channel_at(self, x: usize, y: usize) -> usize {
        const R: usize = 0;
        const G: usize = 1;
        const B: usize = 2;
        let table = match self {
            Self::Rggb => [[R, G], [G, B]],
            Self::Grbg => [[G, R], [B, G]],
            Self::Gbrg => [[G, B], [R, G]],
            Self::Bggr => [[B, G], [G, R]],
        };
        table[y & 1][x & 1]
    }

    /// The pattern of the same image read from the bottom row up. With an odd
    /// height the first row keeps its parity and the pattern is unchanged.
    #[must_use]
    pub fn flipped_vertically(self, height: usize) -> Self {
        if height % 2 == 1 {
            return self;
        }
        match self {
            Self::Rggb => Self::Gbrg,
            Self::Gbrg => Self::Rggb,
            Self::Grbg => Self::Bggr,
            Self::Bggr => Self::Grbg,
        }
    }
}

/// Axis-aligned pixel rectangle (`x1`, `y1` exclusive).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    /// Left edge.
    pub x0: usize,
    /// Top edge.
    pub y0: usize,
    /// Right edge, exclusive.
    pub x1: usize,
    /// Bottom edge, exclusive.
    pub y1: usize,
}

impl Rect {
    /// Rectangle from origin and size.
    #[must_use]
    pub fn new(x: usize, y: usize, w: usize, h: usize) -> Self {
        Self {
            x0: x,
            y0: y,
            x1: x + w,
            y1: y + h,
        }
    }

    /// Width in pixels.
    #[must_use]
    pub fn width(&self) -> usize {
        self.x1.saturating_sub(self.x0)
    }

    /// Height in pixels.
    #[must_use]
    pub fn height(&self) -> usize {
        self.y1.saturating_sub(self.y0)
    }

    /// Pixel count.
    #[must_use]
    pub fn area(&self) -> usize {
        self.width() * self.height()
    }

    /// True when the rectangle contains no pixels.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.area() == 0
    }

    /// Clip to an image of `w` x `h`.
    #[must_use]
    pub fn clamp(&self, w: usize, h: usize) -> Self {
        Self {
            x0: self.x0.min(w),
            y0: self.y0.min(h),
            x1: self.x1.min(w),
            y1: self.y1.min(h),
        }
    }
}

/// Mono luminance image, row-major, normalised to roughly 0..1.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// Row-major samples. For Bayer frames these are raw mosaic values.
    pub data: Vec<f32>,
    /// Set while `data` is an undemosaiced colour-filter-array image.
    pub bayer: Option<BayerPattern>,
}

impl Frame {
    /// Wrap existing samples; fails when the length does not match.
    ///
    /// # Errors
    /// [`Error::Invalid`] on a size mismatch or zero dimension.
    pub fn new(width: usize, height: usize, data: Vec<f32>) -> Result<Self> {
        let expected = width.checked_mul(height);
        if width == 0 || height == 0 || expected != Some(data.len()) {
            return Err(Error::Invalid(format!(
                "frame {width}x{height} does not match {} samples",
                data.len()
            )));
        }
        Ok(Self {
            width,
            height,
            data,
            bayer: None,
        })
    }

    /// All-zero frame.
    #[must_use]
    pub fn zeros(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            data: vec![0.0; width * height],
            bayer: None,
        }
    }

    /// Tag the data as a raw Bayer mosaic.
    #[must_use]
    pub fn with_bayer(mut self, pattern: Option<BayerPattern>) -> Self {
        self.bayer = pattern;
        self
    }

    /// Sample at `(x, y)`; panics only on out-of-range indices (caller bug).
    #[must_use]
    pub fn get(&self, x: usize, y: usize) -> f32 {
        self.data[y * self.width + x]
    }

    /// One row of samples.
    #[must_use]
    pub fn row(&self, y: usize) -> &[f32] {
        &self.data[y * self.width..(y + 1) * self.width]
    }

    /// Whole-frame rectangle.
    #[must_use]
    pub fn rect(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    /// Arithmetic mean, accumulated in f64.
    #[must_use]
    pub fn mean(&self) -> f32 {
        let s: f64 = self.data.iter().map(|&v| f64::from(v)).sum();
        (s / self.data.len().max(1) as f64) as f32
    }

    /// Smallest and largest finite sample.
    #[must_use]
    pub fn min_max(&self) -> (f32, f32) {
        self.data
            .iter()
            .filter(|v| v.is_finite())
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &v| {
                (lo.min(v), hi.max(v))
            })
    }

    /// Mono luminance with the default demosaic ([`Debayer::default`]: MHC,
    /// mean of R, G and B), or a plain copy for mono data.
    #[must_use]
    pub fn to_mono(&self) -> Frame {
        Debayer::default().apply(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_rejects_bad_sizes() {
        assert!(Frame::new(2, 2, vec![0.0; 3]).is_err());
        assert!(Frame::new(0, 2, vec![]).is_err());
        assert!(Frame::new(2, 2, vec![0.0; 4]).is_ok());
    }

    #[test]
    fn debayer_flat_field_is_flat() {
        for p in [
            BayerPattern::Rggb,
            BayerPattern::Grbg,
            BayerPattern::Gbrg,
            BayerPattern::Bggr,
        ] {
            let mut f = Frame::zeros(7, 6);
            f.data.fill(0.5);
            let m = f.with_bayer(Some(p)).to_mono();
            assert!(m.bayer.is_none());
            assert!(m.data.iter().all(|&v| (v - 0.5).abs() < 1e-6));
        }
    }

    #[test]
    fn debayer_separates_channels() {
        // Pure red mosaic gives luminance 1/3 in the interior.
        let (w, h) = (8, 8);
        let mut d = vec![0.0; w * h];
        for y in 0..h {
            for x in 0..w {
                if BayerPattern::Rggb.channel_at(x, y) == 0 {
                    d[y * w + x] = 1.0;
                }
            }
        }
        let m = Frame {
            width: w,
            height: h,
            data: d,
            bayer: Some(BayerPattern::Rggb),
        }
        .to_mono();
        assert!((m.get(3, 3) - 1.0 / 3.0).abs() < 1e-6);
    }
}
