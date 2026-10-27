// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! From what the user points at to a ready [`Calibration`].
//!
//! The command line, the API and the app all say the same thing: "use this
//! dark, this flat, this bad-pixel map". Each may be a master file (FITS) or
//! raw calibration frames (a SER, AVI or video file, a FITS still, or a
//! folder of them), which are combined on the spot with the same code
//! `helia calibrate` runs. This module turns that into a [`Calibration`].

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use crate::error::{Error, Result};
use crate::io::source::OpenOptions;

use super::badpix::{self, BadPixelMap, DetectOptions};
use super::master::{BuildOptions, CalInput, Combine, Master, MasterKind, build_dark, build_flat};
use super::selfflat::{self, SelfFlatOptions, build_selfflat};
use super::{ApplyOptions, Calibration, diag};

/// Where the masters come from.
#[derive(Clone, Debug)]
pub struct CalibrationSpec {
    /// Master dark, or the frames to make one from.
    pub dark: Option<PathBuf>,
    /// Master bias, or its frames. Only used to scale the dark by exposure.
    pub bias: Option<PathBuf>,
    /// Master flat, or the frames to make one from.
    pub flat: Option<PathBuf>,
    /// Dark frames at the exposure of the flats (a master, or frames).
    /// Without it a flat built here keeps its offset.
    pub dark_flat: Option<PathBuf>,
    /// Captures of the Sun at many places on the sensor (the mount off
    /// tracking, or nudged between frames): a flat is solved from them, dust and
    /// fringes included, instead of from `flat`. See [`super::selfflat`].
    pub displaced: Option<PathBuf>,
    /// With `displaced`, keep every scale of the solution instead of only the
    /// structure finer than the displacements allow (see [`selfflat::Smooth`]).
    pub displaced_keep_smooth: bool,
    /// Bad-pixel map: a FITS map or an `x y` text list.
    pub bad_pixels: Option<PathBuf>,
    /// Find the hot and cold pixels in the dark and the flat, and correct
    /// them, when no map is given. Default on.
    pub cosmetic: bool,
    /// Scale the dark by the ratio of the exposures.
    pub scale_dark: bool,
    /// How frames are combined into masters.
    pub combine: Combine,
    /// Most frames used for a master; 0 for as many as fit in memory.
    pub max_frames: usize,
    /// How calibration captures are read.
    pub open: OpenOptions,
    /// Bad-pixel thresholds.
    pub detect: DetectOptions,
}

impl Default for CalibrationSpec {
    fn default() -> Self {
        Self {
            dark: None,
            bias: None,
            flat: None,
            dark_flat: None,
            displaced: None,
            displaced_keep_smooth: false,
            bad_pixels: None,
            cosmetic: true,
            scale_dark: false,
            combine: Combine::default(),
            max_frames: 0,
            open: OpenOptions::default(),
            detect: DetectOptions::default(),
        }
    }
}

/// A calibration ready to use, and what to tell the user about it.
#[derive(Clone, Debug)]
pub struct Resolved {
    /// The masters.
    pub calibration: Calibration,
    /// Warnings: a flat with rings, a dark of another exposure, files skipped.
    pub warnings: Vec<String>,
}

fn is_master_file(p: &Path) -> bool {
    p.is_file()
        && p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "fit" | "fits" | "fts"))
}

impl CalibrationSpec {
    /// True when nothing is asked for.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.dark.is_none()
            && self.flat.is_none()
            && self.displaced.is_none()
            && self.bad_pixels.is_none()
    }

    /// Build or read every master. `light_exposure_s` is the exposure of the
    /// frames the masters will be applied to, when known.
    ///
    /// # Errors
    /// An unreadable file, frames of different sizes, a bad-pixel list
    /// without a master to give its frame size, or cancellation.
    #[allow(
        clippy::too_many_lines,
        reason = "the masters one after the other, then the bad-pixel map"
    )]
    pub fn resolve(
        &self,
        light_exposure_s: Option<f64>,
        progress: &dyn Fn(&str, f32),
        cancel: &AtomicBool,
    ) -> Result<Resolved> {
        let opts = BuildOptions {
            combine: self.combine,
            max_frames: self.max_frames,
            ..BuildOptions::default()
        };
        let mut warnings = Vec::new();
        let mut dark_for = |kind: MasterKind, path: &Path, what: &str| -> Result<Master> {
            if is_master_file(path) {
                return Master::read(path, kind);
            }
            let mut input = CalInput::open(&[path.to_path_buf()], &self.open)?;
            warnings.extend(input.notes.iter().cloned());
            build_dark(kind, &mut input, &opts, &|f| progress(what, f), cancel)
        };
        let dark = self
            .dark
            .as_deref()
            .map(|p| dark_for(MasterKind::Dark, p, "building the dark"))
            .transpose()?;
        let bias = self
            .bias
            .as_deref()
            .map(|p| dark_for(MasterKind::Bias, p, "building the bias"))
            .transpose()?;
        let dark_flat = self
            .dark_flat
            .as_deref()
            .map(|p| dark_for(MasterKind::Dark, p, "building the dark flat"))
            .transpose()?;
        let flat = match self.flat.as_deref().or(self.displaced.as_deref()) {
            None => None,
            Some(p) if self.flat.is_none() => {
                let mut input = CalInput::open(&[p.to_path_buf()], &self.open)?;
                warnings.extend(input.notes.iter().cloned());
                let few = BuildOptions {
                    max_frames: if opts.max_frames == 0 {
                        selfflat::DEFAULT_FRAMES
                    } else {
                        opts.max_frames
                    },
                    ..opts
                };
                let built = build_selfflat(
                    &mut input,
                    dark.as_ref(),
                    &few,
                    &SelfFlatOptions {
                        smooth: if self.displaced_keep_smooth {
                            selfflat::Smooth::Keep
                        } else {
                            selfflat::Smooth::Auto
                        },
                        ..SelfFlatOptions::default()
                    },
                    &|f| progress("solving the flat from the displaced frames", f),
                    cancel,
                )?;
                warnings.extend(built.warnings);
                Some(built.master)
            }
            Some(p) if is_master_file(p) => {
                let m = Master::read(p, MasterKind::Flat)?;
                let d = diag::analyse(&m.frame);
                warnings.extend(d.warnings);
                Some(m)
            }
            Some(p) => {
                let mut input = CalInput::open(&[p.to_path_buf()], &self.open)?;
                warnings.extend(input.notes.iter().cloned());
                let built = build_flat(
                    &mut input,
                    dark_flat.as_ref().or(bias.as_ref()),
                    &opts,
                    &|f| progress("building the flat", f),
                    cancel,
                )?;
                warnings.extend(built.warnings);
                Some(built.master)
            }
        };

        let size = dark
            .as_ref()
            .or(flat.as_ref())
            .map(|m| (m.frame.width, m.frame.height));
        let mosaic = dark
            .as_ref()
            .or(flat.as_ref())
            .is_some_and(|m| m.frame.bayer.is_some() || m.info.bayer.is_some());
        let bad = match (&self.bad_pixels, size) {
            (Some(p), Some((w, h))) => Some(BadPixelMap::load(p, w, h)?),
            (Some(p), None) if is_master_file(p) => Some(BadPixelMap::read(p)?),
            (Some(p), None) => {
                return Err(Error::Invalid(format!(
                    "{} is a list of pixels: give a dark or a flat too, so the frame size is known",
                    p.display()
                )));
            }
            (None, _) if self.cosmetic && (dark.is_some() || flat.is_some()) => {
                let map = badpix::detect(
                    dark.as_ref().map(|m| &m.frame),
                    flat.as_ref().map(|m| &m.frame),
                    mosaic,
                    &self.detect,
                    "the dark and flat",
                );
                (!map.indices.is_empty()).then_some(map)
            }
            (None, _) => None,
        };

        Ok(Resolved {
            calibration: Calibration {
                dark,
                bias,
                flat,
                bad,
                options: ApplyOptions {
                    scale_dark: self.scale_dark,
                    light_exposure_s,
                    correct_bad: true,
                },
            },
            warnings,
        })
    }
}
