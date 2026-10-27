// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! True colour from a raw colour camera: three stacked planes made one image.
//!
//! The mono presets stack one plane (a channel or the luminance) and colour it
//! afterwards from a table. This is the other path, for white light taken with
//! a colour camera: the frames are aligned once, on the luminance, and the red,
//! green and blue planes are stacked with that shared alignment (the same
//! global shifts, the same alignment point field, the same frame weights), so
//! the three planes see the same sky. Three things then turn them into a
//! picture:
//!
//! 1. **Channel alignment.** The air disperses colours, so at a low Sun the
//!    red and blue planes sit a fraction of a pixel to either side of the
//!    green. Each is registered to the green by phase correlation of the
//!    stacked planes and moved by that shift. The planes are sharper than any
//!    one frame, so the shift is measured to a small part of a pixel.
//! 2. **White balance.** A raw sensor does not weigh the colours as the eye
//!    does. Gains for red and blue (green is one) are set so the middle of the
//!    disc is neutral (`auto`), left alone (`off`) or given (`R,G,B`).
//! 3. **Scale.** All three planes share one scale, chosen so that the 99.99th
//!    percentile of the brightest channel of each pixel is one: nothing clips,
//!    and the colour balance is the white balance, not the scale.

use crate::align::Shift;
use crate::align::global::{PhaseCorrelator, PhaseOptions};
use crate::align::method::phase_confidence;
use crate::colour::RgbImage;
use crate::error::{Error, Result};
use crate::frame::Frame;
use crate::mask::{self, Disc};
use crate::resample::{Kernel, shifted};

/// Largest channel shift accepted, in pixels. Atmospheric dispersion is a
/// pixel or two at most; more means the correlation locked on something else.
pub const MAX_CHANNEL_SHIFT: f32 = 6.0;

/// Smallest channel shift applied, in pixels: below this it is noise.
pub const MIN_CHANNEL_SHIFT: f32 = 0.05;

/// Largest gain the automatic balance gives a channel. A channel that holds
/// next to nothing (blue behind a sodium filter) would otherwise be amplified
/// into noise.
pub const MAX_AUTO_GAIN: f32 = 4.0;

/// Least confidence (0 to 1, from the peak-to-sidelobe ratio) at which a
/// channel shift is believed and applied.
pub const MIN_SHIFT_CONFIDENCE: f32 = 0.3;

/// Share of the disc radius used to measure the colour balance.
const BALANCE_RADIUS: f32 = 0.8;

/// How the three planes are weighed against each other.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum WhiteBalance {
    /// Make the middle of the disc neutral: gains for red and blue that bring
    /// their mean to the green's.
    #[default]
    Auto,
    /// Leave the sensor's balance alone.
    Off,
    /// Gains for red, green and blue, as given.
    Manual([f32; 3]),
}

impl WhiteBalance {
    /// Short description.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Auto => "automatic (the disc centre is made neutral)".to_owned(),
            Self::Off => "off".to_owned(),
            Self::Manual([r, g, b]) => format!("given: {r:.3}, {g:.3}, {b:.3}"),
        }
    }
}

impl std::str::FromStr for WhiteBalance {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "off" | "none" | "raw" => Ok(Self::Off),
            other => {
                let parts: Vec<&str> = other.split(',').collect();
                let bad = || {
                    Error::Invalid(format!(
                        "white balance {s:?}: use auto, off or three gains such as 1.9,1,1.5"
                    ))
                };
                if parts.len() != 3 {
                    return Err(bad());
                }
                let mut g = [0.0f32; 3];
                for (slot, p) in g.iter_mut().zip(parts) {
                    *slot = p.trim().parse().map_err(|_| bad())?;
                    if !(slot.is_finite() && *slot > 0.0 && *slot <= 100.0) {
                        return Err(bad());
                    }
                }
                Ok(Self::Manual(g))
            }
        }
    }
}

/// What the user asked for when they asked for true colour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrueColour {
    /// The colour balance.
    pub white_balance: WhiteBalance,
    /// Register red and blue to green (atmospheric dispersion).
    pub align_channels: bool,
}

impl Default for TrueColour {
    fn default() -> Self {
        Self {
            white_balance: WhiteBalance::Auto,
            align_channels: true,
        }
    }
}

/// The three planes made one image, and what it took.
#[derive(Clone, Debug, PartialEq)]
pub struct ColourStack {
    /// The picture, balanced and scaled.
    pub rgb: RgbImage,
    /// The gains applied to red, green and blue (including the common scale).
    pub gains: [f32; 3],
    /// How red and blue were registered to green; `None` when that was off.
    pub channels: Option<[ChannelShift; 2]>,
    /// Lines for the history of an output file saying what was done.
    pub notes: Vec<String>,
}

impl ColourStack {
    /// Make `rects` grey (the mean of the three channels). The areas the
    /// pipeline flattens to one level (burned-in text, exclusions) are the same
    /// level in every plane, so the white balance would tint them.
    pub fn neutralise(&mut self, rects: &[crate::frame::Rect]) {
        let w = self.rgb.width;
        for rect in rects {
            let rect = rect.clamp(w, self.rgb.height);
            for y in rect.y0..rect.y1 {
                for x in rect.x0..rect.x1 {
                    let o = 3 * (y * w + x);
                    let m = (self.rgb.data[o] + self.rgb.data[o + 1] + self.rgb.data[o + 2]) / 3.0;
                    self.rgb.data[o..o + 3].fill(m);
                }
            }
        }
    }

    /// The same image cut to `rect`.
    #[must_use]
    pub fn cut(&self, rect: crate::frame::Rect) -> Self {
        let mut data = Vec::with_capacity(rect.area() * 3);
        for y in rect.y0..rect.y1 {
            let row = 3 * (y * self.rgb.width);
            data.extend_from_slice(&self.rgb.data[row + 3 * rect.x0..row + 3 * rect.x1]);
        }
        Self {
            rgb: RgbImage {
                width: rect.width(),
                height: rect.height(),
                data,
            },
            ..self.clone()
        }
    }
}

/// Mean of `plane` inside `radius` of `disc`'s centre.
fn centre_mean(plane: &Frame, disc: Disc, radius: f32) -> f32 {
    let (mut sum, mut n) = (0.0f64, 0usize);
    for y in 0..plane.height {
        let dy = y as f32 - disc.cy;
        for (x, &v) in plane.row(y).iter().enumerate() {
            if (x as f32 - disc.cx).hypot(dy) <= radius {
                sum += f64::from(v);
                n += 1;
            }
        }
    }
    if n == 0 { 0.0 } else { (sum / n as f64) as f32 }
}

/// Gains that make the centre of the disc neutral (green is one), and which
/// of them were held at [`MAX_AUTO_GAIN`].
fn neutral_gains(planes: &[Frame; 3], luminance: &Frame) -> ([f32; 3], [bool; 3]) {
    let disc = mask::fit_disc(luminance).unwrap_or(Disc {
        cx: planes[1].width as f32 / 2.0,
        cy: planes[1].height as f32 / 2.0,
        r: planes[1].width.min(planes[1].height) as f32 / 2.0,
    });
    let m: Vec<f32> = planes
        .iter()
        .map(|p| centre_mean(p, disc, BALANCE_RADIUS * disc.r))
        .collect();
    let want = |c: usize| {
        if m[c] > 1e-6 && m[1] > 1e-6 {
            (m[1] / m[c]).max(0.05)
        } else {
            1.0
        }
    };
    let (gr, gb) = (want(0), want(2));
    (
        [gr.min(MAX_AUTO_GAIN), 1.0, gb.min(MAX_AUTO_GAIN)],
        [gr > MAX_AUTO_GAIN, false, gb > MAX_AUTO_GAIN],
    )
}

/// How a channel shift came out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChannelShift {
    /// The shift measured, pixels (zero when it was not applied).
    pub shift: Shift,
    /// Confidence of the correlation, 0 to 1.
    pub confidence: f32,
    /// The plane was moved.
    pub applied: bool,
}

/// Register `plane` to `green` by phase correlation, when the shift is
/// believable. Returns the plane, moved if the shift was believed.
fn register(corr: &PhaseCorrelator, plane: &Frame) -> Result<(Frame, ChannelShift)> {
    let e = corr.estimate(plane)?;
    let confidence = phase_confidence(e.psr);
    let len = e.shift.length();
    let believed = confidence >= MIN_SHIFT_CONFIDENCE
        && (MIN_CHANNEL_SHIFT..=MAX_CHANNEL_SHIFT).contains(&len);
    if !believed {
        let found = ChannelShift {
            shift: if confidence >= MIN_SHIFT_CONFIDENCE && len < MIN_CHANNEL_SHIFT {
                e.shift
            } else {
                Shift::new(0.0, 0.0)
            },
            confidence,
            applied: false,
        };
        return Ok((plane.clone(), found));
    }
    Ok((
        shifted(plane, e.shift, Kernel::Lanczos3),
        ChannelShift {
            shift: e.shift,
            confidence,
            applied: true,
        },
    ))
}

/// Brightest channel of every pixel, 99.99th percentile.
fn bright_end(planes: &[Frame; 3], gains: [f32; 3]) -> f32 {
    let n = planes[0].data.len();
    let mut v: Vec<f32> = (0..n)
        .map(|i| {
            (0..3)
                .map(|c| planes[c].data[i] * gains[c])
                .fold(0.0f32, f32::max)
        })
        .filter(|x| x.is_finite())
        .collect();
    if v.is_empty() {
        return 1.0;
    }
    let k = (((v.len() as f64) * 0.9999) as usize).min(v.len() - 1);
    *v.select_nth_unstable_by(k, f32::total_cmp).1
}

/// Make the three stacked planes (red, green, blue) one picture.
///
/// `normalise` scales the result as described in the module notes; without it
/// the gains are the white balance alone.
///
/// # Errors
/// [`Error::Invalid`] when the planes differ in size.
pub fn compose(planes: [Frame; 3], tc: &TrueColour, normalise: bool) -> Result<ColourStack> {
    let (w, h) = (planes[1].width, planes[1].height);
    if planes.iter().any(|p| p.width != w || p.height != h) {
        return Err(Error::Invalid("colour planes differ in size".into()));
    }
    let mut planes = planes;
    let mut channels = None;
    if tc.align_channels {
        let corr = PhaseCorrelator::new(&planes[1], PhaseOptions::default())?;
        let (red, sr) = register(&corr, &planes[0])?;
        let (blue, sb) = register(&corr, &planes[2])?;
        planes[0] = red;
        planes[2] = blue;
        channels = Some([sr, sb]);
    }
    let lum = luminance(&planes)?;
    let (mut gains, capped) = match tc.white_balance {
        WhiteBalance::Off => ([1.0; 3], [false; 3]),
        WhiteBalance::Auto => neutral_gains(&planes, &lum),
        WhiteBalance::Manual(g) => (g, [false; 3]),
    };
    if normalise {
        let p = bright_end(&planes, gains);
        if p > 0.0 {
            for g in &mut gains {
                *g /= p;
            }
        }
    }
    let mut data = Vec::with_capacity(w * h * 3);
    for i in 0..w * h {
        for c in 0..3 {
            data.push(planes[c].data[i] * gains[c]);
        }
    }
    let [gr, gg, gb] = gains;
    let mut notes = vec![format!(
        "True colour: red, green and blue stacked on one alignment; white balance {}; gains {gr:.3}, {gg:.3}, {gb:.3}",
        tc.white_balance.describe()
    )];
    for (name, held) in [("red", capped[0]), ("blue", capped[2])] {
        if held {
            notes.push(format!(
                "Warning: the {name} plane holds far less than the green, so its gain was held at {MAX_AUTO_GAIN}; is a narrow-band filter in use? True colour is for white light"
            ));
        }
    }
    if let Some(found) = channels {
        for (name, c) in [("red", found[0]), ("blue", found[1])] {
            if c.applied {
                notes.push(format!(
                    "Channel alignment: {name} moved ({:+.2}, {:+.2}) px onto green (confidence {:.2})",
                    c.shift.dx, c.shift.dy, c.confidence
                ));
            } else {
                notes.push(format!(
                    "Channel alignment: {name} not moved (confidence {:.2}, shift ({:+.2}, {:+.2}) px)",
                    c.confidence, c.shift.dx, c.shift.dy
                ));
            }
        }
    }
    Ok(ColourStack {
        rgb: RgbImage {
            width: w,
            height: h,
            data,
        },
        gains,
        channels,
        notes,
    })
}

/// Mean of the three planes.
///
/// # Errors
/// [`Error::Invalid`] for planes of different sizes.
pub fn luminance(planes: &[Frame; 3]) -> Result<Frame> {
    let data = (0..planes[0].data.len())
        .map(|i| (planes[0].data[i] + planes[1].data[i] + planes[2].data[i]) / 3.0)
        .collect();
    Frame::new(planes[0].width, planes[0].height, data)
}

/// The luminance (mean of red, green and blue) of a colour image.
///
/// # Errors
/// Never for a well-formed image.
pub fn rgb_luminance(img: &RgbImage) -> Result<Frame> {
    let data = img
        .data
        .as_chunks::<3>()
        .0
        .iter()
        .map(|p| (p[0] + p[1] + p[2]) / 3.0)
        .collect();
    Frame::new(img.width, img.height, data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Rect;

    /// A disc of radius 60 about (100, 80) with a texture, scaled by `scale`
    /// and moved by `shift`.
    fn plane(scale: f32, shift: Shift) -> Frame {
        let (w, h) = (200usize, 160usize);
        let mut data = vec![0.01f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let (sx, sy) = (x as f32 - shift.dx, y as f32 - shift.dy);
                let d = (sx - 100.0).hypot(sy - 80.0);
                if d < 60.0 {
                    let mu = (1.0 - (d / 60.0).powi(2)).sqrt();
                    let tex = 0.05 * (sx * 0.9).sin() * (sy * 1.1).cos();
                    data[y * w + x] = scale * (0.4 + 0.5 * mu + tex);
                }
            }
        }
        Frame::new(w, h, data).expect("frame")
    }

    fn mean_disc(img: &RgbImage) -> [f32; 3] {
        let mut sum = [0.0f64; 3];
        let mut n = 0;
        for y in 60..100 {
            for x in 80..120 {
                let p = img.pixel(x, y);
                for c in 0..3 {
                    sum[c] += f64::from(p[c]);
                }
                n += 1;
            }
        }
        sum.map(|s| (s / f64::from(n)) as f32)
    }

    #[test]
    fn the_balance_parses() {
        assert_eq!(
            "auto".parse::<WhiteBalance>().ok(),
            Some(WhiteBalance::Auto)
        );
        assert_eq!("Off".parse::<WhiteBalance>().ok(), Some(WhiteBalance::Off));
        assert_eq!(
            "1.9, 1, 1.5".parse::<WhiteBalance>().ok(),
            Some(WhiteBalance::Manual([1.9, 1.0, 1.5]))
        );
        for bad in ["", "1,2", "1,2,x", "0,1,1", "-1,1,1", "1,1,1,1", "grey"] {
            assert!(bad.parse::<WhiteBalance>().is_err(), "{bad}");
        }
    }

    #[test]
    fn automatic_balance_makes_the_disc_neutral() {
        // A sensor that sees red at 0.5, green at 1 and blue at 0.7 of a grey disc.
        let planes = [
            plane(0.5, Shift::new(0.0, 0.0)),
            plane(1.0, Shift::new(0.0, 0.0)),
            plane(0.7, Shift::new(0.0, 0.0)),
        ];
        let tc = TrueColour {
            align_channels: false,
            ..TrueColour::default()
        };
        let out = compose(planes.clone(), &tc, true).expect("compose");
        let m = mean_disc(&out.rgb);
        assert!(
            (m[0] / m[1] - 1.0).abs() < 0.01 && (m[2] / m[1] - 1.0).abs() < 0.01,
            "{m:?}"
        );
        assert!(
            (out.gains[0] / out.gains[1] - 2.0).abs() < 0.05,
            "{:?}",
            out.gains
        );
        assert!((out.gains[2] / out.gains[1] - 1.0 / 0.7).abs() < 0.05);
        // Off leaves the sensor's balance: the ratios are the scales.
        let off = compose(
            planes,
            &TrueColour {
                white_balance: WhiteBalance::Off,
                align_channels: false,
            },
            false,
        )
        .expect("compose");
        let m = mean_disc(&off.rgb);
        assert!(
            (m[0] / m[1] - 0.5).abs() < 0.01 && (m[2] / m[1] - 0.7).abs() < 0.01,
            "{m:?}"
        );
        assert_eq!(off.gains, [1.0; 3]);
    }

    #[test]
    fn manual_gains_are_applied_as_given() {
        let planes = [
            plane(1.0, Shift::new(0.0, 0.0)),
            plane(1.0, Shift::new(0.0, 0.0)),
            plane(1.0, Shift::new(0.0, 0.0)),
        ];
        let tc = TrueColour {
            white_balance: WhiteBalance::Manual([2.0, 1.0, 0.5]),
            align_channels: false,
        };
        let out = compose(planes, &tc, false).expect("compose");
        let m = mean_disc(&out.rgb);
        assert!(
            (m[0] / m[1] - 2.0).abs() < 1e-4 && (m[2] / m[1] - 0.5).abs() < 1e-4,
            "{m:?}"
        );
    }

    #[test]
    fn the_scale_keeps_every_channel_inside_white() {
        let planes = [
            plane(1.6, Shift::new(0.0, 0.0)),
            plane(1.0, Shift::new(0.0, 0.0)),
            plane(0.8, Shift::new(0.0, 0.0)),
        ];
        let out = compose(planes, &TrueColour::default(), true).expect("compose");
        let (mut over, mut max) = (0usize, 0.0f32);
        for &v in &out.rgb.data {
            max = max.max(v);
            if v > 1.0 {
                over += 1;
            }
        }
        // The 99.99th percentile of the brightest channel is one.
        assert!(
            over as f32 / out.rgb.data.len() as f32 <= 1e-3,
            "{over} samples over 1 (max {max})"
        );
        assert!(max < 1.2, "{max}");
    }

    #[test]
    fn dispersion_is_measured_and_removed() {
        // Red a little up and to the left of green, blue down and to the right.
        let sr = Shift::new(-0.7, -0.4);
        let sb = Shift::new(0.5, 0.9);
        let planes = [
            plane(1.0, sr),
            plane(1.0, Shift::new(0.0, 0.0)),
            plane(1.0, sb),
        ];
        let green = planes[1].clone();
        let tc = TrueColour {
            white_balance: WhiteBalance::Off,
            align_channels: true,
        };
        let out = compose(planes.clone(), &tc, false).expect("compose");
        let [mr, mb] = out.channels.expect("channels");
        assert!(
            mr.applied && mb.applied && mr.confidence > 0.5,
            "{mr:?} {mb:?}"
        );
        let (mr, mb) = (mr.shift, mb.shift);
        assert!(
            (mr.dx - sr.dx).abs() < 0.1 && (mr.dy - sr.dy).abs() < 0.1,
            "{mr:?}"
        );
        assert!(
            (mb.dx - sb.dx).abs() < 0.1 && (mb.dy - sb.dy).abs() < 0.1,
            "{mb:?}"
        );
        // After the move each plane agrees with green far better than before.
        let rms = |c: usize, img: &RgbImage| {
            let mut s = 0.0f64;
            for y in 30..130 {
                for x in 50..150 {
                    s += f64::from((img.pixel(x, y)[c] - green.get(x, y)).powi(2));
                }
            }
            (s / 10_000.0).sqrt()
        };
        let before = RgbImage {
            width: 200,
            height: 160,
            data: (0..200 * 160)
                .flat_map(|i| [planes[0].data[i], planes[1].data[i], planes[2].data[i]])
                .collect(),
        };
        for c in [0, 2] {
            let (a, b) = (rms(c, &before), rms(c, &out.rgb));
            assert!(b < 0.5 * a, "channel {c}: {a} to {b}");
        }
        // Without the option nothing moves.
        let still = compose(
            planes,
            &TrueColour {
                align_channels: false,
                ..tc
            },
            false,
        )
        .expect("compose");
        assert!(still.channels.is_none());
        assert!(rms(0, &still.rgb) > 0.01);
    }

    #[test]
    fn identical_planes_are_not_moved() {
        let p = plane(1.0, Shift::new(0.0, 0.0));
        let out =
            compose([p.clone(), p.clone(), p], &TrueColour::default(), true).expect("compose");
        let [sr, sb] = out.channels.expect("channels");
        assert!(sr.shift.length() < 1e-3 && sb.shift.length() < 1e-3);
        assert!(!sr.applied && !sb.applied);
    }

    #[test]
    fn flattened_areas_are_made_grey() {
        let planes = [
            plane(0.5, Shift::new(0.0, 0.0)),
            plane(1.0, Shift::new(0.0, 0.0)),
            plane(0.7, Shift::new(0.0, 0.0)),
        ];
        let tc = TrueColour {
            white_balance: WhiteBalance::Off,
            align_channels: false,
        };
        let mut out = compose(planes, &tc, false).expect("compose");
        let before = out.rgb.pixel(100, 80);
        let elsewhere = out.rgb.pixel(10, 10);
        assert!((before[0] - before[1]).abs() > 0.1);
        out.neutralise(&[Rect::new(95, 75, 10, 10), Rect::new(190, 150, 100, 100)]);
        let p = out.rgb.pixel(100, 80);
        assert!(
            (p[0] - p[1]).abs() < 1e-7 && (p[1] - p[2]).abs() < 1e-7,
            "{p:?}"
        );
        assert!((p[0] - (before[0] + before[1] + before[2]) / 3.0).abs() < 1e-6);
        // Elsewhere is untouched.
        assert_eq!(out.rgb.pixel(10, 10), elsewhere);
    }

    #[test]
    fn a_cut_keeps_the_pixels_and_the_record() {
        let p = plane(1.0, Shift::new(0.0, 0.0));
        let out =
            compose([p.clone(), p.clone(), p], &TrueColour::default(), true).expect("compose");
        let rect = Rect::new(10, 20, 50, 40);
        let cut = out.cut(rect);
        assert_eq!((cut.rgb.width, cut.rgb.height), (50, 40));
        assert_eq!(cut.rgb.pixel(3, 4), out.rgb.pixel(13, 24));
        assert_eq!(cut.gains, out.gains);
    }

    #[test]
    fn planes_of_different_sizes_are_refused() {
        let a = plane(1.0, Shift::new(0.0, 0.0));
        let b = Frame::zeros(10, 10);
        assert!(compose([a.clone(), a, b], &TrueColour::default(), true).is_err());
    }

    #[test]
    fn notes_say_what_was_done() {
        let p = plane(1.0, Shift::new(0.0, 0.0));
        let tc = TrueColour::default();
        let out = compose([p.clone(), p.clone(), p], &tc, true).expect("compose");
        let n = &out.notes;
        assert!(n[0].contains("white balance automatic") && n[1].starts_with("Channel alignment"));
        assert!(n.iter().all(|l| l.is_ascii()));
    }

    #[test]
    fn a_channel_with_no_signal_is_not_amplified_into_noise() {
        // Blue sees one hundredth of the green, as behind a sodium filter.
        let planes = [
            plane(1.0, Shift::new(0.0, 0.0)),
            plane(1.0, Shift::new(0.0, 0.0)),
            plane(0.01, Shift::new(0.0, 0.0)),
        ];
        let tc = TrueColour {
            align_channels: false,
            ..TrueColour::default()
        };
        let out = compose(planes, &tc, false).expect("compose");
        assert!(
            (out.gains[2] - MAX_AUTO_GAIN).abs() < 1e-5,
            "{:?}",
            out.gains
        );
        assert!(
            out.notes
                .iter()
                .any(|l| l.contains("blue plane holds far less"))
        );
        // A balanced sensor says nothing of the kind.
        let p = plane(1.0, Shift::new(0.0, 0.0));
        let ok = compose([p.clone(), p.clone(), p], &tc, false).expect("compose");
        assert!(ok.notes.iter().all(|l| !l.starts_with("Warning")));
    }

    #[test]
    fn a_shift_that_does_not_correlate_is_not_applied() {
        // Red is unrelated to green: noise, not a displaced copy.
        let mut rng = crate::testutil::Rng::new(9);
        let mut noise = plane(1.0, Shift::new(0.0, 0.0));
        for v in &mut noise.data {
            *v = 0.5 + 0.2 * rng.gauss();
        }
        let planes = [
            noise,
            plane(1.0, Shift::new(0.0, 0.0)),
            plane(1.0, Shift::new(0.4, 0.3)),
        ];
        let out = compose(planes, &TrueColour::default(), false).expect("compose");
        let [red, blue] = out.channels.expect("channels");
        assert!(!red.applied && red.shift.length() == 0.0, "{red:?}");
        assert!(blue.applied, "{blue:?}");
        assert!(out.notes.iter().any(|l| l.contains("red not moved")));
    }
}
