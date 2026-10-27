// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Where the Sun is and how it is turned: apparent right ascension and
//! declination, and the position angle `P` of the solar north pole and the
//! heliographic latitude `B0` and longitude `L0` of the disc centre.
//!
//! The series are the low-precision ones of Meeus, Astronomical Algorithms
//! (chapters 25 and 29). They are good to about 0.01 degree for `P`, `B0` and
//! `L0`, far finer than a sunspot position measured on a disc a thousand
//! pixels across. The difference between UT and terrestrial time (about a
//! minute) is ignored; it moves `L0` by less than 0.001 degree.
//!
//! # References
//!
//! - Meeus, J. (1998), Astronomical Algorithms, 2nd ed., Willmann-Bell, chapters 25 and 29
//!   (ISBN 0-943396-61-1, 978-0-943396-61-3).
//! - Thompson, W. T. (2006), Coordinate systems for solar image data, Astron. Astrophys. 449, 791-803,
//!   doi:10.1051/0004-6361:20054262 (the conventions for `P`, `B0` and `L0`).

#![allow(
    clippy::many_single_char_names,
    reason = "the series use the book's symbols"
)]

/// Julian date of a unix time in UTC seconds.
#[must_use]
pub fn julian_date(unix: f64) -> f64 {
    unix / 86_400.0 + 2_440_587.5
}

/// Reduce an angle to `0..360` degrees.
fn wrap360(deg: f64) -> f64 {
    deg.rem_euclid(360.0)
}

/// The Sun at one instant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ephemeris {
    /// Julian date.
    pub jd: f64,
    /// Apparent right ascension, degrees.
    pub ra_deg: f64,
    /// Apparent declination, degrees.
    pub dec_deg: f64,
    /// Position angle of the solar rotation axis, degrees east of celestial
    /// north (counter-clockwise on a sky chart with north up and east left).
    pub p_deg: f64,
    /// Heliographic latitude of the disc centre, degrees (tilt of the axis
    /// towards the observer).
    pub b0_deg: f64,
    /// Carrington longitude of the disc centre, degrees `0..360`.
    pub l0_deg: f64,
}

/// The Sun's coordinates and orientation at `unix` seconds UTC.
#[must_use]
pub fn solar_ephemeris(unix: f64) -> Ephemeris {
    let jd = julian_date(unix);
    let t = (jd - 2_451_545.0) / 36_525.0;
    // Apparent longitude of the Sun (chapter 25).
    let l0 = wrap360(280.466_46 + 36_000.769_83 * t + 0.000_303_2 * t * t);
    let m = wrap360(357.529_11 + 35_999.050_29 * t - 0.000_153_7 * t * t).to_radians();
    let c = (1.914_602 - 0.004_817 * t - 0.000_014 * t * t) * m.sin()
        + (0.019_993 - 0.000_101 * t) * (2.0 * m).sin()
        + 0.000_289 * (3.0 * m).sin();
    let true_long = l0 + c;
    let omega = (125.04 - 1_934.136 * t).to_radians();
    let lambda = (true_long - 0.005_69 - 0.004_78 * omega.sin()).to_radians();
    // True obliquity corrected for nutation (the leading term).
    let eps0 = 23.439_291 - 0.013_004 * t;
    let eps = (eps0 + 0.002_56 * omega.cos()).to_radians();
    let ra = wrap360((eps.cos() * lambda.sin()).atan2(lambda.cos()).to_degrees());
    let dec = (eps.sin() * lambda.sin()).asin().to_degrees();

    // Orientation of the axis (chapter 29): inclination of the solar equator
    // to the ecliptic and the longitude of its ascending node.
    let incl = 7.25f64.to_radians();
    let node = (73.666_7 + 1.395_833_3 * (jd - 2_396_758.0) / 36_525.0).to_radians();
    let theta = (jd - 2_398_220.0) * 360.0 / 25.38;
    let x = (-lambda.cos() * eps.tan()).atan();
    let y = (-(lambda - node).cos() * incl.tan()).atan();
    let p = (x + y).to_degrees();
    let b0 = ((lambda - node).sin() * incl.sin()).asin().to_degrees();
    let eta = (-(lambda - node).sin() * incl.cos()).atan2(-(lambda - node).cos());
    let l0_carr = wrap360(eta.to_degrees() - theta);
    Ephemeris {
        jd,
        ra_deg: ra,
        dec_deg: dec,
        p_deg: p,
        b0_deg: b0,
        l0_deg: l0_carr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unix seconds of a Julian date.
    fn unix(jd: f64) -> f64 {
        (jd - 2_440_587.5) * 86_400.0
    }

    #[test]
    fn meeus_example_29a() {
        // 1992 October 13, 0h TD: P = 26.27, B0 = 5.99, L0 = 238.63 (Meeus 29.a).
        let e = solar_ephemeris(unix(2_448_908.5));
        assert!((e.p_deg - 26.27).abs() < 0.05, "P {}", e.p_deg);
        assert!((e.b0_deg - 5.99).abs() < 0.05, "B0 {}", e.b0_deg);
        assert!((e.l0_deg - 238.63).abs() < 0.1, "L0 {}", e.l0_deg);
    }

    #[test]
    fn meeus_example_25a_position() {
        // 1992 October 13, 0h TD: apparent RA 198.38083, Dec -7.78507 (Meeus 25.a).
        let e = solar_ephemeris(unix(2_448_908.5));
        assert!((e.ra_deg - 198.380).abs() < 0.02, "RA {}", e.ra_deg);
        assert!((e.dec_deg + 7.785).abs() < 0.02, "Dec {}", e.dec_deg);
    }

    #[test]
    fn the_disc_turns_about_thirteen_degrees_a_day() {
        let a = solar_ephemeris(unix(2_460_000.5));
        let b = solar_ephemeris(unix(2_460_001.5));
        let d = (a.l0_deg - b.l0_deg).rem_euclid(360.0);
        assert!((d - 13.2).abs() < 0.2, "{d}");
    }
}
