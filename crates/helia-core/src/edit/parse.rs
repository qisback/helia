// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Text forms of edit parameters, for the command line.

use super::deconv::{Psf, airy_radius_px};
use super::geometry::{Crop, Resize};
use super::tone::Curve;
use crate::error::{Error, Result};
use crate::mask::parse_region;

fn bad(what: &str, text: &str, hint: &str) -> Error {
    Error::Invalid(format!("{what} {text:?}: {hint}"))
}

fn number(what: &str, text: &str) -> Result<f32> {
    text.trim()
        .parse::<f32>()
        .ok()
        .filter(|v| v.is_finite())
        .ok_or_else(|| bad(what, text, "not a number"))
}

/// A comma-separated list of numbers: `1.5,1.4,1.2`.
///
/// # Errors
/// [`Error::Invalid`] for an empty list or a field that is not a number.
pub fn number_list(what: &str, text: &str) -> Result<Vec<f32>> {
    let v: Vec<f32> = text
        .split(',')
        .map(|p| number(what, p))
        .collect::<Result<_>>()?;
    if v.is_empty() {
        return Err(bad(what, text, "give at least one number"));
    }
    Ok(v)
}

/// A PSF: `auto`, `limb` (measured from the solar limb), `gaussian:SIGMA`, `airy:RADIUS` (first null, pixels) or
/// `airy-scope:WAVELENGTH_NM,APERTURE_MM,ARCSEC_PER_PX`.
///
/// # Errors
/// [`Error::Invalid`] with the accepted forms.
pub fn psf(text: &str) -> Result<Psf> {
    let hint = "use auto, limb, gaussian:SIGMA, airy:RADIUS or airy-scope:NM,MM,ARCSEC_PER_PX";
    let t = text.trim().to_ascii_lowercase();
    if t == "auto" {
        return Ok(Psf::Auto);
    }
    if t == "limb" {
        return Ok(Psf::Limb);
    }
    let (kind, rest) = t.split_once(':').ok_or_else(|| bad("PSF", text, hint))?;
    match kind {
        "gaussian" | "gauss" => Ok(Psf::Gaussian {
            sigma: number("PSF sigma", rest)?,
        }),
        "airy" => Ok(Psf::Airy {
            radius: number("PSF radius", rest)?,
        }),
        "airy-scope" | "scope" => {
            let v = number_list("PSF telescope", rest)?;
            let [nm, mm, scale] = v[..] else {
                return Err(bad("PSF telescope", text, hint));
            };
            if nm <= 0.0 || mm <= 0.0 || scale <= 0.0 {
                return Err(bad("PSF telescope", text, "all three must be positive"));
            }
            Ok(Psf::Airy {
                radius: airy_radius_px(nm, mm, scale),
            })
        }
        _ => Err(bad("PSF", text, hint)),
    }
}

/// A resize: `50%` or `0.5` (a factor), `1200x800`, `1200x` (width) or
/// `x800` (height).
///
/// # Errors
/// [`Error::Invalid`] with the accepted forms.
pub fn resize(text: &str) -> Result<Resize> {
    let hint = "use 50%, 0.5, 1200x800, 1200x or x800";
    let t = text.trim().to_ascii_lowercase();
    if let Some(p) = t.strip_suffix('%') {
        return Ok(Resize::by(number("resize", p)? / 100.0));
    }
    if let Some((w, h)) = t.split_once('x') {
        let side = |s: &str| -> Result<Option<usize>> {
            if s.trim().is_empty() {
                Ok(None)
            } else {
                s.trim()
                    .parse::<usize>()
                    .map(Some)
                    .map_err(|_| bad("resize", text, hint))
            }
        };
        let (width, height) = (side(w)?, side(h)?);
        if width.is_none() && height.is_none() {
            return Err(bad("resize", text, hint));
        }
        return Ok(Resize {
            scale: None,
            width,
            height,
        });
    }
    Ok(Resize::by(number("resize", &t)?))
}

/// A crop rectangle `X,Y,W,H`.
///
/// # Errors
/// [`Error::Invalid`] for anything else.
pub fn crop(text: &str) -> Result<Crop> {
    let r = parse_region(text)?;
    Ok(Crop {
        x: r.x0,
        y: r.y0,
        width: r.width(),
        height: r.height(),
    })
}

/// A tone curve through `IN:OUT` points: `0.25:0.15,0.75:0.85`.
///
/// # Errors
/// [`Error::Invalid`] for a malformed point or a curve that fails validation.
pub fn curve(text: &str) -> Result<Curve> {
    let mut points = Vec::new();
    for p in text.split(',') {
        let (a, b) = p
            .split_once(':')
            .ok_or_else(|| bad("curve point", p, "use IN:OUT, e.g. 0.25:0.15"))?;
        points.push([number("curve input", a)?, number("curve output", b)?]);
    }
    let c = Curve { points };
    c.validate()?;
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn psf_forms() {
        assert_eq!(psf("auto").ok(), Some(Psf::Auto));
        assert_eq!(psf("Gaussian:1.8").ok(), Some(Psf::Gaussian { sigma: 1.8 }));
        assert_eq!(psf("airy:2.5").ok(), Some(Psf::Airy { radius: 2.5 }));
        let Ok(Psf::Airy { radius }) = psf("airy-scope:656,100,1.0") else {
            panic!("scope form")
        };
        assert!((radius - 1.65).abs() < 0.02);
        assert!(psf("blob").is_err());
        assert!(psf("gaussian:x").is_err());
        assert!(psf("airy-scope:656,100").is_err());
    }

    #[test]
    fn resize_forms() {
        assert_eq!(resize("50%").ok(), Some(Resize::by(0.5)));
        assert_eq!(resize("2").ok(), Some(Resize::by(2.0)));
        assert_eq!(
            resize("1200x800").ok(),
            Some(Resize {
                scale: None,
                width: Some(1200),
                height: Some(800)
            })
        );
        assert_eq!(resize("x800").ok().and_then(|r| r.width), None);
        assert!(resize("x").is_err());
        assert!(resize("big").is_err());
    }

    #[test]
    fn crop_and_curve_forms() {
        assert_eq!(
            crop("10,20,300,200").ok(),
            Some(Crop {
                x: 10,
                y: 20,
                width: 300,
                height: 200
            })
        );
        assert!(crop("1,2,3").is_err());
        let c = curve("0.25:0.15,0.75:0.85").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(c.points.len(), 2);
        assert!(curve("0.5").is_err());
        assert!(curve("0.2:0.8,0.6:0.3").is_err());
    }

    #[test]
    fn lists() {
        assert_eq!(number_list("g", "1, 2.5,3").ok(), Some(vec![1.0, 2.5, 3.0]));
        assert!(number_list("g", "1,,3").is_err());
    }
}
