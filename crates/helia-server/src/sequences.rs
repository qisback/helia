// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Operations on several stacked images at once: the time-lapse (and, in the
//! same shape, derotation and mosaics).
//!
//! The captures are stacked by ordinary jobs (`POST /api/v1/jobs`, one per
//! capture, with the same settings); these endpoints take the stacked images
//! those jobs wrote. They run in the request and answer when done.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use axum::Json;
use axum::extract::State;
use helia_core::animate::MovieChoice;
use helia_core::io::solarnet::unix_from_iso;
use helia_core::timelapse as tl;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::AppState;
use crate::error::{ApiError, ApiResult, ErrorEnvelope};
use crate::jobs::Region;
use crate::paths::Sandbox;
use crate::routes::ApiJson;

/// Most images one request may combine.
pub const MAX_INPUTS: usize = 200;

fn invalid(code: &'static str, msg: impl Into<String>) -> ApiError {
    ApiError::unprocessable(code, msg)
}

/// A stacked image is a TIFF, PNG or FITS file; a capture is stacked first.
fn check_image(sb: &Sandbox, raw: &str) -> ApiResult<PathBuf> {
    let p = sb.resolve_file(raw)?;
    let ok = p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "tif" | "tiff" | "png" | "fits" | "fit" | "fts"
        )
    });
    if ok {
        Ok(p)
    } else {
        Err(invalid(
            "unsupported_input",
            format!(
                "{raw} is not a stacked image (.tif, .tiff, .png or .fits); stack the capture first with POST /api/v1/jobs"
            ),
        ))
    }
}

/// How the Sun's differential rotation is undone (`derotate` of a time-lapse,
/// and the settings of `POST /solar-derotation`); every field is optional.
#[derive(Clone, Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, default)]
pub struct DerotateConfig {
    /// The time to bring every image to, UTC (`YYYY-MM-DDThh:mm:ss`). Default:
    /// the reference frame's time in a time-lapse, the middle time of the
    /// images in `POST /solar-derotation` (which also takes `first`, `middle`
    /// and `last`).
    pub to: Option<String>,
    /// `howard` (default), `snodgrass`, or `A,B,C`: degrees a day for
    /// `A + B sin^2 + C sin^4` of the latitude (`A` positive).
    pub law: Option<String>,
    /// Degrees counter-clockwise from the top of the image to solar north
    /// (default 0).
    pub north_angle_deg: f32,
    /// The images have celestial north up: solar north is turned by the
    /// position angle P at each image's time, on top of `north_angle_deg`.
    pub celestial_north_up: bool,
    /// The images are mirror images (west on the left).
    pub mirror: bool,
    /// Move the pixel values as they are, not the contrast against the limb
    /// darkening.
    pub raw_brightness: bool,
}

impl DerotateConfig {
    fn law(&self) -> ApiResult<helia_core::derotate::RotationLaw> {
        self.law
            .as_deref()
            .unwrap_or("howard")
            .parse()
            .map_err(|e: helia_core::Error| invalid("invalid_config", e.to_string()))
    }

    fn orientation(&self) -> ApiResult<helia_core::derotate::Orientation> {
        if !self.north_angle_deg.is_finite() {
            return Err(invalid(
                "invalid_config",
                "`north_angle_deg` must be a number",
            ));
        }
        Ok(helia_core::derotate::Orientation {
            rotate_deg: self.north_angle_deg,
            mirror: self.mirror,
            celestial_north_up: self.celestial_north_up,
        })
    }

    fn for_series(&self) -> ApiResult<tl::Derotation> {
        Ok(tl::Derotation {
            law: self.law()?,
            orient: self.orientation()?,
            target: match self.to.as_deref() {
                None => tl::DerotateTarget::Reference,
                Some(t) => tl::DerotateTarget::Unix(parse_time(t)?),
            },
            preserve_contrast: !self.raw_brightness,
        })
    }
}

fn parse_time(s: &str) -> ApiResult<f64> {
    unix_from_iso(s).ok_or_else(|| {
        invalid(
            "invalid_config",
            format!("cannot read the time {s:?}; use YYYY-MM-DDThh:mm:ss in UTC"),
        )
    })
}

/// `POST /timelapse` body.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TimelapseRequest {
    /// Stacked images, relative to the data root, one per frame (2 to 200).
    /// They are put in time order when every one has a time.
    pub inputs: Vec<String>,
    /// UTC capture time of each input (`YYYY-MM-DDThh:mm:ss`), in the order
    /// given in `inputs`. Without it a FITS file's `DATE-AVG` or a file name
    /// that holds a full date and time is used; frames with no time are kept
    /// in the order given.
    #[serde(default)]
    pub times: Option<Vec<String>>,
    /// Directory for the frames, the movie and the table, relative to the data
    /// root. Created when missing.
    pub output: String,
    /// Frame format: `png` (8-bit, default) or `tiff` (16-bit).
    #[serde(default)]
    pub format: Option<String>,
    /// `auto` (default: MP4 when ffmpeg is installed, else an animated PNG),
    /// `mp4`, `apng` or `none`.
    #[serde(default)]
    pub movie: Option<String>,
    /// Frames a second of the movie (default 10, up to 120).
    #[serde(default)]
    pub fps: Option<f32>,
    /// The frame the others are registered to: `first`, `middle` (default) or
    /// a number counting from 1.
    #[serde(default)]
    pub reference: Option<String>,
    /// Refine the limb shift by phase correlation of the disc centre
    /// (default true).
    #[serde(default)]
    pub refine: Option<bool>,
    /// Largest correction in pixels the refinement may make (default 3).
    #[serde(default)]
    pub max_refine_px: Option<f32>,
    /// `off` (default), `match` or `smooth`.
    #[serde(default)]
    pub deflicker: Option<String>,
    /// Frames in the running median of `smooth` (odd, at least 3; default 5).
    #[serde(default)]
    pub deflicker_window: Option<usize>,
    /// Cut every frame to the area all of them cover.
    #[serde(default)]
    pub crop: bool,
    /// Bring every frame to one time by the Sun's differential rotation (an
    /// empty object for the defaults). Needs the time of every input.
    #[serde(default)]
    pub derotate: Option<DerotateConfig>,
    /// Replace an earlier time-lapse in `output`.
    #[serde(default)]
    pub overwrite: bool,
    /// Also write the diagnostics of the series into `output/report/`: how
    /// each frame was registered, the brightness before and after
    /// `deflicker`, and the derotation displacement, as PNG, CSV and JSON and
    /// one self-contained `report.html` (`summary` `report`).
    #[serde(default)]
    pub report: bool,
}

/// One frame of a finished time-lapse.
#[derive(Serialize, ToSchema)]
pub struct TimelapseFrame {
    /// Position in the sequence, from 1.
    pub frame: usize,
    /// The input it came from.
    pub input: String,
    /// The frame file, relative to the data root.
    pub file: String,
    /// Capture time, UTC, when known.
    pub time: Option<String>,
    /// Shift applied, pixels (the frame's content was at the reference plus
    /// this).
    pub shift_x: f32,
    /// Shift applied, pixels.
    pub shift_y: f32,
    /// `reference`, `limb`, `limb+phase`, `phase` or `failed`.
    pub method: String,
    /// Confidence of the frame's limb fit, 0 to 1, when it has one.
    pub limb_confidence: Option<f32>,
    /// Peak-to-sidelobe ratio of the phase correlation, when it ran.
    pub phase_psr: Option<f32>,
    /// Gain applied for the brightness (1 without `deflicker`).
    pub gain: f32,
    /// The most the derotation moved the surface of this frame, pixels, when
    /// it ran.
    pub derotated_px: Option<f32>,
}

/// What a time-lapse made.
#[derive(Serialize, ToSchema)]
pub struct TimelapseResponse {
    /// The frames in order.
    pub frames: Vec<TimelapseFrame>,
    /// Width of the written frames.
    pub width: usize,
    /// Height of the written frames.
    pub height: usize,
    /// The reference frame, from 1.
    pub reference: usize,
    /// The area of the stacked images that was kept, with `crop`.
    pub crop: Option<Region>,
    /// The movie file, relative to the data root, when one was made.
    pub movie: Option<String>,
    /// `mp4` or `apng`.
    pub movie_kind: Option<String>,
    /// The registration table (CSV), relative to the data root.
    pub table: String,
    /// The diagnostics page (`report.html`), relative to the data root, with
    /// `report`; its figures and data are beside it.
    pub report: Option<String>,
    /// Plain-language lines: what registration did, and any caveat.
    pub notes: Vec<String>,
}

fn parse_request(
    req: &TimelapseRequest,
) -> ApiResult<(tl::Options, tl::FrameFormat, MovieChoice, f32)> {
    let format: tl::FrameFormat = req
        .format
        .as_deref()
        .unwrap_or("png")
        .parse()
        .map_err(|e: helia_core::Error| invalid("invalid_config", e.to_string()))?;
    let movie: MovieChoice = req
        .movie
        .as_deref()
        .unwrap_or("auto")
        .parse()
        .map_err(|e: helia_core::Error| invalid("invalid_config", e.to_string()))?;
    let fps = req.fps.unwrap_or(10.0);
    if !(fps.is_finite() && fps > 0.0 && fps <= 120.0) {
        return Err(invalid("invalid_config", "`fps` must be between 0 and 120"));
    }
    let reference = match req
        .reference
        .as_deref()
        .unwrap_or("middle")
        .parse::<tl::Reference>()
        .map_err(|e| invalid("invalid_config", e.to_string()))?
    {
        tl::Reference::Index(0) => {
            return Err(invalid(
                "invalid_config",
                "`reference` counts frames from 1",
            ));
        }
        tl::Reference::Index(n) => tl::Reference::Index(n - 1),
        other => other,
    };
    let window = req.deflicker_window.unwrap_or(5);
    let deflicker = match req.deflicker.as_deref().unwrap_or("off") {
        "off" => tl::Deflicker::Off,
        "match" => tl::Deflicker::Match,
        "smooth" => {
            if window < 3 || window.is_multiple_of(2) {
                return Err(invalid(
                    "invalid_config",
                    "`deflicker_window` must be odd and at least 3",
                ));
            }
            tl::Deflicker::Smooth(window)
        }
        other => {
            return Err(invalid(
                "invalid_config",
                format!("unknown `deflicker` {other:?}; use off, match or smooth"),
            ));
        }
    };
    let max_refine_px = req.max_refine_px.unwrap_or(3.0);
    if !(max_refine_px.is_finite() && max_refine_px >= 0.0) {
        return Err(invalid(
            "invalid_config",
            "`max_refine_px` must not be negative",
        ));
    }
    Ok((
        tl::Options {
            reference,
            refine: req.refine.unwrap_or(true),
            max_refine_px,
            deflicker,
            crop: req.crop,
            kernel: helia_core::Kernel::default(),
            derotate: req
                .derotate
                .as_ref()
                .map(DerotateConfig::for_series)
                .transpose()?,
        },
        format,
        movie,
        fps,
    ))
}

/// Register stacked images to each other and write a time-lapse: numbered
/// frames, a registration table and a movie.
///
/// Stack every capture first, with the same settings, using ordinary jobs. The
/// request runs to the end before it answers; a long series with an MP4 can take
/// a while.
#[utoipa::path(post, path = "/api/v1/timelapse", tag = "sequences",
    request_body = TimelapseRequest,
    responses(
        (status = 200, body = TimelapseResponse),
        (status = 409, description = "`output` already holds a time-lapse and `overwrite` is false (`output_exists`)", body = ErrorEnvelope),
        (status = 422, description = "bad settings, fewer than two inputs, or an input that is not a stacked image", body = ErrorEnvelope)))]
pub(crate) async fn timelapse(
    State(st): State<AppState>,
    ApiJson(req): ApiJson<TimelapseRequest>,
) -> ApiResult<Json<TimelapseResponse>> {
    if req.inputs.len() < 2 || req.inputs.len() > MAX_INPUTS {
        return Err(invalid(
            "invalid_config",
            format!("`inputs` needs 2 to {MAX_INPUTS} stacked images"),
        ));
    }
    let (opts, format, movie, fps) = parse_request(&req)?;
    let inputs: Vec<PathBuf> = req
        .inputs
        .iter()
        .map(|r| check_image(&st.sandbox, r))
        .collect::<ApiResult<_>>()?;
    let given: Vec<Option<f64>> = match &req.times {
        Some(t) => {
            if t.len() != inputs.len() {
                return Err(invalid(
                    "invalid_config",
                    "`times` must have one entry for every input",
                ));
            }
            t.iter()
                .map(|s| {
                    unix_from_iso(s).map(Some).ok_or_else(|| {
                        invalid(
                            "invalid_config",
                            format!("cannot read the time {s:?}; use YYYY-MM-DDThh:mm:ss in UTC"),
                        )
                    })
                })
                .collect::<ApiResult<_>>()?
        }
        None => inputs
            .iter()
            .map(|p| {
                helia_core::captime::capture_time(p, &helia_core::captime::TimeHints::default())
                    .map(|t| t.unix)
            })
            .collect(),
    };
    let dir = st.sandbox.resolve(&req.output)?;
    if dir.is_file() {
        return Err(invalid(
            "invalid_output",
            "`output` is a file, not a directory",
        ));
    }
    if dir.join("timelapse.csv").exists() && !req.overwrite {
        return Err(ApiError::conflict(
            "output_exists",
            "output already holds a time-lapse; set overwrite to replace it",
        ));
    }
    let sb = Arc::clone(&st.sandbox);
    let want_report = req.report;
    let order = tl::order_by_time(&given);
    let result = tokio::task::spawn_blocking(move || -> ApiResult<TimelapseResponse> {
        let sorted: Vec<PathBuf> = order.iter().map(|&i| inputs[i].clone()).collect();
        let times: Vec<Option<f64>> = order.iter().map(|&i| given[i]).collect();
        let out = tl::Output {
            dir: dir.clone(),
            format,
            movie,
            fps,
        };
        let outcome = tl::run(
            &sorted,
            &times,
            &opts,
            &out,
            &|_| {},
            &AtomicBool::new(false),
        )
        .map_err(|e| invalid("timelapse_failed", e.to_string()))?;
        let mut response = respond(&sb, &sorted, &outcome);
        if want_report {
            response.report = Some(write_report(&sb, &dir, &opts, &outcome)?);
        }
        Ok(response)
    })
    .await
    .map_err(|e| ApiError::internal(format!("the worker stopped: {e}")))??;
    Ok(Json(result))
}

/// Write the diagnostics of a registered series into `dir/report/` and give
/// the page's path relative to the data root.
fn write_report(
    sb: &Sandbox,
    dir: &std::path::Path,
    opts: &tl::Options,
    outcome: &tl::Outcome,
) -> ApiResult<String> {
    let data = helia_report::ScienceData::from_sequence(&outcome.sequence);
    let meta = helia_report::html::Meta {
        title: format!("time-lapse of {} frames", outcome.sequence.report.len()),
        inputs: Vec::new(),
        settings: vec![
            ("reference".into(), format!("{:?}", opts.reference)),
            (
                "refine by phase correlation".into(),
                opts.refine.to_string(),
            ),
            ("deflicker".into(), format!("{:?}", opts.deflicker)),
            ("derotation".into(), format!("{:?}", opts.derotate)),
        ],
        generated: helia_core::io::solarnet::iso_from_unix(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0.0, |d| d.as_secs_f64()),
        ),
        version: helia_core::about::display_version(env!("CARGO_PKG_VERSION")),
    };
    let bundle = helia_report::generate(&data, &meta, None, helia_report::canvas::Size::default())
        .map_err(|e| invalid("report_failed", e.to_string()))?;
    let rdir = dir.join("report");
    bundle
        .write(&rdir)
        .map_err(|e| invalid("report_failed", e.to_string()))?;
    Ok(sb.display(&rdir.join(helia_report::REPORT_HTML)))
}

/// The answer for a finished time-lapse: the frames, their registration and the files.
fn respond(sb: &Sandbox, sorted: &[PathBuf], outcome: &tl::Outcome) -> TimelapseResponse {
    let seq = &outcome.sequence;
    let (width, height) = seq.frames.first().map_or((0, 0), helia_core::picture::size);
    let frames = seq
        .report
        .iter()
        .zip(&outcome.files)
        .zip(sorted)
        .map(|((r, file), input)| TimelapseFrame {
            frame: r.index + 1,
            input: sb.display(input),
            file: sb.display(file),
            time: r.time.map(helia_core::io::solarnet::iso_from_unix),
            shift_x: r.shift.dx,
            shift_y: r.shift.dy,
            method: r.method.label().to_owned(),
            limb_confidence: r.limb.map(|l| l.3),
            phase_psr: r.phase.map(|p| p.1),
            gain: r.gain,
            derotated_px: r.derotated_px,
        })
        .collect();
    let mut notes = tl::summary_lines(seq);
    let failed = seq.failed();
    if !failed.is_empty() {
        notes.push(format!(
            "no registration for frame(s) {}: no clear solar limb and no phase lock; they are left as they are",
            failed.iter().map(|i| (i + 1).to_string()).collect::<Vec<_>>().join(", ")
        ));
    }
    if let Some(n) = outcome.movie.as_ref().and_then(|m| m.note.clone()) {
        notes.push(n);
    }
    TimelapseResponse {
        frames,
        width,
        height,
        reference: seq.reference + 1,
        crop: seq.crop.map(|c| Region {
            x: c.x0,
            y: c.y0,
            w: c.width(),
            h: c.height(),
        }),
        movie: outcome.movie.as_ref().map(|m| sb.display(&m.path)),
        movie_kind: outcome.movie.as_ref().map(|m| m.kind.to_owned()),
        table: sb.display(&outcome.table),
        report: None,
        notes,
    }
}

/// `POST /solar-derotation` body.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SolarDerotationRequest {
    /// Images (`.tif`, `.tiff`, `.png`, `.fits`) relative to the data root, one
    /// to 200, each with the whole disc in view.
    pub inputs: Vec<String>,
    /// UTC time of each input, in the order given. Without it a FITS file's
    /// `DATE-AVG` or a file name with a full date and time is used; an input
    /// with no time is an error.
    #[serde(default)]
    pub times: Option<Vec<String>>,
    /// Directory for the results, named `NAME-derotated.EXT`, relative to the
    /// data root. Created when missing.
    pub output: String,
    /// Replace results that exist.
    #[serde(default)]
    pub overwrite: bool,
    /// The target time (`first`, `middle` (default), `last` or a UTC time), the
    /// rotation law and the orientation of the images.
    #[serde(flatten)]
    pub config: DerotateConfig,
}

/// One derotated image.
#[derive(Serialize, ToSchema)]
pub struct DerotatedImage {
    /// The input.
    pub input: String,
    /// The result, relative to the data root.
    pub file: String,
    /// The input's time, UTC.
    pub time: String,
    /// Seconds moved, positive forward.
    pub dt_s: f64,
    /// The most any pixel of the disc came from, pixels.
    pub max_shift_px: f32,
    /// The same at the disc centre.
    pub centre_shift_px: f32,
    /// The disc found in the input: centre x, centre y, radius, pixels.
    pub disc: [f64; 3],
}

/// What a solar derotation made.
#[derive(Serialize, ToSchema)]
pub struct SolarDerotationResponse {
    /// The time every image was brought to, UTC.
    pub target: String,
    /// The images, in the order given.
    pub images: Vec<DerotatedImage>,
}

/// Bring images of the Sun to one time by its differential rotation.
///
/// Each image is derotated on its own, about the disc fitted to it; the
/// result is not shifted, so use the time-lapse (or a mosaic) to register them.
#[utoipa::path(post, path = "/api/v1/solar-derotation", tag = "sequences",
    request_body = SolarDerotationRequest,
    responses(
        (status = 200, body = SolarDerotationResponse),
        (status = 409, description = "a result exists and `overwrite` is false (`output_exists`)", body = ErrorEnvelope),
        (status = 422, description = "bad settings, no time, or no clear disc", body = ErrorEnvelope)))]
pub(crate) async fn solar_derotation(
    State(st): State<AppState>,
    ApiJson(req): ApiJson<SolarDerotationRequest>,
) -> ApiResult<Json<SolarDerotationResponse>> {
    if req.inputs.is_empty() || req.inputs.len() > MAX_INPUTS {
        return Err(invalid(
            "invalid_config",
            format!("`inputs` needs 1 to {MAX_INPUTS} images"),
        ));
    }
    let law = req.config.law()?;
    let orient = req.config.orientation()?;
    let inputs: Vec<PathBuf> = req
        .inputs
        .iter()
        .map(|r| check_image(&st.sandbox, r))
        .collect::<ApiResult<_>>()?;
    let times: Vec<f64> = match &req.times {
        Some(t) => {
            if t.len() != inputs.len() {
                return Err(invalid(
                    "invalid_config",
                    "`times` must have one entry for every input",
                ));
            }
            t.iter().map(|s| parse_time(s)).collect::<ApiResult<_>>()?
        }
        None => inputs
            .iter()
            .zip(&req.inputs)
            .map(|(p, raw)| {
                helia_core::captime::capture_time(p, &helia_core::captime::TimeHints::default())
                    .map(|t| t.unix)
                    .ok_or_else(|| invalid("no_time", format!("{raw} has no time; give `times`")))
            })
            .collect::<ApiResult<_>>()?,
    };
    let mut sorted = times.clone();
    sorted.sort_by(f64::total_cmp);
    let target = match req.config.to.as_deref().unwrap_or("middle") {
        "first" => sorted[0],
        "last" => sorted[sorted.len() - 1],
        "middle" => sorted[sorted.len() / 2],
        t => parse_time(t)?,
    };
    let dir = st.sandbox.resolve(&req.output)?;
    if dir.is_file() {
        return Err(invalid(
            "invalid_output",
            "`output` is a file, not a directory",
        ));
    }
    let outs: Vec<PathBuf> = inputs
        .iter()
        .map(|p| {
            let part = |s: Option<&std::ffi::OsStr>| {
                s.map_or_else(String::new, |s| s.to_string_lossy().into_owned())
            };
            dir.join(format!(
                "{}-derotated.{}",
                part(p.file_stem()),
                part(p.extension())
            ))
        })
        .collect();
    if !req.overwrite && outs.iter().any(|o| o.exists()) {
        return Err(ApiError::conflict(
            "output_exists",
            "a result already exists; set overwrite to replace it",
        ));
    }
    let sb = Arc::clone(&st.sandbox);
    let preserve = !req.config.raw_brightness;
    let result = tokio::task::spawn_blocking(move || {
        derotate_files(
            &sb,
            &Job {
                inputs,
                outs,
                times,
                dir,
            },
            &Settings {
                target,
                law,
                orient,
                preserve,
            },
        )
    })
    .await
    .map_err(|e| ApiError::internal(format!("the worker stopped: {e}")))??;
    Ok(Json(result))
}

/// What `solar_derotation` works on.
struct Job {
    inputs: Vec<PathBuf>,
    outs: Vec<PathBuf>,
    times: Vec<f64>,
    dir: PathBuf,
}

/// How it derotates.
struct Settings {
    target: f64,
    law: helia_core::derotate::RotationLaw,
    orient: helia_core::derotate::Orientation,
    preserve: bool,
}

/// Derotate every image of `job` and write the results.
fn derotate_files(sb: &Sandbox, job: &Job, cfg: &Settings) -> ApiResult<SolarDerotationResponse> {
    let (target, law, orient, preserve) = (cfg.target, cfg.law, cfg.orient, cfg.preserve);
    let (inputs, outs, times, dir) = (&job.inputs, &job.outs, &job.times, &job.dir);
    std::fs::create_dir_all(dir).map_err(|e| ApiError::internal(e.to_string()))?;
    let mut images = Vec::new();
    for ((input, out), &t) in inputs.iter().zip(outs).zip(times) {
        let (pic, _) = helia_core::io::any::read_any(input)
            .map_err(|e| invalid("invalid_input", e.to_string()))?;
        let disc = helia_core::derotate::disc_of(&pic).ok_or_else(|| {
            invalid(
                "no_disc",
                format!("no clear solar disc in {}", sb.display(input)),
            )
        })?;
        let spec = helia_core::derotate::Spec {
            law,
            orient,
            from_unix: t,
            to_unix: target,
            preserve_contrast: preserve,
            kernel: helia_core::Kernel::default(),
        };
        let (res, rep) = helia_core::derotate::derotate(&pic, disc, &spec)
            .map_err(|e| invalid("derotation_failed", e.to_string()))?;
        let note = format!(
            "Derotated by Helia from {} to {} UTC: differential rotation, {} rates",
            helia_core::io::solarnet::iso_from_unix(t),
            helia_core::io::solarnet::iso_from_unix(target),
            law.label()
        );
        helia_core::picture::save(out, &res, &helia_core::io::ImageNotes::line(note))
            .map_err(|e| ApiError::internal(e.to_string()))?;
        images.push(DerotatedImage {
            input: sb.display(input),
            file: sb.display(out),
            time: helia_core::io::solarnet::iso_from_unix(t),
            dt_s: rep.dt_s,
            max_shift_px: rep.max_shift_px,
            centre_shift_px: rep.centre_shift_px,
            disc: [disc.cx, disc.cy, disc.r],
        });
    }
    Ok(SolarDerotationResponse {
        target: helia_core::io::solarnet::iso_from_unix(target),
        images,
    })
}

/// `POST /mosaic` body.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MosaicRequest {
    /// Stacked panels (`.tif`, `.tiff`, `.png`, `.fits`) relative to the data root,
    /// 2 to 64. The first is the reference: the mosaic is in its orientation and
    /// brightness.
    pub inputs: Vec<String>,
    /// UTC time of each panel, in the order given (needed for `derotate`).
    /// Without it a FITS file's `DATE-AVG` or a file name with a full date and time
    /// is used.
    #[serde(default)]
    pub times: Option<Vec<String>>,
    /// The mosaic to write, relative to the data root; the extension picks the
    /// format. The panel and pair tables are written beside it as
    /// `NAME-panels.csv` and `NAME-pairs.csv`.
    pub output: String,
    /// Replace an existing output.
    #[serde(default)]
    pub overwrite: bool,
    /// The most the panels may be turned against each other, degrees (0 to 45,
    /// default 4; 0 holds them square).
    #[serde(default)]
    pub max_rotation_deg: Option<f64>,
    /// Least overlap that counts, percent of the smaller panel (default 5).
    #[serde(default)]
    pub min_overlap_percent: Option<f32>,
    /// Least normalised correlation of the detail on an overlap, 0 to 1 (default
    /// 0.45).
    #[serde(default)]
    pub min_correlation: Option<f32>,
    /// Width of the blend across a seam and at a panel's edge, pixels (0 to 200,
    /// default 12).
    #[serde(default)]
    pub feather_px: Option<f32>,
    /// Match the panels' gain and offset on the overlaps (default true).
    #[serde(default)]
    pub gain_match: Option<bool>,
    /// Join along seams of least difference (default true).
    #[serde(default)]
    pub seams: Option<bool>,
    /// Bring every panel to one time by the Sun's differential rotation first (an
    /// empty object for the defaults; `to` may be `first`, `middle` (default),
    /// `last` or a UTC time).
    #[serde(default)]
    pub derotate: Option<DerotateConfig>,
}

/// Where a panel went.
#[derive(Serialize, ToSchema)]
pub struct MosaicPanel {
    /// Position in `inputs`, from 1.
    pub panel: usize,
    /// The input.
    pub input: String,
    /// Shift in the mosaic's coordinates, pixels (the first panel is at 0).
    pub x: f64,
    /// Shift in y, pixels.
    pub y: f64,
    /// Rotation against the first panel, degrees (clockwise on screen).
    pub angle_deg: f64,
    /// Gain applied for the brightness match.
    pub gain: f32,
    /// Offset applied for the brightness match.
    pub offset: f32,
    /// The most the derotation moved the surface of this panel, pixels, when it
    /// ran.
    pub derotated_px: Option<f32>,
}

/// How a pair of panels matched.
#[derive(Serialize, ToSchema)]
pub struct MosaicPair {
    /// First panel, from 1.
    pub a: usize,
    /// Second panel, from 1.
    pub b: usize,
    /// Normalised correlation of the detail on the overlap.
    pub ncc: f32,
    /// Pixels shared.
    pub overlap_px: usize,
    /// How far the pair is from where the placement put it, pixels.
    pub residual_px: f64,
    /// Whether it was used in the placement.
    pub used: bool,
}

/// What a mosaic made.
#[derive(Serialize, ToSchema)]
pub struct MosaicResponse {
    /// The mosaic, relative to the data root.
    pub output: String,
    /// Width of the mosaic.
    pub width: usize,
    /// Height of the mosaic.
    pub height: usize,
    /// The panels.
    pub panels: Vec<MosaicPanel>,
    /// The matched pairs.
    pub pairs: Vec<MosaicPair>,
    /// RMS difference of the panels beside a seam, as a share of the level there.
    pub seam_rms: f32,
    /// RMS difference of the panels over the whole overlap, the same measure.
    pub overlap_rms: f32,
    /// The disc fitted to the mosaic: centre x, centre y, radius, pixels.
    pub disc: Option<[f64; 3]>,
    /// The time the panels were brought to, UTC, when they were derotated.
    pub derotated_to: Option<String>,
    /// The panel table (CSV), relative to the data root.
    pub panels_table: String,
    /// The pair table (CSV), relative to the data root.
    pub pairs_table: String,
    /// Plain-language lines about what happened.
    pub notes: Vec<String>,
}

/// Most panels one request may join.
pub const MAX_PANELS: usize = 64;

impl DerotateConfig {
    fn for_mosaic(&self) -> ApiResult<helia_core::mosaic::Derotation> {
        use helia_core::mosaic::Target;
        Ok(helia_core::mosaic::Derotation {
            law: self.law()?,
            orient: self.orientation()?,
            target: match self.to.as_deref() {
                None | Some("middle") => Target::Middle,
                Some("first") => Target::First,
                Some("last") => Target::Last,
                Some(t) => Target::Unix(parse_time(t)?),
            },
            preserve_contrast: !self.raw_brightness,
        })
    }
}

fn mosaic_options(req: &MosaicRequest) -> ApiResult<helia_core::mosaic::Options> {
    let d = helia_core::mosaic::Options::default();
    let max_rotation_deg = req.max_rotation_deg.unwrap_or(d.max_rotation_deg);
    if !(max_rotation_deg.is_finite() && (0.0..=45.0).contains(&max_rotation_deg)) {
        return Err(invalid(
            "invalid_config",
            "`max_rotation_deg` must be between 0 and 45",
        ));
    }
    let min_overlap = req.min_overlap_percent.map_or(d.min_overlap, |p| p / 100.0);
    if !(0.0..=1.0).contains(&min_overlap) {
        return Err(invalid(
            "invalid_config",
            "`min_overlap_percent` must be between 0 and 100",
        ));
    }
    let min_ncc = req.min_correlation.unwrap_or(d.min_ncc);
    if !(0.0..=1.0).contains(&min_ncc) {
        return Err(invalid(
            "invalid_config",
            "`min_correlation` must be between 0 and 1",
        ));
    }
    let feather = req.feather_px.unwrap_or(d.feather);
    if !(feather.is_finite() && (0.0..=200.0).contains(&feather)) {
        return Err(invalid(
            "invalid_config",
            "`feather_px` must be between 0 and 200",
        ));
    }
    Ok(helia_core::mosaic::Options {
        max_rotation_deg,
        min_overlap,
        min_ncc,
        feather,
        match_levels: req.gain_match.unwrap_or(true),
        seams: req.seams.unwrap_or(true),
        derotate: req
            .derotate
            .as_ref()
            .map(DerotateConfig::for_mosaic)
            .transpose()?,
        kernel: helia_core::Kernel::default(),
    })
}

/// Join panels of the Sun into a full-disc mosaic.
///
/// Stack every panel first, with the same settings, using ordinary jobs. The
/// request runs to the end before it answers.
#[utoipa::path(post, path = "/api/v1/mosaic", tag = "sequences",
    request_body = MosaicRequest,
    responses(
        (status = 200, body = MosaicResponse),
        (status = 409, description = "`output` exists and `overwrite` is false (`output_exists`)", body = ErrorEnvelope),
        (status = 422, description = "bad settings, panels that do not match, or no time for a derotation", body = ErrorEnvelope)))]
pub(crate) async fn mosaic(
    State(st): State<AppState>,
    ApiJson(req): ApiJson<MosaicRequest>,
) -> ApiResult<Json<MosaicResponse>> {
    if req.inputs.len() < 2 || req.inputs.len() > MAX_PANELS {
        return Err(invalid(
            "invalid_config",
            format!("`inputs` needs 2 to {MAX_PANELS} stacked panels"),
        ));
    }
    let opts = mosaic_options(&req)?;
    let inputs: Vec<PathBuf> = req
        .inputs
        .iter()
        .map(|r| check_image(&st.sandbox, r))
        .collect::<ApiResult<_>>()?;
    let times: Vec<Option<f64>> = match &req.times {
        Some(t) => {
            if t.len() != inputs.len() {
                return Err(invalid(
                    "invalid_config",
                    "`times` must have one entry for every input",
                ));
            }
            t.iter()
                .map(|s| parse_time(s).map(Some))
                .collect::<ApiResult<_>>()?
        }
        None => inputs
            .iter()
            .map(|p| {
                helia_core::captime::capture_time(p, &helia_core::captime::TimeHints::default())
                    .map(|t| t.unix)
            })
            .collect(),
    };
    let output = st.sandbox.resolve(&req.output)?;
    helia_core::io::ImageFormat::from_path(&output)
        .map_err(|e| invalid("unsupported_output_format", e.to_string()))?;
    if output.is_dir() {
        return Err(invalid("invalid_output", "`output` is a directory"));
    }
    if inputs.contains(&output) {
        return Err(ApiError::conflict(
            "output_is_input",
            "output would overwrite an input",
        ));
    }
    if output.exists() && !req.overwrite {
        return Err(ApiError::conflict(
            "output_exists",
            "output exists; set overwrite to replace it",
        ));
    }
    let sb = Arc::clone(&st.sandbox);
    let result =
        tokio::task::spawn_blocking(move || build_mosaic(&sb, &inputs, &times, &output, &opts))
            .await
            .map_err(|e| ApiError::internal(format!("the worker stopped: {e}")))??;
    Ok(Json(result))
}

/// Read the panels, join them and write the mosaic and its tables.
fn build_mosaic(
    sb: &Sandbox,
    inputs: &[PathBuf],
    times: &[Option<f64>],
    output: &std::path::Path,
    opts: &helia_core::mosaic::Options,
) -> ApiResult<MosaicResponse> {
    use helia_core::mosaic as mo;
    let panels: Vec<helia_core::picture::Picture> = inputs
        .iter()
        .map(|p| {
            helia_core::io::any::read_any(p)
                .map(|(l, _)| l)
                .map_err(|e| invalid("invalid_input", format!("{}: {e}", sb.display(p))))
        })
        .collect::<ApiResult<_>>()?;
    let m = mo::build(&panels, times, opts, &|_| {}, &AtomicBool::new(false))
        .map_err(|e| invalid("mosaic_failed", e.to_string()))?;
    if let Some(dir) = output.parent() {
        std::fs::create_dir_all(dir).map_err(|e| ApiError::internal(e.to_string()))?;
    }
    let notes = helia_core::io::ImageNotes {
        lines: mo::summary_lines(&m)
            .into_iter()
            .map(|l| format!("Helia mosaic: {l}"))
            .collect(),
        fits: None,
    };
    helia_core::picture::save(output, &m.image, &notes)
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let stem = output.with_extension("");
    let name = stem
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let panels_csv = stem.with_file_name(format!("{name}-panels.csv"));
    let pairs_csv = stem.with_file_name(format!("{name}-pairs.csv"));
    let names: Vec<String> = inputs.iter().map(|p| sb.display(p)).collect();
    std::fs::write(&panels_csv, mo::panels_csv(&m, &names))
        .and_then(|()| std::fs::write(&pairs_csv, mo::pairs_csv(&m)))
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let (width, height) = m.image.size();
    Ok(MosaicResponse {
        output: sb.display(output),
        width,
        height,
        panels: m
            .panels
            .iter()
            .map(|p| MosaicPanel {
                panel: p.index + 1,
                input: names[p.index].clone(),
                x: p.x,
                y: p.y,
                angle_deg: p.angle_deg,
                gain: p.gain,
                offset: p.offset,
                derotated_px: p.derotated_px,
            })
            .collect(),
        pairs: m
            .pairs
            .iter()
            .map(|p| MosaicPair {
                a: p.a + 1,
                b: p.b + 1,
                ncc: p.ncc,
                overlap_px: p.overlap_px,
                residual_px: p.residual_px,
                used: p.used,
            })
            .collect(),
        seam_rms: m.seam_rms,
        overlap_rms: m.overlap_rms,
        disc: m.disc.map(|d| [d.cx, d.cy, d.r]),
        derotated_to: m.derotated_to.map(helia_core::io::solarnet::iso_from_unix),
        panels_table: sb.display(&panels_csv),
        pairs_table: sb.display(&pairs_csv),
        notes: mo::summary_lines(&m),
    })
}
