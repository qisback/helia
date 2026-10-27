// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Small image filters.

use crate::frame::Frame;

/// The taps [`gaussian_blur`] convolves with: a normalised 1-D Gaussian
/// truncated at 4 sigma (`2 * ceil(4 sigma) + 1` of them), for backends that
/// run the same blur elsewhere.
#[must_use]
pub fn gaussian_taps(sigma: f32) -> Vec<f32> {
    gaussian_kernel(sigma)
}

/// Normalised 1-D Gaussian kernel truncated at 4 sigma.
fn gaussian_kernel(sigma: f32) -> Vec<f32> {
    let radius = (sigma * 4.0).ceil().max(1.0) as i64;
    let mut k: Vec<f32> = (-radius..=radius)
        .map(|i| (-(i as f32).powi(2) / (2.0 * sigma * sigma)).exp())
        .collect();
    let s: f32 = k.iter().sum();
    for v in &mut k {
        *v /= s;
    }
    k
}

/// Separable Gaussian blur with clamp-to-edge borders. `sigma <= 0` copies.
#[must_use]
pub fn gaussian_blur(frame: &Frame, sigma: f32) -> Frame {
    if sigma <= 0.0 {
        return frame.clone();
    }
    let (w, h) = (frame.width, frame.height);
    let k = gaussian_kernel(sigma);
    let r = k.len() / 2;
    // Horizontal pass: pad the row with clamped edge samples, then add one tap
    // at a time across the whole row. The per-pixel sum keeps the tap order
    // of a plain dot product, but each tap is a contiguous multiply-add the
    // compiler vectorises.
    let mut tmp = vec![0.0f32; w * h];
    let mut pad = vec![0.0f32; w + 2 * r];
    for y in 0..h {
        let row = frame.row(y);
        pad[..r].fill(row[0]);
        pad[r..r + w].copy_from_slice(row);
        pad[r + w..].fill(row[w - 1]);
        let out = &mut tmp[y * w..(y + 1) * w];
        for (i, kv) in k.iter().enumerate() {
            for (o, p) in out.iter_mut().zip(&pad[i..i + w]) {
                *o += kv * p;
            }
        }
    }
    // Vertical pass as weighted row sums, which vectorise well.
    let mut out = vec![0.0f32; w * h];
    for y in 0..h {
        let dst = &mut out[y * w..(y + 1) * w];
        for (i, kv) in k.iter().enumerate() {
            let yi = (y + i).saturating_sub(r).min(h - 1);
            for (d, s) in dst.iter_mut().zip(&tmp[yi * w..(yi + 1) * w]) {
                *d += kv * s;
            }
        }
    }
    Frame {
        width: w,
        height: h,
        data: out,
        bayer: frame.bayer,
    }
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_wrap,
    clippy::many_single_char_names,
    reason = "test oracle indexes with signed offsets"
)]
mod tests {
    use super::*;

    /// Textbook clamp-to-edge separable blur, one dot product per sample.
    fn naive(f: &Frame, sigma: f32) -> Frame {
        let k = gaussian_kernel(sigma);
        let r = (k.len() / 2) as i64;
        let (w, h) = (f.width as i64, f.height as i64);
        let at = |x: i64, y: i64| f.data[(y.clamp(0, h - 1) * w + x.clamp(0, w - 1)) as usize];
        let mut tmp = Frame::zeros(f.width, f.height);
        for y in 0..h {
            for x in 0..w {
                let mut s = 0.0f32;
                for (i, kv) in k.iter().enumerate() {
                    s += kv * at(x + i as i64 - r, y);
                }
                tmp.data[(y * w + x) as usize] = s;
            }
        }
        let mut out = Frame::zeros(f.width, f.height);
        for y in 0..h {
            for x in 0..w {
                let mut s = 0.0f32;
                for (i, kv) in k.iter().enumerate() {
                    let yy = (y + i as i64 - r).clamp(0, h - 1);
                    s += kv * tmp.data[(yy * w + x) as usize];
                }
                out.data[(y * w + x) as usize] = s;
            }
        }
        out
    }

    #[test]
    fn blur_matches_a_plain_separable_convolution_bit_for_bit() {
        for (w, h) in [(3, 2), (9, 7), (40, 31)] {
            let f = Frame {
                width: w,
                height: h,
                data: (0..w * h)
                    .map(|i| ((i * 37 % 101) as f32) / 101.0)
                    .collect(),
                bayer: None,
            };
            for sigma in [0.7, 1.0, 4.0] {
                assert_eq!(gaussian_blur(&f, sigma).data, naive(&f, sigma).data);
            }
        }
    }

    #[test]
    fn blur_preserves_mean_of_flat_and_spreads_impulse() {
        let mut f = Frame::zeros(33, 33);
        f.data[16 * 33 + 16] = 1.0;
        let b = gaussian_blur(&f, 2.0);
        let sum: f32 = b.data.iter().sum();
        assert!((sum - 1.0).abs() < 1e-4);
        assert!(b.get(16, 16) < 0.1 && b.get(16, 16) > b.get(20, 16));
    }
}
