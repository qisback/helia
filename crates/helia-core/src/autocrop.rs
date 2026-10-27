// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Crop a stack to the solar disc plus a margin.
//!
//! The disc is fitted on the stacked image ([`mask::fit_disc`]), which is
//! sharper and less noisy than any single frame. The crop is a square centred
//! on the disc, `1 + margin` radii from the centre to each side, clamped to
//! the frame. A margin leaves room for prominences, which the limb fit does
//! not include. When there is no clear disc, or the crop would not remove
//! anything, the image is left alone and the reason is recorded.

use crate::frame::{Frame, Rect};
use crate::mask::{self, Disc};

/// Default margin: a tenth of the radius beyond the limb. Prominences seldom
/// reach further than 5% of the radius; the rest is room to frame the disc.
pub const DEFAULT_MARGIN: f32 = 0.10;

/// Largest margin accepted (in radii): beyond this the crop is the frame.
pub const MAX_MARGIN: f32 = 2.0;

/// Smallest disc radius worth cropping to, in pixels.
const MIN_RADIUS: f32 = 16.0;

/// What a crop did.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CropReport {
    /// Region kept, in the coordinates of the uncropped image.
    pub rect: Rect,
    /// The fitted disc in the coordinates of the cropped image.
    pub disc: Disc,
    /// Margin asked for, in radii.
    pub margin: f32,
}

/// Why nothing was cropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skipped {
    /// No clear disc to fit (a cropped region, a prominence frame, too little contrast).
    NoDisc,
    /// The disc plus margin already fills the frame.
    FillsFrame,
}

impl Skipped {
    /// A sentence for a log or a notice.
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            Self::NoDisc => "no clear solar disc was found, so the image was not cropped",
            Self::FillsFrame => {
                "the disc and margin already fill the frame, so nothing was cropped"
            }
        }
    }
}

/// The square around `disc` that `margin` asks for, in a `width` x `height`
/// frame. A disc near an edge moves the square inwards rather than shrinking
/// it; a side longer than the frame takes the whole of that side. `None` when
/// that is the whole frame.
#[must_use]
pub fn disc_rect(disc: Disc, margin: f32, width: usize, height: usize) -> Option<Rect> {
    let side = (2.0 * disc.r * (1.0 + margin.clamp(0.0, MAX_MARGIN))).ceil();
    let span = |c: f32, len: usize| -> (usize, usize) {
        if side >= len as f32 {
            (0, len)
        } else {
            let lo = (c - side / 2.0).round().clamp(0.0, len as f32 - side) as usize;
            (lo, lo + side as usize)
        }
    };
    let ((x0, x1), (y0, y1)) = (span(disc.cx, width), span(disc.cy, height));
    let rect = Rect { x0, y0, x1, y1 };
    (!rect.is_empty() && (rect.width() < width || rect.height() < height)).then_some(rect)
}

/// Copy `rect` out of `frame`. `rect` must lie inside the frame.
#[must_use]
pub fn cut(frame: &Frame, rect: Rect) -> Frame {
    let mut data = Vec::with_capacity(rect.area());
    for y in rect.y0..rect.y1 {
        data.extend_from_slice(&frame.row(y)[rect.x0..rect.x1]);
    }
    Frame {
        width: rect.width(),
        height: rect.height(),
        data,
        bayer: None,
    }
}

/// Crop `image` to the disc plus `margin`. `fallback` is a disc from elsewhere
/// (the reference frame) used when the stacked image has none.
///
/// # Errors
/// [`Skipped`] when there is nothing to crop to.
pub fn autocrop(
    image: &Frame,
    margin: f32,
    fallback: Option<Disc>,
) -> Result<(Frame, CropReport), Skipped> {
    let disc = mask::fit_disc(image)
        .or(fallback)
        .filter(|d| d.r >= MIN_RADIUS)
        .ok_or(Skipped::NoDisc)?;
    let rect = disc_rect(disc, margin, image.width, image.height).ok_or(Skipped::FillsFrame)?;
    let moved = Disc {
        cx: disc.cx - rect.x0 as f32,
        cy: disc.cy - rect.y0 as f32,
        r: disc.r,
    };
    Ok((
        cut(image, rect),
        CropReport {
            rect,
            disc: moved,
            margin,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disc_image(w: usize, h: usize, cx: f32, cy: f32, r: f32) -> Frame {
        let mut data = vec![0.02f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let d = (x as f32 - cx).hypot(y as f32 - cy);
                if d < r {
                    // Limb-darkened disc.
                    let mu = (1.0 - (d / r).powi(2)).sqrt();
                    data[y * w + x] = 0.3 + 0.6 * mu;
                }
            }
        }
        Frame::new(w, h, data).expect("frame")
    }

    #[test]
    fn the_rect_is_a_square_around_the_disc() {
        let d = Disc {
            cx: 300.0,
            cy: 200.0,
            r: 100.0,
        };
        let r = disc_rect(d, 0.1, 640, 480).expect("rect");
        assert_eq!((r.x0, r.x1), (190, 410));
        assert_eq!((r.y0, r.y1), (90, 310));
        assert_eq!(r.width(), r.height());
    }

    #[test]
    fn a_disc_near_an_edge_moves_the_square_inwards() {
        let d = Disc {
            cx: 60.0,
            cy: 240.0,
            r: 100.0,
        };
        let r = disc_rect(d, 0.1, 640, 480).expect("rect");
        assert_eq!((r.x0, r.x1), (0, 220), "pushed in from the left edge");
        assert_eq!((r.y0, r.y1), (130, 350));
    }

    #[test]
    fn a_disc_that_fills_the_frame_is_not_cropped() {
        let d = Disc {
            cx: 240.0,
            cy: 240.0,
            r: 240.0,
        };
        assert_eq!(disc_rect(d, 0.1, 480, 480), None);
    }

    #[test]
    fn a_disc_bigger_than_the_frame_keeps_the_long_side_whole() {
        let d = Disc {
            cx: 700.0,
            cy: 640.0,
            r: 660.0,
        };
        let r = disc_rect(d, 0.1, 1920, 1080).expect("rect");
        assert_eq!((r.y0, r.y1), (0, 1080));
        assert!(
            (1452..=1453).contains(&r.width()),
            "2 * 660 * 1.1, rounded up: {}",
            r.width()
        );
    }

    #[test]
    fn autocrop_finds_and_recentres_the_disc() {
        let img = disc_image(480, 360, 210.0, 170.0, 100.0);
        let (out, rep) = autocrop(&img, 0.1, None).expect("crop");
        assert_eq!(out.width, rep.rect.width());
        assert_eq!(out.height, rep.rect.height());
        assert!(out.width <= 230 && out.width >= 210, "{}", out.width);
        let half = out.width as f32 / 2.0;
        assert!((rep.disc.cx - half).abs() < 3.0, "{:?}", rep.disc);
        assert!((rep.disc.cy - out.height as f32 / 2.0).abs() < 3.0);
        assert!((rep.disc.r - 100.0).abs() < 3.0);
        // The pixels are the original ones.
        let (x, y) = (rep.rect.x0 + 7, rep.rect.y0 + 9);
        assert_eq!(out.get(7, 9), img.get(x, y));
    }

    #[test]
    fn a_featureless_image_is_left_alone() {
        let img = Frame::new(200, 200, vec![0.5; 200 * 200]).expect("frame");
        assert_eq!(autocrop(&img, 0.1, None).err(), Some(Skipped::NoDisc));
    }
}
