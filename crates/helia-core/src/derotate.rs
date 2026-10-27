// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Solar differential-rotation derotation: an image of the Sun as it would have
//! looked at another time.
//!
//! The Sun's surface does not turn as one piece. A feature at heliographic
//! latitude `phi` goes round at the sidereal rate
//! `omega(phi) = A + B sin^2 phi + C sin^4 phi`, the equator in about 25 days and
//! the mid latitudes slower. Between two captures minutes apart the disc has
//! turned a fraction of a degree, a pixel or less at the limb; over hours it is
//! tens of pixels, and the difference between latitudes a few.
//!
//! To bring an image taken at `t1` to the time `t2`, each pixel of the result is
//! traced back to where its feature was at `t1`:
//!
//! 1. the pixel gives a latitude and a longitude from the central meridian on
//!    the sphere seen at `t2` (the tilt `B0` and the position angle `P` come from
//!    [`crate::ephemeris`]), and so a Carrington longitude `L2`;
//! 2. the feature turned in the Carrington frame (which goes round at 360/25.38
//!    degrees a day) by `(omega(phi) - 14.18) (t2 - t1)`, so it was at
//!    `L1 = L2 - (omega - 14.18) (t2 - t1)` at `t1`;
//! 3. that is a longitude from the central meridian at `t1`, and a pixel in the
//!    image taken then, which is sampled with a Lanczos kernel.
//!
//! Brightness is carried with the feature: the contrast against the limb
//! darkening is kept, so a spot that moves toward the limb is not made brighter
//! or dimmer by the darkening of its new position. Where the source is behind
//! the limb at `t1` (the east edge, going forward) and outside the disc, the
//! pixel is left as it was, with a smooth hand-over.
//!
//! # References
//!
//! - Howard, R. F., Harvey, J. W. and Forgach, S. (1990), Solar surface velocity fields determined from small
//!   magnetic features, Solar Phys. 130, 295-311, doi:10.1007/BF00156795.
//! - Snodgrass, H. B. and Ulrich, R. K. (1990), Rotation of Doppler features in the solar photosphere,
//!   Astrophys. J. 351, 309, doi:10.1086/168467.
//! - Howard, R. (1984), Solar rotation, Annu. Rev. Astron. Astrophys. 22, 131-155,
//!   doi:10.1146/annurev.aa.22.090184.001023 (review).
//! - Duchon, C. E. (1979), Lanczos filtering in one and two dimensions, J. Appl. Meteorol. 18, 1016-1022,
//!   doi:10.1175/1520-0450(1979)018<1016:LFIOAT>2.0.CO;2 (the resampling kernel).

#![allow(
    clippy::many_single_char_names,
    reason = "x, y, w, h and the angles are the natural names in this geometry"
)]

use rayon::prelude::*;

use crate::ephemeris::{Ephemeris, solar_ephemeris};
use crate::error::{Error, Result};
use crate::frame::Frame;
use crate::limbdark::{self, FlattenPlan, Law, Source};
use crate::picture::{self, Picture};
use crate::resample::{Kernel, sample};

/// Sidereal rotation rate of the Carrington frame, degrees a day.
pub const CARRINGTON_DEG_PER_DAY: f64 = 360.0 / 25.38;

/// Degrees a day in one microradian a second.
const MICRORAD_S_TO_DEG_DAY: f64 = 1e-6 * 86_400.0 * 180.0 / std::f64::consts::PI;

/// Width of the hand-over to the unmoved image at the limb, in radii.
const LIMB_FADE: f64 = 0.02;
/// Source `z` (cosine of the angle from disc centre) below which the source
/// is taken to be unseen at `t1`, and over which the hand-over is made.
const SOURCE_FADE: f64 = 0.10;

/// A rotation law: the sidereal rate in degrees a day at a latitude.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RotationLaw {
    /// Howard, Harvey and Forgach (1990), sunspots and plage:
    /// 2.894, -0.428, -0.370 microradians a second.
    Howard,
    /// Snodgrass and Ulrich (1990), Doppler features:
    /// 2.851, -0.343, -0.474 microradians a second.
    Snodgrass,
    /// Your own `A + B sin^2 + C sin^4`, degrees a day.
    Custom {
        /// Equatorial rate.
        a: f64,
        /// Coefficient of `sin^2 phi`.
        b: f64,
        /// Coefficient of `sin^4 phi`.
        c: f64,
    },
}

impl RotationLaw {
    /// The coefficients `(A, B, C)` in degrees a day.
    #[must_use]
    pub fn coefficients(self) -> (f64, f64, f64) {
        let k = MICRORAD_S_TO_DEG_DAY;
        match self {
            Self::Howard => (2.894 * k, -0.428 * k, -0.370 * k),
            Self::Snodgrass => (2.851 * k, -0.343 * k, -0.474 * k),
            Self::Custom { a, b, c } => (a, b, c),
        }
    }

    /// The sidereal rate at latitude `lat_deg`, degrees a day.
    #[must_use]
    pub fn rate(self, lat_deg: f64) -> f64 {
        let (a, b, c) = self.coefficients();
        let s2 = lat_deg.to_radians().sin().powi(2);
        a + b * s2 + c * s2 * s2
    }

    /// Short name.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::Howard => "Howard".into(),
            Self::Snodgrass => "Snodgrass".into(),
            Self::Custom { a, b, c } => format!("custom ({a}, {b}, {c} deg/day)"),
        }
    }
}

impl std::str::FromStr for RotationLaw {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "howard" => Ok(Self::Howard),
            "snodgrass" => Ok(Self::Snodgrass),
            other => {
                let v: Vec<f64> = other
                    .split(',')
                    .map(|p| p.trim().parse::<f64>())
                    .collect::<std::result::Result<_, _>>()
                    .map_err(|_| bad_law(s))?;
                match v.as_slice() {
                    [a, b, c] if v.iter().all(|x| x.is_finite()) && *a > 0.0 => Ok(Self::Custom {
                        a: *a,
                        b: *b,
                        c: *c,
                    }),
                    _ => Err(bad_law(s)),
                }
            }
        }
    }
}

fn bad_law(s: &str) -> Error {
    Error::Invalid(format!(
        "unknown rotation law {s:?}; use howard, snodgrass, or A,B,C in degrees a day (A positive)"
    ))
}

/// How the image sits against the sky; the same three settings as the sunspot
/// search.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Orientation {
    /// Degrees counter-clockwise from the top of the image to solar north (0
    /// when solar north is up).
    pub rotate_deg: f32,
    /// The image is a mirror image (west is on the left).
    pub mirror: bool,
    /// Celestial north is up: solar north is then turned by `P` at the time,
    /// on top of `rotate_deg`.
    pub celestial_north_up: bool,
}

/// The disc in the image: centre and radius, pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DiscGeometry {
    /// Centre, x.
    pub cx: f64,
    /// Centre, y (top row 0).
    pub cy: f64,
    /// Radius.
    pub r: f64,
}

/// What to do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spec {
    /// The rotation law.
    pub law: RotationLaw,
    /// How the image sits against the sky.
    pub orient: Orientation,
    /// When the image was taken, unix seconds UTC.
    pub from_unix: f64,
    /// The time to bring it to.
    pub to_unix: f64,
    /// Carry the contrast against the limb darkening with each feature.
    pub preserve_contrast: bool,
    /// Interpolation.
    pub kernel: Kernel,
}

impl Spec {
    /// Whether the two times are the same, so that nothing moves.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        (self.to_unix - self.from_unix).abs() < 1e-6
    }
}

/// What was done.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Report {
    /// Time moved, seconds (positive forward).
    pub dt_s: f64,
    /// The largest distance a pixel of the disc came from, pixels.
    pub max_shift_px: f32,
    /// The distance at the disc centre, pixels.
    pub centre_shift_px: f32,
}

/// Pixel and sphere at one instant.
struct Geo {
    disc: DiscGeometry,
    sin_a: f64,
    cos_a: f64,
    mirror: bool,
    sin_b0: f64,
    cos_b0: f64,
    l0_deg: f64,
}

impl Geo {
    fn new(disc: DiscGeometry, orient: Orientation, e: &Ephemeris) -> Self {
        let north = if orient.celestial_north_up {
            f64::from(orient.rotate_deg) + e.p_deg
        } else {
            f64::from(orient.rotate_deg)
        };
        let a = north.to_radians();
        let b0 = e.b0_deg.to_radians();
        Self {
            disc,
            sin_a: a.sin(),
            cos_a: a.cos(),
            mirror: orient.mirror,
            sin_b0: b0.sin(),
            cos_b0: b0.cos(),
            l0_deg: e.l0_deg,
        }
    }

    /// Latitude and longitude from the central meridian, radians, of a pixel
    /// inside the disc; and its distance from the centre in radii.
    fn to_helio(&self, x: f64, y: f64) -> (f64, f64, f64) {
        let mut dx = x - self.disc.cx;
        let dy = self.disc.cy - y;
        if self.mirror {
            dx = -dx;
        }
        let xs = (dx * self.cos_a + dy * self.sin_a) / self.disc.r;
        let ys = (-dx * self.sin_a + dy * self.cos_a) / self.disc.r;
        let rho = xs.hypot(ys);
        let z = (1.0 - xs * xs - ys * ys).max(0.0).sqrt();
        let lat = (ys * self.cos_b0 + z * self.sin_b0).clamp(-1.0, 1.0).asin();
        let cmd = xs.atan2(z * self.cos_b0 - ys * self.sin_b0);
        (lat, cmd, rho)
    }

    /// Pixel of a latitude and longitude from the central meridian, and the
    /// cosine of its distance from the disc centre (negative: behind the limb).
    fn pixel_of(&self, lat: f64, cmd: f64) -> (f64, f64, f64) {
        let (sl, cl) = lat.sin_cos();
        let (sm, cm) = cmd.sin_cos();
        let xs = cl * sm;
        let ys = sl * self.cos_b0 - cl * cm * self.sin_b0;
        let z = sl * self.sin_b0 + cl * cm * self.cos_b0;
        let mut dx = (xs * self.cos_a - ys * self.sin_a) * self.disc.r;
        let dy = (xs * self.sin_a + ys * self.cos_a) * self.disc.r;
        if self.mirror {
            dx = -dx;
        }
        (self.disc.cx + dx, self.disc.cy - dy, z)
    }
}

/// Where a pixel of the result comes from in the source.
struct Mapper {
    to: Geo,
    from: Geo,
    law: RotationLaw,
    dt_days: f64,
}

/// One pixel's trace.
#[derive(Clone, Copy)]
struct Trace {
    /// Source position.
    sx: f32,
    sy: f32,
    /// Weight of the traced pixel (0 keeps the pixel as it was).
    weight: f32,
    /// Radii from the centre in the result and in the source.
    rho_out: f32,
    rho_src: f32,
}

impl Mapper {
    fn new(disc: DiscGeometry, spec: &Spec) -> Self {
        let e_from = solar_ephemeris(spec.from_unix);
        let e_to = solar_ephemeris(spec.to_unix);
        Self {
            to: Geo::new(disc, spec.orient, &e_to),
            from: Geo::new(disc, spec.orient, &e_from),
            law: spec.law,
            dt_days: (spec.to_unix - spec.from_unix) / 86_400.0,
        }
    }

    fn trace(&self, x: f64, y: f64) -> Trace {
        let identity = Trace {
            sx: x as f32,
            sy: y as f32,
            weight: 0.0,
            rho_out: 0.0,
            rho_src: 0.0,
        };
        let (lat, cmd2, rho) = self.to.to_helio(x, y);
        if rho >= 1.0 {
            return identity;
        }
        // Carrington longitude at the new time, and where it was.
        let rate = self.law.rate(lat.to_degrees()) - CARRINGTON_DEG_PER_DAY;
        let l2 = cmd2.to_degrees() + self.to.l0_deg;
        let l1 = l2 - rate * self.dt_days;
        let cmd1 = (l1 - self.from.l0_deg).to_radians();
        let (sx, sy, z) = self.from.pixel_of(lat, cmd1);
        let fade_limb = ((1.0 - rho) / LIMB_FADE).clamp(0.0, 1.0);
        let fade_src = (z / SOURCE_FADE).clamp(0.0, 1.0);
        let rho_src = (sx - self.from.disc.cx).hypot(sy - self.from.disc.cy) / self.from.disc.r;
        Trace {
            sx: sx as f32,
            sy: sy as f32,
            weight: (fade_limb * fade_src) as f32,
            rho_out: rho as f32,
            rho_src: rho_src as f32,
        }
    }
}

/// The sky level of a plane: the mean in a ring just outside the limb.
fn sky_of(f: &Frame, d: DiscGeometry) -> f32 {
    let (mut s, mut n) = (0.0f64, 0usize);
    let (lo, hi) = (1.15 * d.r, 1.4 * d.r);
    let y0 = (d.cy - hi).floor().max(0.0) as usize;
    let y1 = ((d.cy + hi).ceil().max(0.0) as usize + 1).min(f.height);
    let x0 = (d.cx - hi).floor().max(0.0) as usize;
    let x1 = ((d.cx + hi).ceil().max(0.0) as usize + 1).min(f.width);
    for y in y0..y1 {
        for x in x0..x1 {
            let r = (x as f64 - d.cx).hypot(y as f64 - d.cy);
            if r >= lo && r <= hi {
                s += f64::from(f.data[y * f.width + x]);
                n += 1;
            }
        }
    }
    if n == 0 { 0.0 } else { (s / n as f64) as f32 }
}

/// The disc of a picture, from its limb fit. `None` without a clear limb.
#[must_use]
pub fn disc_of(p: &Picture) -> Option<DiscGeometry> {
    crate::align::limb::LimbReference::new(&picture::luma(p)).map(|l| DiscGeometry {
        cx: l.fit.cx,
        cy: l.fit.cy,
        r: l.fit.r,
    })
}

/// The largest and the central displacement of the disc, pixels, for `spec`.
#[must_use]
pub fn displacement(disc: DiscGeometry, spec: &Spec) -> Report {
    if spec.is_identity() {
        return Report {
            dt_s: 0.0,
            max_shift_px: 0.0,
            centre_shift_px: 0.0,
        };
    }
    let m = Mapper::new(disc, spec);
    let centre = {
        let t = m.trace(disc.cx, disc.cy);
        (f64::from(t.sx) - disc.cx).hypot(f64::from(t.sy) - disc.cy) as f32
    };
    let mut max = 0.0f32;
    let step = (disc.r / 40.0).max(1.0);
    let mut y = disc.cy - 0.95 * disc.r;
    while y <= disc.cy + 0.95 * disc.r {
        let mut x = disc.cx - 0.95 * disc.r;
        while x <= disc.cx + 0.95 * disc.r {
            let t = m.trace(x, y);
            if t.weight > 0.99 {
                max = max.max((f64::from(t.sx) - x).hypot(f64::from(t.sy) - y) as f32);
            }
            x += step;
        }
        y += step;
    }
    Report {
        dt_s: spec.to_unix - spec.from_unix,
        max_shift_px: max,
        centre_shift_px: centre,
    }
}

/// Bring `p`, taken at `spec.from_unix`, to `spec.to_unix`.
///
/// # Errors
/// [`Error::Invalid`] for a disc with no area.
pub fn derotate(p: &Picture, disc: DiscGeometry, spec: &Spec) -> Result<(Picture, Report)> {
    let plan = if spec.preserve_contrast {
        limbdark::plan(&picture::luma(p), Source::Model, Law::Quadratic).map(|(pl, _)| pl)
    } else {
        None
    };
    derotate_with(p, disc, spec, plan.as_ref())
}

/// As [`derotate`], with the limb-darkening plan that carries the contrast
/// given (a panel of a mosaic shows too little of the disc to fit its own).
/// `None` moves the pixel values as they are.
///
/// # Errors
/// [`Error::Invalid`] for a disc with no area.
pub fn derotate_with(
    p: &Picture,
    disc: DiscGeometry,
    spec: &Spec,
    plan: Option<&FlattenPlan>,
) -> Result<(Picture, Report)> {
    if !(disc.r > 8.0 && disc.r.is_finite()) {
        return Err(Error::Invalid("no solar disc to derotate".into()));
    }
    let report = displacement(disc, spec);
    if spec.is_identity() {
        return Ok((p.clone(), report));
    }
    let (w, h) = p.size();
    let mapper = Mapper::new(disc, spec);
    let traces: Vec<Trace> = (0..w * h)
        .into_par_iter()
        .map(|i| mapper.trace((i % w) as f64, (i / w) as f64))
        .collect();
    let gain = plan.filter(|_| spec.preserve_contrast);
    let planes: Vec<Frame> = picture::planes(p)
        .iter()
        .map(|f| {
            let sky = sky_of(f, disc);
            let mut data = vec![0.0f32; w * h];
            data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
                for (x, v) in row.iter_mut().enumerate() {
                    let t = traces[y * w + x];
                    let here = f.data[y * w + x];
                    if t.weight <= 0.0 {
                        *v = here;
                        continue;
                    }
                    let mut s = sample(f, t.sx, t.sy, spec.kernel);
                    if let Some(g) = gain {
                        let ratio = g.gain_at(t.rho_src) / g.gain_at(t.rho_out).max(1e-6);
                        s = sky + (s - sky) * ratio;
                    }
                    *v = here + t.weight * (s - here);
                }
            });
            Frame {
                width: w,
                height: h,
                data,
                bayer: None,
            }
        })
        .collect();
    Ok((picture::from_planes(&planes)?, report))
}

/// Synthetic views of the rotating Sun with known ground truth.
#[cfg(any(test, feature = "testutil"))]
pub mod synth {
    use super::{
        CARRINGTON_DEG_PER_DAY, DiscGeometry, Frame, Geo, Orientation, RotationLaw, solar_ephemeris,
    };

    /// The Sun at `t` (unix seconds) with features, given as latitude and
    /// longitude from the central meridian at `t0` in degrees, carried round by
    /// `law`: dark spots on a limb-darkened disc, drawn by their distance on the
    /// sphere. For tests of anything that moves, matches or joins views of the Sun.
    #[must_use]
    #[allow(
        clippy::too_many_arguments,
        reason = "everything the picture is made from"
    )]
    pub fn render_sun(
        width: usize,
        height: usize,
        disc: DiscGeometry,
        t: f64,
        t0: f64,
        law: RotationLaw,
        orient: Orientation,
        features: &[(f64, f64)],
    ) -> Frame {
        let e = solar_ephemeris(t);
        let geo = Geo::new(disc, orient, &e);
        let days = (t - t0) / 86_400.0;
        let l0_t0 = solar_ephemeris(t0).l0_deg;
        let spots: Vec<(f64, f64)> = features
            .iter()
            .map(|&(lat, l)| {
                (
                    lat.to_radians(),
                    (l0_t0 + l + (law.rate(lat) - CARRINGTON_DEG_PER_DAY) * days).to_radians(),
                )
            })
            .collect();
        let mut f = Frame::zeros(width, height);
        for y in 0..height {
            for x in 0..width {
                let (lat, cmd, rho) = geo.to_helio(x as f64, y as f64);
                if rho >= 1.0 {
                    continue;
                }
                let l = cmd + (e.l0_deg).to_radians();
                let mu = (1.0 - rho * rho).sqrt();
                let mut v = 1.0 - 0.6 * (1.0 - mu);
                for &(sl, sc) in &spots {
                    let c = lat.sin() * sl.sin() + lat.cos() * sl.cos() * (l - sc).cos();
                    let ang = c.clamp(-1.0, 1.0).acos();
                    v *= 1.0 - 0.7 * (-(ang * ang) / (2.0 * 0.03f64.powi(2))).exp();
                }
                f.data[y * width + x] = v as f32;
            }
        }
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::solarnet::unix_from_iso;

    const N: usize = 640;
    const M: usize = 520;
    const R: f64 = 240.0;

    fn disc() -> DiscGeometry {
        DiscGeometry {
            cx: 320.0,
            cy: 260.0,
            r: R,
        }
    }

    /// Features at (latitude, longitude from the central meridian at `t0`),
    /// degrees.
    const FEATURES: [(f64, f64); 7] = [
        (0.0, -5.0),
        (2.0, -30.0),
        (35.0, -12.0),
        (-35.0, -8.0),
        (20.0, -35.0),
        (-20.0, 10.0),
        (50.0, -18.0),
    ];

    fn render(t: f64, t0: f64, law: RotationLaw, orient: Orientation) -> Frame {
        synth::render_sun(N, M, disc(), t, t0, law, orient, &FEATURES)
    }

    /// Centre of the dark spot nearest to `(x, y)`: centroid of the deficit.
    fn spot_centre(f: &Frame, x: f64, y: f64) -> (f64, f64) {
        let win = 9i64;
        let (mut sx, mut sy, mut sw) = (0.0, 0.0, 0.0);
        for j in -win..=win {
            for i in -win..=win {
                let (px, py) = ((x as i64 + i) as usize, (y as i64 + j) as usize);
                let mu_ref = 1.0
                    - 0.6
                        * (1.0
                            - (1.0 - ((px as f64 - 320.0).hypot(py as f64 - 260.0) / R).powi(2))
                                .max(0.0)
                                .sqrt());
                let d = (mu_ref - f64::from(f.data[py * N + px])).max(0.0);
                sx += d * px as f64;
                sy += d * py as f64;
                sw += d;
            }
        }
        (sx / sw, sy / sw)
    }

    fn rms(a: &Frame, b: &Frame) -> f64 {
        let (mut s, mut n) = (0.0f64, 0.0f64);
        for y in 0..M {
            for x in 0..N {
                if (x as f64 - 320.0).hypot(y as f64 - 260.0) < 0.9 * R {
                    s += f64::from(a.data[y * N + x] - b.data[y * N + x]).powi(2);
                    n += 1.0;
                }
            }
        }
        (s / n).sqrt()
    }

    fn spec(from: f64, to: f64, law: RotationLaw) -> Spec {
        Spec {
            law,
            orient: Orientation::default(),
            from_unix: from,
            to_unix: to,
            preserve_contrast: true,
            kernel: Kernel::Lanczos3,
        }
    }

    #[test]
    fn the_laws_have_the_published_equatorial_and_mid_latitude_rates() {
        // 2.894 microrad/s is 14.33 degrees a day; 2.851 is 14.11.
        assert!((RotationLaw::Howard.rate(0.0) - 14.327).abs() < 0.01);
        assert!((RotationLaw::Snodgrass.rate(0.0) - 14.113).abs() < 0.01);
        // The mid latitudes are slower by about a degree a day.
        let d = RotationLaw::Howard.rate(0.0) - RotationLaw::Howard.rate(40.0);
        assert!((d - 1.19).abs() < 0.05, "{d}");
        assert!(
            "2.0,-0.3,-0.4"
                .parse::<RotationLaw>()
                .is_ok_and(|l| (l.rate(0.0) - 2.0).abs() < 1e-9)
        );
        assert!("fast".parse::<RotationLaw>().is_err());
        assert!("1,2".parse::<RotationLaw>().is_err());
    }

    #[test]
    fn a_pixel_goes_to_the_sphere_and_back() {
        let e = solar_ephemeris(unix_from_iso("2026-10-20T14:30:00").unwrap_or(0.0));
        let g = Geo::new(
            disc(),
            Orientation {
                rotate_deg: 17.0,
                mirror: true,
                celestial_north_up: true,
            },
            &e,
        );
        for (x, y) in [(320.0, 260.0), (400.0, 200.0), (250.0, 380.0)] {
            let (lat, cmd, _) = g.to_helio(x, y);
            let (bx, by, z) = g.pixel_of(lat, cmd);
            assert!(z > 0.0);
            assert!((bx - x).abs() < 1e-6 && (by - y).abs() < 1e-6, "{x},{y}");
        }
    }

    #[test]
    fn six_hours_of_rotation_is_undone_to_a_fraction_of_a_pixel() {
        let t1 = unix_from_iso("2026-10-20T09:00:00").unwrap_or(0.0);
        let t2 = t1 + 6.0 * 3600.0;
        let law = RotationLaw::Howard;
        let before = render(t1, t1, law, Orientation::default());
        let truth = render(t2, t1, law, Orientation::default());
        let (out, rep) = derotate(&Picture::Mono(before.clone()), disc(), &spec(t1, t2, law))
            .unwrap_or_else(|e| panic!("{e}"));
        let Picture::Mono(out) = out else {
            panic!("mono")
        };
        // The disc turned by some 14 pixels at the centre.
        assert!(rep.centre_shift_px > 10.0, "{rep:?}");
        let (bad, good) = (rms(&before, &truth), rms(&out, &truth));
        assert!(good < 0.25 * bad && good < 0.02, "rms {bad} -> {good}");
        // Every feature that is well on the disc lands within 0.3 pixel.
        let mut checked = 0;
        for &(lat, l) in &FEATURES {
            let days = (t2 - t1) / 86_400.0;
            let e2 = solar_ephemeris(t2);
            let geo = Geo::new(disc(), Orientation::default(), &e2);
            let l2 =
                solar_ephemeris(t1).l0_deg + l + (law.rate(lat) - CARRINGTON_DEG_PER_DAY) * days;
            let (px, py, z) = geo.pixel_of(lat.to_radians(), (l2 - e2.l0_deg).to_radians());
            if z < 0.5 {
                continue;
            }
            let (ex, ey) = spot_centre(&truth, px, py);
            let (ox, oy) = spot_centre(&out, px, py);
            assert!(
                (ex - ox).hypot(ey - oy) < 0.3,
                "feature at {lat}: truth ({ex:.2}, {ey:.2}) result ({ox:.2}, {oy:.2})"
            );
            checked += 1;
        }
        assert!(checked >= 4, "only {checked} features were on the disc");
    }

    #[test]
    fn the_differential_part_matters() {
        // Twenty-four hours: the mid latitudes lag the equator by 1.2 degrees,
        // some 5 pixels. A rigid law at the equatorial rate misses them.
        let t1 = unix_from_iso("2026-10-20T09:00:00").unwrap_or(0.0);
        let t2 = t1 + 24.0 * 3600.0;
        let law = RotationLaw::Howard;
        let before = render(t1, t1, law, Orientation::default());
        let truth = render(t2, t1, law, Orientation::default());
        let run = |l: RotationLaw| {
            let (o, _) = derotate(&Picture::Mono(before.clone()), disc(), &spec(t1, t2, l))
                .unwrap_or_else(|e| panic!("{e}"));
            let Picture::Mono(o) = o else { panic!("mono") };
            rms(&o, &truth)
        };
        let rigid = run(RotationLaw::Custom {
            a: law.rate(0.0),
            b: 0.0,
            c: 0.0,
        });
        let differential = run(law);
        assert!(
            differential < 0.5 * rigid && differential < 0.03,
            "rigid {rigid} differential {differential}"
        );
    }

    #[test]
    fn going_there_and_back_returns_the_picture() {
        let t1 = unix_from_iso("2026-10-20T09:00:00").unwrap_or(0.0);
        let t2 = t1 + 2.0 * 3600.0;
        let law = RotationLaw::Snodgrass;
        let a = render(t1, t1, law, Orientation::default());
        let (b, _) = derotate(&Picture::Mono(a.clone()), disc(), &spec(t1, t2, law))
            .unwrap_or_else(|e| panic!("{e}"));
        let (c, _) = derotate(&b, disc(), &spec(t2, t1, law)).unwrap_or_else(|e| panic!("{e}"));
        let Picture::Mono(c) = c else { panic!("mono") };
        assert!(rms(&a, &c) < 0.02, "{}", rms(&a, &c));
    }

    #[test]
    fn the_orientation_of_the_image_is_followed() {
        // The same Sun turned in the image by 30 degrees and mirrored.
        let t1 = unix_from_iso("2026-10-20T09:00:00").unwrap_or(0.0);
        let t2 = t1 + 4.0 * 3600.0;
        let law = RotationLaw::Howard;
        let orient = Orientation {
            rotate_deg: 30.0,
            mirror: true,
            celestial_north_up: false,
        };
        let before = render(t1, t1, law, orient);
        let truth = render(t2, t1, law, orient);
        let mut s = spec(t1, t2, law);
        s.orient = orient;
        let (o, _) =
            derotate(&Picture::Mono(before.clone()), disc(), &s).unwrap_or_else(|e| panic!("{e}"));
        let Picture::Mono(o) = o else { panic!("mono") };
        assert!(rms(&o, &truth) < 0.3 * rms(&before, &truth));
    }

    #[test]
    fn no_time_change_changes_nothing_and_a_colour_picture_keeps_its_planes() {
        let t1 = unix_from_iso("2026-10-20T09:00:00").unwrap_or(0.0);
        let f = render(t1, t1, RotationLaw::Howard, Orientation::default());
        let rgb = picture::from_planes(&[f.clone(), f.clone(), f.clone()])
            .unwrap_or_else(|e| panic!("{e}"));
        let (same, rep) = derotate(&rgb, disc(), &spec(t1, t1, RotationLaw::Howard))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(same, rgb);
        assert!(rep.max_shift_px < 1e-6);
        let (moved, _) = derotate(&rgb, disc(), &spec(t1, t1 + 3600.0, RotationLaw::Howard))
            .unwrap_or_else(|e| panic!("{e}"));
        let planes = picture::planes(&moved);
        assert_eq!(planes.len(), 3);
        assert_eq!(planes[0].data, planes[2].data);
    }

    #[test]
    fn the_limb_and_the_sky_are_left_alone() {
        let t1 = unix_from_iso("2026-10-20T09:00:00").unwrap_or(0.0);
        let before = render(t1, t1, RotationLaw::Howard, Orientation::default());
        let (o, _) = derotate(
            &Picture::Mono(before.clone()),
            disc(),
            &spec(t1, t1 + 8.0 * 3600.0, RotationLaw::Howard),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let Picture::Mono(o) = o else { panic!("mono") };
        for y in 0..M {
            for x in 0..N {
                if (x as f64 - 320.0).hypot(y as f64 - 260.0) >= R {
                    assert_eq!(o.data[y * N + x], before.data[y * N + x]);
                }
            }
        }
    }
}
