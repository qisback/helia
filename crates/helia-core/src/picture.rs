// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Whole-image operations on a mono or colour picture: the tools that the
//! time-lapse, derotation and mosaic stages share.
//!
//! A [`Picture`] is what an image file holds (see [`crate::io::any::Loaded`]).
//! Every operation treats the colour planes alike, so a colour stack moves,
//! scales and crops exactly as its luminance does.

#![allow(
    clippy::many_single_char_names,
    reason = "x, y, w and h are the natural names in image maths"
)]

use rayon::prelude::*;

use crate::align::Shift;
use crate::colour::RgbImage;
use crate::error::{Error, Result};
use crate::frame::{Frame, Rect};
use crate::io::any::Loaded;
use crate::resample::{Kernel, sample, shifted};

/// A mono or colour image.
pub type Picture = Loaded;

/// Width and height of a picture.
#[must_use]
pub fn size(p: &Picture) -> (usize, usize) {
    p.size()
}

/// Luminance: the mono image itself, or the mean of red, green and blue.
#[must_use]
pub fn luma(p: &Picture) -> Frame {
    match p {
        Picture::Mono(f) => f.clone(),
        Picture::Rgb(i) => {
            let data = i
                .data
                .as_chunks::<3>()
                .0
                .iter()
                .map(|c| (c[0] + c[1] + c[2]) / 3.0)
                .collect();
            Frame {
                width: i.width,
                height: i.height,
                data,
                bayer: None,
            }
        }
    }
}

/// The planes of a picture: one for mono, three for colour.
#[must_use]
pub fn planes(p: &Picture) -> Vec<Frame> {
    match p {
        Picture::Mono(f) => vec![f.clone()],
        Picture::Rgb(i) => (0..3)
            .map(|c| Frame {
                width: i.width,
                height: i.height,
                data: i.data.as_chunks::<3>().0.iter().map(|px| px[c]).collect(),
                bayer: None,
            })
            .collect(),
    }
}

/// Put planes back together: one plane is mono, three are colour.
///
/// # Errors
/// [`Error::Invalid`] for any other number of planes or planes of unequal size.
pub fn from_planes(planes: &[Frame]) -> Result<Picture> {
    match planes {
        [m] => Ok(Picture::Mono(m.clone())),
        [r, g, b] => {
            if g.width != r.width
                || b.width != r.width
                || g.height != r.height
                || b.height != r.height
            {
                return Err(Error::Invalid("colour planes differ in size".into()));
            }
            let data = (0..r.data.len())
                .flat_map(|i| [r.data[i], g.data[i], b.data[i]])
                .collect();
            Ok(Picture::Rgb(RgbImage {
                width: r.width,
                height: r.height,
                data,
            }))
        }
        _ => Err(Error::Invalid(format!(
            "a picture has one or three planes, not {}",
            planes.len()
        ))),
    }
}

/// Apply `f` to every plane and reassemble the picture.
fn map_planes(p: &Picture, f: impl Fn(&Frame) -> Frame) -> Picture {
    match p {
        Picture::Mono(m) => Picture::Mono(f(m)),
        Picture::Rgb(_) => {
            let out: Vec<Frame> = planes(p).iter().map(f).collect();
            // Cannot fail: `f` keeps the size of each plane or all alike.
            from_planes(&out).unwrap_or_else(|_| p.clone())
        }
    }
}

/// The picture moved by `shift` (see [`crate::resample`]: the result at `x` is
/// the source at `x + shift`). Pixels from outside the picture take the nearest
/// edge value.
#[must_use]
pub fn shift(p: &Picture, shift: Shift, kernel: Kernel) -> Picture {
    map_planes(p, |f| shifted(f, shift, kernel))
}

/// Resample through a coordinate map: the result at `(x, y)` is the source at
/// `map(x, y)`. Samples whose source lies outside the picture take the nearest
/// edge value.
#[must_use]
pub fn warp(p: &Picture, map: &(dyn Fn(f32, f32) -> (f32, f32) + Sync), kernel: Kernel) -> Picture {
    map_planes(p, |f| {
        let (w, h) = (f.width, f.height);
        let mut data = vec![0.0f32; w * h];
        data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, v) in row.iter_mut().enumerate() {
                let (sx, sy) = map(x as f32, y as f32);
                *v = sample(f, sx, sy, kernel);
            }
        });
        Frame {
            width: w,
            height: h,
            data,
            bayer: None,
        }
    })
}

/// Multiply every sample by `gain` and add `offset`.
#[must_use]
pub fn gain_offset(p: &Picture, gain: f32, offset: f32) -> Picture {
    map_planes(p, |f| Frame {
        width: f.width,
        height: f.height,
        data: f.data.iter().map(|&v| v * gain + offset).collect(),
        bayer: None,
    })
}

/// The part of the picture inside `rect`.
///
/// # Errors
/// [`Error::Invalid`] when the rectangle is empty or outside the picture.
pub fn crop(p: &Picture, rect: Rect) -> Result<Picture> {
    let (w, h) = p.size();
    let r = rect.clamp(w, h);
    if r.is_empty() || r != rect {
        return Err(Error::Invalid(format!(
            "crop {}x{} at ({}, {}) does not fit a {w}x{h} picture",
            rect.width(),
            rect.height(),
            rect.x0,
            rect.y0
        )));
    }
    let cut = |f: &Frame| {
        let mut data = Vec::with_capacity(r.area());
        for y in r.y0..r.y1 {
            data.extend_from_slice(&f.row(y)[r.x0..r.x1]);
        }
        Frame {
            width: r.width(),
            height: r.height(),
            data,
            bayer: None,
        }
    };
    Ok(map_planes(p, cut))
}

/// Mean of the luminance inside the circle `(cx, cy, r)`; `None` when no pixel
/// falls inside it.
#[must_use]
pub fn mean_in_disc(f: &Frame, cx: f32, cy: f32, r: f32) -> Option<f32> {
    let (mut sum, mut n) = (0.0f64, 0usize);
    let y0 = (cy - r).floor().max(0.0) as usize;
    let y1 = ((cy + r).ceil().max(0.0) as usize + 1).min(f.height);
    let x0 = (cx - r).floor().max(0.0) as usize;
    let x1 = ((cx + r).ceil().max(0.0) as usize + 1).min(f.width);
    for y in y0..y1 {
        for x in x0..x1 {
            if (x as f32 - cx).hypot(y as f32 - cy) <= r {
                sum += f64::from(f.data[y * f.width + x]);
                n += 1;
            }
        }
    }
    (n > 0).then(|| (sum / n as f64) as f32)
}

/// Save a picture; the format follows the extension. A colour picture needs a
/// TIFF, PNG or FITS file as for any colour output.
///
/// # Errors
/// I/O or encoding failures.
pub fn save(path: &std::path::Path, p: &Picture, notes: &crate::io::ImageNotes) -> Result<()> {
    match p {
        Picture::Mono(f) => crate::io::save_image_with(path, f, notes),
        Picture::Rgb(i) => crate::io::save_rgb(path, i, notes),
    }
}

/// 8-bit interleaved RGB of a picture (a mono picture is repeated into three
/// channels), samples clamped to `0..=1`.
#[must_use]
pub fn to_rgb8(p: &Picture) -> Vec<u8> {
    let q = |v: f32| {
        if v.is_finite() {
            (v.clamp(0.0, 1.0) * 255.0).round() as u8
        } else {
            0
        }
    };
    match p {
        Picture::Mono(f) => f.data.iter().flat_map(|&v| [q(v); 3]).collect(),
        Picture::Rgb(i) => i.to_u8(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(w: usize, h: usize) -> Frame {
        Frame {
            width: w,
            height: h,
            data: (0..w * h).map(|i| (i % w) as f32 / w as f32).collect(),
            bayer: None,
        }
    }

    #[test]
    fn colour_planes_round_trip() {
        let rgb = Picture::Rgb(RgbImage {
            width: 2,
            height: 1,
            data: vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6],
        });
        let back = from_planes(&planes(&rgb)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back, rgb);
        assert!((luma(&rgb).data[0] - 0.2).abs() < 1e-6);
    }

    #[test]
    fn crop_keeps_the_right_samples() {
        let p = Picture::Mono(ramp(8, 4));
        let c = crop(&p, Rect::new(2, 1, 3, 2)).unwrap_or_else(|e| panic!("{e}"));
        let Picture::Mono(f) = c else { panic!("mono") };
        assert_eq!((f.width, f.height), (3, 2));
        assert!((f.data[0] - 0.25).abs() < 1e-6);
        assert!(crop(&p, Rect::new(6, 0, 4, 2)).is_err());
    }

    #[test]
    fn a_warp_by_the_identity_changes_nothing() {
        let p = Picture::Mono(ramp(16, 8));
        let q = warp(&p, &|x, y| (x, y), Kernel::Lanczos3);
        let (Picture::Mono(a), Picture::Mono(b)) = (&p, &q) else {
            panic!("mono")
        };
        assert!(
            a.data
                .iter()
                .zip(&b.data)
                .all(|(x, y)| (x - y).abs() < 1e-5)
        );
    }

    #[test]
    fn a_shift_of_a_colour_picture_moves_every_plane() {
        let g = ramp(16, 8);
        let rgb = from_planes(&[g.clone(), g.clone(), g]).unwrap_or_else(|e| panic!("{e}"));
        let moved = shift(&rgb, Shift::new(2.0, 0.0), Kernel::Bilinear);
        let Picture::Rgb(i) = moved else {
            panic!("rgb")
        };
        let want = 5.0 / 16.0;
        assert!((i.pixel(3, 4)[1] - want).abs() < 1e-5);
        assert!((i.pixel(3, 4)[0] - want).abs() < 1e-5);
    }
}
