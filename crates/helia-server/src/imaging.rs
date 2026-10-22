// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! 8-bit previews: stretch, box downscale, PNG encoding.

use helia_core::Frame;

use crate::error::{ApiError, ApiResult};

/// Largest width a stored job preview keeps.
pub const PREVIEW_STORE_WIDTH: usize = 2048;
/// Largest `max_width` a client may request.
pub const MAX_PREVIEW_WIDTH: usize = 8192;
/// Smallest `max_width` a client may request.
pub const MIN_PREVIEW_WIDTH: usize = 16;

/// Greyscale 8-bit image.
#[derive(Clone, Debug)]
pub struct Grey8 {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// Row-major samples.
    pub data: Vec<u8>,
}

/// How sample values map to display levels.
#[derive(Clone, Copy, Debug)]
pub enum Stretch {
    /// Clip to 0..1, then gamma 0.5 (for normalised stacks).
    Gamma,
    /// Map the 0.1 and 99.9 percentiles to black and white.
    Percentile,
}

const BINS: usize = 4096;

fn percentile_bounds(data: &[f32]) -> (f32, f32) {
    let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
    for &v in data.iter().filter(|v| v.is_finite()) {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    if !(lo.is_finite() && hi.is_finite()) || hi <= lo {
        return (0.0, 1.0);
    }
    let mut hist = vec![0u64; BINS];
    let scale = (BINS - 1) as f32 / (hi - lo);
    let mut total = 0u64;
    for &v in data.iter().filter(|v| v.is_finite()) {
        hist[((v - lo) * scale) as usize] += 1;
        total += 1;
    }
    let find = |frac: f64| {
        let target = (total as f64 * frac) as u64;
        let mut acc = 0u64;
        for (i, &c) in hist.iter().enumerate() {
            acc += c;
            if acc >= target {
                return lo + i as f32 / scale;
            }
        }
        hi
    };
    let (a, b) = (find(0.001), find(0.999));
    if b > a { (a, b) } else { (lo, hi) }
}

/// Quantise a frame to 8 bits.
#[must_use]
pub fn to_grey8(frame: &Frame, stretch: Stretch) -> Grey8 {
    let data = match stretch {
        Stretch::Gamma => frame
            .data
            .iter()
            .map(|&v| (v.clamp(0.0, 1.0).sqrt() * 255.0 + 0.5) as u8)
            .collect(),
        Stretch::Percentile => {
            let (lo, hi) = percentile_bounds(&frame.data);
            let k = 255.0 / (hi - lo);
            frame
                .data
                .iter()
                .map(|&v| ((v - lo) * k + 0.5).clamp(0.0, 255.0) as u8)
                .collect()
        }
    };
    Grey8 {
        width: frame.width,
        height: frame.height,
        data,
    }
}

/// Box-filter downscale so the width is at most `max_width`.
#[must_use]
pub fn downscale(img: &Grey8, max_width: usize) -> Grey8 {
    if img.width <= max_width || max_width == 0 {
        return img.clone();
    }
    let ow = max_width;
    let oh = ((img.height * ow) / img.width).max(1);
    let mut out = vec![0u8; ow * oh];
    for oy in 0..oh {
        let y0 = oy * img.height / oh;
        let y1 = ((oy + 1) * img.height / oh).max(y0 + 1).min(img.height);
        for ox in 0..ow {
            let x0 = ox * img.width / ow;
            let x1 = ((ox + 1) * img.width / ow).max(x0 + 1).min(img.width);
            let mut sum = 0u32;
            for y in y0..y1 {
                sum += img.data[y * img.width + x0..y * img.width + x1]
                    .iter()
                    .map(|&v| u32::from(v))
                    .sum::<u32>();
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            out[oy * ow + ox] = ((sum + n / 2) / n) as u8;
        }
    }
    Grey8 {
        width: ow,
        height: oh,
        data: out,
    }
}

/// Encode as an 8-bit greyscale PNG.
///
/// # Errors
/// Encoder failure (not expected for valid dimensions).
pub fn encode_png(img: &Grey8) -> ApiResult<Vec<u8>> {
    let mut out = Vec::new();
    let w = u32::try_from(img.width).map_err(|_| ApiError::internal("image too wide"))?;
    let h = u32::try_from(img.height).map_err(|_| ApiError::internal("image too tall"))?;
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(png::ColorType::Grayscale);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    let mut writer = enc
        .write_header()
        .map_err(|e| ApiError::internal(format!("png: {e}")))?;
    writer
        .write_image_data(&img.data)
        .map_err(|e| ApiError::internal(format!("png: {e}")))?;
    writer
        .finish()
        .map_err(|e| ApiError::internal(format!("png: {e}")))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downscale_halves_and_keeps_mean() {
        let img = Grey8 {
            width: 8,
            height: 4,
            data: vec![100; 32],
        };
        let d = downscale(&img, 4);
        assert_eq!((d.width, d.height), (4, 2));
        assert!(d.data.iter().all(|&v| v == 100));
    }

    #[test]
    fn png_has_signature() {
        let img = Grey8 {
            width: 2,
            height: 2,
            data: vec![0, 64, 128, 255],
        };
        let png = encode_png(&img).expect("encode");
        assert_eq!(&png[..4], &[0x89, b'P', b'N', b'G']);
    }
}
