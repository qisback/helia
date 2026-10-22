// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Dark, flat and bad-pixel calibration.
//!
//! Dust on the sensor window, etalon fringes and a camera's fixed pattern stay
//! put on the sensor while the Sun moves across it. Local alignment then warps
//! them into false detail, and sharpening makes them worse. So the frames are
//! calibrated first, before they are demosaiced, scored or aligned:
//!
//! ```text
//! calibrated = (raw - dark) / flat      (clamped at zero)
//! bad pixels  = median of their same-colour neighbours
//! ```
//!
//! * [`master`] builds the masters from calibration captures (SER, AVI, video
//!   or FITS stills): a master dark and bias by median or sigma-clipped mean,
//!   and a master flat from dark-subtracted frames, each scaled to its own
//!   level and normalised to 1 within each colour of the mosaic.
//! * [`badpix`] finds hot pixels in a dark, cold and dead ones in a flat, and
//!   stuck ones in the light frames, and corrects them.
//! * [`diag`] looks at a flat for what a flat cannot fix: Newton rings and
//!   etalon fringes (which move with focus), and strong vignetting.
//! * [`spec`] turns "use this dark and this flat" (master files, or the
//!   frames to make them from) into a [`Calibration`].
//! * [`source`] wraps a capture so that every frame it serves, to the CPU
//!   decoder and to the GPU's decode kernel alike, is already calibrated.
//!
//! Calibration works on the plane as stored (the mosaic of a raw colour camera,
//! not a demosaiced image), so a master built from one capture applies to
//! another from the same camera and mode.

pub mod badpix;
pub mod diag;
pub mod master;
pub mod selfflat;
pub mod source;
pub mod spec;

use std::sync::Arc;

use crate::error::{Error, Result};
use crate::frame::{BayerPattern, Frame};
use crate::io::solarnet::ProvStep;

pub use badpix::{BadPixelMap, DetectOptions};
pub use master::{BuildOptions, CalInput, Combine, Master, MasterKind};
pub use source::CalibratedSource;
pub use spec::{CalibrationSpec, Resolved};

/// Flat values below this fraction of the colour's mean are treated as dead
/// pixels, not divided by (a gain of more than 20 would only amplify noise).
pub const MIN_FLAT: f32 = 0.05;

/// How the masters are applied.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ApplyOptions {
    /// Scale the master dark by the ratio of the light and dark exposures.
    /// The offset (a master bias, when there is one) is kept as it is.
    pub scale_dark: bool,
    /// Exposure of the light frames in seconds, for `scale_dark`.
    pub light_exposure_s: Option<f64>,
    /// Replace bad pixels by the median of their neighbours.
    pub correct_bad: bool,
}

impl Default for ApplyOptions {
    fn default() -> Self {
        Self {
            scale_dark: false,
            light_exposure_s: None,
            correct_bad: true,
        }
    }
}

/// The masters to apply to the frames of a stack.
#[derive(Clone, Debug, Default)]
pub struct Calibration {
    /// Master dark: signal with the lens capped.
    pub dark: Option<Master>,
    /// Master bias (offset), used to scale the dark by exposure.
    pub bias: Option<Master>,
    /// Master flat, normalised to a mean of 1 within each colour.
    pub flat: Option<Master>,
    /// Hot, cold and dead pixels.
    pub bad: Option<BadPixelMap>,
    /// How to apply them.
    pub options: ApplyOptions,
}

impl Calibration {
    /// True when there is nothing to apply.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.dark.is_none() && self.flat.is_none() && self.bad.is_none()
    }

    /// Wrap in an [`Arc`] for [`crate::pipeline::StackConfig`].
    #[must_use]
    pub fn shared(self) -> Arc<Self> {
        Arc::new(self)
    }

    /// Check the masters against each other and the frames they will meet, and
    /// work out what to apply.
    ///
    /// `mosaic` says the frames are a raw colour mosaic: same-colour neighbours
    /// are then two pixels apart. `pattern` is the mosaic pattern of the frames
    /// when known.
    ///
    /// # Errors
    /// [`Error::Invalid`] when a master is not the size of the frames or was
    /// made from a different mosaic pattern.
    #[allow(clippy::too_many_lines, reason = "checks, then the three tables")]
    pub fn plan(&self, width: usize, height: usize, pattern: Option<BayerPattern>) -> Result<Plan> {
        let mosaic = pattern.is_some();
        let check = |what: &str, f: &Frame, bayer: Option<BayerPattern>| -> Result<()> {
            if (f.width, f.height) != (width, height) {
                return Err(Error::Invalid(format!(
                    "the {what} is {}x{} but the frames are {width}x{height}: it must come from the same camera, mode and region",
                    f.width, f.height
                )));
            }
            if let (Some(a), Some(b)) = (bayer, pattern)
                && a != b
            {
                return Err(Error::Invalid(format!(
                    "the {what} has a {a:?} mosaic but the frames have {b:?}"
                )));
            }
            Ok(())
        };
        for (what, m) in [
            ("master dark", &self.dark),
            ("master bias", &self.bias),
            ("master flat", &self.flat),
        ] {
            if let Some(m) = m {
                check(what, &m.frame, m.info.bayer)?;
            }
        }
        if let Some(b) = &self.bad
            && (b.width, b.height) != (width, height)
        {
            return Err(Error::Invalid(format!(
                "the bad-pixel map is {}x{} but the frames are {width}x{height}",
                b.width, b.height
            )));
        }

        let n = width * height;
        let mut notes = Vec::new();
        let offset = self.dark.as_ref().map(|dark| {
            let mut k = 1.0f64;
            if self.options.scale_dark {
                match (self.options.light_exposure_s, dark.info.exposure_s) {
                    (Some(l), Some(d)) if d > 0.0 => k = l / d,
                    _ => notes.push(
                        "dark not scaled: the exposure of the lights or of the dark is unknown"
                            .to_owned(),
                    ),
                }
            }
            #[allow(clippy::cast_possible_truncation, reason = "a scale factor near 1")]
            let k = k as f32;
            match (&self.bias, (k - 1.0).abs() > 1e-6) {
                (Some(bias), true) => dark
                    .frame
                    .data
                    .iter()
                    .zip(&bias.frame.data)
                    .map(|(&d, &b)| b + (d - b) * k)
                    .collect::<Vec<f32>>(),
                (None, true) => {
                    notes.push(
                        "dark scaled whole: with no master bias the offset is scaled too"
                            .to_owned(),
                    );
                    dark.frame.data.iter().map(|&d| d * k).collect()
                }
                (_, false) => dark.frame.data.clone(),
            }
        });
        let mut dead: Vec<u32> = Vec::new();
        let gain = self.flat.as_ref().map(|flat| {
            let mut g = vec![1.0f32; n];
            for (i, (&f, o)) in flat.frame.data.iter().zip(g.iter_mut()).enumerate() {
                if f >= MIN_FLAT {
                    *o = 1.0 / f;
                } else {
                    dead.push(i as u32);
                }
            }
            g
        });
        let mut bad: Vec<u32> = self
            .bad
            .as_ref()
            .map_or_else(Vec::new, |b| b.indices.clone());
        if !dead.is_empty() {
            notes.push(format!(
                "{} pixel(s) read under {:.0}% of the flat and are corrected as bad",
                dead.len(),
                MIN_FLAT * 100.0
            ));
            bad.extend(dead);
            bad.sort_unstable();
            bad.dedup();
        }
        let cosmetic = if self.options.correct_bad && !bad.is_empty() {
            Some(badpix::Cosmetic::new(width, height, mosaic, &bad))
        } else {
            None
        };
        Ok(Plan {
            width,
            height,
            offset,
            gain,
            cosmetic,
            notes,
        })
    }

    /// The `PRSTEPn` records for a stack that used these masters.
    #[must_use]
    pub fn provenance(&self) -> Vec<ProvStep> {
        let mut v = Vec::new();
        if let Some(d) = &self.dark {
            let scaled = if self.options.scale_dark {
                "; scaled by exposure"
            } else {
                ""
            };
            v.push(ProvStep::new(
                "DARK-SUBTRACTION",
                "helia-core calibrate",
                format!("{}{scaled}", d.describe()),
            ));
        }
        if let Some(f) = &self.flat {
            v.push(ProvStep::new(
                "FLAT-FIELDING",
                "helia-core calibrate",
                f.describe(),
            ));
        }
        if let Some(b) = &self.bad
            && self.options.correct_bad
        {
            v.push(ProvStep::new(
                "BAD-PIXEL-CORRECTION",
                "helia-core calibrate",
                b.describe(),
            ));
        }
        v
    }

    /// One line per master, for the history of a TIFF or PNG and the
    /// `HISTORY` cards of a FITS file.
    #[must_use]
    pub fn note_lines(&self) -> Vec<String> {
        let mut v = Vec::new();
        if let Some(d) = &self.dark {
            v.push(format!("Calibration: dark subtracted ({})", d.describe()));
        }
        if let Some(f) = &self.flat {
            v.push(format!("Calibration: flat-fielded ({})", f.describe()));
        }
        if let Some(b) = &self.bad
            && self.options.correct_bad
        {
            v.push(format!(
                "Calibration: bad pixels corrected ({})",
                b.describe()
            ));
        }
        v
    }

    /// Short lines for the command line and the app.
    #[must_use]
    pub fn summary_lines(&self) -> Vec<String> {
        let mut v = Vec::new();
        if let Some(d) = &self.dark {
            v.push(format!("dark: {}", d.describe()));
        }
        if let Some(f) = &self.flat {
            v.push(format!("flat: {}", f.describe()));
        }
        if let Some(b) = &self.bad {
            v.push(format!("bad pixels: {}", b.describe()));
        }
        v
    }
}

/// A [`Calibration`] checked against the frames it will meet and compiled to
/// per-pixel tables.
#[derive(Clone, Debug)]
pub struct Plan {
    width: usize,
    height: usize,
    offset: Option<Vec<f32>>,
    gain: Option<Vec<f32>>,
    cosmetic: Option<badpix::Cosmetic>,
    /// Things to tell the user (a dark that could not be scaled, dead pixels
    /// found in the flat).
    pub notes: Vec<String>,
}

impl Plan {
    /// Frame size the plan was made for.
    #[must_use]
    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// Number of pixels the cosmetic correction replaces.
    #[must_use]
    pub fn corrected_pixels(&self) -> usize {
        self.cosmetic.as_ref().map_or(0, badpix::Cosmetic::len)
    }

    /// Calibrate one decoded plane in place: subtract the dark, divide by the
    /// flat, clamp at zero, then replace the bad pixels.
    ///
    /// # Panics
    /// If `data` is not `width * height` long (caller bug).
    pub fn apply(&self, data: &mut [f32]) {
        assert_eq!(data.len(), self.width * self.height);
        match (&self.offset, &self.gain) {
            (Some(o), Some(g)) => {
                for ((v, &o), &g) in data.iter_mut().zip(o).zip(g) {
                    *v = ((*v - o) * g).max(0.0);
                }
            }
            (Some(o), None) => {
                for (v, &o) in data.iter_mut().zip(o) {
                    *v = (*v - o).max(0.0);
                }
            }
            (None, Some(g)) => {
                for (v, &g) in data.iter_mut().zip(g) {
                    *v *= g;
                }
            }
            (None, None) => {}
        }
        if let Some(c) = &self.cosmetic {
            c.apply(data);
        }
    }
}

#[cfg(test)]
mod tests;
