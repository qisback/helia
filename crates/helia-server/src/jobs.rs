// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Job model, bounded worker pool and event stream.
//!
//! Lock order: the manager's job map, then a job's own state. A job's state
//! lock is never held while taking the map lock.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::http::StatusCode;
use helia_core::align::method::AlignMethod;
use helia_core::align::multipoint::MultipointConfig;
use helia_core::align::refine::SmoothConfig;
use helia_core::debayer::{Channel, Debayer, Method};
use helia_core::edit::{EditList, Op, SharpenMethod, Wavelet, sharpen_edits};
use helia_core::io::ImageFormat;
use helia_core::io::capture::{CaptureReport, open_reported_with};
use helia_core::io::ffmpeg::VideoOptions;
use helia_core::io::solarnet::{CaptureInfo, FitsMeta, Site};
use helia_core::io::source::{
    BayerSetting, Endian, FrameSource, MemorySource, OpenOptions, RowOrder,
};
use helia_core::mask::MaskConfig;
use helia_core::output::{OutputPlan, finish, prepare};
use helia_core::pipeline::{ApRefine, Pipeline, StackConfig, Stage};
use helia_core::stack::{RobustKind, StackMode};
use helia_core::{Backend, ColourParams, Error as CoreError, Keep, Metric, Preset};
use helia_gpu::{BackendChoice, select_backend};
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, Semaphore, broadcast};
use utoipa::ToSchema;

use crate::error::{ApiError, ApiResult, ErrorBody};
use crate::imaging::{self, Grey8, PREVIEW_STORE_WIDTH, Stretch};

/// Compute backend request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum BackendArg {
    /// GPU if a real adapter exists, else CPU.
    #[default]
    Auto,
    /// The CPU reference path.
    Cpu,
    /// A GPU (software adapter allowed); fails if none.
    Gpu,
}

/// Colour plane taken from a Bayer capture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ChannelArg {
    /// Red.
    R,
    /// Green.
    G,
    /// Blue.
    B,
    /// Mean of the three planes.
    #[default]
    Lum,
}

impl From<ChannelArg> for Channel {
    fn from(c: ChannelArg) -> Self {
        match c {
            ChannelArg::R => Self::Red,
            ChannelArg::G => Self::Green,
            ChannelArg::B => Self::Blue,
            ChannelArg::Lum => Self::Luminance,
        }
    }
}

/// Outlier-robust combination of the frames at each pixel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum RobustArg {
    /// Weighted mean of every sample.
    #[default]
    None,
    /// Drop samples further than `robust_k` sigma from the mean (default 2.5).
    Sigma,
    /// Huber M-estimator: down-weight samples beyond `robust_k` sigma
    /// (default 1.345).
    Huber,
    /// Winsorised mean: clamp samples to `robust_k` sigma (default 2).
    Winsor,
}

/// Demosaicing method.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum DebayerArg {
    /// Bilinear.
    Bilinear,
    /// Malvar-He-Cutler.
    #[default]
    Mhc,
}

/// Sharpness metric.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum MetricArg {
    /// Gradient energy.
    #[default]
    Gradient,
    /// Laplacian variance.
    Laplacian,
    /// Helmli and Scherer's mean (CPU only).
    Helmli,
    /// Median-filter gradient similarity (CPU only).
    Mfgs,
    /// DCT energy ratio (CPU only).
    Dct,
    /// Band-limited Fourier power less a noise floor (CPU only).
    Fourier,
    /// Mean local RMS contrast (CPU only).
    Contrast,
}

impl MetricArg {
    /// The core metric this names.
    fn metric(self) -> Metric {
        match self {
            Self::Gradient => Metric::GradientEnergy,
            Self::Laplacian => Metric::LaplacianVariance,
            Self::Helmli => Metric::HelmliScherer,
            Self::Mfgs => Metric::Mfgs,
            Self::Dct => Metric::DctEnergyRatio,
            Self::Fourier => Metric::FourierBand,
            Self::Contrast => Metric::LocalContrast,
        }
    }
}

/// A rectangle of the frame, in pixels from the top-left corner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Region {
    /// Left edge.
    pub x: usize,
    /// Top edge.
    pub y: usize,
    /// Width; at least 1.
    pub w: usize,
    /// Height; at least 1.
    pub h: usize,
}

/// How to undo the field rotation of an altazimuth mount.
#[derive(Clone, Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, default)]
pub struct FieldRotationConfig {
    /// The picture is a mirror image (an odd number of reflections): the turn
    /// is the other way.
    pub mirror: bool,
    /// The orientation every frame is turned to: `middle` (default), `first`
    /// or `last`.
    pub reference: Option<String>,
}

impl FieldRotationConfig {
    fn reference(&self) -> ApiResult<helia_core::field_rotation::Reference> {
        self.reference.as_deref().map_or(
            Ok(helia_core::field_rotation::Reference::default()),
            |s| {
                s.parse().map_err(|_| {
                    invalid("`field_rotation.reference` must be `middle`, `first` or `last`")
                })
            },
        )
    }
}

/// Tuning of the sunspot detection; every field is optional.
#[derive(Clone, Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, default)]
pub struct SunspotsConfig {
    /// Umbra threshold, a share of the quiet disc (0 to 1). Default 0.55.
    pub umbra: Option<f32>,
    /// Penumbra threshold, a share of the quiet disc (0 to 1). Default 0.88.
    pub penumbra: Option<f32>,
    /// Smallest spot, millionths of the hemisphere. Default 2.
    pub min_area: Option<f32>,
    /// Largest spot, millionths of the hemisphere; bigger dark regions are
    /// left out as a cloud or vignette. Default 6000.
    pub max_area: Option<f32>,
    /// Spots closer than this on the Sun, degrees, form one group. Default 3.
    pub group_gap_deg: Option<f32>,
    /// Observer factor k of the Wolf number. Default 1.
    pub wolf_k: Option<f32>,
    /// Counter-clockwise angle from the top of the image to solar north,
    /// degrees. Default 0.
    pub north_angle_deg: Option<f32>,
    /// The image has celestial north up: add the position angle P of the Sun
    /// at the capture time to `north_angle_deg`.
    pub celestial_north_up: bool,
    /// The image is a mirror image.
    pub mirror: bool,
}

impl SunspotsConfig {
    fn params(&self) -> helia_core::sunspots::Params {
        let d = helia_core::sunspots::Params::default();
        helia_core::sunspots::Params {
            umbra: self.umbra.unwrap_or(d.umbra),
            penumbra: self.penumbra.unwrap_or(d.penumbra),
            min_msh: self.min_area.unwrap_or(d.min_msh),
            max_msh: self.max_area.unwrap_or(d.max_msh),
            group_gap_deg: self.group_gap_deg.unwrap_or(d.group_gap_deg),
            k: self.wolf_k.unwrap_or(d.k),
            rotate_deg: self.north_angle_deg.unwrap_or(0.0),
            mirror: self.mirror,
            celestial_north_up: self.celestial_north_up,
        }
    }

    #[allow(
        clippy::nonminimal_bool,
        reason = "a NaN threshold must fail every comparison, so the negation stays"
    )]
    fn check(&self) -> ApiResult<()> {
        let p = self.params();
        if !(p.umbra > 0.0 && p.umbra < p.penumbra && p.penumbra < 1.0) {
            return Err(invalid(
                "`sunspots`: thresholds must satisfy 0 < umbra < penumbra < 1",
            ));
        }
        if !(p.min_msh.is_finite() && p.min_msh >= 0.0 && p.max_msh > p.min_msh) {
            return Err(invalid(
                "`sunspots`: `min_area` and `max_area` are not a range",
            ));
        }
        if !(p.group_gap_deg.is_finite() && (0.0..=90.0).contains(&p.group_gap_deg)) {
            return Err(invalid("`sunspots`: `group_gap_deg` must be 0 to 90"));
        }
        if !(p.k.is_finite() && p.k > 0.0) || !p.rotate_deg.is_finite() {
            return Err(invalid(
                "`sunspots`: `wolf_k` and the angle must be finite, k positive",
            ));
        }
        Ok(())
    }
}

/// The same knobs as `helia stack`.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is an independent switch of the request"
)]
#[serde(deny_unknown_fields, default)]
pub struct JobConfig {
    /// Share (`"25%"`) or count (`"500"`) of frames to keep. Default `25%`.
    pub keep: Option<String>,
    /// Number of frames to keep; excludes `keep`.
    pub keep_frames: Option<usize>,
    /// Alignment point size in pixels (8 to 1024). Absent: chosen from the
    /// image size.
    pub ap_size: Option<usize>,
    /// Choose the alignment point size, spacing and search range from the
    /// image scale (the fitted limb) and the seeing (a probe of a dozen
    /// frames). Excludes `ap_size`; needs `multipoint`. What was chosen comes
    /// back in `summary.auto_ap`.
    pub ap_auto: bool,
    /// Alignment point passes, 1 or 2. 2 stacks once and measures the points
    /// again against that stack. Default 1; needs `multipoint`.
    pub ap_passes: Option<u8>,
    /// Replace alignment point shifts that disagree with their neighbours by
    /// the neighbours' median. Needs `multipoint`.
    pub ap_smooth: bool,
    /// Measure the alignment points once more against the finished stack, for
    /// the residual figure of the `report` (`ap-residual.png`). Costs one more
    /// registration pass. Needs `multipoint`.
    pub ap_residual: bool,
    /// Crop the result to the solar disc plus `autocrop_margin`, FITS too. A
    /// capture without a clear disc is left uncropped; `summary.crop` is then
    /// absent.
    pub autocrop: bool,
    /// Estimate the seeing from the capture (`summary.seeing`): the Fried
    /// parameter from the spectral ratio, which needs `aperture_mm`, and a
    /// relative alignment point motion index.
    pub seeing: bool,
    /// Telescope aperture in millimetres, for the diffraction cut-off in
    /// `summary.granulation` and for `r0` in `summary.seeing`.
    pub aperture_mm: Option<f32>,
    /// Margin of `autocrop` as a percentage of the disc radius, 0 to 200.
    /// Default 10.
    pub autocrop_margin: Option<f32>,
    /// Undo the field rotation of an altazimuth mount: every frame is turned to
    /// one orientation from the parallactic angle of the Sun at its SER
    /// timestamp, before it is scored, aligned or stacked. Needs `site` (or the
    /// server's `HELIA_SITE`) and a capture with timestamps; what it did is in
    /// `summary.field_rotation`. An empty object takes the defaults; not for an
    /// equatorial mount, and not with `colour: "true"`.
    pub field_rotation: Option<FieldRotationConfig>,
    /// Find the sunspots on the stack (`summary.sunspots`, the `SPOT*` FITS
    /// cards, `GET /jobs/{id}/sunspots.csv` and `sunspots.png`). An empty
    /// object takes the defaults. For a white-light capture with the whole
    /// disc in view.
    pub sunspots: Option<SunspotsConfig>,
    /// When the capture was taken, UTC (`YYYY-MM-DDThh:mm:ss`), for a capture
    /// that does not hold its own time (an AVI or a video; a SER's time stamps
    /// are used otherwise). Gives P, B0 and L0 to `sunspots` and to the
    /// heliographic grids of `report`.
    pub date: Option<String>,
    /// Make the science report: the frame quality graphs (sorted and in
    /// capture order, with the keep threshold), the quality histogram, the
    /// global shift and drift and the seeing over the capture, as PNG with
    /// their data as CSV and JSON, and one self-contained HTML page. Turns
    /// the seeing probe on (about 120 more frames are read). The files are
    /// listed in `summary.report`; fetch them with
    /// `GET /jobs/{id}/report` and `GET /jobs/{id}/report/{file}`.
    pub report: bool,
    /// Multi-point alignment; `false` is global alignment only.
    pub multipoint: bool,
    /// Compute backend.
    pub backend: BackendArg,
    /// Sigma clipping threshold in standard deviations; off when absent.
    /// Same as `robust: "sigma"` with `robust_k`.
    pub sigma_clip: Option<f32>,
    /// Outlier-robust combination: `none` (the default weighted mean),
    /// `sigma`, `huber` or `winsor`. Excludes `sigma_clip`.
    pub robust: Option<RobustArg>,
    /// Threshold of `robust` in standard deviations. Default 2.5 for `sigma`,
    /// 1.345 for `huber`, 2 for `winsor`.
    pub robust_k: Option<f32>,
    /// `auto`, `none`, `rggb`, `grbg`, `gbrg` or `bggr`. `auto` trusts the
    /// file's header, but checks a SER file labelled mono for a hidden Bayer
    /// mosaic and debayers when it finds one. `none` treats the frames as
    /// mono whatever the header says; a pattern forces it.
    pub bayer: String,
    /// Bits per sample instead of the SER header's (1 to 8 read one byte per
    /// pixel, 9 to 16 two). SER only.
    pub bit_depth: Option<u32>,
    /// Byte order of 16-bit samples instead of the SER header flag:
    /// `little` or `big`. SER only.
    pub endian: Option<String>,
    /// Read the rows bottom to top. SER only.
    pub flip_rows: bool,
    /// Row order of FITS frames: `auto` (default), `top-down` or `bottom-up`.
    /// `auto` follows a `ROWORDER` card, else Seestar S50 frames are
    /// top-down and all others bottom-up.
    pub row_order: Option<String>,
    /// Colour plane to stack from a Bayer capture. Absent: the preset's
    /// channel, else `lum`.
    pub channel: Option<ChannelArg>,
    /// Demosaicing method.
    pub debayer: DebayerArg,
    /// Frame ranking metric.
    pub metric: MetricArg,
    /// Rectangles to keep alignment and scoring off. They are filled in
    /// every frame from the sky around them, so no patch shows in the result. Burned-in timestamps are
    /// found automatically (see `detect_overlay`).
    pub exclude: Vec<Region>,
    /// Pixels outside the fitted limb where alignment points are still
    /// allowed; 0 keeps them on the disc.
    pub prominence_band: usize,
    /// Fit the limb and place alignment points on the disc only.
    pub disc_mask: bool,
    /// Look for burned-in text (a `SharpCap` timestamp) and mask it.
    pub detect_overlay: bool,
    /// Filter preset (`ha`, `cak`, `cah`, `mg`, `na`, `he-d3`, `hbeta`,
    /// `white`; see `GET /presets`): its channel and frame share apply
    /// wherever `channel` and `keep` are absent, and a `.tif` or `.png`
    /// output is coloured with it. Absent: the filter is detected from the file
    /// name, folder, sidecar or header, for the settings only.
    pub preset: Option<String>,
    /// Colour of the output: `auto` (the preset in force), `mono`, a preset
    /// id, or `true`. `true` is true colour from a raw colour camera: red,
    /// green and blue stacked on one alignment, channels aligned, white
    /// balanced, written as an RGB TIFF or PNG or a three-plane FITS cube; it
    /// needs a Bayer capture and excludes `channel`, `invert` and
    /// `colour_strength`. The other looks colour a `.tif` or `.png` from the
    /// mono stack, and FITS output is mono and unaltered for them. Absent: the
    /// `preset`'s colours if one was given, else mono.
    pub colour: Option<String>,
    /// White balance of `colour: "true"`: `auto` (make the disc centre
    /// neutral), `off` (the sensor's own balance) or `"R,G,B"` gains such as
    /// `"1.9,1,1.5"`. Default `auto`. Only with `colour: "true"`.
    pub white_balance: Option<String>,
    /// With `colour: "true"`, register red and blue to green (atmospheric
    /// dispersion). Default `true`.
    pub align_channels: bool,
    /// Invert before colouring (shows filaments and prominences).
    pub invert: bool,
    /// Mix of colour and grey, 0 to 1.
    pub colour_strength: f32,
    /// Colour grading on top of the look: `hue=DEG,sat=S,gamma=G,black=B,
    /// white=W,r=R,g=G,b=B`, any of them (hue -180 to 180, sat 0 to 2, gamma 0.3
    /// to 3, black 0 to 0.9, white 0.1 to 1, r, g and b channel curves 0.3 to
    /// 3). Absent: no grading. Not with `colour: "true"`.
    pub grade: Option<String>,
    /// Observing site as `"lat,lon,alt"` (degrees north, degrees east, metres),
    /// written to a FITS output as `OBSGEO-X/Y/Z`. Absent: the server's
    /// `HELIA_SITE` environment variable, else the keywords are left out.
    pub site: Option<String>,
    /// How whole frames are aligned: `auto` (fit the solar limb when the full
    /// disc is in view and the fit is confident, else phase correlation),
    /// `phase`, or `limb`. The choice and its confidence come back in
    /// `summary.alignment`.
    pub align: String,
    /// Sharpening of the stack: `wavelet` (Registax-style layers, the usual
    /// choice), `unsharp`, `rl` (Richardson-Lucy) or `none`, with the
    /// recommended values of the filter in force. Absent: `none`. It runs
    /// first, on the mono image, before `edits` and before any colour.
    pub sharpen: Option<String>,
    /// Wavelets as in RegiStax 6, replacing the recommended layers: the
    /// `registax:` form of the CLI's `--wavelets`, for example
    /// `registax:l1=100/0.14/0.2,l2=100/0.11/0.15,scheme=dyadic,filter=gaussian`.
    /// A layer is `amount/sharpen[/denoise]` (amount 0 to 200 %, sharpen -0.1
    /// to 1, denoise 0 to 1.5); the gain is `1 + amount * sharpen * |sharpen|`
    /// (0.14 at 100 % is 3x, 1 is 101x) and the noise cut
    /// `10 * denoise * amount / 100` noise sigmas. Other keys: `scheme`
    /// (`dyadic` or `linear`), `filter` (`default` or `gaussian`), `initial`,
    /// `step`, `linked` (`on`). Implies `sharpen: "wavelet"`; with
    /// `sharpen: "rl"` the wavelets run after the deconvolution (the usual
    /// order: deconvolve, then lift and denoise); excludes `unsharp`.
    pub wavelets: Option<String>,
    /// Flatten the disc's limb darkening before anything else: `model`
    /// (divide by the fitted law) or `profile` (divide by the measured
    /// azimuthal median). Outside the limb nothing changes. It runs first, so
    /// `sharpen` and `edits` see flat contrast. A capture with no clear disc is
    /// left as it is and `summary.limb_darkening` is absent.
    pub flatten: Option<String>,
    /// The limb-darkening law fitted to the stack and used by `flatten`:
    /// `linear`, `quadratic` (the default) or `neckel` (five terms). The fit
    /// comes back in `summary.limb_darkening` whether or not `flatten` is set.
    pub limb_law: Option<String>,
    /// Further edits, applied in order after `sharpen`: objects with an `op`
    /// of `unsharp`, `wavelet`, `deconvolve`, `denoise`, `levels`, `curve`,
    /// `crop`, `rotate`, `flip` or `resize` and that operation's parameters
    /// (see the sharpening and adjustments page of the user guide). Unset
    /// parameters take their defaults.
    #[schema(value_type = Vec<Object>)]
    pub edits: Vec<Op>,
    /// Apply `sharpen` and `edits` to FITS output too. By default a FITS file
    /// is the unsharpened stack.
    pub sharpen_fits: bool,
    /// Dark, flat and bad-pixel calibration, applied to every frame before it
    /// is demosaiced, scored or aligned. Absent: none.
    pub calibration: Option<CalibrationConfig>,
    /// Steps of plugins to run, each `PLUGIN/STEP`, optionally followed by
    /// `:KEY=VALUE,KEY=VALUE` or `:{"json":"object"}` for its parameters and
    /// optionally preceded by `pre:` for an image step that runs before
    /// `sharpen` and `edits`. A `frame-op` step runs on every frame before it
    /// is scored; an `image-op` step on the stack (after `sharpen` and `edits`
    /// unless `pre:`); an `analyse` step on the result, and what it reports comes
    /// back in `summary.plugins`. The plugins must have been enabled on the
    /// server (`helia plugins enable`); a plugin that is off, changed or
    /// unknown, or a value outside the step's range, is a 422. They run with
    /// the rights of the server. At most 16.
    pub plugin_steps: Vec<String>,
    /// Stack with this `stack-backend` plugin instead of on the server's own
    /// pipeline. Not with `report`, true colour, calibration or field rotation.
    pub stack_backend: Option<String>,
}

/// Calibration masters for a job. Each path is relative to the data root and
/// names a master file (FITS, from `helia calibrate`) or raw calibration
/// frames to build one from (a SER, AVI or FITS file, or a folder of them).
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, default)]
pub struct CalibrationConfig {
    /// Master dark, or dark captures.
    pub dark: Option<String>,
    /// Master flat, or flat captures.
    pub flat: Option<String>,
    /// Captures of the Sun at many places on the sensor, to solve a flat from
    /// when there is no `flat` (self-calibration: dust and fringes included).
    /// The dark is taken off first. Excludes `flat`.
    pub displaced: Option<String>,
    /// Dark captures at the exposure of the flats, when `flat` is captures.
    pub flat_dark: Option<String>,
    /// Master bias, or bias captures. Only used with `scale_dark`.
    pub bias: Option<String>,
    /// Bad-pixel map: a FITS map from `helia calibrate`, or a text file with
    /// an `x y` pair (from 0, top-left) on each line.
    pub bad_pixels: Option<String>,
    /// Without `bad_pixels`, find the hot pixels of the dark and the cold ones
    /// of the flat and correct them. Default `true`.
    pub cosmetic: bool,
    /// Scale the dark by the ratio of the exposures (read from the `SharpCap`
    /// sidecar of the capture).
    pub scale_dark: bool,
    /// How frames are combined into a master: `median`, `sigma` or `sigma:K`.
    /// Default `sigma`.
    pub combine: String,
}

impl Default for CalibrationConfig {
    fn default() -> Self {
        Self {
            dark: None,
            flat: None,
            displaced: None,
            flat_dark: None,
            bias: None,
            bad_pixels: None,
            cosmetic: true,
            scale_dark: false,
            combine: "sigma".into(),
        }
    }
}

impl Default for JobConfig {
    fn default() -> Self {
        Self {
            keep: None,
            keep_frames: None,
            ap_size: None,
            ap_auto: false,
            ap_passes: None,
            ap_smooth: false,
            ap_residual: false,
            autocrop: false,
            seeing: false,
            aperture_mm: None,
            autocrop_margin: None,
            sunspots: None,
            date: None,
            report: false,
            field_rotation: None,
            multipoint: true,
            backend: BackendArg::Auto,
            sigma_clip: None,
            robust: None,
            robust_k: None,
            bayer: "auto".into(),
            bit_depth: None,
            endian: None,
            flip_rows: false,
            row_order: None,
            channel: None,
            debayer: DebayerArg::Mhc,
            metric: MetricArg::Gradient,
            exclude: Vec::new(),
            prominence_band: 0,
            disc_mask: true,
            detect_overlay: true,
            preset: None,
            colour: None,
            white_balance: None,
            align_channels: true,
            invert: false,
            colour_strength: 1.0,
            grade: None,
            site: None,
            align: "auto".into(),
            flatten: None,
            limb_law: None,
            sharpen: None,
            wavelets: None,
            edits: Vec::new(),
            sharpen_fits: false,
            calibration: None,
            plugin_steps: Vec::new(),
            stack_backend: None,
        }
    }
}

/// What a job's `colour` field asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColourChoice {
    Auto,
    Mono,
    True,
    Preset(Preset),
}

fn invalid(msg: impl Into<String>) -> ApiError {
    ApiError::unprocessable("invalid_config", msg)
}

/// Validate the fields that say how to read a capture.
///
/// # Errors
/// 422 `invalid_config`.
pub fn parse_open(
    bayer: &str,
    bit_depth: Option<u32>,
    endian: Option<&str>,
    flip_rows: bool,
    row_order: Option<&str>,
) -> ApiResult<OpenOptions> {
    if bit_depth.is_some_and(|d| !(1..=16).contains(&d)) {
        return Err(invalid("`bit_depth` must be between 1 and 16"));
    }
    Ok(OpenOptions {
        bayer: bayer
            .parse::<BayerSetting>()
            .map_err(|e| invalid(e.to_string()))?,
        bit_depth,
        endian: endian
            .map(|e| e.parse::<Endian>().map_err(|e| invalid(e.to_string())))
            .transpose()?,
        flip_rows,
        row_order: row_order
            .map(|r| r.parse::<RowOrder>().map_err(|e| invalid(e.to_string())))
            .transpose()?
            .unwrap_or_default(),
        ..OpenOptions::default()
    })
}

impl JobConfig {
    fn explicit_preset(&self) -> ApiResult<Option<Preset>> {
        self.preset
            .as_deref()
            .map(|p| p.parse::<Preset>().map_err(|e| invalid(e.to_string())))
            .transpose()
    }

    fn site(&self) -> ApiResult<Option<Site>> {
        self.site
            .as_deref()
            .map(|s| s.parse::<Site>().map_err(|e| invalid(e.to_string())))
            .transpose()
    }

    fn sharpen_method(&self) -> ApiResult<SharpenMethod> {
        self.sharpen
            .as_deref()
            .map_or(Ok(SharpenMethod::None), |m| {
                m.parse::<SharpenMethod>()
                    .map_err(|e| invalid(e.to_string()))
            })
    }

    /// The edit list for an image of `width` x `height` shot through
    /// `preset`'s filter: the sharpening method's recommended edit, then
    /// `edits`.
    fn edit_list(
        &self,
        preset: Option<Preset>,
        width: usize,
        height: usize,
    ) -> ApiResult<EditList> {
        let mut method = self.sharpen_method()?;
        let wavelets = self
            .wavelets
            .as_deref()
            .map(|s| Wavelet::from_registax_spec(s).map_err(|e| invalid(e.to_string())))
            .transpose()?;
        if wavelets.is_some() {
            if !matches!(
                method,
                SharpenMethod::None | SharpenMethod::Wavelet | SharpenMethod::RichardsonLucy
            ) {
                return Err(invalid(
                    "`wavelets` goes with `sharpen: \"wavelet\"` or `\"rl\"` (deconvolution first, then the wavelets)",
                ));
            }
            if method == SharpenMethod::Wavelet {
                method = SharpenMethod::None;
            }
        }
        let mut list = sharpen_edits(method, preset, width, height);
        if let Some(w) = wavelets {
            list.ops.push(Op::Wavelet(w));
        }
        list.ops.extend(self.edits.iter().cloned());
        if let Some(source) = self.flatten_source()? {
            list.ops.insert(
                0,
                helia_core::Op::Flatten(helia_core::edit::Flatten::new(source, self.law()?)),
            );
        }
        list.validate(width, height)
            .map_err(|e| invalid(e.to_string()))?;
        Ok(list)
    }

    fn flatten_source(&self) -> ApiResult<Option<helia_core::limbdark::Source>> {
        self.flatten
            .as_deref()
            .map(|s| {
                s.parse()
                    .map_err(|_| invalid("`flatten` must be `model` or `profile`"))
            })
            .transpose()
    }

    fn law(&self) -> ApiResult<helia_core::limbdark::Law> {
        self.limb_law
            .as_deref()
            .map_or(Ok(helia_core::limbdark::Law::default()), |s| {
                s.parse()
                    .map_err(|_| invalid("`limb_law` must be `linear`, `quadratic` or `neckel`"))
            })
    }

    /// The true-colour settings, when `colour` is `true`.
    fn true_colour(&self) -> ApiResult<Option<helia_core::truecolour::TrueColour>> {
        if self.colour_choice()? != Some(ColourChoice::True) {
            if self.white_balance.is_some() || !self.align_channels {
                return Err(invalid(
                    "`white_balance` and `align_channels` belong to `colour: \"true\"`",
                ));
            }
            return Ok(None);
        }
        if self.channel.is_some()
            || self.invert
            || (self.colour_strength - 1.0).abs() > f32::EPSILON
        {
            return Err(invalid(
                "`channel`, `invert` and `colour_strength` do not apply to `colour: \"true\"`: it stacks all three channels and has no colour look",
            ));
        }
        let white_balance = self
            .white_balance
            .as_deref()
            .map_or(Ok(helia_core::truecolour::WhiteBalance::Auto), |s| {
                s.parse().map_err(|e: CoreError| invalid(e.to_string()))
            })?;
        Ok(Some(helia_core::truecolour::TrueColour {
            white_balance,
            align_channels: self.align_channels,
        }))
    }

    fn colour_choice(&self) -> ApiResult<Option<ColourChoice>> {
        let Some(c) = self.colour.as_deref() else {
            return Ok(None);
        };
        Ok(Some(match c.trim().to_ascii_lowercase().as_str() {
            "auto" => ColourChoice::Auto,
            "mono" | "none" => ColourChoice::Mono,
            "true" | "truecolour" | "truecolor" | "rgb" => ColourChoice::True,
            other => ColourChoice::Preset(other.parse::<Preset>().map_err(|_| {
                invalid(format!(
                    "`colour` must be auto, mono, true or a preset id (see GET /presets), not {c:?}"
                ))
            })?),
        }))
    }

    /// The stacking mode the robust options ask for.
    fn stack_mode(&self) -> StackMode {
        let kind = match self.robust {
            Some(RobustArg::Sigma) => RobustKind::Sigma,
            Some(RobustArg::Huber) => RobustKind::Huber,
            Some(RobustArg::Winsor) => RobustKind::Winsor,
            Some(RobustArg::None) => return StackMode::Mean,
            None => {
                return self
                    .sigma_clip
                    .map_or(StackMode::Mean, |kappa| StackMode::SigmaClip { kappa });
            }
        };
        StackMode::from_kind(kind, self.robust_k.unwrap_or_else(|| kind.default_k()))
    }

    /// The automatic alignment point and crop options agree with the rest.
    fn check_ap_and_crop(&self) -> ApiResult<()> {
        self.sunspots
            .as_ref()
            .map_or(Ok(()), SunspotsConfig::check)?;
        if let Some(f) = &self.field_rotation {
            f.reference()?;
            if self.site()?.or_else(Site::from_env).is_none() {
                return Err(invalid(
                    "`field_rotation` needs the observing site: give `site` (\"lat,lon,alt\") or set HELIA_SITE on the server",
                ));
            }
        }
        if self.ap_auto && self.ap_size.is_some() {
            return Err(invalid("`ap_auto` and `ap_size` conflict"));
        }
        if self.ap_auto && !self.multipoint {
            return Err(invalid("`ap_auto` needs `multipoint`"));
        }
        if let Some(d) = &self.date
            && helia_core::io::solarnet::unix_from_iso(d).is_none()
        {
            return Err(invalid("`date` must be a UTC time, YYYY-MM-DDThh:mm:ss"));
        }
        if self.ap_passes.is_some_and(|n| !(1..=2).contains(&n)) {
            return Err(invalid("`ap_passes` must be 1 or 2"));
        }
        if (self.ap_passes.is_some() || self.ap_smooth || self.ap_residual) && !self.multipoint {
            return Err(invalid(
                "`ap_passes`, `ap_smooth` and `ap_residual` need `multipoint`",
            ));
        }
        if self
            .aperture_mm
            .is_some_and(|d| !(d.is_finite() && d > 0.0 && d <= 100_000.0))
        {
            return Err(invalid(
                "`aperture_mm` must be a positive number of millimetres",
            ));
        }
        let max_margin = 100.0 * helia_core::autocrop::MAX_MARGIN;
        if self
            .autocrop_margin
            .is_some_and(|m| !(m.is_finite() && (0.0..=max_margin).contains(&m)))
        {
            return Err(invalid(format!(
                "`autocrop_margin` must be between 0 and {max_margin} percent"
            )));
        }
        if self.autocrop_margin.is_some() && !self.autocrop {
            return Err(invalid("`autocrop_margin` needs `autocrop`"));
        }
        Ok(())
    }

    fn check_grade(&self) -> ApiResult<()> {
        self.grade
            .as_deref()
            .map_or(Ok(()), |g| helia_core::grade::Grade::parse(g).map(drop))
            .map_err(|e| invalid(e.to_string()))
    }

    /// The plugin steps, parsed.
    fn plugin_specs(&self) -> ApiResult<Vec<helia_plugin::StepSpec>> {
        if self.plugin_steps.len() > 16 {
            return Err(invalid("`plugin_steps` takes at most 16 steps"));
        }
        self.plugin_steps
            .iter()
            .map(|t| {
                helia_plugin::StepSpec::parse(t)
                    .map_err(|e| invalid(format!("`plugin_steps`: {e}")))
            })
            .collect()
    }

    /// Check the plugin steps and the stack backend against the plugins enabled on this
    /// server, before the job is queued.
    fn check_plugins(&self) -> ApiResult<()> {
        if self.plugin_steps.is_empty() && self.stack_backend.is_none() {
            return Ok(());
        }
        let specs = self.plugin_specs()?;
        let runner = helia_plugin::Runner::new(helia_plugin::Registry::open(&[]));
        runner
            .check_all(&specs)
            .map_err(|e| invalid(e.to_string()))?;
        let frames = runner
            .frame_steps(&specs)
            .map_err(|e| invalid(e.to_string()))?;
        if let Some(b) = &self.stack_backend {
            let l = runner
                .registry
                .runnable(b)
                .map_err(|e| invalid(e.to_string()))?;
            if !l.has("stack-backend") {
                return Err(invalid(format!(
                    "the plugin {b} does not offer the stack-backend capability"
                )));
            }
            if self.report {
                return Err(invalid(
                    "`report` needs the per-frame results that only the server's own pipeline has; leave out `report` or `stack_backend`",
                ));
            }
        }
        if self.report && !frames.is_empty() {
            return Err(invalid(
                "`report` cannot be combined with a plugin step that works on frames",
            ));
        }
        Ok(())
    }

    /// Validate and convert to pipeline settings.
    ///
    /// # Errors
    /// 422 `invalid_config`.
    pub fn resolve(&self) -> ApiResult<(StackConfig, OpenOptions)> {
        self.check_plugins()?;
        let keep = match (&self.keep, self.keep_frames) {
            (Some(_), Some(_)) => return Err(invalid("`keep` and `keep_frames` conflict")),
            (_, Some(0)) => return Err(invalid("`keep_frames` must be at least 1")),
            (_, Some(n)) => Keep::Count(n),
            (Some(k), None) => k.parse::<Keep>().map_err(|e| invalid(e.to_string()))?,
            (None, None) => Keep::default(),
        };
        if self.sigma_clip.is_some_and(|k| !(k.is_finite() && k > 0.0)) {
            return Err(invalid("`sigma_clip` must be a positive number"));
        }
        if self.robust_k.is_some_and(|k| !(k.is_finite() && k > 0.0)) {
            return Err(invalid("`robust_k` must be a positive number"));
        }
        if self.robust.is_some() && self.sigma_clip.is_some() {
            return Err(invalid("`robust` and `sigma_clip` conflict"));
        }
        if self.robust_k.is_some() && self.robust.is_none_or(|r| r == RobustArg::None) {
            return Err(invalid("`robust_k` needs `robust` other than `none`"));
        }
        if self.multipoint && self.ap_size.is_some_and(|s| !(8..=1024).contains(&s)) {
            return Err(invalid("`ap_size` must be between 8 and 1024"));
        }
        self.check_ap_and_crop()?;
        self.flatten_source()?;
        self.law()?;
        let true_colour = self.true_colour()?;
        self.explicit_preset()?;
        self.colour_choice()?;
        self.site()?;
        // Sizes are not known until the capture is open; this catches the rest.
        self.edit_list(
            None,
            helia_core::edit::geometry::MAX_SIDE,
            helia_core::edit::geometry::MAX_SIDE,
        )?;
        if !(0.0..=1.0).contains(&self.colour_strength) {
            return Err(invalid("`colour_strength` must be between 0 and 1"));
        }
        self.check_grade()?;
        if self.exclude.iter().any(|r| r.w == 0 || r.h == 0) {
            return Err(invalid(
                "`exclude` regions need a width and height of at least 1",
            ));
        }
        if self.prominence_band > 4096 {
            return Err(invalid("`prominence_band` must be at most 4096"));
        }
        let open = parse_open(
            &self.bayer,
            self.bit_depth,
            self.endian.as_deref(),
            self.flip_rows,
            self.row_order.as_deref(),
        )?;
        let align = self
            .align
            .parse::<AlignMethod>()
            .map_err(|e| invalid(e.to_string()))?;
        let cfg = StackConfig {
            keep,
            metric: self.metric.metric(),
            multipoint: self.multipoint.then(|| MultipointConfig {
                ap_size: self.ap_size.unwrap_or(64),
                ..MultipointConfig::default()
            }),
            mode: self.stack_mode(),
            debayer: Debayer {
                method: match self.debayer {
                    DebayerArg::Bilinear => Method::Bilinear,
                    DebayerArg::Mhc => Method::Mhc,
                },
                channel: self.channel.unwrap_or_default().into(),
            },
            mask: MaskConfig {
                disc: self.disc_mask,
                prominence_band: self.prominence_band,
                overlay: self.detect_overlay,
                exclude: self
                    .exclude
                    .iter()
                    .map(|r| helia_core::Rect::new(r.x, r.y, r.w, r.h))
                    .collect(),
            },
            align,
            limb_law: Some(self.law()?),
            ap_refine: ApRefine {
                passes: self.ap_passes.unwrap_or(ApRefine::default().passes),
                smooth: self.ap_smooth.then(SmoothConfig::default),
                residual: self.ap_residual,
            },
            true_colour,
            autocrop: self.autocrop.then(|| {
                self.autocrop_margin
                    .map_or(helia_core::autocrop::DEFAULT_MARGIN, |m| m / 100.0)
            }),
            ..StackConfig::default()
        };
        Ok((cfg, open))
    }
}

/// `POST /jobs` body.
#[derive(Clone, Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct JobRequest {
    /// Capture path (`.ser` or `.avi`), relative to the data root.
    pub input: String,
    /// Output path; `.tif`/`.tiff` or `.fits`/`.fit`/`.fts`.
    pub output: String,
    /// Stacking settings.
    #[serde(default)]
    pub config: JobConfig,
    /// Replace an existing output file.
    #[serde(default)]
    pub overwrite: bool,
}

/// Lifecycle state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    /// Waiting for a worker.
    Queued,
    /// Running.
    Running,
    /// Finished; the result is available.
    Succeeded,
    /// Ended with an error.
    Failed,
    /// Cancelled before completion.
    Cancelled,
}

impl JobState {
    /// True once nothing more will happen to the job.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }
}

/// Wall time of one stage.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct StageTiming {
    /// Stage name.
    pub stage: String,
    /// Seconds.
    pub seconds: f32,
}

/// What a finished stack contains.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct JobSummary {
    /// Frames in the capture.
    pub frames: usize,
    /// Frames in the kept set.
    pub kept: usize,
    /// Frames registered.
    pub registered: usize,
    /// Active alignment points (multi-point only).
    pub alignment_points_active: Option<usize>,
    /// Alignment points in the grid (multi-point only).
    pub alignment_points_total: Option<usize>,
    /// Output width.
    pub width: usize,
    /// Output height.
    pub height: usize,
    /// Preset id in force (chosen or detected), if any.
    pub filter: Option<String>,
    /// Where the filter was read from (`chosen`, or the text it was found in).
    pub filter_source: Option<String>,
    /// How the output was coloured, in words; absent for a mono output.
    pub colour: Option<String>,
    /// Bayer pattern the frames were decoded with (`rggb` ...); absent for mono.
    pub bayer: Option<String>,
    /// Where that came from: `header`, `sidecar`, `detection` or `override`.
    pub bayer_source: String,
    /// Things worth knowing about how the capture was read, such as a Bayer
    /// mosaic found in a file whose header says mono.
    pub notices: Vec<String>,
    /// How whole frames were aligned, and how confident that was.
    pub alignment: AlignmentSummary,
    /// The edits recorded in the output file, one line each. For FITS output
    /// without `sharpen_fits` this is the note that the file is unedited.
    pub edits: Vec<String>,
    /// The edits were applied to the pixels of the output.
    pub edited: bool,
    /// Whether the sharpening added detail or mostly noise, from the bands of
    /// the stack before and after the edits. Absent when nothing was edited or
    /// the edits changed the image size.
    pub sharpen_check: Option<SharpenCheckSummary>,
    /// The calibration masters applied, one line each, and any warning about
    /// them (a flat with rings, a dark that could not be scaled). Empty when
    /// the job had no `calibration`.
    pub calibration: Vec<String>,
    /// What `ap_auto` chose. Absent when it was off.
    pub auto_ap: Option<AutoApSummary>,
    /// The limb-darkening law fitted to the stack. Absent when there was no
    /// clear disc.
    pub limb_darkening: Option<LimbSummary>,
    /// What true colour did. Absent unless `colour` was `true`.
    pub true_colour: Option<TrueColourSummary>,
    /// The region `autocrop` kept, in the pixels of the stacked image before
    /// the crop. Absent when it was off or found no disc (see `notices`).
    pub crop: Option<Region>,
    /// Granulation report of the unsharpened stack, for the white-light and
    /// sodium filters. Absent for other filters or when the disc centre has
    /// no usable signal.
    pub granulation: Option<GranulationSummary>,
    /// The seeing estimated from the capture, with `seeing` on.
    pub seeing: Option<SeeingSummary>,
    /// Sunspots found on the stack, with `sunspots` set.
    pub sunspots: Option<SunspotsSummary>,
    /// What the field rotation correction did, with `field_rotation` set.
    pub field_rotation: Option<FieldRotationSummary>,
    /// The files of the science report, with `report` set.
    pub report: Option<ReportSummary>,
    /// What the `analyse` steps of plugins reported, with `plugin_steps` set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<PluginSummary>,
    /// The `stack-backend` plugin that made the stack, with `stack_backend` set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stacked_by: Option<String>,
}

/// What one `analyse` step of a plugin reported.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct PluginSummary {
    /// The plugin.
    pub plugin: String,
    /// The step.
    pub step: String,
    /// Named numbers.
    pub metrics: BTreeMap<String, f64>,
    /// Shapes in pixels of the output image, origin top-left: objects with a `kind` of
    /// `circle`, `rect`, `line`, `points` or `text`.
    #[schema(value_type = Vec<Object>)]
    pub overlays: Vec<serde_json::Value>,
    /// Lines of text.
    pub notes: Vec<String>,
}

/// The science report of a job.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct ReportSummary {
    /// File names: `report.html`, the figures (`*.png`) and their data
    /// (`*.csv`, `*.json`). Fetch one with `GET /jobs/{id}/report/{file}`.
    pub files: Vec<String>,
    /// Figures that were not drawn, and why (for example, no seeing data).
    pub not_drawn: Vec<String>,
}

/// What undoing the field rotation did.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct FieldRotationSummary {
    /// Parallactic angle from the first frame to the last, degrees.
    pub span_deg: f64,
    /// Mean rate of rotation, degrees per minute.
    pub rate_deg_per_min: f64,
    /// Largest turn applied to a frame, degrees.
    pub max_turn_deg: f64,
    /// The orientation every frame was turned to: the time of that frame,
    /// ISO 8601 UTC.
    pub reference_time: String,
    /// Parallactic angle at the reference, degrees.
    pub reference_angle_deg: f64,
    /// Altitude of the Sun at the reference, degrees.
    pub altitude_deg: f64,
    /// Displacement at the limb, pixels, of the frame that turned most; absent
    /// without a fitted limb.
    pub limb_shift_px: Option<f32>,
    /// Frames that had a timestamp of their own.
    pub frames_stamped: usize,
    /// The report in lines, as the command line prints it.
    pub lines: Vec<String>,
}

impl FieldRotationSummary {
    fn new(p: &helia_core::field_rotation::Plan, radius: Option<f32>) -> Self {
        Self {
            span_deg: p.span_deg,
            rate_deg_per_min: p.rate_deg_per_min,
            max_turn_deg: p.max_turn_deg,
            reference_time: helia_core::io::solarnet::iso_from_unix(p.reference_unix),
            reference_angle_deg: p.reference_q_deg,
            altitude_deg: p.altitude_deg,
            limb_shift_px: radius.map(|r| p.limb_shift_px(r)),
            frames_stamped: p.stamped,
            lines: p.lines(radius),
        }
    }
}

/// Sunspots on the stack: counts, areas and the table.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct SunspotsSummary {
    /// Sunspot groups, `g`.
    pub groups: usize,
    /// Individual spots, `s` (umbrae and pores).
    pub spots: usize,
    /// Relative Wolf number `k (10 g + s)`.
    pub wolf: f32,
    /// The observer factor used.
    pub k: f32,
    /// Everything as seen, millionths of the visible hemisphere.
    pub projected_msh: f32,
    /// Everything corrected for foreshortening, millionths of the hemisphere.
    pub area_msh: f32,
    /// Umbrae corrected, millionths of the hemisphere.
    pub umbra_msh: f32,
    /// Capture time used, ISO 8601 UTC; absent when the capture had none.
    pub time: Option<String>,
    /// Position angle of the solar axis, degrees (needs `time`).
    pub p_deg: Option<f32>,
    /// Heliographic latitude of the disc centre, degrees (needs `time`).
    pub b0_deg: Option<f32>,
    /// Carrington longitude of the disc centre, degrees (needs `time`).
    pub l0_deg: Option<f32>,
    /// Disc centre x, centre y and radius the positions are measured on, pixels.
    pub disc: [f32; 3],
    /// One row per spot.
    pub spot_table: Vec<SunspotRow>,
    /// One row per group.
    pub group_table: Vec<SunspotGroupRow>,
    /// Things to be careful about.
    pub notes: Vec<String>,
    /// The report in lines, as the command line prints it.
    pub lines: Vec<String>,
}

/// One sunspot.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct SunspotRow {
    /// Number from 1.
    pub id: usize,
    /// Its group.
    pub group: usize,
    /// Centroid, pixels.
    pub x: f32,
    /// Centroid, pixels, top row 0.
    pub y: f32,
    /// Heliographic latitude, degrees north.
    pub lat_deg: f32,
    /// Central meridian distance, degrees, west positive.
    pub cmd_deg: f32,
    /// Carrington longitude, degrees (needs `time`).
    pub lon_deg: Option<f32>,
    /// Cosine of the angle from disc centre.
    pub mu: f32,
    /// Umbrae in it.
    pub umbrae: usize,
    /// Umbra pixels.
    pub umbra_px: usize,
    /// Penumbra pixels.
    pub penumbra_px: usize,
    /// As seen, millionths of the visible hemisphere.
    pub projected_msh: f32,
    /// Corrected for foreshortening, millionths of the hemisphere.
    pub area_msh: f32,
    /// Umbra corrected, millionths of the hemisphere.
    pub umbra_msh: f32,
}

/// One group of sunspots.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct SunspotGroupRow {
    /// Number from 1, largest first.
    pub id: usize,
    /// Spots in it.
    pub spots: usize,
    /// Umbrae in it.
    pub umbrae: usize,
    /// Area-weighted latitude, degrees north.
    pub lat_deg: f32,
    /// Area-weighted central meridian distance, degrees.
    pub cmd_deg: f32,
    /// Area-weighted Carrington longitude, degrees (needs `time`).
    pub lon_deg: Option<f32>,
    /// As seen, millionths of the visible hemisphere.
    pub projected_msh: f32,
    /// Corrected, millionths of the hemisphere.
    pub area_msh: f32,
    /// Umbrae corrected, millionths of the hemisphere.
    pub umbra_msh: f32,
}

impl From<&helia_core::sunspots::Report> for SunspotsSummary {
    fn from(r: &helia_core::sunspots::Report) -> Self {
        Self {
            groups: r.group_count,
            spots: r.spot_count,
            wolf: r.wolf,
            k: r.params.k,
            projected_msh: r.projected_msh,
            area_msh: r.area_msh,
            umbra_msh: r.umbra_msh,
            time: r.when_unix.map(helia_core::io::solarnet::iso_from_unix),
            p_deg: r.ephemeris.map(|e| e.p_deg),
            b0_deg: r.ephemeris.map(|e| e.b0_deg),
            l0_deg: r.ephemeris.map(|e| e.l0_deg),
            disc: [r.disc.cx, r.disc.cy, r.disc.radius],
            spot_table: r
                .spots
                .iter()
                .map(|s| SunspotRow {
                    id: s.id,
                    group: s.group,
                    x: s.x,
                    y: s.y,
                    lat_deg: s.lat_deg,
                    cmd_deg: s.cmd_deg,
                    lon_deg: s.lon_deg,
                    mu: s.mu,
                    umbrae: s.umbrae,
                    umbra_px: s.umbra_px,
                    penumbra_px: s.penumbra_px,
                    projected_msh: s.projected_msh,
                    area_msh: s.area_msh,
                    umbra_msh: s.umbra_msh,
                })
                .collect(),
            group_table: r
                .groups
                .iter()
                .map(|g| SunspotGroupRow {
                    id: g.id,
                    spots: g.spots,
                    umbrae: g.umbrae,
                    lat_deg: g.lat_deg,
                    cmd_deg: g.cmd_deg,
                    lon_deg: g.lon_deg,
                    projected_msh: g.projected_msh,
                    area_msh: g.area_msh,
                    umbra_msh: g.umbra_msh,
                })
                .collect(),
            notes: r.notes.clone(),
            lines: r.lines(),
        }
    }
}

/// The files kept for `sunspots.csv` and `sunspots.png`.
#[derive(Clone, Debug)]
pub struct SunspotFiles {
    /// The spot table.
    pub csv: String,
    /// The overlay picture.
    pub png: Vec<u8>,
}

/// Seeing estimated from the capture.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct SeeingSummary {
    /// Frames followed (spread over the whole capture).
    pub frames: usize,
    /// Tiles of the disc followed.
    pub tiles: usize,
    /// RMS differential motion between neighbouring alignment points, per
    /// axis, pixels. A relative index, not `r0`.
    pub motion_px: Option<f32>,
    /// The same in arcseconds, with a fitted limb.
    pub motion_arcsec: Option<f32>,
    /// `D / r0` from the spectral ratio; needs `aperture_mm`.
    pub d_over_r0: Option<f32>,
    /// Fried parameter at the capture's wavelength, centimetres.
    pub r0_cm: Option<f32>,
    /// Fried parameter at 500 nm, centimetres.
    pub r0_500nm_cm: Option<f32>,
    /// `excellent`, `good`, `fair`, `poor` or `bad`, from `r0_500nm_cm`.
    pub grade: Option<String>,
    /// The report in lines, as the command line prints it.
    pub lines: Vec<String>,
}

impl From<&helia_core::seeing::SeeingReport> for SeeingSummary {
    fn from(r: &helia_core::seeing::SeeingReport) -> Self {
        Self {
            frames: r.frames,
            tiles: r.tiles,
            motion_px: r.motion.map(|m| m.rms_px),
            motion_arcsec: r.motion.zip(r.arcsec_per_px).map(|(m, s)| m.rms_px * s),
            d_over_r0: r.spectral.map(|s| s.d_over_r0),
            r0_cm: r.spectral.map(|s| s.r0_cm),
            r0_500nm_cm: r.spectral.map(|s| s.r0_500_cm),
            grade: r.grade.map(|g| g.label().to_owned()),
            lines: r.lines(),
        }
    }
}

/// Granulation statistics of the stack (`helia measure`, the quality report).
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct GranulationSummary {
    /// Standard deviation over mean of the flattened disc-centre region (a
    /// fraction, 0.04 is 4%).
    pub rms_contrast: f32,
    /// Mean spacing of granules, pixels.
    pub cell_pitch_px: f32,
    /// Mean diameter of whole granules, pixels.
    pub cell_diameter_px: f32,
    /// Image scale, arcseconds per pixel (absent without a fitted limb).
    pub arcsec_per_pixel: Option<f32>,
    /// Spatial frequency (cycles per pixel) where the noise-subtracted power
    /// spectrum has fallen to 10% of its peak.
    pub rolloff_freq: f32,
    /// Highest frequency (cycles per pixel) above 1% of the peak power.
    pub resolved_freq: f32,
    /// Diffraction cut-off `D / lambda` in cycles per pixel; needs
    /// `aperture_mm` and a fitted limb.
    pub cutoff_freq: Option<f32>,
    /// The report in lines, as the command line prints it.
    pub lines: Vec<String>,
}

impl From<&helia_core::granulation::Granulation> for GranulationSummary {
    fn from(g: &helia_core::granulation::Granulation) -> Self {
        Self {
            rms_contrast: g.rms_contrast,
            cell_pitch_px: g.cell_pitch_px,
            cell_diameter_px: g.cell_diameter_px,
            arcsec_per_pixel: g.arcsec_per_px,
            rolloff_freq: g.rolloff_freq,
            resolved_freq: g.resolved_freq,
            cutoff_freq: g.cutoff_freq,
            lines: g.lines(),
        }
    }
}

/// How red and blue were registered to green in a true-colour stack.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct ChannelSummary {
    /// `red` or `blue`.
    pub channel: String,
    /// Shift measured, pixels: `[dx, dy]`.
    pub shift: [f32; 2],
    /// Confidence of the correlation, 0 to 1.
    pub confidence: f32,
    /// The plane was moved. False when the shift was too small, too large or
    /// not trustworthy.
    pub applied: bool,
}

/// What a true-colour job did.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct TrueColourSummary {
    /// Gains applied to red, green and blue, including the common scale that
    /// keeps the picture inside white.
    pub gains: [f32; 3],
    /// Red and blue against green; absent when `align_channels` was off.
    pub channels: Option<Vec<ChannelSummary>>,
    /// The notes written into the file's history, including warnings (a
    /// channel held at the largest automatic gain, a shift not applied).
    pub notes: Vec<String>,
}

impl From<&helia_core::truecolour::ColourStack> for TrueColourSummary {
    fn from(c: &helia_core::truecolour::ColourStack) -> Self {
        Self {
            gains: c.gains,
            channels: c.channels.map(|ch| {
                ["red", "blue"]
                    .into_iter()
                    .zip(ch)
                    .map(|(name, s)| ChannelSummary {
                        channel: name.to_owned(),
                        shift: [s.shift.dx, s.shift.dy],
                        confidence: s.confidence,
                        applied: s.applied,
                    })
                    .collect()
            }),
            notes: c.notes.clone(),
        }
    }
}

/// Did the sharpening add detail, or mostly noise (see the sharpening page of
/// the user guide).
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct SharpenCheckSummary {
    /// The 1 to 4 pixel band of the stack after the edits over before.
    pub detail_gain: f32,
    /// The pixel-scale band after over before: the noise.
    pub noise_gain: f32,
    /// Deconvolution residual as a share of the mean, when there was one.
    pub residual: Option<f32>,
    /// Noise level of the stack as a share of the mean.
    pub noise_level: f32,
    /// `fine`, `noise_dominated` or `overfit`.
    pub verdict: String,
    /// A sentence for a person, when the verdict is not `fine`.
    pub warning: Option<String>,
}

impl From<&helia_core::edit::diagnose::SharpenCheck> for SharpenCheckSummary {
    fn from(c: &helia_core::edit::diagnose::SharpenCheck) -> Self {
        use helia_core::edit::diagnose::Verdict;
        Self {
            detail_gain: c.detail_gain,
            noise_gain: c.noise_gain,
            residual: c.residual,
            noise_level: c.noise_level,
            verdict: match c.verdict {
                Verdict::Fine => "fine",
                Verdict::NoiseDominated => "noise_dominated",
                Verdict::Overfit => "overfit",
            }
            .to_owned(),
            warning: c.warning(),
        }
    }
}

/// A limb-darkening law fitted to the stack's azimuthal median profile.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct LimbSummary {
    /// `linear`, `quadratic` or `neckel`.
    pub law: String,
    /// Names of the coefficients, in order (`u1`, `u2`, or `a1` to `a5`).
    pub names: Vec<String>,
    /// The coefficients, `I(mu) / I(1) = 1 - sum c_k (1 - mu^k)` for `neckel`,
    /// `1 - u (1 - mu) - u2 (1 - mu)^2` for the others.
    pub coefficients: Vec<f32>,
    /// The law at the limb as a share of the disc centre.
    pub limb_level: f32,
    /// Root mean square departure of the profile from the law, as a share of
    /// the centre intensity.
    pub rms: f32,
    /// Fitted disc radius, pixels.
    pub radius_px: f32,
    /// The fit in a sentence.
    pub text: String,
}

impl From<&helia_core::limbdark::LimbFit> for LimbSummary {
    fn from(l: &helia_core::limbdark::LimbFit) -> Self {
        Self {
            law: l.law.id().to_owned(),
            names: l.law.coefficient_names(),
            coefficients: l.coefficients.clone(),
            limb_level: l.limb_level(),
            rms: l.rms,
            radius_px: l.radius,
            text: l.summary(),
        }
    }
}

/// What automatic alignment points chose and what they were based on.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct AutoApSummary {
    /// Window edge, pixels.
    pub size: usize,
    /// Distance between window centres, pixels.
    pub step: usize,
    /// Search radius around the global shift, pixels.
    pub search_radius: usize,
    /// Image scale from the fitted limb, arcseconds per pixel. Absent when
    /// there was no limb.
    pub arcsec_per_pixel: Option<f32>,
    /// Local warp of the seeing, pixels (root mean square distance of an
    /// alignment point's shift from its frame's median). Absent when too few
    /// points locked.
    pub warp_px: Option<f32>,
    /// The choice in a sentence.
    pub text: String,
}

impl From<&helia_core::autoap::ApChoice> for AutoApSummary {
    fn from(c: &helia_core::autoap::ApChoice) -> Self {
        Self {
            size: c.size,
            step: c.step,
            search_radius: c.search_radius,
            arcsec_per_pixel: c.scale,
            warp_px: c.seeing.map(|s| s.warp_px),
            text: c.summary(),
        }
    }
}

/// Global alignment: the method that ran, why, and its confidence.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct AlignmentSummary {
    /// What `config.align` asked for: `auto`, `phase` or `limb`.
    pub requested: String,
    /// What ran: `limb` or `phase`.
    pub method: String,
    /// Why, in a sentence (for `auto`, whether the full disc was in view).
    pub reason: String,
    /// Mean confidence of the frames, 0 to 1. Absent when it was not measured.
    pub confidence: Option<f32>,
    /// Least confidence of any frame, 0 to 1.
    pub confidence_min: Option<f32>,
    /// Frames that wanted the limb fit but were aligned by phase correlation.
    pub fallbacks: usize,
    /// The confidence comes from a sample of the frames (a GPU run).
    pub sampled: bool,
    /// Fitted limb of the reference: centre x, centre y and radius, pixels.
    pub limb: Option<[f32; 3]>,
}

impl From<&helia_core::align::method::AlignReport> for AlignmentSummary {
    fn from(a: &helia_core::align::method::AlignReport) -> Self {
        Self {
            requested: a.requested.id().to_owned(),
            method: match a.method {
                helia_core::align::method::GlobalMethod::Limb => "limb",
                helia_core::align::method::GlobalMethod::Phase => "phase",
            }
            .to_owned(),
            reason: a.reason.clone(),
            confidence: a.confidence_mean,
            confidence_min: a.confidence_min,
            fallbacks: a.fallbacks,
            sampled: a.sampled,
            limb: a.limb.map(|l| [l.cx as f32, l.cy as f32, l.r as f32]),
        }
    }
}

/// Public view of a job.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct JobView {
    /// Job id.
    pub id: u64,
    /// State.
    pub state: JobState,
    /// Current stage name, once running.
    pub stage: Option<String>,
    /// Progress within the current stage, 0 to 1.
    pub stage_progress: f32,
    /// Approximate overall progress, 0 to 1.
    pub progress: f32,
    /// Position among queued jobs (0 is next), while queued.
    pub queue_position: Option<usize>,
    /// Input path relative to the data root.
    pub input: String,
    /// Output path relative to the data root.
    pub output: String,
    /// The settings in force.
    pub config: JobConfig,
    /// Creation time, Unix milliseconds.
    pub created_at: u64,
    /// Start time, Unix milliseconds.
    pub started_at: Option<u64>,
    /// End time, Unix milliseconds.
    pub finished_at: Option<u64>,
    /// Seconds spent running so far or in total.
    pub run_seconds: Option<f32>,
    /// Per-stage wall times of a finished run.
    pub timings: Vec<StageTiming>,
    /// Backend used.
    pub backend: Option<String>,
    /// Result summary once succeeded.
    pub summary: Option<JobSummary>,
    /// Failure reason.
    pub error: Option<ErrorBody>,
}

/// Message on the `/events` WebSocket.
#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JobEvent {
    /// Full job view, sent once on connect.
    Snapshot {
        /// The job.
        job: Box<JobView>,
    },
    /// State change.
    State {
        /// Job id.
        job: u64,
        /// New state.
        state: JobState,
        /// Failure reason when `failed`.
        error: Option<ErrorBody>,
    },
    /// Progress tick.
    Progress {
        /// Job id.
        job: u64,
        /// Stage name.
        stage: String,
        /// Progress within the stage.
        stage_progress: f32,
        /// Approximate overall progress.
        progress: f32,
    },
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Stable name and overall progress window for a stage.
fn stage_info(s: Stage) -> (&'static str, f32, f32) {
    match s {
        Stage::Quality => ("quality", 0.0, 0.20),
        Stage::Reference => ("reference", 0.20, 0.25),
        Stage::GlobalAlign => ("global_align", 0.25, 0.45),
        Stage::LocalAlign => ("local_align", 0.45, 0.65),
        Stage::Stack => ("stack", 0.65, 0.95),
        Stage::Finish => ("finish", 0.95, 1.0),
    }
}

/// Stored result preview.
#[derive(Clone, Debug)]
pub struct Preview(pub Grey8);

struct JobData {
    state: JobState,
    stage: Option<&'static str>,
    stage_progress: f32,
    progress: f32,
    last_sent: (Option<&'static str>, f32),
    started: Option<Instant>,
    started_at: Option<u64>,
    finished_at: Option<u64>,
    run: Option<Duration>,
    timings: Vec<StageTiming>,
    backend: Option<String>,
    summary: Option<JobSummary>,
    error: Option<ErrorBody>,
    preview: Option<Arc<Preview>>,
    sunspots: Option<Arc<SunspotFiles>>,
    report: Option<Arc<helia_report::Bundle>>,
}

/// A resolved, validated job ready to queue.
pub struct JobSpec {
    /// Canonical input file.
    pub input: PathBuf,
    /// Canonical output file.
    pub output: PathBuf,
    /// Input as shown to clients.
    pub input_display: String,
    /// Output as shown to clients.
    pub output_display: String,
    /// Settings as submitted.
    pub config: JobConfig,
    /// Converted pipeline settings.
    pub stack: StackConfig,
    /// How to read the capture.
    pub open: OpenOptions,
    /// Directory for decoded video.
    pub cache_dir: Option<PathBuf>,
    /// Masters to apply, with canonical paths.
    pub calibration: Option<helia_core::calibrate::CalibrationSpec>,
}

/// One job.
pub struct Job {
    id: u64,
    spec: JobSpec,
    created_at: u64,
    cancel: Arc<AtomicBool>,
    wake: Notify,
    events: broadcast::Sender<JobEvent>,
    data: Mutex<JobData>,
}

impl Job {
    /// Id.
    #[must_use]
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Output file.
    #[must_use]
    pub fn output_path(&self) -> &PathBuf {
        &self.spec.output
    }

    /// Current state.
    #[must_use]
    pub fn state(&self) -> JobState {
        lock(&self.data).state
    }

    /// Stored preview of a finished job.
    #[must_use]
    pub fn preview(&self) -> Option<Arc<Preview>> {
        lock(&self.data).preview.clone()
    }

    /// The sunspot table and overlay of a finished job that asked for them.
    #[must_use]
    pub fn sunspot_files(&self) -> Option<Arc<SunspotFiles>> {
        lock(&self.data).sunspots.clone()
    }

    /// The science report of a finished job that asked for `report`.
    #[must_use]
    pub fn report(&self) -> Option<Arc<helia_report::Bundle>> {
        lock(&self.data).report.clone()
    }

    /// Receive subsequent events.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<JobEvent> {
        self.events.subscribe()
    }

    fn view(&self, queue_position: Option<usize>) -> JobView {
        let d = lock(&self.data);
        let run_seconds = match (d.run, d.started) {
            (Some(r), _) => Some(r.as_secs_f32()),
            (None, Some(s)) => Some(s.elapsed().as_secs_f32()),
            _ => None,
        };
        JobView {
            id: self.id,
            state: d.state,
            stage: d.stage.map(str::to_owned),
            stage_progress: d.stage_progress,
            progress: d.progress,
            queue_position: if d.state == JobState::Queued {
                queue_position
            } else {
                None
            },
            input: self.spec.input_display.clone(),
            output: self.spec.output_display.clone(),
            config: self.spec.config.clone(),
            created_at: self.created_at,
            started_at: d.started_at,
            finished_at: d.finished_at,
            run_seconds,
            timings: d.timings.clone(),
            backend: d.backend.clone(),
            summary: d.summary.clone(),
            error: d.error.clone(),
        }
    }

    fn emit(&self, ev: JobEvent) {
        // No receivers is the normal case.
        let _ = self.events.send(ev);
    }

    fn on_progress(&self, stage: Stage, fraction: f32) {
        let (name, lo, hi) = stage_info(stage);
        self.progress_at(name, lo, hi, fraction);
    }

    /// A stage outside the pipeline proper: `opening` before it, `edit`,
    /// `colourise` and `write` after it. They move the stage name, not the overall bar.
    fn on_named(&self, name: &'static str, fraction: f32) {
        let (lo, hi) = if name == "opening" {
            (0.0, 0.0)
        } else {
            (1.0, 1.0)
        };
        self.progress_at(name, lo, hi, fraction);
    }

    fn progress_at(&self, name: &'static str, lo: f32, hi: f32, fraction: f32) {
        let mut d = lock(&self.data);
        // Monotonic: the combined align pass interleaves GlobalAlign and LocalAlign reports.
        let overall = (lo + (hi - lo) * fraction).max(d.progress);
        d.stage = Some(name);
        d.stage_progress = fraction;
        d.progress = overall;
        // Throttle: stage change or at least 0.5% overall movement.
        if d.last_sent.0 != Some(name) || overall - d.last_sent.1 >= 0.005 || fraction >= 1.0 {
            d.last_sent = (Some(name), overall);
            self.emit(JobEvent::Progress {
                job: self.id,
                stage: name.to_owned(),
                stage_progress: fraction,
                progress: overall,
            });
        }
    }

    /// Move to `state` unless already terminal; returns whether it moved.
    fn transition(&self, state: JobState, error: Option<ErrorBody>) -> bool {
        let mut d = lock(&self.data);
        if d.state.is_terminal() {
            return false;
        }
        d.state = state;
        match state {
            JobState::Running => {
                d.started = Some(Instant::now());
                d.started_at = Some(now_ms());
            }
            s if s.is_terminal() => {
                d.finished_at = Some(now_ms());
                d.run = d.started.map(|t| t.elapsed());
                if s == JobState::Succeeded {
                    d.progress = 1.0;
                    d.stage_progress = 1.0;
                }
            }
            _ => {}
        }
        d.error.clone_from(&error);
        self.emit(JobEvent::State {
            job: self.id,
            state,
            error,
        });
        true
    }
}

/// Cache of backends, so a GPU context is opened once.
#[derive(Default)]
struct BackendCache(Mutex<HashMap<BackendArg, Arc<dyn Backend>>>);

impl BackendCache {
    fn get(&self, arg: BackendArg) -> Result<Arc<dyn Backend>, CoreError> {
        let mut m = lock(&self.0);
        if let Some(b) = m.get(&arg) {
            return Ok(Arc::clone(b));
        }
        let b = select_backend(match arg {
            BackendArg::Auto => BackendChoice::Auto,
            BackendArg::Cpu => BackendChoice::Cpu,
            BackendArg::Gpu => BackendChoice::Gpu,
        })?;
        m.insert(arg, Arc::clone(&b));
        Ok(b)
    }
}

struct Registry {
    jobs: BTreeMap<u64, Arc<Job>>,
    next: u64,
}

struct Shared {
    registry: Mutex<Registry>,
    permits: Arc<Semaphore>,
    max_active: usize,
    max_retained: usize,
    backends: BackendCache,
}

/// Queue plus worker pool.
#[derive(Clone)]
pub struct JobManager {
    shared: Arc<Shared>,
}

enum RunError {
    Cancelled,
    Failed(ErrorBody),
}

impl JobManager {
    /// Pool of `workers` concurrent jobs; at most `max_active` queued or
    /// running; the newest `max_retained` finished jobs are remembered.
    #[must_use]
    pub fn new(workers: usize, max_active: usize, max_retained: usize) -> Self {
        Self {
            shared: Arc::new(Shared {
                registry: Mutex::new(Registry {
                    jobs: BTreeMap::new(),
                    next: 1,
                }),
                permits: Arc::new(Semaphore::new(workers.max(1))),
                max_active: max_active.max(1),
                max_retained,
                backends: BackendCache::default(),
            }),
        }
    }

    /// Queue a job.
    ///
    /// # Errors
    /// 429 `queue_full`, 409 `output_in_use`.
    pub fn submit(&self, spec: JobSpec) -> ApiResult<JobView> {
        let job = {
            let mut reg = lock(&self.shared.registry);
            let active = reg
                .jobs
                .values()
                .filter(|j| !j.state().is_terminal())
                .collect::<Vec<_>>();
            if active.len() >= self.shared.max_active {
                return Err(ApiError::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    "queue_full",
                    format!("{} jobs already queued or running", active.len()),
                ));
            }
            if active.iter().any(|j| j.spec.output == spec.output) {
                return Err(ApiError::conflict(
                    "output_in_use",
                    "another active job writes the same output",
                ));
            }
            let id = reg.next;
            reg.next += 1;
            let job = Arc::new(Job {
                id,
                spec,
                created_at: now_ms(),
                cancel: Arc::new(AtomicBool::new(false)),
                wake: Notify::new(),
                events: broadcast::channel(256).0,
                data: Mutex::new(JobData {
                    state: JobState::Queued,
                    stage: None,
                    stage_progress: 0.0,
                    progress: 0.0,
                    last_sent: (None, 0.0),
                    started: None,
                    started_at: None,
                    finished_at: None,
                    run: None,
                    timings: Vec::new(),
                    backend: None,
                    summary: None,
                    error: None,
                    preview: None,
                    sunspots: None,
                    report: None,
                }),
            });
            reg.jobs.insert(id, Arc::clone(&job));
            // Forget the oldest finished jobs beyond the retention limit.
            let finished: Vec<u64> = reg
                .jobs
                .values()
                .filter(|j| j.state().is_terminal())
                .map(|j| j.id)
                .collect();
            if finished.len() > self.shared.max_retained {
                for id in &finished[..finished.len() - self.shared.max_retained] {
                    reg.jobs.remove(id);
                }
            }
            job
        };
        let view = self.view_of(&job);
        let shared = Arc::clone(&self.shared);
        tokio::spawn(run_job(shared, job));
        Ok(view)
    }

    fn view_of(&self, job: &Job) -> JobView {
        let reg = lock(&self.shared.registry);
        queue_view(&reg, job)
    }

    /// Look a job up.
    ///
    /// # Errors
    /// 404 `job_not_found`.
    pub fn get(&self, id: u64) -> ApiResult<Arc<Job>> {
        lock(&self.shared.registry)
            .jobs
            .get(&id)
            .cloned()
            .ok_or_else(|| ApiError::not_found("job_not_found", format!("no job {id}")))
    }

    /// View of one job.
    ///
    /// # Errors
    /// 404 `job_not_found`.
    pub fn view(&self, id: u64) -> ApiResult<JobView> {
        let reg = lock(&self.shared.registry);
        let job = reg
            .jobs
            .get(&id)
            .ok_or_else(|| ApiError::not_found("job_not_found", format!("no job {id}")))?;
        Ok(queue_view(&reg, job))
    }

    /// All remembered jobs, oldest first.
    #[must_use]
    pub fn list(&self) -> Vec<JobView> {
        let reg = lock(&self.shared.registry);
        reg.jobs.values().map(|j| queue_view(&reg, j)).collect()
    }

    /// Cancel an active job, or forget a finished one. Returns the view after
    /// the request and whether the job was removed.
    ///
    /// # Errors
    /// 404 `job_not_found`.
    pub fn cancel(&self, id: u64) -> ApiResult<(JobView, bool)> {
        let job = self.get(id)?;
        if job.state().is_terminal() {
            let view = self.view_of(&job);
            lock(&self.shared.registry).jobs.remove(&id);
            return Ok((view, true));
        }
        job.cancel.store(true, Ordering::Relaxed);
        // A queued job has no pipeline to notice the flag; finish it here.
        if job.state() == JobState::Queued {
            job.transition(JobState::Cancelled, None);
        }
        job.wake.notify_one();
        Ok((self.view_of(&job), false))
    }

    /// Ask every active job to stop.
    pub fn shutdown(&self) {
        let jobs: Vec<Arc<Job>> = lock(&self.shared.registry).jobs.values().cloned().collect();
        for j in jobs {
            if !j.state().is_terminal() {
                j.cancel.store(true, Ordering::Relaxed);
                if j.state() == JobState::Queued {
                    j.transition(JobState::Cancelled, None);
                }
                j.wake.notify_one();
            }
        }
    }

    /// Counts for `/version`.
    #[must_use]
    pub fn active_count(&self) -> usize {
        lock(&self.shared.registry)
            .jobs
            .values()
            .filter(|j| !j.state().is_terminal())
            .count()
    }
}

/// Snapshot of a view, including the queue position.
pub fn snapshot(job: &Job, mgr: &JobManager) -> JobView {
    mgr.view_of(job)
}

fn queue_view(reg: &Registry, job: &Job) -> JobView {
    let pos = reg
        .jobs
        .values()
        .filter(|j| j.id < job.id && j.state() == JobState::Queued)
        .count();
    job.view(Some(pos))
}

async fn run_job(shared: Arc<Shared>, job: Arc<Job>) {
    // Semaphore waiters are served in arrival order, so the queue is FIFO.
    let permit = tokio::select! {
        p = Arc::clone(&shared.permits).acquire_owned() => p.ok(),
        () = job.wake.notified() => None,
    };
    let Some(_permit) = permit else { return };
    if job.cancel.load(Ordering::Relaxed) || !job.transition(JobState::Running, None) {
        return;
    }
    let worker = Arc::clone(&job);
    let sh = Arc::clone(&shared);
    let result = tokio::task::spawn_blocking(move || execute(&worker, &sh.backends)).await;
    match result {
        Ok(Ok(())) => {
            job.transition(JobState::Succeeded, None);
        }
        Ok(Err(RunError::Cancelled)) => {
            job.transition(JobState::Cancelled, None);
        }
        Ok(Err(RunError::Failed(e))) => {
            job.transition(JobState::Failed, Some(e));
        }
        Err(_) => {
            job.transition(
                JobState::Failed,
                Some(ErrorBody {
                    code: "internal".into(),
                    message: "worker panicked".into(),
                }),
            );
        }
    }
}

fn failed(code: &str, e: impl std::fmt::Display) -> RunError {
    RunError::Failed(ErrorBody {
        code: code.to_owned(),
        message: e.to_string(),
    })
}

/// The field rotation correction the job asks for: the site and the capture's
/// timestamps, or why it cannot be had.
fn field_rotation_for(
    spec: &JobSpec,
    preset: Option<Preset>,
) -> Result<Option<helia_core::field_rotation::FieldRotation>, RunError> {
    let Some(f) = &spec.config.field_rotation else {
        return Ok(None);
    };
    let site = spec
        .config
        .site()
        .map_err(|e| failed("invalid_config", e.message))?
        .or_else(Site::from_env)
        .ok_or_else(|| {
            failed(
                "invalid_config",
                "`field_rotation` needs the observing site: give `site` or set HELIA_SITE",
            )
        })?;
    let info = CaptureInfo::read(&spec.input, preset);
    if info.frame_unix.iter().all(Option::is_none) {
        return Err(failed(
            "invalid_config",
            "`field_rotation` needs the time of each frame, and this capture has none (SER files carry timestamps; AVI and video do not)",
        ));
    }
    Ok(Some(helia_core::field_rotation::FieldRotation {
        site,
        mirror: f.mirror,
        reference: f
            .reference()
            .map_err(|e| failed("invalid_config", e.message))?,
        target: None,
        auto_mirror: false,
        frame_unix: info.frame_unix,
    }))
}

/// What the request left unsaid, filled from the filter and the image scale,
/// and how the output is to be coloured.
fn plan_run(
    spec: &JobSpec,
    src: &mut dyn helia_core::io::source::FrameSource,
) -> Result<
    (
        StackConfig,
        OutputPlan,
        helia_core::recommend::Recommendation,
    ),
    RunError,
> {
    let (width, height) = src.dimensions();
    let is_bayer = src.read(0).is_ok_and(|f| f.bayer.is_some());
    let explicit = spec
        .config
        .explicit_preset()
        .map_err(|e| failed("invalid_config", e.message))?;
    let rec = helia_core::recommend::recommend(
        width,
        height,
        is_bayer,
        &helia_core::recommend::capture_texts(&spec.input),
        explicit,
    );
    let mut stack = spec.stack.clone();
    if spec.config.channel.is_none() {
        stack.debayer.channel = rec.channel;
        stack.clip_fallback = true;
    }
    if spec.config.keep.is_none() && spec.config.keep_frames.is_none() {
        stack.keep = rec.keep;
    }
    if spec.config.ap_size.is_none()
        && let Some(mp) = stack.multipoint.as_mut()
    {
        mp.ap_size = rec.ap_size;
    }
    if stack.true_colour.is_some() && !is_bayer {
        return Err(failed(
            "invalid_config",
            "`colour: \"true\"` needs a raw colour capture (a Bayer mosaic); this capture is mono or already demosaiced",
        ));
    }
    stack.field_rotation = field_rotation_for(spec, rec.preset)?;
    if spec.config.ap_auto {
        stack.auto_ap = Some(helia_core::autoap::AutoAp::for_preset(
            rec.preset,
            rec.ap_size,
        ));
    }
    if spec.config.seeing || spec.config.report {
        stack.seeing = Some(helia_core::seeing::SeeingSettings {
            aperture_mm: spec.config.aperture_mm,
            wavelength_nm: rec.preset.map(helia_core::seeing::preset_wavelength_nm),
        });
    }
    let colour_preset = match spec
        .config
        .colour_choice()
        .map_err(|e| failed("invalid_config", e.message))?
    {
        Some(ColourChoice::Mono | ColourChoice::True) => None,
        Some(ColourChoice::Preset(p)) => Some(p),
        Some(ColourChoice::Auto) => Some(rec.preset.ok_or_else(|| {
            failed(
                "no_filter",
                "`colour` is `auto` but no filter was recognised; give `preset`",
            )
        })?),
        None => explicit,
    };
    let colour = ColourParams {
        preset: colour_preset,
        invert: spec.config.invert,
        strength: spec.config.colour_strength,
        grade: spec
            .config
            .grade
            .as_deref()
            .map(helia_core::grade::Grade::parse)
            .transpose()
            .map_err(|e| failed("invalid_config", e.to_string()))?
            .unwrap_or_default(),
    };
    let edits = spec
        .config
        .edit_list(rec.preset, width, height)
        .map_err(|e| failed("invalid_config", e.message))?;
    let plan = OutputPlan {
        edits,
        colour,
        edit_fits: spec.config.sharpen_fits,
        lead_notes: Vec::new(),
        fits: None,
        rgb: None,
    };
    Ok((stack, plan, rec))
}

/// Draw the science report of a finished stack.
fn science_report(
    spec: &JobSpec,
    out: &helia_core::pipeline::StackOutput,
    src: &mut dyn FrameSource,
    sharpened: Option<&helia_core::Frame>,
    solar: helia_report::SolarInputs,
) -> Result<helia_report::Bundle, RunError> {
    let metric = format!("{:?}", spec.stack.metric);
    let mut extras = helia_report::Extras::gather(src, spec.stack.debayer, out);
    extras.sharpened = sharpened.cloned();
    extras.solar = solar;
    extras.aperture_mm = spec.config.aperture_mm;
    extras.wavelength_nm = spec.stack.seeing.and_then(|s| s.wavelength_nm);
    let data = helia_report::ScienceData::from_output(out, &metric, &extras);
    let meta = helia_report::html::Meta {
        title: spec.input_display.clone(),
        inputs: vec![
            ("capture".into(), spec.input_display.clone()),
            ("stack".into(), spec.output_display.clone()),
        ],
        settings: helia_report::settings::from_config(&spec.stack, out),
        generated: helia_core::io::solarnet::iso_from_unix(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0.0, |d| d.as_secs_f64()),
        ),
        version: helia_core::about::display_version(env!("CARGO_PKG_VERSION")),
    };
    let fits = helia_report::settings::fits_header_summary(&spec.output);
    helia_report::generate(&data, &meta, fits, helia_report::canvas::Size::default())
        .map_err(|e| failed("report_failed", e))
}

/// Edit, colour (when asked, and not FITS) and write the output. Returns
/// what was written and the image that went into the file.
#[allow(
    clippy::too_many_arguments,
    reason = "the plan, the capture and the plugin hook"
)]
fn write_output(
    job: &Job,
    backend: &Arc<dyn Backend>,
    out: &helia_core::pipeline::StackOutput,
    filter: Option<Preset>,
    sunspots: Option<&helia_core::sunspots::Report>,
    plan: &OutputPlan,
    capture: &CaptureReport,
    post: Option<&dyn Fn(helia_core::Frame) -> Result<helia_core::Frame, RunError>>,
    provenance: &dyn Fn() -> (Vec<String>, Vec<helia_core::io::fits::HeaderCard>),
) -> Result<(helia_core::output::Written, helia_core::Frame), RunError> {
    let spec = &job.spec;
    let image = &out.image;
    if let Some(parent) = spec.output.parent() {
        std::fs::create_dir_all(parent).map_err(|e| failed("write_failed", e))?;
    }
    let format =
        ImageFormat::from_path(&spec.output).map_err(|e| failed("unsupported_output_format", e))?;
    let mut plan = plan.clone();
    plan.lead_notes.extend(capture.note_lines());
    plan.lead_notes
        .extend(out.calibration_notes.iter().cloned());
    plan.lead_notes
        .extend(out.channel_note.iter().map(|n| format!("Channel: {n}")));
    if let Some(c) = &out.colour {
        plan.rgb = Some(c.rgb.clone());
        plan.lead_notes.extend(c.notes.iter().cloned());
    }
    if format == ImageFormat::Fits {
        let site = spec.config.site().ok().flatten().or_else(Site::from_env);
        plan.fits = Some(
            FitsMeta::from_stack(&CaptureInfo::read(&spec.input, filter), out, site)
                .with_sunspots(sunspots.cloned()),
        );
    }
    let plan = &plan;
    if !plan.edits.is_empty() && (format != ImageFormat::Fits || plan.edit_fits) {
        job.on_named("edit", 0.0);
    }
    let mut prepared = prepare(backend.as_ref(), image, &spec.output, plan)
        .map_err(|e| failed("edit_failed", e))?;
    // Plugin image steps follow Helia's own edits, and like them skip FITS unless asked.
    if let Some(post) = post
        && prepared.rgb.is_none()
        && (format != ImageFormat::Fits || plan.edit_fits)
    {
        prepared.image = post(prepared.image.clone())?;
    }
    // Every plugin that took part, now that the post-edit steps have run.
    let (lines, cards) = provenance();
    prepared.lines.extend(lines);
    let mut plan = plan.clone();
    if let Some(f) = plan.fits.take() {
        plan.fits = Some(f.with_plugin_cards(cards));
    }
    let plan = &plan;
    if prepared.edited {
        job.on_named("edit", 1.0);
    }
    if plan.colour.preset.is_some() && format != ImageFormat::Fits {
        job.on_named("colourise", 0.0);
        job.on_named("colourise", 1.0);
    }
    job.on_named("write", 0.0);
    let written = finish(backend.as_ref(), &prepared, &spec.output, plan).map_err(|e| {
        let _ = std::fs::remove_file(&spec.output);
        failed("write_failed", e)
    })?;
    job.on_named("write", 1.0);
    Ok((written, prepared.image))
}

#[allow(
    clippy::too_many_lines,
    reason = "one sequence: open, calibrate, run, write"
)]
fn execute(job: &Arc<Job>, backends: &BackendCache) -> Result<(), RunError> {
    let spec = &job.spec;
    job.on_named("opening", 0.0);
    // A compressed video is decoded first; its progress is the "opening" stage.
    let reporter = Arc::clone(job);
    let video = VideoOptions {
        cache_dir: spec.cache_dir.clone(),
        cancel: Some(Arc::clone(&job.cancel)),
        progress: Some(Arc::new(move |done, total| {
            reporter.on_named("opening", done as f32 / total.max(1) as f32);
        })),
        ..VideoOptions::default()
    };
    let (mut src, capture) = open_reported_with(
        &spec.input,
        &OpenOptions {
            channel: spec.stack.debayer.channel,
            ..spec.open
        },
        &video,
    )
    .map_err(|e| match e {
        CoreError::Cancelled => RunError::Cancelled,
        CoreError::Ffmpeg(
            helia_core::io::ffmpeg::FfmpegError::NotFound { .. }
            | helia_core::io::ffmpeg::FfmpegError::TooOld { .. },
        ) => failed("ffmpeg_unavailable", e),
        other => failed("invalid_capture", other),
    })?;
    let backend = backends
        .get(spec.config.backend)
        .map_err(|e| failed("backend_unavailable", e))?;
    lock(&job.data).backend = Some(backend.name());
    let (mut stack, plan, rec) = plan_run(spec, src.as_mut())?;
    let mut calibration_lines = Vec::new();
    if let Some(cal) = &spec.calibration {
        let exposure = helia_core::io::solarnet::CaptureInfo::read(&spec.input, None).exposure_s;
        let opening = Arc::clone(job);
        let resolved = cal
            .resolve(
                exposure,
                &|_what, f| opening.on_named("opening", f),
                &job.cancel,
            )
            .map_err(|e| match e {
                CoreError::Cancelled => RunError::Cancelled,
                other => failed("invalid_calibration", other),
            })?;
        calibration_lines = resolved.calibration.summary_lines();
        calibration_lines.extend(resolved.warnings.iter().map(|w| format!("warning: {w}")));
        stack.calibration = Some(resolved.calibration.shared());
    }

    let progress = |stage: Stage, f: f32| job.on_progress(stage, f);
    let plugin_specs = spec
        .config
        .plugin_specs()
        .map_err(|e| failed("invalid_config", e.message))?;
    let runner = Arc::new(Mutex::new(helia_plugin::Runner::new(
        helia_plugin::Registry::open(&[]),
    )));
    let plugin_failed = |e: helia_plugin::HostError| {
        if e.is_cancelled() {
            RunError::Cancelled
        } else {
            failed("plugin_failed", e)
        }
    };
    let mut out = if let Some(name) = &spec.config.stack_backend {
        let job_json = helia_plugin::job::stack_job(
            &stack,
            &spec.open,
            rec.preset.map(helia_core::Preset::id),
        )
        .map_err(|m| failed("plugin_unsupported", m))?;
        let stacked = lock(&runner)
            .stack(name, &spec.input, &job_json, &job.cancel, &mut |p| {
                if let Some(f) = p.fraction {
                    progress(Stage::Stack, f as f32);
                }
            })
            .map_err(plugin_failed)?;
        helia_core::pipeline::StackOutput::from_image(stacked.frame, format!("plugin {name}"))
    } else {
        let frame_steps = lock(&runner)
            .frame_steps(&plugin_specs)
            .map_err(plugin_failed)?;
        // A frame step sees each frame on its way in; `report` is refused with one, so the
        // reader left in `src` is only needed when there is none.
        let (mut piped, failure) = if frame_steps.is_empty() {
            (None, None)
        } else {
            let inner = std::mem::replace(&mut src, Box::new(MemorySource(Vec::new())));
            let frames = helia_plugin::PluginFrames::new(
                inner,
                Arc::clone(&runner),
                frame_steps,
                Arc::clone(&job.cancel),
            );
            let failure = frames.failure();
            (Some(frames), Some(failure))
        };
        let run = match piped.as_mut() {
            Some(p) => Pipeline::new(stack).with_backend(Arc::clone(&backend)).run(
                p,
                &progress,
                &job.cancel,
            ),
            None => Pipeline::new(stack).with_backend(Arc::clone(&backend)).run(
                &mut src,
                &progress,
                &job.cancel,
            ),
        };
        match run {
            Ok(o) => o,
            Err(CoreError::Cancelled) => return Err(RunError::Cancelled),
            Err(other) => {
                let own = failure.and_then(|f| lock(&f).take());
                return Err(match own {
                    Some(h) => failed("plugin_failed", h),
                    None => failed("stack_failed", other),
                });
            }
        }
    };
    if job.cancel.load(Ordering::Relaxed) {
        return Err(RunError::Cancelled);
    }
    if spec
        .config
        .plugin_steps
        .iter()
        .any(|t| t.starts_with("pre:"))
    {
        let frame = lock(&runner)
            .image_steps(
                &plugin_specs,
                helia_plugin::StepStage::PreEdit,
                out.image.clone(),
                &job.cancel,
                &mut |_, _| {},
            )
            .map_err(plugin_failed)?;
        out.image = frame;
    }
    let info = CaptureInfo::read(&spec.input, rec.preset);
    let when = spec
        .config
        .date
        .as_deref()
        .and_then(helia_core::io::solarnet::unix_from_iso)
        .or_else(|| {
            FitsMeta::from_stack(&info, &out, None)
                .date_avg
                .or_else(|| info.mid_unix())
        });
    let spot_params = spec.config.sunspots.as_ref().map_or_else(
        helia_core::sunspots::Params::default,
        SunspotsConfig::params,
    );
    let detection = spec
        .config
        .sunspots
        .as_ref()
        .and_then(|_| helia_core::sunspots::detect(&out.image, when, &spot_params));
    let post_steps = |frame: helia_core::Frame| -> Result<helia_core::Frame, RunError> {
        lock(&runner)
            .image_steps(
                &plugin_specs,
                helia_plugin::StepStage::PostEdit,
                frame,
                &job.cancel,
                &mut |_, _| {},
            )
            .map_err(plugin_failed)
    };
    let post_ref: &dyn Fn(helia_core::Frame) -> Result<helia_core::Frame, RunError> = &post_steps;
    let (written, final_image) = write_output(
        job,
        &backend,
        &out,
        rec.preset,
        detection.as_ref().map(|d| &d.report),
        &plan,
        &capture,
        (!plugin_specs.is_empty()).then_some(post_ref),
        &|| {
            let r = lock(&runner);
            (r.provenance(), r.fits_cards())
        },
    )?;
    let analyses = lock(&runner)
        .analyse_steps(&plugin_specs, &final_image, &job.cancel, &mut |_, _| {})
        .map_err(plugin_failed)?;
    lock(&runner).close_all();
    let colour = plan.colour;
    let edited = !plan.edits.is_empty()
        && (ImageFormat::from_path(&spec.output).is_ok_and(|f| f != ImageFormat::Fits)
            || plan.edit_fits);
    let report = if spec.config.report {
        let solar = helia_report::SolarInputs {
            when_unix: when,
            celestial_north_up: spot_params.celestial_north_up,
            rotate_deg: spot_params.rotate_deg,
            mirror: spot_params.mirror,
            sunspots: detection
                .as_ref()
                .map(|d| (d.report.clone(), d.overlay.clone())),
        };
        Some(Arc::new(science_report(
            spec,
            &out,
            &mut src,
            edited.then_some(&final_image),
            solar,
        )?))
    } else {
        None
    };
    let preview = imaging::downscale(
        &imaging::to_grey8(&final_image, Stretch::Gamma),
        PREVIEW_STORE_WIDTH,
    );
    let mut d = lock(&job.data);
    d.preview = Some(Arc::new(Preview(preview)));
    d.report.clone_from(&report);
    d.sunspots = detection.as_ref().and_then(|det| {
        let mut png = Vec::new();
        helia_core::io::png::write_png8_rgb(&mut png, &det.overlay, None).ok()?;
        Some(Arc::new(SunspotFiles {
            csv: det.report.csv(),
            png,
        }))
    });
    d.timings = out
        .timings
        .iter()
        .map(|(s, t)| StageTiming {
            stage: stage_info(*s).0.to_owned(),
            seconds: t.as_secs_f32(),
        })
        .collect();
    d.summary = Some(JobSummary {
        frames: out.scores.len(),
        kept: out.kept.len(),
        registered: out.shifts.len(),
        alignment_points_active: out
            .grid
            .as_ref()
            .map(helia_core::align::multipoint::ApGrid::active_count),
        alignment_points_total: out.grid.as_ref().map(|g| g.aps.len()),
        width: written.width,
        height: written.height,
        filter: rec.preset.map(|p| p.id().to_owned()),
        filter_source: rec.detected_from.clone(),
        colour: if out.colour.is_some() {
            Some("true colour".to_owned())
        } else {
            colour.preset.is_some().then(|| colour.describe())
        },
        alignment: AlignmentSummary::from(&out.alignment),
        bayer: capture
            .bayer()
            .map(|p| helia_core::io::capture::pattern_name(p).to_ascii_lowercase()),
        bayer_source: capture.effective.bayer.origin.label().to_owned(),
        notices: capture
            .notices
            .iter()
            .map(|n| n.text.clone())
            .chain(out.crop_skipped.map(|w| w.reason().to_owned()))
            .collect(),
        edits: written.edit_notes.clone(),
        edited: written.edited,
        sharpen_check: written
            .edited
            .then(|| {
                let psf = plan
                    .edits
                    .resolve(&out.image)
                    .ops
                    .iter()
                    .find_map(|op| match op {
                        Op::Deconvolve(d) => Some(d.psf),
                        _ => None,
                    });
                helia_core::edit::diagnose::check(&out.image, &final_image, psf)
            })
            .flatten()
            .map(|c| SharpenCheckSummary::from(&c)),
        calibration: calibration_lines,
        auto_ap: out.auto_ap.as_ref().map(AutoApSummary::from),
        limb_darkening: out.limb.as_ref().map(LimbSummary::from),
        true_colour: out.colour.as_ref().map(TrueColourSummary::from),
        crop: out.crop.map(|c| Region {
            x: c.rect.x0,
            y: c.rect.y0,
            w: c.rect.width(),
            h: c.rect.height(),
        }),
        granulation: helia_core::granulation::for_stack(
            &out.image,
            rec.preset,
            out.disc.map(|d| d.r),
            spec.config.aperture_mm,
        )
        .as_ref()
        .map(GranulationSummary::from),
        seeing: out.seeing.as_ref().map(SeeingSummary::from),
        sunspots: detection.as_ref().map(|d| SunspotsSummary::from(&d.report)),
        plugins: analyses
            .into_iter()
            .map(|(s, a)| PluginSummary {
                plugin: s.plugin,
                step: s.step,
                metrics: a.metrics,
                overlays: a.overlays,
                notes: a.notes,
            })
            .collect(),
        stacked_by: spec.config.stack_backend.clone(),
        report: report.as_ref().map(|r| ReportSummary {
            files: r.files.iter().map(|f| f.name.clone()).collect(),
            not_drawn: r
                .sections
                .iter()
                .flat_map(|s| s.notes.iter().cloned())
                .collect(),
        }),
        field_rotation: out
            .field_rotation
            .as_ref()
            .map(|p| FieldRotationSummary::new(p, out.disc.map(|d| d.r))),
    });
    Ok(())
}
