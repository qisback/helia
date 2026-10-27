// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Request handlers.

use std::path::Path;
use std::sync::Arc;

use axum::Json;
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequest, FromRequestParts, Path as UrlPath, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use helia_core::debayer::{Channel, Debayer, Method};
use helia_core::io::ImageFormat;
use helia_core::io::capture::{CaptureReport, Level, Origin, inspect as inspect_report};
use helia_core::io::detect::{Detection, Outcome};
use helia_core::io::ffmpeg;
use helia_core::io::sidecar::CameraSettings;
use helia_core::io::source::{Endian, open_capture};
use helia_gpu::{AdapterRequest, GpuBackend};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;
use tokio_util::io::ReaderStream;
use utoipa::ToSchema;

use crate::AppState;
use crate::error::{ApiError, ApiResult, ErrorEnvelope};
use crate::imaging::{self, MAX_PREVIEW_WIDTH, MIN_PREVIEW_WIDTH, Stretch};
use crate::jobs::{ChannelArg, JobEvent, JobRequest, JobSpec, JobState, JobView, parse_open};
use crate::paths::Sandbox;

/// JSON body extractor with typed rejections.
#[derive(FromRequest)]
#[from_request(via(axum::Json), rejection(ApiError))]
pub(crate) struct ApiJson<T>(pub T);

/// Query extractor with typed rejections.
#[derive(FromRequestParts)]
#[from_request(via(axum::extract::Query), rejection(ApiError))]
pub(crate) struct ApiQuery<T>(pub T);

fn blocking_failed(e: &tokio::task::JoinError) -> ApiError {
    ApiError::internal(format!("worker failed: {e}"))
}

fn core_err(e: impl Into<helia_core::Error>) -> ApiError {
    ApiError::from(e.into())
}

fn parse_id(raw: &str) -> ApiResult<u64> {
    raw.parse()
        .map_err(|_| ApiError::bad_request("invalid_job_id", "job ids are integers"))
}

pub(crate) async fn not_found() -> ApiError {
    ApiError::not_found("no_such_route", "no such route")
}

pub(crate) async fn method_not_allowed() -> ApiError {
    ApiError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        "method not allowed for this route",
    )
}

/// Liveness probe.
#[derive(Serialize, ToSchema)]
pub struct Health {
    /// Always `ok`.
    pub status: &'static str,
}

/// Liveness probe. Always answers `{"status":"ok"}`; needs no token.
#[utoipa::path(get, path = "/api/v1/health", tag = "meta",
    responses((status = 200, body = Health)), security(()))]
pub(crate) async fn health() -> Json<Health> {
    Json(Health { status: "ok" })
}

/// `GET /presets`: the filter presets, their recommended settings and colours.
#[utoipa::path(get, path = "/api/v1/presets", tag = "meta",
    responses((status = 200, body = crate::presets::PresetList)))]
pub(crate) async fn presets() -> Json<crate::presets::PresetList> {
    Json(crate::presets::list())
}

/// `GET /presets/{id}/lut` query.
#[derive(Deserialize, ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct LutQuery {
    /// `cube` (default), `csv`, `imagej` or `png`.
    pub format: Option<String>,
    /// Entries, 2 to 4096 (default 256; an `imagej` table always has 256).
    pub size: Option<usize>,
    /// Invert before colouring.
    #[serde(default)]
    pub invert: bool,
    /// Mix of colour and grey, 0 to 1 (default 1).
    pub strength: Option<f32>,
}

/// A preset's colour table as a file other programs read.
#[utoipa::path(get, path = "/api/v1/presets/{id}/lut", tag = "meta",
    params(("id" = String, Path, description = "preset id, see `GET /presets`"), LutQuery),
    responses((status = 200, description = "the table: a .cube, .csv, ImageJ .lut or a PNG strip"), (status = 404, description = "no such preset", body = ErrorEnvelope), (status = 422, description = "a bad format, size or strength", body = ErrorEnvelope)))]
pub(crate) async fn preset_lut(
    UrlPath(id): UrlPath<String>,
    ApiQuery(q): ApiQuery<LutQuery>,
) -> ApiResult<Response> {
    use helia_core::lut_export::{LutFormat, export};
    let preset = helia_core::Preset::from_id(&id)
        .ok_or_else(|| ApiError::not_found("not_found", format!("no preset {id:?}")))?;
    let invalid = |m: String| ApiError::unprocessable("invalid_config", m);
    let format: LutFormat = q
        .format
        .as_deref()
        .map_or(Ok(LutFormat::Cube), str::parse)
        .map_err(|e: helia_core::Error| invalid(e.to_string()))?;
    let strength = q.strength.unwrap_or(1.0);
    if !(strength.is_finite() && (0.0..=1.0).contains(&strength)) {
        return Err(invalid("`strength` must be 0 to 1".into()));
    }
    let params = helia_core::ColourParams {
        preset: Some(preset),
        invert: q.invert,
        strength,
        ..helia_core::ColourParams::default()
    };
    let title = format!("Helia {}", preset.name());
    let bytes = export(&params.lut(), &title, format, q.size.unwrap_or(256))
        .map_err(|e| invalid(e.to_string()))?;
    let mut res = Response::new(Body::from(bytes));
    let headers = res.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(format.media_type()),
    );
    if let Ok(v) = HeaderValue::from_str(&format!(
        "attachment; filename=\"helia-{}.{}\"",
        preset.id(),
        format.ext()
    )) {
        headers.insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(res)
}

/// What Helia would do with a capture if given no settings.
#[derive(Serialize, ToSchema)]
pub struct Recommended {
    /// Preset id detected from the file name, folder, sidecar or SER header.
    pub filter: Option<String>,
    /// The text the filter was read from.
    pub filter_source: Option<String>,
    /// Bayer channel it would stack (`r`, `g`, `b`, `lum`).
    pub channel: &'static str,
    /// Share of frames it would keep, percent.
    pub keep_percent: f32,
    /// Alignment point size in pixels.
    pub ap_size: usize,
}

fn recommended_for(path: &Path, width: usize, height: usize, bayer: bool) -> Recommended {
    let rec = helia_core::recommend::recommend(
        width,
        height,
        bayer,
        &helia_core::recommend::capture_texts(path),
        None,
    );
    Recommended {
        filter: rec.preset.map(|p| p.id().to_owned()),
        filter_source: rec.detected_from,
        channel: crate::presets::channel_name(rec.channel),
        keep_percent: rec.preset.map_or(25.0, |p| p.info().keep_percent),
        ap_size: rec.ap_size,
    }
}

/// GPU availability.
#[derive(Clone, Serialize, ToSchema)]
pub struct GpuInfo {
    /// A GPU backend can be created (a software adapter counts).
    pub available: bool,
    /// Adapter description.
    pub adapter: Option<String>,
    /// The adapter is a software rasteriser.
    pub software: bool,
}

/// CPU availability (always true).
#[derive(Serialize, ToSchema)]
pub struct CpuInfo {
    /// Always true.
    pub available: bool,
}

/// Backend availability.
#[derive(Serialize, ToSchema)]
pub struct Backends {
    /// CPU reference path.
    pub cpu: CpuInfo,
    /// wgpu path.
    pub gpu: GpuInfo,
}

/// Server limits.
#[derive(Serialize, ToSchema)]
pub struct Limits {
    /// Concurrent jobs.
    pub workers: usize,
    /// Queued plus running jobs allowed.
    pub max_queue: usize,
    /// Request body limit in bytes.
    pub max_body_bytes: usize,
    /// Jobs queued or running now.
    pub active_jobs: usize,
}

/// How the server binary was built.
#[derive(Serialize, ToSchema)]
pub struct BuildDetails {
    /// Short git commit hash (`unknown` outside a git checkout).
    pub commit: &'static str,
    /// Built from a tree with uncommitted changes.
    pub dirty: bool,
    /// Build time, UTC.
    pub built: &'static str,
    /// Cargo profile.
    pub profile: &'static str,
    /// Target triple.
    pub target: &'static str,
    /// Compiler version.
    pub rust: &'static str,
    /// wgpu version.
    pub wgpu: &'static str,
}

/// Licensing of the software.
#[derive(Serialize, ToSchema)]
pub struct LicenceDetails {
    /// Copyright line.
    pub copyright: &'static str,
    /// SPDX licence identifier.
    pub licence: &'static str,
    /// Source code location.
    pub source: &'static str,
}

/// `/version` response.
#[derive(Serialize, ToSchema)]
pub struct VersionInfo {
    /// Package name.
    pub name: &'static str,
    /// Package version.
    pub version: &'static str,
    /// API version in the URL prefix.
    pub api: &'static str,
    /// Build record.
    pub build: BuildDetails,
    /// Copyright and licence.
    pub licence: LicenceDetails,
    /// Backends.
    pub backends: Backends,
    /// Limits.
    pub limits: Limits,
    /// Whether a token is required.
    pub auth_required: bool,
}

fn probe_gpu() -> GpuInfo {
    match GpuBackend::shared(AdapterRequest::Hardware) {
        Ok(g) => GpuInfo {
            available: true,
            adapter: Some(g.adapter_summary()),
            software: g.is_software(),
        },
        Err(_) => match GpuBackend::shared(AdapterRequest::Software) {
            Ok(g) => GpuInfo {
                available: true,
                adapter: Some(g.adapter_summary()),
                software: true,
            },
            Err(_) => GpuInfo {
                available: false,
                adapter: None,
                software: false,
            },
        },
    }
}

/// Server version, backend availability, limits and whether a token is required.
///
/// The GPU is probed once, on first use.
#[utoipa::path(get, path = "/api/v1/version", tag = "meta",
    responses((status = 200, body = VersionInfo)))]
pub(crate) async fn version(State(st): State<AppState>) -> ApiResult<Json<VersionInfo>> {
    let gpu = st
        .gpu
        .get_or_try_init(|| async {
            tokio::task::spawn_blocking(probe_gpu)
                .await
                .map_err(|e| blocking_failed(&e))
        })
        .await?
        .clone();
    Ok(Json(VersionInfo {
        name: "helia-server",
        version: env!("CARGO_PKG_VERSION"),
        api: "v1",
        build: {
            let b = helia_core::about::build_info();
            BuildDetails {
                commit: b.commit,
                dirty: b.dirty,
                built: b.built,
                profile: b.profile,
                target: b.target,
                rust: b.rustc,
                wgpu: helia_gpu::WGPU_VERSION,
            }
        },
        licence: LicenceDetails {
            copyright: helia_core::about::COPYRIGHT,
            licence: helia_core::about::LICENCE_ID,
            source: helia_core::about::SOURCE_URL,
        },
        backends: Backends {
            cpu: CpuInfo { available: true },
            gpu,
        },
        limits: Limits {
            workers: st.cfg.workers,
            max_queue: st.cfg.max_queue,
            max_body_bytes: st.cfg.max_body_bytes,
            active_jobs: st.jobs.active_count(),
        },
        auth_required: st.token.is_some(),
    }))
}

/// `POST /captures/inspect` body.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct InspectRequest {
    /// Capture path relative to the data root.
    pub path: String,
    /// `auto` (default), `none`, `rggb`, `grbg`, `gbrg` or `bggr`: see what an
    /// override would change.
    #[serde(default)]
    pub bayer: Option<String>,
    /// Bits per sample instead of the SER header's (1 to 16).
    #[serde(default)]
    pub bit_depth: Option<u32>,
    /// `little` or `big`, instead of the SER header flag.
    #[serde(default)]
    pub endian: Option<String>,
    /// Read the rows bottom to top.
    #[serde(default)]
    pub flip_rows: bool,
    /// Row order of FITS frames: `auto` (default), `top-down` or `bottom-up`.
    #[serde(default)]
    pub row_order: Option<String>,
}

impl InspectRequest {
    fn open_options(&self) -> ApiResult<helia_core::io::source::OpenOptions> {
        parse_open(
            self.bayer.as_deref().unwrap_or("auto"),
            self.bit_depth,
            self.endian.as_deref(),
            self.flip_rows,
            self.row_order.as_deref(),
        )
    }
}

/// A value Helia uses and where it came from.
#[derive(Serialize, ToSchema)]
pub struct SourcedText {
    /// The value.
    pub value: String,
    /// `header`, `sidecar`, `detection` or `override`.
    pub source: &'static str,
}

/// A number Helia uses and where it came from.
#[derive(Serialize, ToSchema)]
pub struct SourcedNumber {
    /// The value.
    pub value: u32,
    /// `header`, `sidecar`, `detection` or `override`.
    pub source: &'static str,
}

/// A switch and where its setting came from.
#[derive(Serialize, ToSchema)]
pub struct SourcedFlag {
    /// The value.
    pub value: bool,
    /// `header`, `sidecar`, `detection` or `override`.
    pub source: &'static str,
}

/// What Helia decodes the capture with.
#[derive(Serialize, ToSchema)]
pub struct EffectiveInfo {
    /// Bayer pattern (`rggb` ...), or `none` for mono.
    pub bayer: SourcedText,
    /// Bits per sample.
    pub bit_depth: SourcedNumber,
    /// Byte order of 16-bit samples: `little` or `big`.
    pub byte_order: SourcedText,
    /// Rows are read bottom to top.
    pub flip_rows: SourcedFlag,
}

/// A SER header exactly as written in the file, decoded.
#[derive(Serialize, ToSchema)]
pub struct SerHeaderInfo {
    /// `FileID`.
    pub file_id: String,
    /// `LuID`.
    pub lu_id: i32,
    /// `ColorID` as stored.
    pub colour_id: i32,
    /// Its meaning (`MONO`, `BAYER_RGGB` ...).
    pub colour_meaning: &'static str,
    /// `LittleEndian` flag as stored.
    pub little_endian: bool,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Bits per sample.
    pub pixel_depth: u32,
    /// Frame count.
    pub frame_count: u32,
    /// Observer.
    pub observer: String,
    /// Instrument.
    pub instrument: String,
    /// Telescope.
    pub telescope: String,
    /// `DateTime` (local) decoded; absent when unset.
    pub date_time: Option<String>,
    /// `DateTimeUTC` decoded; absent when unset.
    pub date_time_utc: Option<String>,
    /// Timestamps in the trailer.
    pub timestamps: Option<TimestampInfo>,
}

/// SER trailer timestamps.
#[derive(Serialize, ToSchema)]
pub struct TimestampInfo {
    /// How many.
    pub count: usize,
    /// First, UTC.
    pub first: Option<String>,
    /// Last, UTC.
    pub last: Option<String>,
}

/// The test for a hidden Bayer mosaic.
#[derive(Serialize, ToSchema)]
pub struct DetectionInfo {
    /// Frames measured.
    pub frames_used: usize,
    /// Area measured: x, y, width, height.
    pub region: [usize; 4],
    /// Mean level (0 to 1) of the pixels at even x/even y, odd x/even y,
    /// even x/odd y and odd x/odd y.
    pub position_means: [f64; 4],
    /// (largest - smallest) / average of the four means.
    pub spread: f64,
    /// `mosaic`, `mono` or `inconclusive`.
    pub verdict: &'static str,
    /// Pattern found, for `mosaic`.
    pub pattern: Option<String>,
    /// How red was told from blue, for `mosaic`.
    pub red_blue_basis: Option<&'static str>,
    /// Why it could not tell, for `inconclusive`.
    pub reason: Option<&'static str>,
}

/// A message about how the capture was understood.
#[derive(Serialize, ToSchema)]
pub struct NoticeInfo {
    /// `info` or `warning`.
    pub level: &'static str,
    /// The message.
    pub text: String,
}

/// `SharpCap` sidecar contents that Helia uses.
#[derive(Serialize, ToSchema)]
pub struct SidecarInfo {
    /// Sidecar path relative to the data root.
    pub path: String,
    /// `Colour Space` value.
    pub colour_space: Option<String>,
    /// Whether the sidecar says the data is raw colour.
    pub raw_colour: Option<bool>,
    /// Bayer pattern implied by the sidecar.
    pub bayer_hint: Option<String>,
    /// Frame count from the sidecar.
    pub frame_count: Option<usize>,
    /// Section headings (camera model).
    pub sections: Vec<String>,
}

/// Header information for a capture.
#[derive(Serialize, ToSchema)]
pub struct CaptureInfo {
    /// Capture path relative to the data root.
    pub path: String,
    /// `ser`, `avi`, or the extension of a video read through ffmpeg (`mp4`,
    /// `mov`, `mkv`, `webm`, `m4v`; also `avi` for a compressed AVI).
    pub format: &'static str,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Bits per sample.
    pub bit_depth: u32,
    /// Pixel layout: `mono`, `bayer`, `rgb`, `bgr`, or `video` for compressed
    /// video, which ffmpeg converts to luminance or one colour channel.
    pub colour: &'static str,
    /// Bayer pattern (`rggb` ...) when raw colour.
    pub bayer: Option<String>,
    /// Number of frames.
    pub frames: usize,
    /// SER observer string.
    pub observer: Option<String>,
    /// SER instrument string.
    pub instrument: Option<String>,
    /// SER telescope string.
    pub telescope: Option<String>,
    /// SER per-frame timestamps present.
    pub timestamps: Option<bool>,
    /// AVI `FourCC`.
    pub fourcc: Option<String>,
    /// `SharpCap` sidecar, when one sits next to the capture.
    pub sidecar: Option<SidecarInfo>,
    /// The settings a job would use if none were given.
    pub recommended: Recommended,
    /// Video codec (`h264`, ...), for video read through ffmpeg.
    pub codec: Option<String>,
    /// Frames per second, for video.
    pub frame_rate: Option<f64>,
    /// Quality caveat when the input is lossy video, for example "lossy 8-bit
    /// 4:2:0 video (h264): fine for quick results, SER/AVI raw captures keep
    /// more detail".
    pub quality_note: Option<String>,
    /// The SER header exactly as written in the file.
    pub header: Option<SerHeaderInfo>,
    /// What Helia decodes the capture with, and where each value came from.
    pub effective: EffectiveInfo,
    /// The mosaic test, when it was run (a SER file labelled mono, no override).
    pub detection: Option<DetectionInfo>,
    /// The overrides in force, as command-line flags.
    pub overrides: Vec<String>,
    /// What to tell the user about how the file was read.
    pub notices: Vec<NoticeInfo>,
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_owned())
}

fn pattern_name(p: helia_core::BayerPattern) -> String {
    helia_core::io::capture::pattern_name(p).to_ascii_lowercase()
}

fn read_sidecar(sb: &Sandbox, capture: &Path) -> Option<SidecarInfo> {
    // A symlinked sidecar must not read outside the root either.
    let rel = sb.display(&CameraSettings::sidecar_path(capture));
    sb.resolve(&rel).ok()?;
    let s = CameraSettings::for_capture(capture)?;
    Some(SidecarInfo {
        path: rel,
        colour_space: s.get("Colour Space").map(str::to_owned),
        raw_colour: s.is_raw_colour(),
        bayer_hint: s.bayer_hint().map(pattern_name),
        frame_count: s.frame_count(),
        sections: s.sections.clone(),
    })
}

/// A compressed video, described by `ffprobe` (nothing is decoded).
fn inspect_video(sb: &Sandbox, path: &Path) -> ApiResult<CaptureInfo> {
    let i = ffmpeg::inspect(path).map_err(core_err_ffmpeg)?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    let format = match ext.as_deref() {
        Some("mp4") => "mp4",
        Some("mov") => "mov",
        Some("mkv") => "mkv",
        Some("webm") => "webm",
        Some("m4v") => "m4v",
        _ => "avi",
    };
    let (w, h) = i.output_size();
    Ok(CaptureInfo {
        path: sb.display(path),
        format,
        width: u32::try_from(w).unwrap_or(u32::MAX),
        height: u32::try_from(h).unwrap_or(u32::MAX),
        bit_depth: u32::from(i.bit_depth),
        colour: "video",
        bayer: None,
        frames: usize::try_from(i.frames).unwrap_or(usize::MAX),
        observer: None,
        instrument: None,
        telescope: None,
        timestamps: None,
        fourcc: None,
        sidecar: None,
        recommended: recommended_for(path, w, h, false),
        codec: Some(i.codec.clone()),
        frame_rate: i.frame_rate,
        quality_note: i.quality_note(),
        header: None,
        effective: EffectiveInfo {
            bayer: SourcedText {
                value: "none".into(),
                source: "header",
            },
            bit_depth: SourcedNumber {
                value: u32::from(i.bit_depth),
                source: "header",
            },
            byte_order: SourcedText {
                value: "little".into(),
                source: "header",
            },
            flip_rows: SourcedFlag {
                value: false,
                source: "header",
            },
        },
        detection: None,
        overrides: Vec::new(),
        notices: Vec::new(),
    })
}

fn core_err_ffmpeg(e: ffmpeg::FfmpegError) -> ApiError {
    ApiError::from(helia_core::Error::Ffmpeg(e))
}

fn source_name(o: Origin) -> &'static str {
    o.label()
}

fn detection_info(d: &Detection) -> DetectionInfo {
    let (verdict, pattern, basis, reason) = match d.outcome {
        Outcome::Mosaic { pattern, basis } => (
            "mosaic",
            Some(pattern_name(pattern)),
            Some(basis.describe()),
            None,
        ),
        Outcome::NoMosaic => ("mono", None, None, None),
        Outcome::Inconclusive(why) => ("inconclusive", None, None, Some(why)),
    };
    DetectionInfo {
        frames_used: d.frames_used,
        region: [
            d.region.x0,
            d.region.y0,
            d.region.width(),
            d.region.height(),
        ],
        position_means: d.means,
        spread: d.spread,
        verdict,
        pattern,
        red_blue_basis: basis,
        reason,
    }
}

fn effective_info(report: &CaptureReport) -> EffectiveInfo {
    let e = report.effective;
    EffectiveInfo {
        bayer: SourcedText {
            value: e.bayer.value.map_or_else(|| "none".into(), pattern_name),
            source: source_name(e.bayer.origin),
        },
        bit_depth: SourcedNumber {
            value: e.bit_depth.value,
            source: source_name(e.bit_depth.origin),
        },
        byte_order: SourcedText {
            value: if e.endian.value == Endian::Little {
                "little"
            } else {
                "big"
            }
            .into(),
            source: source_name(e.endian.origin),
        },
        flip_rows: SourcedFlag {
            value: e.flip_rows.value,
            source: source_name(e.flip_rows.origin),
        },
    }
}

fn ser_header_info(h: &helia_core::io::capture::SerFacts) -> SerHeaderInfo {
    SerHeaderInfo {
        file_id: h.file_id.clone(),
        lu_id: h.lu_id,
        colour_id: h.colour_id,
        colour_meaning: h.colour_meaning,
        little_endian: h.little_endian_flag,
        width: h.width,
        height: h.height,
        pixel_depth: h.pixel_depth,
        frame_count: h.frame_count,
        observer: h.observer.clone(),
        instrument: h.instrument.clone(),
        telescope: h.telescope.clone(),
        date_time: h.date_time.clone(),
        date_time_utc: h.date_time_utc.clone(),
        timestamps: h.stamps.as_ref().map(|s| TimestampInfo {
            count: s.count,
            first: s.first.clone(),
            last: s.last.clone(),
        }),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one constructor per container, easiest read in order"
)]
fn capture_info(
    sb: &Sandbox,
    path: &Path,
    opts: &helia_core::io::source::OpenOptions,
) -> ApiResult<CaptureInfo> {
    if ffmpeg::is_video_path(path) {
        return inspect_video(sb, path);
    }
    let supported = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("ser") || e.eq_ignore_ascii_case("avi"));
    if !supported {
        return Err(ApiError::unprocessable(
            "unsupported_capture",
            "expected a .ser, .avi, .mp4, .mov, .mkv, .webm or .m4v file",
        ));
    }
    let report: CaptureReport = match inspect_report(path, opts) {
        Ok(r) => r,
        // Compressed AVI (MJPEG, H.264, ...) goes to ffmpeg.
        Err(helia_core::Error::Avi(helia_core::io::avi::AviError::UnsupportedFormat(_))) => {
            return inspect_video(sb, path);
        }
        Err(e) => return Err(core_err(e)),
    };
    let sidecar = read_sidecar(sb, path);
    let bayer = report.effective.bayer.value.map(pattern_name);
    let is_bayer = bayer.is_some();
    let bit_depth = report.effective.bit_depth.value;
    let rel = sb.display(path);
    let notices = report
        .notices
        .iter()
        .map(|n| NoticeInfo {
            level: if n.level == Level::Warning {
                "warning"
            } else {
                "info"
            },
            text: n.text.clone(),
        })
        .collect();
    let (effective, detection, overrides) = (
        effective_info(&report),
        report.detection.as_ref().map(detection_info),
        report.overrides.clone(),
    );
    if let Some(h) = &report.ser {
        let colour = if is_bayer {
            "bayer"
        } else {
            match h.colour_id {
                100 => "rgb",
                101 => "bgr",
                _ => "mono",
            }
        };
        return Ok(CaptureInfo {
            path: rel,
            format: "ser",
            width: h.width,
            height: h.height,
            bit_depth,
            colour,
            frames: h.frame_count as usize,
            observer: non_empty(&h.observer),
            instrument: non_empty(&h.instrument),
            telescope: non_empty(&h.telescope),
            timestamps: Some(h.stamps.is_some()),
            fourcc: None,
            sidecar,
            recommended: recommended_for(path, h.width as usize, h.height as usize, is_bayer),
            bayer,
            codec: None,
            frame_rate: None,
            quality_note: None,
            header: Some(ser_header_info(h)),
            effective,
            detection,
            overrides,
            notices,
        });
    }
    let a = report.avi.as_ref().ok_or_else(|| {
        ApiError::unprocessable("unsupported_capture", "expected a .ser or .avi file")
    })?;
    Ok(CaptureInfo {
        path: rel,
        format: "avi",
        width: a.width,
        height: a.height,
        bit_depth,
        colour: match (a.format.as_str(), is_bayer) {
            ("Bgr24", _) => "bgr",
            (_, true) => "bayer",
            _ => "mono",
        },
        frames: a.frames,
        observer: None,
        instrument: None,
        telescope: None,
        timestamps: None,
        fourcc: Some(a.fourcc.clone()),
        recommended: recommended_for(path, a.width as usize, a.height as usize, is_bayer),
        bayer,
        sidecar,
        codec: None,
        frame_rate: None,
        quality_note: None,
        header: None,
        effective,
        detection,
        overrides,
        notices,
    })
}

/// Header information for a `.ser`, `.avi` or video capture, without reading frames.
#[utoipa::path(post, path = "/api/v1/captures/inspect", tag = "captures",
    request_body = InspectRequest,
    responses((status = 200, body = CaptureInfo)))]
pub(crate) async fn inspect(
    State(st): State<AppState>,
    ApiJson(req): ApiJson<InspectRequest>,
) -> ApiResult<Json<CaptureInfo>> {
    let path = st.sandbox.resolve_file(&req.path)?;
    let opts = req.open_options()?;
    let sb = Arc::clone(&st.sandbox);
    tokio::task::spawn_blocking(move || capture_info(&sb, &path, &opts))
        .await
        .map_err(|e| blocking_failed(&e))?
        .map(Json)
}

/// `GET /captures/frame` query.
#[derive(Deserialize, ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct FrameQuery {
    /// Capture path relative to the data root.
    pub path: String,
    /// Frame index (default 0).
    #[serde(default)]
    pub index: usize,
    /// `r`, `g`, `b` or `lum` (default) for Bayer captures.
    #[serde(default)]
    pub channel: ChannelArg,
    /// Downscale to at most this width (16 to 8192).
    pub max_width: Option<usize>,
    /// Bayer handling, as for jobs: `auto` (default), `none` or a pattern.
    pub bayer: Option<String>,
    /// Bits per sample instead of the SER header's.
    pub bit_depth: Option<u32>,
    /// `little` or `big`.
    pub endian: Option<String>,
    /// Read the rows bottom to top.
    #[serde(default)]
    pub flip_rows: bool,
    /// Row order of FITS frames: `auto` (default), `top-down` or `bottom-up`.
    pub row_order: Option<String>,
}

fn check_width(w: Option<usize>) -> ApiResult<Option<usize>> {
    match w {
        Some(w) if !(MIN_PREVIEW_WIDTH..=MAX_PREVIEW_WIDTH).contains(&w) => {
            Err(ApiError::bad_request(
                "invalid_max_width",
                format!("max_width must be {MIN_PREVIEW_WIDTH} to {MAX_PREVIEW_WIDTH}"),
            ))
        }
        other => Ok(other),
    }
}

fn png_response(bytes: Vec<u8>) -> Response {
    let mut res = Response::new(Body::from(bytes));
    res.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

/// One frame of a capture as an auto-stretched 8-bit greyscale PNG.
#[utoipa::path(get, path = "/api/v1/captures/frame", tag = "captures",
    params(FrameQuery),
    responses((status = 200, content_type = "image/png", description = "8-bit greyscale PNG")))]
pub(crate) async fn frame(
    State(st): State<AppState>,
    ApiQuery(q): ApiQuery<FrameQuery>,
) -> ApiResult<Response> {
    let path = st.sandbox.resolve_file(&q.path)?;
    let width = check_width(q.max_width)?;
    let opts = parse_open(
        q.bayer.as_deref().unwrap_or("auto"),
        q.bit_depth,
        q.endian.as_deref(),
        q.flip_rows,
        q.row_order.as_deref(),
    )?;
    let png = tokio::task::spawn_blocking(move || -> ApiResult<Vec<u8>> {
        // A video is not decoded whole for one preview frame.
        let video = ffmpeg::is_video_path(&path)
            .then(|| ffmpeg::inspect(&path).map_err(core_err_ffmpeg))
            .transpose()?;
        if let Some(info) = &video
            && q.index as u64 >= info.frames
        {
            return Err(ApiError::bad_request(
                "frame_out_of_range",
                format!("frame {} of {}", q.index, info.frames),
            ));
        }
        let raw = if video.is_some() {
            ffmpeg::read_frame(&path, q.index, Channel::from(q.channel)).map_err(ApiError::from)?
        } else {
            let mut src = open_capture(&path, &opts).map_err(ApiError::from)?;
            let n = helia_core::io::source::FrameSource::frame_count(&src);
            if q.index >= n {
                return Err(ApiError::bad_request(
                    "frame_out_of_range",
                    format!("frame {} of {n}", q.index),
                ));
            }
            helia_core::io::source::FrameSource::read(&mut src, q.index).map_err(ApiError::from)?
        };
        let mono = Debayer {
            method: Method::Bilinear,
            channel: Channel::from(q.channel),
        }
        .apply_owned(raw);
        let mut img = imaging::to_grey8(&mono, Stretch::Percentile);
        if let Some(w) = width {
            img = imaging::downscale(&img, w);
        }
        imaging::encode_png(&img)
    })
    .await
    .map_err(|e| blocking_failed(&e))??;
    Ok(png_response(png))
}

/// Resolve the calibration paths inside the data root.
fn calibration_spec(
    sb: &crate::paths::Sandbox,
    c: &crate::jobs::CalibrationConfig,
    open: &helia_core::io::source::OpenOptions,
) -> ApiResult<helia_core::calibrate::CalibrationSpec> {
    let path = |raw: &Option<String>| -> ApiResult<Option<std::path::PathBuf>> {
        raw.as_deref()
            .map(|r| {
                let p = sb.resolve(r)?;
                if p.exists() {
                    Ok(p)
                } else {
                    Err(ApiError::not_found(
                        "not_found",
                        format!("calibration path {r:?} does not exist"),
                    ))
                }
            })
            .transpose()
    };
    let combine = c
        .combine
        .parse::<helia_core::calibrate::Combine>()
        .map_err(|e| ApiError::unprocessable("invalid_config", e.to_string()))?;
    if c.dark.is_none() && c.flat.is_none() && c.displaced.is_none() && c.bad_pixels.is_none() {
        return Err(ApiError::unprocessable(
            "invalid_config",
            "`calibration` needs at least one of `dark`, `flat`, `displaced` or `bad_pixels`",
        ));
    }
    if c.flat.is_some() && c.displaced.is_some() {
        return Err(ApiError::unprocessable(
            "invalid_config",
            "`flat` and `displaced` both give the flat: use one",
        ));
    }
    Ok(helia_core::calibrate::CalibrationSpec {
        dark: path(&c.dark)?,
        flat: path(&c.flat)?,
        dark_flat: path(&c.flat_dark)?,
        displaced: path(&c.displaced)?,
        bias: path(&c.bias)?,
        bad_pixels: path(&c.bad_pixels)?,
        cosmetic: c.cosmetic,
        scale_dark: c.scale_dark,
        combine,
        open: *open,
        ..helia_core::calibrate::CalibrationSpec::default()
    })
}

/// Queue a stack. Answers `202` with the job; poll it or follow its events.
#[utoipa::path(post, path = "/api/v1/jobs", tag = "jobs",
    request_body = JobRequest,
    responses((status = 202, body = JobView), (status = 429, description = "queue full (`queue_full`)", body = ErrorEnvelope)))]
pub(crate) async fn create_job(
    State(st): State<AppState>,
    ApiJson(req): ApiJson<JobRequest>,
) -> ApiResult<(StatusCode, Json<JobView>)> {
    let (stack, open) = req.config.resolve()?;
    let input = st.sandbox.resolve_file(&req.input)?;
    let output = st.sandbox.resolve(&req.output)?;
    ImageFormat::from_path(&output)
        .map_err(|e| ApiError::unprocessable("unsupported_output_format", e.to_string()))?;
    if output == input {
        return Err(ApiError::conflict(
            "output_is_input",
            "output would overwrite the input",
        ));
    }
    if output.is_dir() {
        return Err(ApiError::unprocessable(
            "invalid_output",
            "output is a directory",
        ));
    }
    if output.exists() && !req.overwrite {
        return Err(ApiError::conflict(
            "output_exists",
            "output exists; set overwrite to replace it",
        ));
    }
    // Fail fast on an unreadable capture rather than inside the queue. A video
    // is only probed: decoding it is the job's work.
    let calibration = match &req.config.calibration {
        Some(c) => Some(calibration_spec(&st.sandbox, c, &open)?),
        None => None,
    };
    let probe = input.clone();
    let sb = Arc::clone(&st.sandbox);
    tokio::task::spawn_blocking(move || {
        if ffmpeg::is_video_path(&probe) {
            inspect_video(&sb, &probe).map(|_| ())
        } else {
            open_capture(&probe, &open)
                .map(|_| ())
                .map_err(ApiError::from)
        }
    })
    .await
    .map_err(|e| blocking_failed(&e))??;
    let spec = JobSpec {
        input_display: st.sandbox.display(&input),
        output_display: st.sandbox.display(&output),
        input,
        output,
        config: req.config,
        stack,
        open,
        cache_dir: st.cfg.cache_dir.clone(),
        calibration,
    };
    let view = st.jobs.submit(spec)?;
    Ok((StatusCode::ACCEPTED, Json(view)))
}

/// Remembered jobs, oldest first. The newest 100 finished jobs are kept.
#[utoipa::path(get, path = "/api/v1/jobs", tag = "jobs",
    responses((status = 200, body = Vec<JobView>)))]
pub(crate) async fn list_jobs(State(st): State<AppState>) -> Json<Vec<JobView>> {
    Json(st.jobs.list())
}

/// One job: state, stage, progress, timings and summary.
#[utoipa::path(get, path = "/api/v1/jobs/{id}", tag = "jobs",
    params(("id" = u64, Path, description = "job id")),
    responses((status = 200, body = JobView), (status = 404, description = "unknown job id", body = ErrorEnvelope)))]
pub(crate) async fn get_job(
    State(st): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Json<JobView>> {
    st.jobs.view(parse_id(&id)?).map(Json)
}

/// Cancel a queued or running job, or forget a finished one.
///
/// A finished job's output file stays on disk.
#[utoipa::path(delete, path = "/api/v1/jobs/{id}", tag = "jobs",
    params(("id" = u64, Path, description = "job id")),
    responses(
        (status = 202, body = JobView, description = "cancellation requested"),
        (status = 204, description = "finished job forgotten"),
        (status = 404, description = "unknown job id", body = ErrorEnvelope)))]
pub(crate) async fn delete_job(
    State(st): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Response> {
    let (view, removed) = st.jobs.cancel(parse_id(&id)?)?;
    Ok(if removed {
        StatusCode::NO_CONTENT.into_response()
    } else {
        (StatusCode::ACCEPTED, Json(view)).into_response()
    })
}

fn require_succeeded(job: &crate::jobs::Job) -> ApiResult<()> {
    if job.state() == JobState::Succeeded {
        Ok(())
    } else {
        Err(ApiError::conflict(
            "not_ready",
            format!("job is {:?}", job.state()).to_ascii_lowercase(),
        ))
    }
}

/// The written TIFF or FITS file, streamed. Only once the job has succeeded.
#[utoipa::path(get, path = "/api/v1/jobs/{id}/result", tag = "jobs",
    params(("id" = u64, Path, description = "job id")),
    responses((status = 200, description = "the stacked TIFF, PNG or FITS file"), (status = 409, description = "the job has not succeeded (`not_ready`)", body = ErrorEnvelope)))]
pub(crate) async fn job_result(
    State(st): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Response> {
    let job = st.jobs.get(parse_id(&id)?)?;
    require_succeeded(&job)?;
    let path = job.output_path().clone();
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| ApiError::not_found("result_missing", "the output file is gone"))?;
    let len = file.metadata().await.map_or(0, |m| m.len());
    let ctype = match ImageFormat::from_path(&path) {
        Ok(ImageFormat::Tiff) => "image/tiff",
        Ok(ImageFormat::Png) => "image/png",
        _ => "application/fits",
    };
    let name: String = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(ctype));
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")) {
        headers.insert(header::CONTENT_DISPOSITION, v);
    }
    Ok((headers, Body::from_stream(ReaderStream::new(file))).into_response())
}

/// `preview.png` query.
#[derive(Deserialize, ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct PreviewQuery {
    /// Downscale to at most this width (16 to 8192; stored previews are at most 2048 wide).
    pub max_width: Option<usize>,
}

/// An 8-bit PNG of the result (square-root stretch). Only once the job has succeeded.
#[utoipa::path(get, path = "/api/v1/jobs/{id}/preview.png", tag = "jobs",
    params(("id" = u64, Path, description = "job id"), PreviewQuery),
    responses((status = 200, content_type = "image/png", description = "PNG preview"), (status = 409, description = "the job has not succeeded (`not_ready`)", body = ErrorEnvelope)))]
pub(crate) async fn job_preview(
    State(st): State<AppState>,
    UrlPath(id): UrlPath<String>,
    ApiQuery(q): ApiQuery<PreviewQuery>,
) -> ApiResult<Response> {
    let job = st.jobs.get(parse_id(&id)?)?;
    require_succeeded(&job)?;
    let width = check_width(q.max_width)?;
    let preview = job
        .preview()
        .ok_or_else(|| ApiError::not_found("preview_missing", "no preview stored"))?;
    let png = tokio::task::spawn_blocking(move || {
        let img = width.map_or_else(|| preview.0.clone(), |w| imaging::downscale(&preview.0, w));
        imaging::encode_png(&img)
    })
    .await
    .map_err(|e| blocking_failed(&e))??;
    Ok(png_response(png))
}

/// The sunspot table of a finished job that asked for `sunspots`, as CSV.
#[utoipa::path(get, path = "/api/v1/jobs/{id}/sunspots.csv", tag = "jobs",
    params(("id" = u64, Path, description = "job id")),
    responses((status = 200, content_type = "text/csv", description = "one row per spot"), (status = 404, description = "the job did not ask for sunspots (`sunspots_missing`)", body = ErrorEnvelope), (status = 409, description = "the job has not succeeded (`not_ready`)", body = ErrorEnvelope)))]
pub(crate) async fn job_sunspots_csv(
    State(st): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Response> {
    let job = st.jobs.get(parse_id(&id)?)?;
    require_succeeded(&job)?;
    let files = job.sunspot_files().ok_or_else(|| {
        ApiError::not_found("sunspots_missing", "the job did not ask for sunspots")
    })?;
    let mut res = Response::new(Body::from(files.csv.clone()));
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    Ok(res)
}

/// The sunspot overlay of a finished job that asked for `sunspots`: the flattened
/// disc with penumbra, umbra and group numbers drawn on it.
#[utoipa::path(get, path = "/api/v1/jobs/{id}/sunspots.png", tag = "jobs",
    params(("id" = u64, Path, description = "job id")),
    responses((status = 200, content_type = "image/png", description = "overlay PNG"), (status = 404, description = "the job did not ask for sunspots (`sunspots_missing`)", body = ErrorEnvelope), (status = 409, description = "the job has not succeeded (`not_ready`)", body = ErrorEnvelope)))]
pub(crate) async fn job_sunspots_png(
    State(st): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Response> {
    let job = st.jobs.get(parse_id(&id)?)?;
    require_succeeded(&job)?;
    let files = job.sunspot_files().ok_or_else(|| {
        ApiError::not_found("sunspots_missing", "the job did not ask for sunspots")
    })?;
    Ok(png_response(files.png.clone()))
}

/// The science report of a finished job that asked for `report`: one
/// self-contained HTML page with the figures, tables and settings.
#[utoipa::path(get, path = "/api/v1/jobs/{id}/report", tag = "jobs",
    params(("id" = u64, Path, description = "job id")),
    responses((status = 200, content_type = "text/html", description = "the report page"), (status = 404, description = "the job did not ask for a report (`report_missing`)", body = ErrorEnvelope), (status = 409, description = "the job has not succeeded (`not_ready`)", body = ErrorEnvelope)))]
pub(crate) async fn job_report(
    State(st): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Response> {
    report_file(&st, &id, helia_report::REPORT_HTML)
}

/// One file of the science report: a figure (`quality-sorted.png`), its data
/// (`frame-quality.csv`, `frame-quality.json`) or `fits-header.txt`. The files
/// are listed in `summary.report.files`.
#[utoipa::path(get, path = "/api/v1/jobs/{id}/report/{file}", tag = "jobs",
    params(("id" = u64, Path, description = "job id"), ("file" = String, Path, description = "file name from `summary.report.files`")),
    responses((status = 200, description = "the file, with the matching content type"), (status = 404, description = "no such report or file (`report_missing`, `report_file_missing`)", body = ErrorEnvelope), (status = 409, description = "the job has not succeeded (`not_ready`)", body = ErrorEnvelope)))]
pub(crate) async fn job_report_file(
    State(st): State<AppState>,
    UrlPath((id, file)): UrlPath<(String, String)>,
) -> ApiResult<Response> {
    report_file(&st, &id, &file)
}

fn report_file(st: &AppState, id: &str, name: &str) -> ApiResult<Response> {
    let job = st.jobs.get(parse_id(id)?)?;
    require_succeeded(&job)?;
    let bundle = job
        .report()
        .ok_or_else(|| ApiError::not_found("report_missing", "the job did not ask for a report"))?;
    let file = bundle
        .file(name)
        .ok_or_else(|| ApiError::not_found("report_file_missing", "the report has no such file"))?;
    let mut res = Response::new(Body::from(file.bytes.clone()));
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(file.kind.mime()),
    );
    Ok(res)
}

/// WebSocket stream of job events: a snapshot, then state and progress messages.
///
/// The server closes the stream after the final state message.
#[utoipa::path(get, path = "/api/v1/jobs/{id}/events", tag = "jobs",
    params(("id" = u64, Path, description = "job id")),
    responses((status = 101, description = "WebSocket; text frames are JobEvent JSON")))]
pub(crate) async fn job_events(
    State(st): State<AppState>,
    UrlPath(id): UrlPath<String>,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    let job = st.jobs.get(parse_id(&id)?)?;
    Ok(ws.on_upgrade(move |socket| stream_events(socket, job, st)))
}

async fn send_event(socket: &mut WebSocket, ev: &JobEvent) -> bool {
    match serde_json::to_string(ev) {
        Ok(text) => socket.send(Message::Text(text.into())).await.is_ok(),
        Err(_) => false,
    }
}

async fn stream_events(mut socket: WebSocket, job: Arc<crate::jobs::Job>, st: AppState) {
    // Subscribe before the snapshot so no transition is missed.
    let mut rx = job.subscribe();
    let snap = |job: &crate::jobs::Job| JobEvent::Snapshot {
        job: Box::new(crate::jobs::snapshot(job, &st.jobs)),
    };
    if send_event(&mut socket, &snap(&job)).await && !job.state().is_terminal() {
        loop {
            tokio::select! {
                ev = rx.recv() => match ev {
                    Ok(ev) => {
                        let done = matches!(&ev, JobEvent::State { state, .. } if state.is_terminal());
                        if !send_event(&mut socket, &ev).await || done {
                            break;
                        }
                    }
                    Err(RecvError::Lagged(_)) => {
                        let s = snap(&job);
                        if !send_event(&mut socket, &s).await || job.state().is_terminal() {
                            break;
                        }
                    }
                    Err(RecvError::Closed) => break,
                },
                msg = socket.recv() => match msg {
                    Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                    Some(Ok(_)) => {}
                },
            }
        }
    }
    // Finish the close handshake so the peer sees a clean close, not a reset.
    if socket.send(Message::Close(None)).await.is_ok() {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while let Some(Ok(_)) = socket.recv().await {}
        })
        .await;
    }
}

/// This API as an `OpenAPI` document.
#[utoipa::path(get, path = "/api/v1/openapi.json", tag = "meta",
    responses((status = 200, description = "OpenAPI 3 document")))]
pub(crate) async fn openapi_json() -> ApiResult<Response> {
    let json =
        crate::openapi::document().map_err(|e| ApiError::internal(format!("openapi: {e}")))?;
    Ok(([(header::CONTENT_TYPE, "application/json")], json).into_response())
}

/// A small HTML page that lists the operations of the `OpenAPI` document; needs no token.
#[utoipa::path(get, path = "/api/v1/docs", tag = "meta",
    responses((status = 200, content_type = "text/html")), security(()))]
pub(crate) async fn docs() -> Html<&'static str> {
    Html(include_str!("docs.html"))
}
