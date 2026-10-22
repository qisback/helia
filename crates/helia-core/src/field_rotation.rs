// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Field rotation of an altazimuth mount, and its correction.
//!
//! On an altazimuth mount the camera keeps its orientation to the horizon while
//! the sky turns about the optical axis: the picture of the Sun rotates, at a rate
//! that depends on where the Sun is in the sky, by `15 degrees per hour * cos(lat) *
//! cos(azimuth) / cos(altitude)`. A capture of a few minutes can turn by a degree
//! or more, which at the limb of a disc 400 pixels in radius is a displacement of
//! seven pixels or more: more than the alignment points can follow, and a
//! time-lapse of stacks taken hours apart shows the surface turning in the frame.
//!
//! The angle is the **parallactic angle** `q`, the angle at the Sun between the
//! direction to the north celestial pole and the direction to the zenith:
//!
//! ```text
//! H = LST - RA                         (hour angle; LST from the time and longitude)
//! tan q = sin H / (tan(lat) cos(dec) - sin(dec) cos H)
//! ```
//!
//! (Meeus, Astronomical Algorithms, equation 14.1.) `q` is positive when the Sun is
//! west of the meridian, and then celestial north lies **clockwise** of the zenith
//! direction in a picture that is not a mirror image. A frame taken at `q` is
//! brought to the orientation of a frame taken at `q_ref` by turning it
//! **counter-clockwise** by `q - q_ref` (clockwise in a mirror image, which an
//! odd number of reflections makes: a star diagonal, for one). The Sun's right
//! ascension and declination are those of [`crate::ephemeris`].
//!
//! The turn is about the centre of the frame, which is the axis the mount points
//! at, and uses Lanczos interpolation. Whatever shift it leaves in the disc (the
//! disc is not exactly on the axis) is for the alignment that follows.
//!
//! An equatorial mount has no field rotation: do not use this with one.
//!
//! # References
//!
//! - Meeus, J. (1998), Astronomical Algorithms, 2nd ed., Willmann-Bell, chapter 14 (the parallactic angle).

#![allow(
    clippy::cast_possible_wrap,
    clippy::many_single_char_names,
    reason = "pixel coordinates are far below i64::MAX; image maths uses w, h, x, y"
)]

use rayon::prelude::*;

use crate::debayer::Debayer;
use crate::ephemeris::{julian_date, solar_ephemeris};
use crate::error::{Error, Result};
use crate::frame::Frame;
use crate::io::raw::SharedFrames;
use crate::io::solarnet::Site;
use crate::io::source::FrameSource;
use crate::resample::{Kernel, sample};

/// Greenwich mean sidereal time, degrees, at `unix` seconds UTC.
#[must_use]
pub fn gmst_deg(unix: f64) -> f64 {
    let jd = julian_date(unix);
    let t = (jd - 2_451_545.0) / 36_525.0;
    (280.460_618_37 + 360.985_647_366_29 * (jd - 2_451_545.0) + 0.000_387_933 * t * t
        - t * t * t / 38_710_000.0)
        .rem_euclid(360.0)
}

/// Parallactic angle in degrees (`-180..=180`) of an object at right ascension
/// `ra_deg` and declination `dec_deg`, seen from latitude `lat_deg` and east
/// longitude `lon_deg` at `unix` seconds UTC. Positive west of the meridian.
#[must_use]
pub fn parallactic_angle_deg(
    lat_deg: f64,
    lon_deg: f64,
    unix: f64,
    ra_deg: f64,
    dec_deg: f64,
) -> f64 {
    let h = (gmst_deg(unix) + lon_deg - ra_deg).to_radians();
    let (lat, dec) = (lat_deg.to_radians(), dec_deg.to_radians());
    let y = h.sin();
    let x = lat.tan() * dec.cos() - dec.sin() * h.cos();
    y.atan2(x).to_degrees()
}

/// Parallactic angle of the Sun, degrees, for an observer at `site`.
#[must_use]
pub fn sun_parallactic_angle_deg(site: &Site, unix: f64) -> f64 {
    let e = solar_ephemeris(unix);
    parallactic_angle_deg(site.lat_deg, site.lon_deg, unix, e.ra_deg, e.dec_deg)
}

/// Altitude of the Sun above the horizon, degrees (no refraction), for `site`.
#[must_use]
pub fn sun_altitude_deg(site: &Site, unix: f64) -> f64 {
    let e = solar_ephemeris(unix);
    let h = (gmst_deg(unix) + site.lon_deg - e.ra_deg).to_radians();
    let (lat, dec) = (site.lat_deg.to_radians(), e.dec_deg.to_radians());
    (lat.sin() * dec.sin() + lat.cos() * dec.cos() * h.cos())
        .asin()
        .to_degrees()
}

/// Which frame's orientation the others are turned to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Reference {
    /// The middle of the capture: the largest turn is half the span.
    #[default]
    Middle,
    /// The first frame.
    First,
    /// The last frame.
    Last,
}

impl Reference {
    /// Name used on the command line and in the API.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Middle => "middle",
            Self::First => "first",
            Self::Last => "last",
        }
    }
}

impl std::str::FromStr for Reference {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "middle" | "mid" | "centre" | "center" => Ok(Self::Middle),
            "first" | "start" => Ok(Self::First),
            "last" | "end" => Ok(Self::Last),
            _ => Err(Error::Invalid(format!(
                "unknown field rotation reference {s:?}; use middle, first or last"
            ))),
        }
    }
}

/// How to undo the field rotation of a capture.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldRotation {
    /// Where the telescope stands.
    pub site: Site,
    /// The picture is a mirror image (an odd number of reflections).
    pub mirror: bool,
    /// The orientation to turn every frame to.
    pub reference: Reference,
    /// When each frame began, unix seconds UTC; the capture's timestamps. A
    /// frame without one takes the nearest stamp.
    pub frame_unix: Vec<Option<f64>>,
}

/// What the correction does to a capture.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    /// Degrees to turn each frame counter-clockwise (clockwise when the picture
    /// is a mirror image is already folded in: this is the angle to apply).
    pub turn_deg: Vec<f64>,
    /// Parallactic angle of each frame, degrees.
    pub q_deg: Vec<f64>,
    /// Time of the reference orientation, unix seconds UTC.
    pub reference_unix: f64,
    /// Parallactic angle of the reference, degrees.
    pub reference_q_deg: f64,
    /// Largest turn applied, degrees.
    pub max_turn_deg: f64,
    /// Parallactic angle from the first stamp to the last, degrees.
    pub span_deg: f64,
    /// Mean rate of rotation, degrees per minute.
    pub rate_deg_per_min: f64,
    /// Altitude of the Sun at the reference, degrees.
    pub altitude_deg: f64,
    /// Frames with a timestamp of their own.
    pub stamped: usize,
}

impl Plan {
    /// Displacement in pixels, at the limb of a disc of `radius` pixels, of the
    /// frame that turns the most.
    #[must_use]
    pub fn limb_shift_px(&self, radius: f32) -> f32 {
        (self.max_turn_deg.to_radians() as f32 * radius).abs()
    }

    /// The correction in lines for a log.
    #[must_use]
    pub fn lines(&self, radius: Option<f32>) -> Vec<String> {
        let mut v = vec![format!(
            "the parallactic angle runs {:.2} degrees over the capture ({:.3} degrees per minute, Sun at {:.0} degrees altitude); frames are turned by up to {:.2} degrees to the orientation at {}",
            self.span_deg,
            self.rate_deg_per_min,
            self.altitude_deg,
            self.max_turn_deg,
            crate::io::solarnet::iso_from_unix(self.reference_unix)
        )];
        if let Some(r) = radius {
            v.push(format!(
                "at the limb that is up to {:.1} pixels",
                self.limb_shift_px(r)
            ));
        }
        v
    }
}

impl FieldRotation {
    /// Work out the turn of every frame.
    ///
    /// # Errors
    /// [`Error::Invalid`] when no frame has a timestamp, or the stamps are all
    /// the same moment (there is nothing to correct).
    pub fn plan(&self) -> Result<Plan> {
        let stamps: Vec<(usize, f64)> = self
            .frame_unix
            .iter()
            .enumerate()
            .filter_map(|(i, t)| t.map(|t| (i, t)))
            .collect();
        if stamps.is_empty() {
            return Err(Error::Invalid(
                "field rotation needs the time of each frame, and this capture has none (SER files carry timestamps; AVI and video do not)".into(),
            ));
        }
        // A frame with no stamp takes the nearest one before it, else after.
        let mut times = Vec::with_capacity(self.frame_unix.len());
        for i in 0..self.frame_unix.len() {
            let t = self.frame_unix[i]
                .or_else(|| stamps.iter().rev().find(|s| s.0 < i).map(|s| s.1))
                .or_else(|| stamps.iter().find(|s| s.0 > i).map(|s| s.1));
            times.push(t.unwrap_or(stamps[0].1));
        }
        let q: Vec<f64> = times
            .iter()
            .map(|&t| sun_parallactic_angle_deg(&self.site, t))
            .collect();
        // Unwrap, so that a capture across the +-180 degree jump is one run.
        let mut run = q.clone();
        for i in 1..run.len() {
            let mut d = run[i] - run[i - 1];
            while d > 180.0 {
                d -= 360.0;
            }
            while d < -180.0 {
                d += 360.0;
            }
            run[i] = run[i - 1] + d;
        }
        let ref_index = match self.reference {
            Reference::First => 0,
            Reference::Last => run.len() - 1,
            Reference::Middle => {
                let (lo, hi) = (
                    run.iter().copied().fold(f64::INFINITY, f64::min),
                    run.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                );
                let mid = f64::midpoint(lo, hi);
                (0..run.len())
                    .min_by(|&a, &b| (run[a] - mid).abs().total_cmp(&(run[b] - mid).abs()))
                    .unwrap_or(0)
            }
        };
        let sign = if self.mirror { -1.0 } else { 1.0 };
        let turn: Vec<f64> = run.iter().map(|&r| sign * (r - run[ref_index])).collect();
        let first = *times.first().unwrap_or(&0.0);
        let last = *times.last().unwrap_or(&0.0);
        let span = run.last().unwrap_or(&0.0) - run.first().unwrap_or(&0.0);
        let minutes = (last - first) / 60.0;
        Ok(Plan {
            max_turn_deg: turn.iter().map(|t| t.abs()).fold(0.0, f64::max),
            turn_deg: turn,
            q_deg: q,
            reference_unix: times[ref_index],
            reference_q_deg: run[ref_index],
            span_deg: span,
            rate_deg_per_min: if minutes.abs() > 1e-9 {
                span / minutes
            } else {
                0.0
            },
            altitude_deg: sun_altitude_deg(&self.site, times[ref_index]),
            stamped: stamps.len(),
        })
    }
}

/// The median of the outer ring of `f`, the level that fills what a turn
/// uncovers.
fn border_level(frame: &Frame) -> f32 {
    let (w, h) = (frame.width, frame.height);
    let mut v: Vec<f32> = (0..w)
        .flat_map(|x| [frame.get(x, 0), frame.get(x, h - 1)])
        .chain((0..h).flat_map(|y| [frame.get(0, y), frame.get(w - 1, y)]))
        .filter(|v| v.is_finite())
        .collect();
    if v.is_empty() {
        return 0.0;
    }
    let k = v.len() / 2;
    *v.select_nth_unstable_by(k, f32::total_cmp).1
}

/// `frame` turned counter-clockwise by `deg` about its centre, the same size,
/// with what comes in from outside the frame filled with the level of its edge.
#[must_use]
pub fn rotate_ccw(frame: &Frame, deg: f64, kernel: Kernel) -> Frame {
    if deg.abs() < 1e-9 {
        return frame.clone();
    }
    let (w, h) = (frame.width, frame.height);
    let (s, c) = deg.to_radians().sin_cos();
    let (s, c) = (s as f32, c as f32);
    let fill = border_level(frame);
    let (cx, cy) = ((w as f32 - 1.0) / 2.0, (h as f32 - 1.0) / 2.0);
    let mut out = frame.clone();
    out.bayer = None;
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, v) in row.iter_mut().enumerate() {
            let (dx, dy) = (x as f32 - cx, y as f32 - cy);
            // The source of an output pixel is a clockwise turn of its offset.
            let sx = c * dx - s * dy + cx;
            let sy = s * dx + c * dy + cy;
            *v = if sx >= -0.5 && sx <= w as f32 - 0.5 && sy >= -0.5 && sy <= h as f32 - 0.5 {
                sample(frame, sx, sy, kernel)
            } else {
                fill
            };
        }
    });
    out
}

/// A capture with every frame turned to one orientation, in everything it
/// serves through [`FrameSource::read`]. It cannot be read from several threads
/// at once (a frame is resampled, not just decoded), so [`FrameSource::shared`]
/// is `None`.
pub struct DerotatedSource<'a> {
    inner: &'a mut dyn FrameSource,
    turn_deg: Vec<f64>,
    debayer: Debayer,
    kernel: Kernel,
}

impl<'a> DerotatedSource<'a> {
    /// Wrap `inner`, turning frame `i` by `plan.turn_deg[i]`. A raw colour frame
    /// is demosaiced with `debayer` first (a mosaic cannot be turned).
    ///
    /// # Errors
    /// [`Error::Invalid`] when the plan is for another number of frames.
    pub fn new(inner: &'a mut dyn FrameSource, plan: &Plan, debayer: Debayer) -> Result<Self> {
        if plan.turn_deg.len() != inner.frame_count() {
            return Err(Error::Invalid(format!(
                "the field rotation plan is for {} frames but the capture has {}",
                plan.turn_deg.len(),
                inner.frame_count()
            )));
        }
        Ok(Self {
            inner,
            turn_deg: plan.turn_deg.clone(),
            debayer,
            kernel: Kernel::Lanczos3,
        })
    }
}

impl FrameSource for DerotatedSource<'_> {
    fn dimensions(&self) -> (usize, usize) {
        self.inner.dimensions()
    }
    fn frame_count(&self) -> usize {
        self.inner.frame_count()
    }
    fn read(&mut self, index: usize) -> Result<Frame> {
        let f = self.debayer.apply_owned(self.inner.read(index)?);
        Ok(rotate_ccw(&f, self.turn_deg[index], self.kernel))
    }
    fn describe(&self) -> String {
        format!("{} (field rotation corrected)", self.inner.describe())
    }
    fn shared(&self) -> Option<&dyn SharedFrames> {
        None
    }
    fn quality_note(&self) -> Option<String> {
        self.inner.quality_note()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use super::*;
    use crate::io::source::MemorySource;
    use crate::pipeline::{Pipeline, StackConfig};
    use crate::quality::{Metric, score};
    use crate::testutil::{SyntheticConfig, SyntheticSun};

    const SITE: Site = Site {
        lat_deg: 51.5,
        lon_deg: 0.0,
        alt_m: 30.0,
    };

    /// 2026-03-20 12:00 UTC, near the equinox: the Sun is at declination 0.
    const NOON: f64 = 1_774_008_000.0;

    #[test]
    fn the_parallactic_angle_has_the_textbook_values() {
        // On the meridian it is zero; a star on the equator in the west at 6 h
        // from a latitude of 45 degrees is at 45 degrees; in the east, -45.
        assert!(parallactic_angle_deg(45.0, 0.0, 0.0, gmst_deg(0.0), 0.0).abs() < 1e-9);
        let west = parallactic_angle_deg(45.0, 0.0, 0.0, gmst_deg(0.0) - 90.0, 0.0);
        assert!((west - 45.0).abs() < 1e-6, "{west}");
        let east = parallactic_angle_deg(45.0, 0.0, 0.0, gmst_deg(0.0) + 90.0, 0.0);
        assert!((east + 45.0).abs() < 1e-6, "{east}");
        // Southern hemisphere, west: the north pole is below the horizon, 135.
        let south = parallactic_angle_deg(-45.0, 0.0, 0.0, gmst_deg(0.0) - 90.0, 0.0);
        assert!((south - 135.0).abs() < 1e-6, "{south}");
    }

    #[test]
    fn sidereal_time_matches_meeus_example_12a() {
        // 1987 April 10, 0h UT: GMST 13h10m46.3668s = 197.693195 degrees.
        let unix = (2_446_895.5 - 2_440_587.5) * 86_400.0;
        assert!(
            (gmst_deg(unix) - 197.693_195).abs() < 1e-4,
            "{}",
            gmst_deg(unix)
        );
    }

    #[test]
    fn the_rate_at_noon_follows_the_formula() {
        // 15 deg/h * cos(lat) * |cos(azimuth)| / cos(altitude), at transit.
        let step = 60.0;
        let q0 = sun_parallactic_angle_deg(&SITE, NOON - step);
        let q1 = sun_parallactic_angle_deg(&SITE, NOON + step);
        let rate = (q1 - q0) / 2.0; // degrees per minute
        let alt = sun_altitude_deg(&SITE, NOON).to_radians();
        let want = 15.0 / 60.0 * SITE.lat_deg.to_radians().cos() / alt.cos();
        assert!(rate > 0.0, "the angle grows after the meridian: {rate}");
        assert!((rate - want).abs() < 0.01, "{rate} against {want}");
        assert!((sun_altitude_deg(&SITE, NOON) - 38.5).abs() < 0.6);
    }

    fn rotation(frames: usize, spacing_s: f64, mirror: bool) -> FieldRotation {
        FieldRotation {
            site: SITE,
            mirror,
            reference: Reference::Middle,
            frame_unix: (0..frames)
                .map(|i| Some(NOON - 1800.0 + i as f64 * spacing_s))
                .collect(),
        }
    }

    #[test]
    fn the_plan_turns_every_frame_to_the_middle_one() {
        let p = rotation(31, 120.0, false).plan().unwrap();
        // An hour at 0.2 degrees a minute.
        assert!((p.span_deg - 12.0).abs() < 0.4, "{}", p.span_deg);
        assert!(p.turn_deg[15].abs() < 1e-9);
        // A frame before the reference turns the other way from one after.
        assert!(p.turn_deg[0] < 0.0 && p.turn_deg[30] > 0.0);
        assert!((p.max_turn_deg - p.span_deg / 2.0).abs() < 0.1);
        // A mirror image turns the other way.
        let m = rotation(31, 120.0, true).plan().unwrap();
        assert!((m.turn_deg[0] + p.turn_deg[0]).abs() < 1e-9);
        // First and last references.
        let mut r = rotation(31, 120.0, false);
        r.reference = Reference::First;
        assert!(r.plan().unwrap().turn_deg[0].abs() < 1e-9);
        r.reference = Reference::Last;
        assert!(r.plan().unwrap().turn_deg[30].abs() < 1e-9);
        assert!(p.limb_shift_px(400.0) > 40.0);
    }

    #[test]
    fn a_plan_needs_timestamps_and_fills_gaps() {
        let mut r = rotation(5, 60.0, false);
        r.frame_unix = vec![None; 5];
        assert!(r.plan().is_err());
        let mut r = rotation(5, 60.0, false);
        r.frame_unix[2] = None;
        let p = r.plan().unwrap();
        assert_eq!(p.stamped, 4);
        assert_eq!(p.turn_deg.len(), 5);
    }

    #[test]
    fn a_capture_across_the_jump_at_180_degrees_is_one_run() {
        // The Sun on the meridian at the pole of the sky has q passing through 180;
        // here, just check that unwrapping keeps the span small.
        let mut r = rotation(10, 30.0, false);
        r.site.lat_deg = 85.0;
        let p = r.plan().unwrap();
        assert!(p.span_deg.abs() < 60.0, "{}", p.span_deg);
    }

    fn image() -> Frame {
        let mut data = vec![0.1f32; 101 * 101];
        // A bright block right of centre.
        for y in 48..53 {
            for x in 80..85 {
                data[y * 101 + x] = 1.0;
            }
        }
        Frame::new(101, 101, data).unwrap()
    }

    #[test]
    fn a_counter_clockwise_turn_takes_the_right_to_the_top() {
        let f = image();
        let r = rotate_ccw(&f, 90.0, Kernel::Lanczos3);
        // The block was 31 px right of the centre; now it is 31 px above it.
        assert!(r.get(50, 19) > 0.9, "{}", r.get(50, 19));
        assert!(r.get(81, 50) < 0.2);
        // And a clockwise one (negative) takes it to the bottom.
        let r = rotate_ccw(&f, -90.0, Kernel::Lanczos3);
        assert!(r.get(50, 81) > 0.9);
    }

    #[test]
    fn a_turn_and_its_inverse_give_the_picture_back() {
        let sun = SyntheticSun::new(&SyntheticConfig {
            width: 128,
            height: 128,
            ..SyntheticConfig::default()
        });
        let truth = sun.truth().clone();
        let back = rotate_ccw(
            &rotate_ccw(&truth, 7.3, Kernel::Lanczos3),
            -7.3,
            Kernel::Lanczos3,
        );
        // Away from the corners, which the first turn cut.
        let mut worst = 0.0f32;
        for y in 24..104 {
            for x in 24..104 {
                worst = worst.max((back.get(x, y) - truth.get(x, y)).abs());
            }
        }
        assert!(worst < 0.04, "{worst}");
    }

    #[test]
    fn the_source_serves_frames_in_one_orientation() {
        let sun = SyntheticSun::new(&SyntheticConfig {
            width: 128,
            height: 128,
            ..SyntheticConfig::default()
        });
        let truth = sun.truth().clone();
        let r = rotation(9, 360.0, false);
        let plan = r.plan().unwrap();
        // Frames as the camera saw them: the sky turned the other way by the
        // parallactic angle (clockwise by the angle itself, counter-clockwise
        // by minus it).
        let reference = plan.reference_q_deg;
        let frames: Vec<Frame> = plan
            .q_deg
            .iter()
            .map(|&q| rotate_ccw(&truth, -(q - reference), Kernel::Lanczos3))
            .collect();
        let mut raw = MemorySource(frames.clone());
        let mut source = DerotatedSource::new(&mut raw, &plan, Debayer::default()).unwrap();
        assert!(source.shared().is_none());
        let mut worst_before = 0.0f32;
        let mut worst_after = 0.0f32;
        for (i, f) in frames.iter().enumerate() {
            let fixed = source.read(i).unwrap();
            for y in 32..96 {
                for x in 32..96 {
                    worst_before = worst_before.max((f.get(x, y) - truth.get(x, y)).abs());
                    worst_after = worst_after.max((fixed.get(x, y) - truth.get(x, y)).abs());
                }
            }
        }
        assert!(worst_after < 0.06, "{worst_after}");
        assert!(
            worst_before > 3.0 * worst_after,
            "{worst_before} against {worst_after}"
        );
        // A plan for another number of frames is refused.
        let mut other = MemorySource(frames[..3].to_vec());
        assert!(DerotatedSource::new(&mut other, &plan, Debayer::default()).is_err());
    }

    /// A capture of the synthetic Sun on an altazimuth mount, one frame every
    /// two and a half minutes for an hour, noiseless, with its stamps.
    fn rotating_capture() -> (Vec<Frame>, FieldRotation, Frame) {
        let sun = SyntheticSun::new(&SyntheticConfig::default());
        let truth = sun.truth().clone();
        let r = rotation(25, 150.0, false);
        let plan = r.plan().unwrap();
        let frames = plan
            .q_deg
            .iter()
            .map(|&q| rotate_ccw(&truth, -(q - plan.reference_q_deg), Kernel::Lanczos3))
            .collect();
        (frames, r, truth)
    }

    fn stack(
        frames: Vec<Frame>,
        field_rotation: Option<FieldRotation>,
    ) -> crate::pipeline::StackOutput {
        let cfg = StackConfig {
            keep: crate::Keep::Fraction(0.6),
            normalise: false,
            multipoint: None,
            field_rotation,
            ..StackConfig::default()
        };
        Pipeline::new(cfg)
            .run(
                &mut MemorySource(frames),
                &|_, _| {},
                &std::sync::atomic::AtomicBool::new(false),
            )
            .unwrap()
    }

    #[test]
    fn the_stack_of_a_rotating_capture_is_sharper_and_closer_to_the_truth_when_corrected() {
        let (frames, r, truth) = rotating_capture();
        let plain = stack(frames.clone(), None);
        let fixed = stack(frames, Some(r));
        let sharp = |f: &Frame| {
            score(
                &crate::filter::gaussian_blur(f, 1.5),
                Metric::GradientEnergy,
                None,
            )
        };
        let (a, b, t) = (sharp(&plain.image), sharp(&fixed.image), sharp(&truth));
        eprintln!("sharpness: uncorrected {a:.5}, corrected {b:.5}, truth {t:.5}");
        assert!(b > 1.03 * a, "{a} against {b}");
        assert!(b > 0.8 * t, "{b} against the truth {t}");
        // The plan is reported, and recorded for the FITS history.
        let p = fixed.field_rotation.as_ref().unwrap();
        assert!(
            p.max_turn_deg > 5.0 && p.max_turn_deg < 7.0,
            "{}",
            p.max_turn_deg
        );
        assert!(fixed.provenance.iter().any(|s| s.step == "FIELD-ROTATION"));
        assert!(plain.field_rotation.is_none());
        // Closer to the truth, which the corrected stack is registered to.
        let err = |f: &Frame| crate::testutil::rmse(f, &truth, 14);
        eprintln!(
            "rmse to the truth: uncorrected {:.5}, corrected {:.5}",
            err(&plain.image),
            err(&fixed.image)
        );
        assert!(
            err(&fixed.image) < 0.6 * err(&plain.image),
            "{} against {}",
            err(&fixed.image),
            err(&plain.image)
        );
    }

    #[test]
    fn a_capture_with_the_wrong_number_of_stamps_is_refused() {
        let (frames, mut r, _) = rotating_capture();
        r.frame_unix.pop();
        let cfg = StackConfig {
            field_rotation: Some(r),
            multipoint: None,
            ..StackConfig::default()
        };
        let res = Pipeline::new(cfg).run(
            &mut MemorySource(frames),
            &|_, _| {},
            &std::sync::atomic::AtomicBool::new(false),
        );
        assert!(res.is_err());
    }
}
