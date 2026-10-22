// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Seeing estimation from the capture itself (issue #49).
//!
//! Two independent estimates from tiles of the disc followed through a sample
//! of all the frames: the absolute Fried parameter from the spectral ratio
//! ([`spectral`], von der Luehe 1984) and a relative index from how much
//! neighbouring alignment points move against each other ([`motion`]).
//!
//! # References
//!
//! - Fried, D. L. (1966), Optical resolution through a randomly inhomogeneous medium for very long and very
//!   short exposures, J. Opt. Soc. Am. 56, 1372-1379, doi:10.1364/JOSA.56.001372 (the parameter `r0`).
//! - von der Luehe, O. (1984), Estimating Fried's parameter from a time series of an arbitrary resolved
//!   object, J. Opt. Soc. Am. A 1, 510-519, doi:10.1364/josaa.1.000510.
//! - Scharmer, G. B. and van Werkhoven, T. I. M. (2010), S-DIMM+ height characterization of day-time seeing
//!   using solar granulation, Astron. Astrophys. 513, A25, doi:10.1051/0004-6361/200913791.

#![allow(
    clippy::many_single_char_names,
    reason = "x, y, n, f, u and the frequencies are the natural names in this maths"
)]
#![allow(
    clippy::cast_possible_wrap,
    reason = "pixel coordinates and grid indices are far below the i64 range"
)]

pub mod motion;
pub mod probe;
pub mod screen;
pub mod spectral;
#[cfg(any(test, feature = "testutil"))]
pub mod synth;
pub mod table;

/// The values of `D / r0` the theory table has rows for.
pub const RHO_GRID: [f32; 15] = [
    0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 10.0, 12.0, 14.0, 17.0, 20.0,
];

/// Fried parameter at 500 nm, from `r0` measured at `wavelength_nm` (it scales
/// as the wavelength to the power 6/5). Same unit in and out.
#[must_use]
pub fn r0_at_500(r0: f32, wavelength_nm: f32) -> f32 {
    r0 * (500.0 / wavelength_nm).powf(1.2)
}

/// How many frames of the capture the probe follows, spread evenly over all of
/// it (not the best ones: the seeing of the session, not of its lucky moments).
pub const SAMPLE_FRAMES: usize = 120;

/// Fewest frames the spectral ratio is made from.
pub const MIN_SPECTRAL_FRAMES: usize = 40;

/// A patch of the Sun larger than this, in arcseconds, is not one isoplanatic
/// patch (the daytime isoplanatic angle is a few arcseconds). The method is
/// validated for a tile with one transfer function; on a wider one r0 is
/// reported as indicative and not graded.
pub const ISOPLANATIC_ARCSEC: f32 = 15.0;

/// What the seeing estimate is told about the optics.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SeeingSettings {
    /// Telescope aperture in millimetres; without it only the motion index is made.
    pub aperture_mm: Option<f32>,
    /// Wavelength of the capture in nanometres (the filter's).
    pub wavelength_nm: Option<f32>,
}

/// The wavelength of a filter preset for `r0`, in nanometres (550 for white light).
#[must_use]
pub fn preset_wavelength_nm(preset: crate::Preset) -> f32 {
    use crate::Preset;
    match preset {
        Preset::Ha => 656.28,
        Preset::CaK => 393.37,
        Preset::CaH => 396.85,
        Preset::Mg => 517.27,
        Preset::Na => 589.3,
        Preset::HeD3 => 587.56,
        Preset::Hbeta => 486.13,
        Preset::White => 550.0,
    }
}

/// A seeing grade from the Fried parameter at 500 nm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Grade {
    /// Under 2.5 cm.
    Bad,
    /// 2.5 to 4 cm.
    Poor,
    /// 4 to 7 cm.
    Fair,
    /// 7 to 10 cm.
    Good,
    /// 10 cm or more.
    Excellent,
}

impl Grade {
    /// The grade of a Fried parameter at 500 nm, in centimetres. Day-time
    /// solar seeing: a few centimetres is poor, ten is excellent.
    #[must_use]
    pub fn from_r0_500_cm(r0: f32) -> Self {
        match r0 {
            r if r >= 10.0 => Self::Excellent,
            r if r >= 7.0 => Self::Good,
            r if r >= 4.0 => Self::Fair,
            r if r >= 2.5 => Self::Poor,
            _ => Self::Bad,
        }
    }

    /// One word.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Bad => "bad",
            Self::Poor => "poor",
            Self::Fair => "fair",
            Self::Good => "good",
            Self::Excellent => "excellent",
        }
    }
}

/// The absolute estimate from the spectral ratio.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpectralFit {
    /// `D / r0` at the wavelength of the capture.
    pub d_over_r0: f32,
    /// `r0` at the wavelength of the capture, centimetres.
    pub r0_cm: f32,
    /// `r0` scaled to 500 nm, centimetres.
    pub r0_500_cm: f32,
    /// Rings of the spectrum the fit used.
    pub rings: usize,
    /// Tiles that went into it.
    pub tiles: usize,
    /// Relative scatter of `D / r0` between tiles (standard deviation over
    /// mean of each tile fitted alone); `None` when fewer than three tiles fit.
    pub spread: Option<f32>,
    /// Diffraction cut-off in cycles per pixel the frequencies were scaled by.
    pub cutoff: f32,
}

/// The seeing of a capture, estimated from the capture.
#[derive(Clone, Debug, PartialEq)]
pub struct SeeingReport {
    /// Frames followed.
    pub frames: usize,
    /// Tiles followed.
    pub tiles: usize,
    /// The relative index from the alignment point shifts.
    pub motion: Option<motion::MotionIndex>,
    /// Image scale in arcseconds per pixel, when the limb gave one.
    pub arcsec_per_px: Option<f32>,
    /// The absolute `r0` from the spectral ratio.
    pub spectral: Option<SpectralFit>,
    /// The grade, from the absolute estimate.
    pub grade: Option<Grade>,
    /// What limits or qualifies the figures.
    pub notes: Vec<String>,
    /// The differential motion of each frame followed, in capture order
    /// ([`motion::motion_series`]); empty when the frames are unknown.
    pub series: Vec<MotionSample>,
}

/// The differential motion of one frame followed by the seeing probe.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MotionSample {
    /// Index of the frame in the capture.
    pub frame: usize,
    /// Differential motion per axis in pixels, on the scale of
    /// [`motion::MotionIndex::rms_px`].
    pub rms_px: f32,
}

impl SeeingReport {
    /// The report as lines for the command line.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut v = vec![format!(
            "followed {} tiles of {} px through {} frames spread over the capture",
            self.tiles,
            spectral::TILE,
            self.frames
        )];
        if let Some(m) = &self.motion {
            let arc = self
                .arcsec_per_px
                .map_or(String::new(), |s| format!(" ({:.2} arcsec)", m.rms_px * s));
            let sep = self.arcsec_per_px.map_or(String::new(), |s| {
                format!(" ({:.0} arcsec)", m.separation_px * s)
            });
            v.push(format!(
                "motion index:   {:.3} px{arc} RMS differential motion per axis between points {:.0} px{sep} apart ({} pairs); relative, not r0",
                m.rms_px, m.separation_px, m.pairs
            ));
        } else {
            v.push("motion index:   not enough points locked".into());
        }
        if let Some(s) = &self.spectral {
            let spread = s.spread.map_or(String::new(), |p| {
                format!(", tiles differ by {:.0}%", p * 100.0)
            });
            v.push(format!(
                "spectral ratio: D/r0 {:.1}, r0 {:.1} cm at the capture's wavelength, {:.1} cm at 500 nm ({} rings, {} tiles{spread})",
                s.d_over_r0, s.r0_cm, s.r0_500_cm, s.rings, s.tiles
            ));
        }
        if let Some(g) = self.grade {
            v.push(format!("seeing grade:   {}", g.label()));
        }
        v.extend(self.notes.iter().map(|n| format!("note:           {n}")));
        v
    }
}

/// Turn what the probe measured into a report. `scale` is the image scale in
/// arcseconds per pixel (from the limb), `settings` what is known of the optics.
#[must_use]
pub fn report(
    probe: &probe::ProbeResult,
    scale: Option<f32>,
    settings: &SeeingSettings,
) -> SeeingReport {
    let mut notes = Vec::new();
    let mut spectral_fit = None;
    if probe.frames < MIN_SPECTRAL_FRAMES {
        notes.push(format!(
            "only {} frames were followed: the spectral ratio wants about {MIN_SPECTRAL_FRAMES} or more, so r0 is too noisy to report",
            probe.frames
        ));
    }
    match (settings.aperture_mm, settings.wavelength_nm, scale) {
        _ if probe.frames < MIN_SPECTRAL_FRAMES => {}
        (Some(d), Some(nm), Some(s)) if d > 0.0 && nm > 0.0 && s > 0.0 => {
            let cutoff = crate::granulation::cutoff_cycles_per_px(d, nm, s);
            let range = spectral::FitRange::new(cutoff);
            if let Some((rho, rings)) = spectral::fit_rho(&probe.rings, &range) {
                let r0_cm = d / rho / 10.0;
                let tile_arcsec = spectral::TILE as f32 * s;
                if tile_arcsec > ISOPLANATIC_ARCSEC {
                    notes.push(format!(
                        "a {} px tile spans {tile_arcsec:.0} arcsec, more than one isoplanatic patch (a few arcseconds); the method is validated for one transfer function per tile, so r0 is indicative and no grade is given",
                        spectral::TILE
                    ));
                }
                if rho <= RHO_GRID[0] * 1.02 {
                    notes.push("D/r0 is at the lowest the table holds (0.5): the turbulence is too weak to measure".into());
                }
                let spread = tile_spread(&probe.per_tile, &range);
                spectral_fit = Some(SpectralFit {
                    d_over_r0: rho,
                    r0_cm,
                    r0_500_cm: r0_at_500(r0_cm, nm),
                    rings,
                    tiles: probe.tiles,
                    spread,
                    cutoff,
                });
                if cutoff > crate::granulation::NYQUIST {
                    notes.push(format!(
                        "the cut-off ({cutoff:.2} cycles/px) is above the sampling limit, so fine detail is aliased and the r0 is indicative; a scale of {:.2} arcsec per pixel or finer avoids it",
                        crate::granulation::NYQUIST / cutoff * s
                    ));
                }
                if rho > 12.0 {
                    notes.push(
                        "D/r0 above 12: the spectral ratio loses sensitivity and reads low".into(),
                    );
                }
            } else {
                notes.push(
                    "the spectral ratio had too few frequencies above the noise to fit".into(),
                );
            }
        }
        (None, ..) => {
            notes.push("give the telescope aperture (--aperture-mm) for r0 and a grade".into());
        }
        (_, _, None) => {
            notes.push("no solar limb was found, so the image scale and r0 are unknown".into());
        }
        _ => notes.push("the wavelength of the filter is unknown, so r0 cannot be scaled".into()),
    }
    SeeingReport {
        frames: probe.frames,
        tiles: probe.tiles,
        motion: probe.motion,
        arcsec_per_px: scale,
        grade: spectral_fit
            .filter(|_| scale.is_some_and(|s| spectral::TILE as f32 * s <= ISOPLANATIC_ARCSEC))
            .map(|s| Grade::from_r0_500_cm(s.r0_500_cm)),
        spectral: spectral_fit,
        notes,
        series: Vec::new(),
    }
}

/// Standard deviation over mean of the tiles' own fits.
fn tile_spread(tiles: &[Vec<spectral::Ring>], range: &spectral::FitRange) -> Option<f32> {
    let fits: Vec<f32> = tiles
        .iter()
        .filter_map(|t| spectral::fit_rho(t, range).map(|f| f.0))
        .collect();
    if fits.len() < 3 {
        return None;
    }
    let mean = fits.iter().sum::<f32>() / fits.len() as f32;
    let var = fits.iter().map(|f| (f - mean).powi(2)).sum::<f32>() / (fits.len() - 1) as f32;
    Some(var.sqrt() / mean)
}
