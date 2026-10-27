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
//! The same angle serves a deep-sky capture: the target is then a fixed point
//! (`RA` and `DEC` in the header of a Seestar sub, say) instead of the Sun, and
//! a sequence of frames taken over half an hour on an altazimuth mount turns by
//! several degrees about the field centre, which trails every star but those
//! at the centre.
//!
//! An equatorial mount has no field rotation: do not use this with one.
//!
//! # References
//!
//! - Meeus, J. (1998), Astronomical Algorithms, 2nd ed., Willmann-Bell, chapter 14 (the parallactic angle).
//! - Reddy, B. S. and Chatterji, B. N. (1996), An FFT-based technique for translation, rotation, and scale-invariant image registration, IEEE Trans. Image Processing 5(8), 1266-1271 (phase correlation).

#![allow(
    clippy::cast_possible_wrap,
    clippy::many_single_char_names,
    reason = "pixel coordinates are far below i64::MAX; image maths uses w, h, x, y"
)]

use rayon::prelude::*;

use crate::align::global::{PhaseCorrelator, PhaseOptions};
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

/// Altitude of a point at `ra_deg`, `dec_deg` above the horizon, degrees (no
/// refraction), for `site`.
#[must_use]
pub fn altitude_deg(site: &Site, unix: f64, ra_deg: f64, dec_deg: f64) -> f64 {
    let h = (gmst_deg(unix) + site.lon_deg - ra_deg).to_radians();
    let (lat, dec) = (site.lat_deg.to_radians(), dec_deg.to_radians());
    (lat.sin() * dec.sin() + lat.cos() * dec.cos() * h.cos())
        .asin()
        .to_degrees()
}

/// Altitude of the Sun above the horizon, degrees (no refraction), for `site`.
#[must_use]
pub fn sun_altitude_deg(site: &Site, unix: f64) -> f64 {
    let e = solar_ephemeris(unix);
    altitude_deg(site, unix, e.ra_deg, e.dec_deg)
}

/// A fixed point on the sky: the target of a deep-sky capture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SkyPoint {
    /// Right ascension, degrees.
    pub ra_deg: f64,
    /// Declination, degrees.
    pub dec_deg: f64,
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
    /// Find out from the frames which way the picture turns, and ignore
    /// `mirror`: see [`measure_parity`]. For FITS subs, whose row order the
    /// header does not always tell truthfully.
    pub auto_mirror: bool,
    /// The orientation to turn every frame to.
    pub reference: Reference,
    /// What the camera points at: a fixed point of the sky (a deep-sky capture),
    /// or `None` for the Sun, whose position comes from the ephemeris.
    pub target: Option<SkyPoint>,
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
    /// Altitude of the Sun (or the target) at the reference, degrees.
    pub altitude_deg: f64,
    /// The target was a point of the sky, not the Sun.
    pub deep_sky: bool,
    /// The picture is a mirror image, and the turns have the opposite sense.
    pub mirror: bool,
    /// How well the frames lined up turned each way, as peak-to-sidelobe
    /// ratios (not mirrored, mirrored), when the sense was measured.
    pub parity_psr: Option<[f32; 2]>,
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
            "the parallactic angle runs {:.2} degrees over the capture ({:.3} degrees per minute, {} at {:.0} degrees altitude); frames are turned by up to {:.2} degrees to the orientation at {}",
            self.span_deg,
            self.rate_deg_per_min,
            if self.deep_sky { "target" } else { "Sun" },
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
        if let Some([plain, mirrored]) = self.parity_psr {
            v.push(format!(
                "the stars line up best with the picture {} (lock {plain:.1} as it is, {mirrored:.1} mirrored), so the frames are turned that way",
                if self.mirror { "mirrored" } else { "as it is" },
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
        let angle = |t: f64| match self.target {
            Some(p) => {
                parallactic_angle_deg(self.site.lat_deg, self.site.lon_deg, t, p.ra_deg, p.dec_deg)
            }
            None => sun_parallactic_angle_deg(&self.site, t),
        };
        let q: Vec<f64> = times.iter().map(|&t| angle(t)).collect();
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
            altitude_deg: match self.target {
                Some(p) => altitude_deg(&self.site, times[ref_index], p.ra_deg, p.dec_deg),
                None => sun_altitude_deg(&self.site, times[ref_index]),
            },
            deep_sky: self.target.is_some(),
            mirror: self.mirror,
            parity_psr: None,
            stamped: stamps.len(),
        })
    }
}

/// Smallest turn between two frames from which [`measure_parity`] can tell the
/// senses apart, degrees.
pub const PARITY_MIN_TURN_DEG: f64 = 0.5;

/// Weakest lock (peak-to-sidelobe ratio) at which a sense is believed.
pub const PARITY_MIN_PSR: f32 = 5.0;

/// How much better one sense must lock than the other to be believed.
pub const PARITY_MARGIN: f32 = 1.5;

/// How well two frames line up once each is turned to the common orientation,
/// for each sense of the turn: `[not mirrored, mirrored]`, as the
/// peak-to-sidelobe ratio of their phase correlation.
///
/// The parallactic angle says by how much the field turns but not which way a
/// given file's rows run: a mirror image turns the other way, and whether a
/// FITS sub is one depends on a row order its header may not give truthfully
/// (a Seestar's does not). The stars say it: turned the right way the two
/// frames show the same field, so their correlation has one sharp peak; turned
/// the wrong way they differ by twice the angle and have none (Reddy and
/// Chatterji 1996 for the correlation).
///
/// # Errors
/// [`Error::Invalid`] when the frames differ in size.
pub fn measure_parity(
    a: &Frame,
    from_deg: f64,
    b: &Frame,
    to_deg: f64,
    opts: PhaseOptions,
) -> Result<[f32; 2]> {
    let mut psr = [0.0f32; 2];
    for (k, sign) in [1.0, -1.0].into_iter().enumerate() {
        let ra = rotate_ccw(a, sign * from_deg, Kernel::Bilinear);
        let rb = rotate_ccw(b, sign * to_deg, Kernel::Bilinear);
        psr[k] = PhaseCorrelator::new(&ra, opts)?.estimate(&rb)?.psr;
    }
    Ok(psr)
}

/// The sense that [`measure_parity`] found: `Some(true)` for a mirror image,
/// or `None` when neither locks clearly better than the other.
#[must_use]
pub fn decide_parity(psr: [f32; 2]) -> Option<bool> {
    let [plain, mirrored] = psr;
    if mirrored >= PARITY_MIN_PSR && mirrored >= PARITY_MARGIN * plain {
        Some(true)
    } else if plain >= PARITY_MIN_PSR && plain >= PARITY_MARGIN * mirrored {
        Some(false)
    } else {
        None
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
/// is `None`. A raw colour capture is demosaiced to the plane the debayer names
/// before it is turned; [`FrameSource::select_channel`] picks another plane,
/// which is how a true colour stack gets red, green and blue turned alike.
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
    fn select_channel(&mut self, channel: crate::debayer::Channel) {
        self.debayer.channel = channel;
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use super::*;
    use crate::align::global::PhaseOptions;
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
            auto_mirror: false,
            reference: Reference::Middle,
            target: None,
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

    /// A deep-sky target and the time of its sub-frames: 25 frames, one a
    /// minute, from a site where the target is well up.
    const TARGET: SkyPoint = SkyPoint {
        ra_deg: 316.4,
        dec_deg: -11.25,
    };
    const SEESTAR_SITE: Site = Site {
        lat_deg: 36.386,
        lon_deg: 28.04,
        alt_m: 0.0,
    };
    /// 2025-09-21 19:30 UTC.
    const SUB_START: f64 = 1_758_483_000.0;

    fn deep_sky(mirror: bool) -> FieldRotation {
        FieldRotation {
            site: SEESTAR_SITE,
            mirror,
            auto_mirror: false,
            reference: Reference::Middle,
            target: Some(TARGET),
            frame_unix: (0..25)
                .map(|i| Some(SUB_START + f64::from(i) * 70.0))
                .collect(),
        }
    }

    /// A noiseless field of 70 Gaussian stars with a faint background.
    fn star_field() -> Frame {
        let (w, h) = (320usize, 320usize);
        let mut rng = crate::testutil::Rng::new(33);
        let mut data = vec![0.05f32; w * h];
        for _ in 0..70 {
            let (cx, cy) = (
                10.0 + rng.uniform() * (w as f32 - 20.0),
                10.0 + rng.uniform() * (h as f32 - 20.0),
            );
            let amp = 0.3 + 0.7 * rng.uniform();
            for y in (cy as usize).saturating_sub(7)..(cy as usize + 8).min(h) {
                for x in (cx as usize).saturating_sub(7)..(cx as usize + 8).min(w) {
                    let r2 = (x as f32 - cx).powi(2) + (y as f32 - cy).powi(2);
                    data[y * w + x] += amp * (-r2 / (2.0 * 1.4 * 1.4)).exp();
                }
            }
        }
        Frame::new(w, h, data).unwrap()
    }

    /// The field as a camera whose picture is (or is not) a mirror image saw it.
    fn sky_frames(mirror: bool) -> Vec<Frame> {
        let base = star_field();
        let plan = deep_sky(mirror).plan().unwrap();
        plan.turn_deg
            .iter()
            .map(|&t| rotate_ccw(&base, -t, Kernel::Lanczos3))
            .collect()
    }

    #[test]
    fn a_deep_sky_target_turns_at_the_rate_of_its_own_parallactic_angle() {
        let p = deep_sky(false).plan().unwrap();
        assert!(p.deep_sky);
        // Measured on the real subs: 7.5 degrees in 29 minutes.
        assert!(p.span_deg > 4.0 && p.span_deg < 8.0, "{}", p.span_deg);
        assert!(
            p.altitude_deg > 20.0 && p.altitude_deg < 70.0,
            "{}",
            p.altitude_deg
        );
        // The Sun is somewhere else, so it turns differently.
        let mut sun = deep_sky(false);
        sun.target = None;
        let s = sun.plan().unwrap();
        assert!(!s.deep_sky && (s.span_deg - p.span_deg).abs() > 0.01);
        assert!(p.lines(None)[0].contains("target at"));
    }

    #[test]
    fn the_stars_say_which_way_the_picture_turns() {
        for mirror in [false, true] {
            let frames = sky_frames(mirror);
            let plan = deep_sky(false).plan().unwrap();
            let (lo, hi) = (0, frames.len() - 1);
            let psr = measure_parity(
                &frames[lo],
                plan.turn_deg[lo],
                &frames[hi],
                plan.turn_deg[hi],
                PhaseOptions::default(),
            )
            .unwrap();
            eprintln!("mirror {mirror}: locks {psr:?}");
            assert_eq!(decide_parity(psr), Some(mirror), "{psr:?}");
        }
        // Neither sense wins, or neither locks: no answer.
        assert_eq!(decide_parity([6.0, 7.0]), None);
        assert_eq!(decide_parity([2.0, 1.0]), None);
    }

    fn stars_stack(
        frames: Vec<Frame>,
        rotation: Option<FieldRotation>,
    ) -> crate::pipeline::StackOutput {
        let cfg = StackConfig {
            keep: crate::Keep::Fraction(0.8),
            normalise: false,
            multipoint: None,
            field_rotation: rotation,
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

    /// The brightest star's peak over the field's flux near it: a trail spreads
    /// a star and lowers this.
    fn peakiness(f: &Frame) -> f32 {
        let blurred = crate::filter::gaussian_blur(f, 1.0);
        score(&blurred, Metric::GradientEnergy, None)
    }

    #[test]
    fn a_rotating_star_field_stacks_sharp_in_either_sense_once_the_frames_settle_it() {
        for mirror in [false, true] {
            let frames = sky_frames(mirror);
            let plain = stars_stack(frames.clone(), None);
            let mut fr = deep_sky(false);
            fr.auto_mirror = true;
            let fixed = stars_stack(frames, Some(fr));
            let (a, b) = (peakiness(&plain.image), peakiness(&fixed.image));
            eprintln!("mirror {mirror}: sharpness {a:.5} uncorrected, {b:.5} corrected");
            let p = fixed.field_rotation.as_ref().expect("a plan");
            assert_eq!(p.mirror, mirror);
            assert!(p.parity_psr.is_some());
            assert!(fixed.field_rotation_note.is_none());
            assert!(b > 1.3 * a, "{a} against {b}");
        }
    }

    #[test]
    fn a_field_that_hardly_turns_is_left_alone_and_the_note_says_why() {
        let frames = sky_frames(false);
        let mut fr = deep_sky(false);
        fr.auto_mirror = true;
        // Frames a second apart turn by hundredths of a degree.
        fr.frame_unix = (0..25).map(|i| Some(SUB_START + f64::from(i))).collect();
        let out = stars_stack(frames, Some(fr));
        assert!(out.field_rotation.is_none());
        let note = out.field_rotation_note.expect("a note");
        assert!(note.contains("too little"), "{note}");
        assert!(note.contains("--field-rotation"), "{note}");
    }

    #[test]
    fn a_source_that_demosaics_serves_the_plane_it_is_asked_for() {
        let mosaic = crate::testutil::colour_mosaic(
            &[star_field()],
            [0.4, 1.0, 0.7],
            [crate::align::Shift::default(); 3],
            crate::frame::BayerPattern::Rggb,
        );
        let mut plan = deep_sky(false).plan().unwrap();
        plan.turn_deg = vec![0.0];
        let mut raw = MemorySource(mosaic.clone());
        let mut source = DerotatedSource::new(&mut raw, &plan, Debayer::default()).unwrap();
        for channel in [
            crate::debayer::Channel::Red,
            crate::debayer::Channel::Green,
            crate::debayer::Channel::Blue,
        ] {
            source.select_channel(channel);
            let want = Debayer {
                channel,
                ..Debayer::default()
            }
            .apply_owned(mosaic[0].clone());
            let got = source.read(0).unwrap();
            assert_eq!(got.data, want.data, "{channel:?}");
        }
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
