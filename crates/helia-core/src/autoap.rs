// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Alignment point size, spacing and search range chosen from the capture.
//!
//! Two things are measured. The image scale (arcseconds per pixel) comes from
//! the fitted limb: the Sun is 959.6 arcseconds in radius. The seeing comes
//! from a short probe before the real alignment: a sample of the frames that
//! will be stacked is registered globally and a coarse grid of points is
//! measured in each. What matters for the choice is how far a point's own
//! shift strays from the frame's median shift, the local warp that multipoint
//! alignment exists to follow. It is the root mean square over the sampled
//! frames and points, in pixels.
//!
//! The window then follows the size of the detail in arcseconds, with the
//! usual half-window spacing, and the search range covers the warp. Without a
//! limb (so no scale) the caller's fallback size is the window.
//!
//! What the real captures say (ZWO ASI482MC, H-alpha seen through the green
//! plane, 2.2 to 4.3 arcsec per pixel, see the user guide): 32 pixels beat 48, 64 and 96 on all four; below
//! 32 the granulation band reads higher but so does the meso band on the
//! capture with a partial limb, which is blocking, not detail, so 32 stays the
//! floor; spacing denser than half a window changed nothing, and wider spacing
//! lost detail. The seeing therefore sets the search range only.

use crate::align::Shift;
use crate::align::multipoint::{ApMeasure, MultipointConfig};
use crate::colour::{GRANULE_ARCSEC, GRANULES_PER_AP, Preset};
use crate::io::solarnet::solar_radius_arcsec;

/// Window edge for photospheric detail, in arcseconds: 2.5 granules.
pub const PHOTOSPHERE_AP_ARCSEC: f32 = GRANULES_PER_AP * GRANULE_ARCSEC;

/// Window edge for chromospheric detail (fibrils, filaments, plage), in
/// arcseconds. Measured on four H-alpha captures (2.2 and 4.3 arcsec per
/// pixel) and a calcium K one (2.0): windows from 35 to 300 arcsec gave
/// fibril-scale contrast within 5% of each other, 90 arcsec among the best, so
/// it stays.
pub const CHROMOSPHERE_AP_ARCSEC: f32 = 90.0;

/// Smallest and largest window edge chosen, in pixels.
pub const MIN_SIZE: usize = 32;
/// See [`MIN_SIZE`].
pub const MAX_SIZE: usize = 96;

/// Smallest and largest search radius chosen, in pixels.
pub const MIN_SEARCH: usize = 4;
/// See [`MIN_SEARCH`].
pub const MAX_SEARCH: usize = 12;

/// Window edge of the probe grid, in pixels.
pub const PROBE_SIZE: usize = 48;

/// Frames measured by the probe.
pub const PROBE_FRAMES: usize = 12;

/// A probe point needs at least this many neighbours measured in a frame for
/// that frame to count.
const MIN_POINTS: usize = 6;

/// What the user asked for when they asked for automatic alignment points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AutoAp {
    /// Window edge for the detail in view, in arcseconds.
    pub target_arcsec: f32,
    /// Window edge in pixels when the image scale is unknown.
    pub fallback_size: usize,
}

impl AutoAp {
    /// Settings for photospheric detail (white light, Na D): granulation.
    #[must_use]
    pub fn photosphere(fallback_size: usize) -> Self {
        Self {
            target_arcsec: PHOTOSPHERE_AP_ARCSEC,
            fallback_size,
        }
    }

    /// Settings for chromospheric detail.
    #[must_use]
    pub fn chromosphere(fallback_size: usize) -> Self {
        Self {
            target_arcsec: CHROMOSPHERE_AP_ARCSEC,
            fallback_size,
        }
    }

    /// The settings for a filter: photospheric ones for white light and Na D,
    /// chromospheric for everything else, including no known filter.
    #[must_use]
    pub fn for_preset(preset: Option<Preset>, fallback_size: usize) -> Self {
        if preset.is_some_and(Preset::is_photospheric) {
            Self::photosphere(fallback_size)
        } else {
            Self::chromosphere(fallback_size)
        }
    }
}

/// How much the sky moved the picture between one point and the next.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seeing {
    /// Root mean square distance of a point's shift from the median shift of
    /// its frame, in pixels.
    pub warp_px: f32,
    /// Frames that counted.
    pub frames: usize,
    /// Point measurements that counted.
    pub points: usize,
}

impl Seeing {
    /// The warp over `rows`, one row per probed frame holding the measurement
    /// of each point that locked. Frames with fewer than a handful of points
    /// are ignored; `None` when none is left.
    #[must_use]
    pub fn from_rows(rows: &[Vec<ApMeasure>]) -> Option<Self> {
        let (mut sum, mut points, mut frames) = (0.0f64, 0usize, 0usize);
        for row in rows {
            let good: Vec<Shift> = row
                .iter()
                .filter(|m| !m.interpolated && m.correlation >= 0.5)
                .map(|m| m.shift)
                .collect();
            if good.len() < MIN_POINTS {
                continue;
            }
            let (mx, my) = (
                median(good.iter().map(|s| s.dx).collect()),
                median(good.iter().map(|s| s.dy).collect()),
            );
            sum += good
                .iter()
                .map(|s| f64::from((s.dx - mx).powi(2) + (s.dy - my).powi(2)))
                .sum::<f64>();
            points += good.len();
            frames += 1;
        }
        (points > 0).then(|| Self {
            warp_px: (sum / points as f64).sqrt() as f32,
            frames,
            points,
        })
    }

    /// The warp in arcseconds, for an image scale in arcseconds per pixel.
    #[must_use]
    pub fn warp_arcsec(&self, scale: f32) -> f32 {
        self.warp_px * scale
    }
}

fn median(mut v: Vec<f32>) -> f32 {
    v.sort_by(f32::total_cmp);
    v[v.len() / 2]
}

/// Image scale in arcseconds per pixel from a fitted limb radius in pixels.
#[must_use]
pub fn arcsec_per_px(disc_radius_px: f32) -> Option<f32> {
    (disc_radius_px.is_finite() && disc_radius_px >= 16.0)
        .then(|| solar_radius_arcsec(1.0) as f32 / disc_radius_px)
}

/// What was chosen and what it was based on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ApChoice {
    /// Window edge, pixels.
    pub size: usize,
    /// Distance between window centres, pixels.
    pub step: usize,
    /// Search radius around the global shift, pixels.
    pub search_radius: usize,
    /// Image scale in arcseconds per pixel, when a limb was fitted.
    pub scale: Option<f32>,
    /// The probe's result, when it found one.
    pub seeing: Option<Seeing>,
}

impl ApChoice {
    /// The multipoint settings with these choices applied.
    #[must_use]
    pub fn apply(&self, base: MultipointConfig) -> MultipointConfig {
        MultipointConfig {
            ap_size: self.size,
            step: self.step,
            search_radius: self.search_radius,
            ..base
        }
    }

    /// One line saying what was chosen and why.
    #[must_use]
    pub fn summary(&self) -> String {
        let scale = self.scale.map_or_else(
            || "image scale unknown".to_owned(),
            |s| format!("{s:.2} arcsec per pixel"),
        );
        let seeing = self.seeing.map_or_else(
            || "seeing not measured".to_owned(),
            |s| match self.scale {
                Some(sc) => format!(
                    "local warp {:.2} px ({:.2} arcsec)",
                    s.warp_px,
                    s.warp_arcsec(sc)
                ),
                None => format!("local warp {:.2} px", s.warp_px),
            },
        );
        format!(
            "alignment points {} px every {} px, search {} px ({scale}; {seeing})",
            self.size, self.step, self.search_radius
        )
    }
}

/// Choose the alignment point settings.
#[must_use]
pub fn choose(auto: AutoAp, scale: Option<f32>, seeing: Option<Seeing>) -> ApChoice {
    let base = match scale {
        Some(s) => auto.target_arcsec / s,
        None => auto.fallback_size as f32,
    };
    let size = ((base / 8.0).round() as usize * 8).clamp(MIN_SIZE, MAX_SIZE);
    let search_radius = seeing.map_or(MIN_SEARCH, |s| {
        ((3.0 * s.warp_px).ceil() as usize + 2).clamp(MIN_SEARCH, MAX_SEARCH)
    });
    ApChoice {
        size,
        step: size / 2,
        search_radius,
        scale,
        seeing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(dx: f32, dy: f32) -> ApMeasure {
        ApMeasure {
            shift: Shift::new(dx, dy),
            quality: 1.0,
            correlation: 0.9,
            interpolated: false,
        }
    }

    #[test]
    fn warp_ignores_the_frames_own_shift() {
        // Every point moved by (5, -3) together: no warp.
        let row: Vec<ApMeasure> = (0..10).map(|_| m(5.0, -3.0)).collect();
        let s = Seeing::from_rows(&[row]).expect("seeing");
        assert!(s.warp_px < 1e-6, "{s:?}");
    }

    #[test]
    fn warp_is_the_spread_of_the_points() {
        // Five points one pixel left of the median, five one pixel right and
        // the median point itself.
        let row: Vec<ApMeasure> = (0..11).map(|i| m(i.cmp(&5) as i32 as f32, 0.0)).collect();
        let s = Seeing::from_rows(&[row]).expect("seeing");
        assert!((s.warp_px - (10.0f32 / 11.0).sqrt()).abs() < 1e-5, "{s:?}");
        assert_eq!((s.frames, s.points), (1, 11));
    }

    #[test]
    fn too_few_points_is_no_measurement() {
        let row: Vec<ApMeasure> = (0..3).map(|_| m(0.0, 0.0)).collect();
        assert_eq!(Seeing::from_rows(&[row]), None);
        assert_eq!(Seeing::from_rows(&[]), None);
    }

    #[test]
    fn interpolated_and_weak_points_do_not_count() {
        let mut row: Vec<ApMeasure> = (0..10).map(|_| m(1.0, 1.0)).collect();
        row[0].interpolated = true;
        row[1].correlation = 0.2;
        let s = Seeing::from_rows(&[row]).expect("seeing");
        assert_eq!(s.points, 8);
    }

    #[test]
    fn the_scale_comes_from_the_limb() {
        let s = arcsec_per_px(480.0).expect("scale");
        assert!((s - 2.0).abs() < 0.01, "{s}");
        assert_eq!(arcsec_per_px(3.0), None);
        assert_eq!(arcsec_per_px(f32::NAN), None);
    }

    #[test]
    fn granulation_windows_follow_the_scale() {
        let a = AutoAp::photosphere(32);
        // 3.75 arcsec at 0.1 arcsec per pixel is 37.5 px: 40.
        assert_eq!(choose(a, Some(0.1), None).size, 40);
        // At 1 arcsec per pixel it is under the floor.
        assert_eq!(choose(a, Some(1.0), None).size, MIN_SIZE);
        // Very fine scales stop at the ceiling.
        assert_eq!(choose(a, Some(0.01), None).size, MAX_SIZE);
    }

    #[test]
    fn without_a_scale_the_fallback_is_the_start() {
        let a = AutoAp::photosphere(40);
        let c = choose(a, None, None);
        assert_eq!((c.size, c.step, c.search_radius), (40, 20, MIN_SEARCH));
    }

    #[test]
    fn the_search_covers_the_warp() {
        let a = AutoAp::photosphere(32);
        let s = Seeing {
            warp_px: 1.5,
            frames: 12,
            points: 500,
        };
        assert_eq!(choose(a, None, Some(s)).search_radius, 7);
        let wild = Seeing { warp_px: 40.0, ..s };
        assert_eq!(choose(a, None, Some(wild)).search_radius, MAX_SEARCH);
    }
}
