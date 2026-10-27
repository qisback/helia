// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Geometry edits: crop, rotate, flip and resize.
//!
//! Crop, flip and quarter-turn rotation move whole pixels and are exact.
//! Resizing down averages exactly the area each output pixel covers; resizing
//! up and rotating by other angles interpolate with Lanczos-3.

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::frame::Frame;
use crate::resample::{Kernel, sample};

/// Largest side of an image an edit may produce.
pub const MAX_SIDE: usize = 20_000;

/// Keep a rectangle of the image.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Crop {
    /// Left edge, pixels from the left.
    pub x: usize,
    /// Top edge, pixels from the top.
    pub y: usize,
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
}

impl Crop {
    /// Check the rectangle against an image of `w` x `h`.
    ///
    /// # Errors
    /// [`Error::Invalid`] when the rectangle is empty or leaves the image.
    pub fn validate(&self, w: usize, h: usize) -> Result<()> {
        if self.width == 0
            || self.height == 0
            || self.x.saturating_add(self.width) > w
            || self.y.saturating_add(self.height) > h
        {
            return Err(Error::Invalid(format!(
                "crop {}x{} at ({},{}) does not fit an image of {w}x{h}",
                self.width, self.height, self.x, self.y
            )));
        }
        Ok(())
    }

    /// One line for the file history.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "crop {}x{} at ({},{})",
            self.width, self.height, self.x, self.y
        )
    }
}

/// Rotate clockwise by an angle in degrees. Multiples of 90 are exact; other
/// angles grow the canvas to hold the whole picture and fill the corners with
/// the level of the image border.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rotate {
    /// Degrees clockwise.
    pub degrees: f32,
}

impl Rotate {
    /// The quarter turns (0 to 3) when the angle is a multiple of 90 degrees.
    #[must_use]
    pub fn quarter_turns(&self) -> Option<u8> {
        let q = self.degrees / 90.0;
        (q.is_finite() && (q - q.round()).abs() < 1e-6).then(|| q.round().rem_euclid(4.0) as u8)
    }

    /// One line for the file history.
    #[must_use]
    pub fn describe(&self) -> String {
        format!("rotate {:.2} degrees clockwise", self.degrees)
    }
}

/// Which way to mirror.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    /// Left becomes right.
    #[default]
    Horizontal,
    /// Top becomes bottom.
    Vertical,
}

/// Mirror the image.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Flip {
    /// The mirror axis.
    pub axis: Axis,
}

impl Flip {
    /// One line for the file history.
    #[must_use]
    pub fn describe(&self) -> String {
        match self.axis {
            Axis::Horizontal => "flip horizontally".into(),
            Axis::Vertical => "flip vertically".into(),
        }
    }
}

/// Change the pixel count: by a factor, or to a width and/or height (one
/// alone keeps the aspect ratio).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Resize {
    /// Factor on both sides; when set, the sizes are ignored.
    pub scale: Option<f32>,
    /// Target width.
    pub width: Option<usize>,
    /// Target height.
    pub height: Option<usize>,
}

impl Resize {
    /// A resize by a factor.
    #[must_use]
    pub fn by(scale: f32) -> Self {
        Self {
            scale: Some(scale),
            ..Self::default()
        }
    }

    /// Output size for an input of `w` x `h`.
    ///
    /// # Errors
    /// [`Error::Invalid`] without a factor or size, with a non-positive one,
    /// or when the result would be empty or above [`MAX_SIDE`].
    pub fn target(&self, w: usize, h: usize) -> Result<(usize, usize)> {
        let round = |v: f32| (v.round() as usize).max(1);
        let (tw, th) = if let Some(s) = self.scale {
            if !s.is_finite() || s <= 0.0 {
                return Err(Error::Invalid("resize scale must be positive".into()));
            }
            (round(w as f32 * s), round(h as f32 * s))
        } else {
            match (self.width, self.height) {
                (Some(a), Some(b)) => (a, b),
                (Some(a), None) => (a, round(h as f32 * a as f32 / w as f32)),
                (None, Some(b)) => (round(w as f32 * b as f32 / h as f32), b),
                (None, None) => {
                    return Err(Error::Invalid(
                        "resize needs a scale, a width or a height".into(),
                    ));
                }
            }
        };
        if tw == 0 || th == 0 || tw > MAX_SIDE || th > MAX_SIDE {
            return Err(Error::Invalid(format!(
                "resize to {tw}x{th} is outside 1..={MAX_SIDE} pixels a side"
            )));
        }
        Ok((tw, th))
    }

    /// One line for the file history.
    #[must_use]
    pub fn describe(&self) -> String {
        match (self.scale, self.width, self.height) {
            (Some(s), ..) => format!("resize by {s:.4}"),
            (None, Some(w), Some(h)) => format!("resize to {w}x{h}"),
            (None, Some(w), None) => format!("resize to width {w}"),
            (None, None, Some(h)) => format!("resize to height {h}"),
            _ => "resize".into(),
        }
    }
}

fn with_data(w: usize, h: usize, data: Vec<f32>) -> Frame {
    Frame {
        width: w,
        height: h,
        data,
        bayer: None,
    }
}

/// Crop.
///
/// # Errors
/// As [`Crop::validate`].
pub fn crop(f: &Frame, c: &Crop) -> Result<Frame> {
    c.validate(f.width, f.height)?;
    let mut data = Vec::with_capacity(c.width * c.height);
    for y in c.y..c.y + c.height {
        data.extend_from_slice(&f.row(y)[c.x..c.x + c.width]);
    }
    Ok(with_data(c.width, c.height, data))
}

/// Mirror.
#[must_use]
pub fn flip(f: &Frame, axis: Axis) -> Frame {
    let (w, h) = (f.width, f.height);
    let mut data = Vec::with_capacity(w * h);
    match axis {
        Axis::Horizontal => {
            for y in 0..h {
                data.extend(f.row(y).iter().rev());
            }
        }
        Axis::Vertical => {
            for y in (0..h).rev() {
                data.extend_from_slice(f.row(y));
            }
        }
    }
    with_data(w, h, data)
}

/// Size of `w` x `h` after rotating by `r`.
#[must_use]
pub fn rotated_size(w: usize, h: usize, r: &Rotate) -> (usize, usize) {
    match r.quarter_turns() {
        Some(0 | 2) => (w, h),
        Some(_) => (h, w),
        None => {
            let (s, c) = r.degrees.to_radians().sin_cos();
            let (wf, hf) = (w as f32, h as f32);
            (
                ((wf * c.abs() + hf * s.abs()).ceil() as usize).max(1),
                ((wf * s.abs() + hf * c.abs()).ceil() as usize).max(1),
            )
        }
    }
}

/// Median of the border pixels: the level of the sky around a disc.
fn border_level(f: &Frame) -> f32 {
    let (w, h) = (f.width, f.height);
    let mut v: Vec<f32> = Vec::with_capacity(2 * (w + h));
    v.extend_from_slice(f.row(0));
    v.extend_from_slice(f.row(h - 1));
    for y in 1..h.saturating_sub(1) {
        v.push(f.get(0, y));
        v.push(f.get(w - 1, y));
    }
    v.sort_by(f32::total_cmp);
    v[v.len() / 2]
}

/// Rotate clockwise.
#[must_use]
pub fn rotate(f: &Frame, r: &Rotate) -> Frame {
    let (w, h) = (f.width, f.height);
    match r.quarter_turns() {
        Some(0) => f.clone(),
        Some(2) => {
            let mut data = f.data.clone();
            data.reverse();
            with_data(w, h, data)
        }
        Some(1) => {
            // out(x', y') = src(y', h - 1 - x')
            let mut data = Vec::with_capacity(w * h);
            for yo in 0..w {
                for xo in 0..h {
                    data.push(f.get(yo, h - 1 - xo));
                }
            }
            with_data(h, w, data)
        }
        Some(_) => {
            // out(x', y') = src(w - 1 - y', x')
            let mut data = Vec::with_capacity(w * h);
            for yo in 0..w {
                for xo in 0..h {
                    data.push(f.get(w - 1 - yo, xo));
                }
            }
            with_data(h, w, data)
        }
        None => {
            let (ow, oh) = rotated_size(w, h, r);
            let (s, c) = r.degrees.to_radians().sin_cos();
            let fill = border_level(f);
            let (cx, cy) = ((w as f32 - 1.0) / 2.0, (h as f32 - 1.0) / 2.0);
            let (ox, oy) = ((ow as f32 - 1.0) / 2.0, (oh as f32 - 1.0) / 2.0);
            let mut data = vec![fill; ow * oh];
            data.par_chunks_mut(ow).enumerate().for_each(|(yo, row)| {
                for (xo, d) in row.iter_mut().enumerate() {
                    let (dx, dy) = (xo as f32 - ox, yo as f32 - oy);
                    let sx = c * dx + s * dy + cx;
                    let sy = -s * dx + c * dy + cy;
                    if sx >= -0.5 && sx <= w as f32 - 0.5 && sy >= -0.5 && sy <= h as f32 - 0.5 {
                        *d = sample(f, sx, sy, Kernel::Lanczos3);
                    }
                }
            });
            with_data(ow, oh, data)
        }
    }
}

/// Area-average weights taking `n` input samples to `m <= n` outputs: for
/// output `i`, the first input and the weights, which sum to one.
fn area_weights(n: usize, m: usize) -> Vec<(usize, Vec<f32>)> {
    let ratio = n as f64 / m as f64;
    (0..m)
        .map(|i| {
            let (a, b) = (i as f64 * ratio, (i + 1) as f64 * ratio);
            let first = a.floor() as usize;
            let last = (b.ceil() as usize).min(n);
            let mut w: Vec<f32> = (first..last)
                .map(|k| {
                    let lo = (k as f64).max(a);
                    let hi = ((k + 1) as f64).min(b);
                    ((hi - lo).max(0.0) / ratio) as f32
                })
                .collect();
            let s: f32 = w.iter().sum();
            for v in &mut w {
                *v /= s;
            }
            (first, w)
        })
        .collect()
}

/// Resize.
///
/// # Errors
/// As [`Resize::target`].
pub fn resize(f: &Frame, r: &Resize) -> Result<Frame> {
    let (w, h) = (f.width, f.height);
    let (tw, th) = r.target(w, h)?;
    if (tw, th) == (w, h) {
        return Ok(f.clone());
    }
    if tw <= w && th <= h {
        let cols = area_weights(w, tw);
        let rows = area_weights(h, th);
        let mut tmp = vec![0.0f32; tw * h];
        tmp.par_chunks_mut(tw).enumerate().for_each(|(y, out)| {
            let src = f.row(y);
            for (o, (first, wts)) in out.iter_mut().zip(&cols) {
                *o = wts
                    .iter()
                    .enumerate()
                    .map(|(k, wt)| wt * src[first + k])
                    .sum();
            }
        });
        let mut data = vec![0.0f32; tw * th];
        data.par_chunks_mut(tw).enumerate().for_each(|(y, out)| {
            let (first, wts) = &rows[y];
            for (x, o) in out.iter_mut().enumerate() {
                *o = wts
                    .iter()
                    .enumerate()
                    .map(|(k, wt)| wt * tmp[(first + k) * tw + x])
                    .sum();
            }
        });
        return Ok(with_data(tw, th, data));
    }
    let (sx, sy) = (w as f32 / tw as f32, h as f32 / th as f32);
    let mut data = vec![0.0f32; tw * th];
    data.par_chunks_mut(tw).enumerate().for_each(|(y, row)| {
        for (x, d) in row.iter_mut().enumerate() {
            *d = sample(
                f,
                (x as f32 + 0.5) * sx - 0.5,
                (y as f32 + 0.5) * sy - 0.5,
                Kernel::Lanczos3,
            );
        }
    });
    Ok(with_data(tw, th, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(w: usize, h: usize) -> Frame {
        Frame::new(w, h, (0..w * h).map(|i| i as f32).collect()).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn crop_takes_exactly_the_rectangle() {
        let f = ramp(6, 5);
        let c = crop(
            &f,
            &Crop {
                x: 1,
                y: 2,
                width: 3,
                height: 2,
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((c.width, c.height), (3, 2));
        assert_eq!(c.data, vec![13.0, 14.0, 15.0, 19.0, 20.0, 21.0]);
        assert!(
            crop(
                &f,
                &Crop {
                    x: 4,
                    y: 0,
                    width: 3,
                    height: 1
                }
            )
            .is_err()
        );
        assert!(crop(&f, &Crop::default()).is_err());
    }

    #[test]
    fn flips_are_exact_and_involutions() {
        let f = ramp(4, 3);
        let h = flip(&f, Axis::Horizontal);
        assert_eq!(h.row(0), &[3.0, 2.0, 1.0, 0.0]);
        let v = flip(&f, Axis::Vertical);
        assert_eq!(v.row(0), f.row(2));
        assert_eq!(flip(&h, Axis::Horizontal), f);
        assert_eq!(flip(&v, Axis::Vertical), f);
    }

    #[test]
    fn quarter_turns_move_corners_clockwise() {
        // 3 wide, 2 tall:  0 1 2 / 3 4 5
        let f = ramp(3, 2);
        let r90 = rotate(&f, &Rotate { degrees: 90.0 });
        assert_eq!((r90.width, r90.height), (2, 3));
        assert_eq!(r90.data, vec![3.0, 0.0, 4.0, 1.0, 5.0, 2.0]);
        let r180 = rotate(&f, &Rotate { degrees: 180.0 });
        assert_eq!(r180.data, vec![5.0, 4.0, 3.0, 2.0, 1.0, 0.0]);
        let r270 = rotate(&f, &Rotate { degrees: 270.0 });
        assert_eq!(r270.data, vec![2.0, 5.0, 1.0, 4.0, 0.0, 3.0]);
        // Four quarter turns, and -90 against 270.
        let mut g = f.clone();
        for _ in 0..4 {
            g = rotate(&g, &Rotate { degrees: 90.0 });
        }
        assert_eq!(g, f);
        assert_eq!(rotate(&f, &Rotate { degrees: -90.0 }), r270);
        assert_eq!(rotate(&f, &Rotate { degrees: 450.0 }), r90);
    }

    #[test]
    fn rotated_size_holds_the_whole_picture() {
        assert_eq!(rotated_size(100, 50, &Rotate { degrees: 90.0 }), (50, 100));
        let (w, h) = rotated_size(100, 100, &Rotate { degrees: 45.0 });
        assert!((140..=143).contains(&w) && w == h);
    }

    #[test]
    fn a_small_rotation_keeps_a_smooth_image_nearly_unchanged() {
        let (w, h) = (64usize, 64usize);
        let f = Frame::new(
            w,
            h,
            (0..w * h)
                .map(|i| {
                    let (x, y) = ((i % w) as f32, (i / w) as f32);
                    0.5 + 0.3 * ((x - 32.0).hypot(y - 32.0) / 40.0).min(1.0)
                })
                .collect(),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let r = rotate(&f, &Rotate { degrees: 0.4 });
        let back = rotate(&r, &Rotate { degrees: -0.4 });
        let (ox, oy) = ((back.width - w) / 2, (back.height - h) / 2);
        let mut worst = 0.0f32;
        for y in 8..h - 8 {
            for x in 8..w - 8 {
                worst = worst.max((back.get(x + ox, y + oy) - f.get(x, y)).abs());
            }
        }
        assert!(worst < 0.01, "{worst}");
    }

    #[test]
    fn halving_averages_two_by_two_blocks() {
        let f = ramp(4, 4);
        let r = resize(&f, &Resize::by(0.5)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((r.width, r.height), (2, 2));
        assert_eq!(r.data, vec![2.5, 4.5, 10.5, 12.5]);
    }

    #[test]
    fn area_resize_keeps_the_mean_for_any_ratio() {
        let f = ramp(37, 23);
        let r = resize(
            &f,
            &Resize {
                width: Some(11),
                height: Some(7),
                ..Resize::default()
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            (r.mean() - f.mean()).abs() < 1e-3,
            "{} vs {}",
            r.mean(),
            f.mean()
        );
    }

    #[test]
    fn upscaling_hits_the_target_size_and_keeps_a_flat_field_flat() {
        let mut f = Frame::zeros(10, 8);
        f.data.fill(0.3);
        let r = resize(&f, &Resize::by(2.5)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((r.width, r.height), (25, 20));
        assert!(r.data.iter().all(|&v| (v - 0.3).abs() < 1e-5));
    }

    #[test]
    fn one_side_keeps_the_aspect_ratio() {
        let r = Resize {
            width: Some(50),
            ..Resize::default()
        };
        assert_eq!(r.target(200, 100).ok(), Some((50, 25)));
        assert!(Resize::default().target(10, 10).is_err());
        assert!(Resize::by(0.0).target(10, 10).is_err());
    }
}
