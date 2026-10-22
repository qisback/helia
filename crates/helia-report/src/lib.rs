// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Science outputs of a stack: the graphs and tables that astrophotography
//! workflows review (AutoStakkert-style frame quality, alignment points, image
//! science, solar specifics).
//!
//! Every output is a PNG drawn here with `plotters` (pure Rust, no system
//! fonts), the numbers behind it as CSV and JSON, and one self-contained HTML
//! report that inlines the figures. The numbers are the contract: a plot is
//! drawn from its data struct, so `helia plot` can redraw a figure from the
//! JSON a report wrote, and a user can redraw it in Python from the CSV.

#![allow(
    clippy::missing_errors_doc,
    reason = "every fallible function returns this crate's Error, described there"
)]
#![allow(
    clippy::too_many_lines,
    reason = "a plot is one flat sequence of drawing calls"
)]
#![allow(
    clippy::many_single_char_names,
    reason = "x, y, n and the axis limits are the natural names in plotting code"
)]

pub mod ap;
pub mod bundle;
pub mod canvas;
pub mod frame_quality;
pub mod html;
pub mod image_plot;
pub mod image_science;
pub mod settings;
pub mod solar;
pub mod stats;
pub mod timelapse;

pub use ap::ApData;
pub use bundle::{Bundle, Figure, FileKind, ReportFile, Section, Table};
pub use frame_quality::FrameQualityData;
pub use image_science::{Extras, ImageScienceData};
pub use solar::{SolarData, SolarInputs};
pub use timelapse::TimelapseData;

/// What can go wrong while making a report.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Drawing a plot failed.
    #[error("plot: {0}")]
    Plot(String),
    /// A data file could not be read or written.
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// A data file is not the JSON this version writes.
    #[error("data file: {0}")]
    Json(#[from] serde_json::Error),
    /// PNG encoding failed.
    #[error("png: {0}")]
    Png(String),
    /// There is nothing to draw.
    #[error("{0}")]
    NoData(String),
}

/// Result alias of this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// The kinds of plot `helia plot` can draw. One kind is one PNG.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlotKind {
    /// Frame quality, best first, with the keep threshold.
    QualitySorted,
    /// Frame quality in capture order, with the keep threshold.
    QualityOrder,
    /// Histogram of the frame quality scores.
    QualityHistogram,
    /// Global shift of each frame (x and y) and the drift.
    ShiftDrift,
    /// Differential image motion (seeing) over the capture.
    SeeingTime,
    /// The alignment point grid on the reference.
    ApGrid,
    /// Heatmap of the sharpness each alignment point stacks.
    ApQuality,
    /// Heatmap of how much each alignment point moves.
    ApShift,
    /// Vector plot of the displacement field.
    ApField,
    /// Vector plot of the residual against the finished stack.
    ApResidual,
    /// Radially averaged power spectrum of stack, best frame and sharpened image.
    PowerSpectrum,
    /// MTF-like transfer curves between those spectra.
    Mtf,
    /// Limb profile against the fitted limb-darkening law.
    LimbProfile,
    /// The limb's measured blur (point spread function).
    LimbPsf,
    /// Histogram per channel with clipping marked.
    Histogram,
    /// Noise and signal to noise.
    NoiseSnr,
    /// Stonyhurst heliographic grid on the stack.
    GridStonyhurst,
    /// Carrington heliographic grid on the stack.
    GridCarrington,
    /// How each frame of a time-lapse was registered.
    TimelapseRegistration,
    /// Brightness of a time-lapse before and after evening out.
    TimelapseBrightness,
    /// Derotation displacement of a time-lapse.
    TimelapseDerotation,
}

impl PlotKind {
    /// Every kind, in the order a report lists them.
    pub const ALL: [PlotKind; 21] = [
        Self::QualitySorted,
        Self::QualityOrder,
        Self::QualityHistogram,
        Self::ShiftDrift,
        Self::SeeingTime,
        Self::ApGrid,
        Self::ApQuality,
        Self::ApShift,
        Self::ApField,
        Self::ApResidual,
        Self::PowerSpectrum,
        Self::Mtf,
        Self::LimbProfile,
        Self::LimbPsf,
        Self::Histogram,
        Self::NoiseSnr,
        Self::GridStonyhurst,
        Self::GridCarrington,
        Self::TimelapseRegistration,
        Self::TimelapseBrightness,
        Self::TimelapseDerotation,
    ];

    /// The name used on the command line and for the file (`<name>.png`).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::QualitySorted => "quality-sorted",
            Self::QualityOrder => "quality-order",
            Self::QualityHistogram => "quality-histogram",
            Self::ShiftDrift => "shift-drift",
            Self::SeeingTime => "seeing-time",
            Self::ApGrid => "ap-grid",
            Self::ApQuality => "ap-quality",
            Self::ApShift => "ap-shift",
            Self::ApField => "ap-field",
            Self::ApResidual => "ap-residual",
            Self::PowerSpectrum => "power-spectrum",
            Self::Mtf => "mtf",
            Self::LimbProfile => "limb-profile",
            Self::LimbPsf => "limb-psf",
            Self::Histogram => "histogram",
            Self::NoiseSnr => "noise-snr",
            Self::GridStonyhurst => "grid-stonyhurst",
            Self::GridCarrington => "grid-carrington",
            Self::TimelapseRegistration => "timelapse-registration",
            Self::TimelapseBrightness => "timelapse-brightness",
            Self::TimelapseDerotation => "timelapse-derotation",
        }
    }

    /// Parse a command-line name.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.name() == s)
    }
}

impl std::str::FromStr for PlotKind {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, String> {
        Self::parse(s).ok_or_else(|| {
            format!(
                "unknown plot {s:?}; one of {}",
                Self::ALL.map(Self::name).join(", ")
            )
        })
    }
}

/// Everything a report is made from. Each group of outputs adds a field; a
/// field is `None` when its data was not measured.
#[derive(Clone, Debug, Default)]
pub struct ScienceData {
    /// Frame quality, shifts and seeing.
    pub frame_quality: Option<FrameQualityData>,
    /// Alignment points.
    pub ap: Option<ApData>,
    /// Spectra, limb, histograms and noise.
    pub image: Option<ImageScienceData>,
    /// Heliographic grids and sunspots.
    pub solar: Option<SolarData>,
    /// Diagnostics of a time-lapse.
    pub timelapse: Option<TimelapseData>,
}

impl ScienceData {
    /// Gather what a finished stack measured.
    #[must_use]
    pub fn from_output(
        out: &helia_core::pipeline::StackOutput,
        metric: &str,
        extras: &Extras,
    ) -> Self {
        Self {
            frame_quality: Some(FrameQualityData::from_output(out, metric)),
            ap: ApData::from_output(out),
            image: Some(ImageScienceData::from_output(out, extras)),
            solar: SolarData::from_output(out, &extras.solar),
            timelapse: None,
        }
    }

    /// The diagnostics of a registered time-lapse (and nothing else).
    #[must_use]
    pub fn from_sequence(seq: &helia_core::timelapse::Sequence) -> Self {
        Self {
            timelapse: Some(TimelapseData::from_sequence(seq)),
            ..Self::default()
        }
    }

    /// Read the JSON files a report wrote into `dir`. Missing files give `None`.
    pub fn load(dir: &std::path::Path) -> Result<Self> {
        let read = |name: &str| -> Result<Option<String>> {
            match std::fs::read_to_string(dir.join(name)) {
                Ok(s) => Ok(Some(s)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e.into()),
            }
        };
        let frame_quality = read(FrameQualityData::JSON)?
            .map(|s| serde_json::from_str(&s))
            .transpose()?;
        let mut ap: Option<ApData> = read(ApData::JSON)?
            .map(|s| serde_json::from_str(&s))
            .transpose()?;
        if let Some(a) = ap.as_mut() {
            a.backdrop = match std::fs::read(dir.join(ApData::BACKDROP)) {
                Ok(bytes) => Some(image_plot::GreyImage::from_png(&bytes)?),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(e.into()),
            };
        }
        let image: Option<ImageScienceData> = read(ImageScienceData::JSON)?
            .map(|s| serde_json::from_str(&s))
            .transpose()?;
        let mut solar: Option<SolarData> = read(SolarData::JSON)?
            .map(|s| serde_json::from_str(&s))
            .transpose()?;
        if let Some(a) = solar.as_mut() {
            a.backdrop = match std::fs::read(dir.join(SolarData::BACKDROP)) {
                Ok(bytes) => Some(image_plot::GreyImage::from_png(&bytes)?),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(e.into()),
            };
        }
        let timelapse: Option<TimelapseData> = read(TimelapseData::JSON)?
            .map(|s| serde_json::from_str(&s))
            .transpose()?;
        if frame_quality.is_none()
            && ap.is_none()
            && image.is_none()
            && solar.is_none()
            && timelapse.is_none()
        {
            return Err(Error::NoData(format!(
                "{} holds no report data (run `helia stack --report` first)",
                dir.display()
            )));
        }
        Ok(Self {
            frame_quality,
            ap,
            image,
            solar,
            timelapse,
        })
    }

    /// Draw one plot as a PNG.
    pub fn plot(&self, kind: PlotKind, size: canvas::Size) -> Result<Vec<u8>> {
        match kind {
            PlotKind::QualitySorted
            | PlotKind::QualityOrder
            | PlotKind::QualityHistogram
            | PlotKind::ShiftDrift
            | PlotKind::SeeingTime => self
                .frame_quality
                .as_ref()
                .ok_or_else(|| Error::NoData("no frame quality data".into()))?
                .plot(kind, size),
            PlotKind::ApGrid
            | PlotKind::ApQuality
            | PlotKind::ApShift
            | PlotKind::ApField
            | PlotKind::ApResidual => self
                .ap
                .as_ref()
                .ok_or_else(|| {
                    Error::NoData(
                        "no alignment point data (multipoint is off, or the report has none)"
                            .into(),
                    )
                })?
                .plot(kind, size),
            PlotKind::PowerSpectrum
            | PlotKind::Mtf
            | PlotKind::LimbProfile
            | PlotKind::LimbPsf
            | PlotKind::Histogram
            | PlotKind::NoiseSnr => self
                .image
                .as_ref()
                .ok_or_else(|| Error::NoData("no image science data".into()))?
                .plot(kind, size),
            PlotKind::GridStonyhurst | PlotKind::GridCarrington => self
                .solar
                .as_ref()
                .ok_or_else(|| Error::NoData("no solar data (no fitted disc in the stack)".into()))?
                .plot(kind, size),
            PlotKind::TimelapseRegistration
            | PlotKind::TimelapseBrightness
            | PlotKind::TimelapseDerotation => self
                .timelapse
                .as_ref()
                .ok_or_else(|| Error::NoData("no time-lapse data".into()))?
                .plot(kind, size),
        }
    }
}

/// Name of the HTML report in a bundle.
pub const REPORT_HTML: &str = "report.html";

/// Draw everything `data` holds and assemble the bundle, HTML included.
///
/// `fits_header` is a summary of the stack's FITS header (see
/// [`settings::fits_header_summary`]), shown in the report and written as
/// `fits-header.txt`.
pub fn generate(
    data: &ScienceData,
    meta: &html::Meta,
    fits_header: Option<String>,
    size: canvas::Size,
) -> Result<Bundle> {
    let mut bundle = Bundle::default();
    if let Some(fq) = &data.frame_quality {
        fq.build(&mut bundle, size)?;
    }
    if let Some(ap) = &data.ap {
        ap.build(&mut bundle, size)?;
    }
    if let Some(im) = &data.image {
        im.build(&mut bundle, size)?;
    }
    if let Some(so) = &data.solar {
        so.build(&mut bundle, size)?;
    }
    if let Some(t) = &data.timelapse {
        t.build(&mut bundle, size)?;
    }
    if let Some(h) = fits_header {
        bundle.add("fits-header.txt", h.into_bytes());
    }
    let page = html::page(&bundle, meta);
    bundle.add(REPORT_HTML, page.into_bytes());
    Ok(bundle)
}
