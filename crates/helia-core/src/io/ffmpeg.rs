// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Compressed video (MP4, MOV, MKV, `WebM`, M4V, and AVI files whose codec the
//! built-in AVI reader cannot handle) through an optional, external `ffmpeg`.
//!
//! Helia stays pure Rust: `ffmpeg` and `ffprobe` are programs found on the
//! `PATH` (or named by `HELIA_FFMPEG` and `HELIA_FFPROBE`) and run as child
//! processes. They are never linked and never bundled.
//!
//! The pipeline reads a capture several times and in any order, and video
//! codecs do not seek by frame. So a video is decoded once, by `ffmpeg`, into an
//! uncompressed SER file in a cache directory; every later read is an
//! ordinary random-access SER read, GPU decode included. The cache file is
//! removed when the last handle to it is dropped. See `docs/ARCHITECTURE.md`.
//!
//! Safety of the child processes: they are started from an argument vector,
//! never through a shell, with the input given as an absolute file path after
//! `-i`, and `-protocol_whitelist file` so the input cannot be redirected to a
//! network address or another protocol. No user string is ever an option.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, SystemTime};

use crate::debayer::Channel;
use crate::error::{Error, Result};
use crate::io::ser::{ColorId, SerHeader, SerWriter};

/// Environment variable naming the `ffmpeg` program.
pub const ENV_FFMPEG: &str = "HELIA_FFMPEG";
/// Environment variable naming the `ffprobe` program.
pub const ENV_FFPROBE: &str = "HELIA_FFPROBE";
/// Environment variable naming the directory for decoded video.
pub const ENV_CACHE_DIR: &str = "HELIA_CACHE_DIR";
/// Oldest `ffmpeg` that is supported, as (major, minor).
pub const MIN_VERSION: (u32, u32) = (4, 4);
/// Where the installation steps are written down.
pub const DOCS_URL: &str = "https://qisback.github.io/helia/user-guide/installation/";

/// File extensions that always go to `ffmpeg`.
pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mov", "m4v", "mkv", "webm"];

/// Cache files older than this, left by a killed run, are removed at the next
/// decode.
const STALE_AFTER: Duration = Duration::from_hours(24);

/// Why a video could not be read.
#[derive(Debug, thiserror::Error)]
pub enum FfmpegError {
    /// No usable `ffmpeg` or `ffprobe`.
    #[error(
        "ffmpeg was not found ({searched}). Helia reads MP4, MOV, MKV, WebM and compressed AVI \
         through the optional ffmpeg program. Install it: Windows `winget install Gyan.FFmpeg`, \
         macOS `brew install ffmpeg`, Debian or Ubuntu `sudo apt install ffmpeg`, Fedora \
         `sudo dnf install ffmpeg`; or set {ENV_FFMPEG} and {ENV_FFPROBE} to the programs. \
         More: {DOCS_URL}"
    )]
    NotFound {
        /// Where it was looked for.
        searched: String,
    },
    /// `ffmpeg` is older than [`MIN_VERSION`].
    #[error(
        "ffmpeg {found} is too old; Helia needs {}.{} or newer. Update it (see {DOCS_URL})",
        MIN_VERSION.0, MIN_VERSION.1
    )]
    TooOld {
        /// Version text reported by the program.
        found: String,
    },
    /// `ffprobe` could not read the file.
    #[error("ffprobe cannot read this file: {0}")]
    Probe(String),
    /// The file has no video stream.
    #[error("the file has no video stream")]
    NoVideoStream,
    /// A stream that cannot be handled.
    #[error("unsupported video: {0}")]
    Unsupported(String),
    /// The file ends early or is damaged.
    #[error("the video is truncated or damaged: {0}")]
    Truncated(String),
    /// `ffmpeg` failed.
    #[error("ffmpeg failed ({status}): {stderr}")]
    Failed {
        /// Exit status text.
        status: String,
        /// The end of its error output.
        stderr: String,
    },
    /// Not enough room to decode the video.
    #[error(
        "decoding needs about {} MiB of free space in {} but only {} MiB is free; free some \
         space or pick another directory with --cache-dir or {ENV_CACHE_DIR}",
        needed >> 20, dir.display(), available >> 20
    )]
    DiskSpace {
        /// Bytes needed.
        needed: u64,
        /// Bytes available.
        available: u64,
        /// The cache directory.
        dir: PathBuf,
    },
}

/// Is `path` a video that `ffmpeg` should read, judging by its extension?
#[must_use]
pub fn is_video_path(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|e| VIDEO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

// ---------------------------------------------------------------- locating

/// The programs and what they report.
#[derive(Clone, Debug)]
pub struct Tools {
    /// `ffmpeg`.
    pub ffmpeg: PathBuf,
    /// `ffprobe`.
    pub ffprobe: PathBuf,
    /// First line of `ffmpeg -version`.
    pub version: String,
    /// (major, minor) when the version string has one (git builds do not).
    pub numeric: Option<(u32, u32)>,
}

/// First match of `name` (or `name.exe`) in the directories of `path`.
fn find_in_path(name: &str, path: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path).find_map(|dir| {
        [name.to_owned(), format!("{name}.exe")]
            .into_iter()
            .map(|n| dir.join(n))
            .find(|p| p.is_file())
    })
}

/// (major, minor) from `ffmpeg version 6.1.1-3ubuntu5`, `... n8.1.3-18-g...`;
/// `None` for development builds (`N-12345-gabcdef`).
#[must_use]
pub fn parse_version(line: &str) -> Option<(u32, u32)> {
    let rest = line.split_once("version ")?.1.trim_start();
    let rest = rest.strip_prefix('n').unwrap_or(rest);
    let mut parts = rest.split(|c: char| !c.is_ascii_digit() && c != '.');
    let mut nums = parts.next()?.split('.');
    let major = nums.next()?.parse().ok()?;
    let minor = nums.next().and_then(|m| m.parse().ok()).unwrap_or(0);
    Some((major, minor))
}

fn version_line(program: &Path) -> std::result::Result<String, FfmpegError> {
    let out = Command::new(program)
        .arg("-version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| FfmpegError::NotFound {
            searched: format!("{}: {e}", program.display()),
        })?;
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned())
}

/// Copies of the programs that the user had installed through the application.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InstalledPrograms {
    /// The installed `ffmpeg`, if any.
    pub ffmpeg: Option<PathBuf>,
    /// The installed `ffprobe`, if any.
    pub ffprobe: Option<PathBuf>,
}

static INSTALLED_LOCATOR: OnceLock<fn() -> InstalledPrograms> = OnceLock::new();

/// Register the function that finds programs installed through the application
/// (`helia tools install ffmpeg` or the install button). The core stays free of the
/// installer: each front end passes its own lookup once at start-up. [`Tools::locate`]
/// asks it on every call, so an install made while the program runs is seen at once.
/// A second registration is ignored.
pub fn set_installed_locator(locate: fn() -> InstalledPrograms) {
    let _ = INSTALLED_LOCATOR.set(locate);
}

/// Where the programs come from, in the order [`Tools::locate`] tries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// `HELIA_FFMPEG` / `HELIA_FFPROBE`.
    Environment,
    /// A copy installed through the application.
    Installed,
    /// The `PATH`.
    Path,
}

impl Tools {
    /// Find `ffmpeg` and `ffprobe`: the environment variables first, then a copy installed
    /// through the application, then the `PATH`.
    ///
    /// # Errors
    /// [`FfmpegError::NotFound`] or [`FfmpegError::TooOld`].
    pub fn locate() -> std::result::Result<Self, FfmpegError> {
        let installed = INSTALLED_LOCATOR.get().map(|f| f()).unwrap_or_default();
        Self::locate_with(
            std::env::var_os(ENV_FFMPEG)
                .filter(|v| !v.is_empty())
                .or_else(|| installed.ffmpeg.map(PathBuf::into_os_string)),
            std::env::var_os(ENV_FFPROBE)
                .filter(|v| !v.is_empty())
                .or_else(|| installed.ffprobe.map(PathBuf::into_os_string)),
            std::env::var_os("PATH"),
        )
    }

    /// Where [`Self::locate`] takes `ffmpeg` from right now, or `None` when it is nowhere.
    #[must_use]
    pub fn source_of_ffmpeg() -> Option<Source> {
        if std::env::var_os(ENV_FFMPEG).is_some_and(|v| !v.is_empty()) {
            return Some(Source::Environment);
        }
        if INSTALLED_LOCATOR
            .get()
            .is_some_and(|f| f().ffmpeg.is_some())
        {
            return Some(Source::Installed);
        }
        let path = std::env::var_os("PATH")?;
        find_in_path("ffmpeg", &path).map(|_| Source::Path)
    }

    /// [`Self::locate`] with the environment given explicitly.
    ///
    /// # Errors
    /// [`FfmpegError::NotFound`] or [`FfmpegError::TooOld`].
    pub fn locate_with(
        ffmpeg: Option<OsString>,
        ffprobe: Option<OsString>,
        path: Option<OsString>,
    ) -> std::result::Result<Self, FfmpegError> {
        let path = path.unwrap_or_default();
        let ffmpeg = match ffmpeg.filter(|v| !v.is_empty()) {
            Some(p) => PathBuf::from(p),
            None => find_in_path("ffmpeg", &path).ok_or_else(|| FfmpegError::NotFound {
                searched: format!("{ENV_FFMPEG} is not set and ffmpeg is not on the PATH"),
            })?,
        };
        let ffprobe = match ffprobe.filter(|v| !v.is_empty()) {
            Some(p) => PathBuf::from(p),
            None => ffmpeg
                .parent()
                .filter(|d| !d.as_os_str().is_empty())
                .and_then(|d| find_in_path("ffprobe", d.as_os_str()))
                .or_else(|| find_in_path("ffprobe", &path))
                .ok_or_else(|| FfmpegError::NotFound {
                    searched: format!(
                        "{ENV_FFPROBE} is not set and ffprobe is not beside ffmpeg or on the PATH"
                    ),
                })?,
        };
        let version = version_line(&ffmpeg)?;
        if !version.contains("version") {
            return Err(FfmpegError::NotFound {
                searched: format!("{} did not report a version", ffmpeg.display()),
            });
        }
        let numeric = parse_version(&version);
        if numeric.is_some_and(|v| v < MIN_VERSION) {
            return Err(FfmpegError::TooOld { found: version });
        }
        Ok(Self {
            ffmpeg,
            ffprobe,
            version,
            numeric,
        })
    }
}

// ----------------------------------------------------------------- probing

/// What `ffprobe` says about the first video stream.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoInfo {
    /// ffprobe's container name list, e.g. `mov,mp4,m4a,3gp,3g2,mj2`.
    pub format_name: String,
    /// Codec short name, e.g. `h264`.
    pub codec: String,
    /// Profile, e.g. `High`.
    pub profile: Option<String>,
    /// Coded width.
    pub width: usize,
    /// Coded height.
    pub height: usize,
    /// Display rotation in degrees, one of 0, 90, 180, 270.
    pub rotation: u32,
    /// Frames in the stream.
    pub frames: u64,
    /// True when `frames` is a count (container field or counted packets),
    /// false when it is estimated from the duration.
    pub frames_exact: bool,
    /// Average frame rate.
    pub frame_rate: Option<f64>,
    /// Pixel format, e.g. `yuv420p`.
    pub pix_fmt: String,
    /// Bits per sample of the stored video.
    pub bit_depth: u8,
    /// Duration in seconds.
    pub duration: Option<f64>,
}

/// Codecs that store the picture without loss.
const LOSSLESS: &[&str] = &[
    "ffv1", "huffyuv", "ffvhuff", "utvideo", "rawvideo", "magicyuv", "png", "tiff", "qtrle",
    "v210", "r210",
];

impl VideoInfo {
    /// Width and height of the decoded frames: `ffmpeg` applies the rotation,
    /// so a quarter turn swaps them.
    #[must_use]
    pub fn output_size(&self) -> (usize, usize) {
        if self.rotation % 180 == 90 {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        }
    }

    /// Chroma subsampling written as `4:2:0`, when the pixel format has one.
    #[must_use]
    pub fn chroma(&self) -> Option<&'static str> {
        let p = &self.pix_fmt;
        if p.starts_with("gray") || p.starts_with("rgb") || p.starts_with("bgr") {
            None
        } else if p.contains("420") {
            Some("4:2:0")
        } else if p.contains("422") {
            Some("4:2:2")
        } else if p.contains("444") {
            Some("4:4:4")
        } else {
            None
        }
    }

    /// Is the codec lossy?
    #[must_use]
    pub fn is_lossy(&self) -> bool {
        !LOSSLESS.contains(&self.codec.as_str())
    }

    /// A caveat for users when the input is lossy.
    #[must_use]
    pub fn quality_note(&self) -> Option<String> {
        if !self.is_lossy() {
            return None;
        }
        let chroma = self.chroma().map_or(String::new(), |c| format!(" {c}"));
        Some(format!(
            "lossy {}-bit{chroma} video ({}): fine for quick results, SER/AVI raw captures keep \
             more detail",
            self.bit_depth, self.codec
        ))
    }

    /// Short container label for a path, for `describe`.
    #[must_use]
    pub fn container_label(&self, path: &Path) -> String {
        match path
            .extension()
            .and_then(OsStr::to_str)
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("mp4") => "MP4".into(),
            Some("mov") => "MOV".into(),
            Some("m4v") => "M4V".into(),
            Some("mkv") => "MKV".into(),
            Some("webm") => "WebM".into(),
            Some("avi") => "AVI".into(),
            _ => self
                .format_name
                .split(',')
                .next()
                .unwrap_or("video")
                .to_owned(),
        }
    }
}

fn str_field<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(serde_json::Value::as_str)
}

fn num_field(v: &serde_json::Value, key: &str) -> Option<f64> {
    v.get(key)
        .and_then(|x| {
            x.as_str()
                .map(str::to_owned)
                .or_else(|| x.as_f64().map(|f| f.to_string()))
        })
        .and_then(|s| s.parse().ok())
}

/// `30000/1001` or `25` to a rate; `0/0` is no rate.
fn parse_rate(s: &str) -> Option<f64> {
    let (n, d) = s.split_once('/').unwrap_or((s, "1"));
    let (n, d): (f64, f64) = (n.parse().ok()?, d.parse().ok()?);
    (d > 0.0 && n > 0.0).then_some(n / d)
}

/// Bits per sample from a pixel format name such as `yuv420p10le`, `gray16le`,
/// `rgb48le`; 8 when it says nothing.
fn depth_from_pix_fmt(pix_fmt: &str) -> u8 {
    for (needle, bits) in [
        ("rgb48", 16),
        ("bgr48", 16),
        ("rgba64", 16),
        ("16", 16),
        ("14", 14),
        ("12", 12),
        ("10", 10),
        ("9", 9),
    ] {
        if pix_fmt.contains(needle) {
            return bits;
        }
    }
    8
}

/// Rotation in degrees (0, 90, 180, 270) from a stream's tags and side data.
fn rotation_of(stream: &serde_json::Value) -> u32 {
    let from_side = stream
        .get("side_data_list")
        .and_then(serde_json::Value::as_array)
        .and_then(|l| l.iter().find_map(|d| num_field(d, "rotation")));
    let from_tag = stream.get("tags").and_then(|t| num_field(t, "rotate"));
    let degrees = from_side.or(from_tag).unwrap_or(0.0).round() as i64;
    degrees.rem_euclid(360) as u32 / 90 * 90
}

/// Parse `ffprobe -print_format json -show_format -show_streams` output.
///
/// # Errors
/// [`FfmpegError::Probe`] for unreadable JSON, [`FfmpegError::NoVideoStream`]
/// when no stream is video.
pub fn parse_probe(json: &str) -> std::result::Result<VideoInfo, FfmpegError> {
    let doc: serde_json::Value =
        serde_json::from_str(json).map_err(|e| FfmpegError::Probe(format!("bad JSON: {e}")))?;
    let stream = doc
        .get("streams")
        .and_then(serde_json::Value::as_array)
        .and_then(|s| {
            s.iter()
                .find(|s| str_field(s, "codec_type") == Some("video"))
        })
        .ok_or(FfmpegError::NoVideoStream)?;
    let format = doc.get("format").unwrap_or(&serde_json::Value::Null);
    let (width, height) = (
        num_field(stream, "width").unwrap_or(0.0) as usize,
        num_field(stream, "height").unwrap_or(0.0) as usize,
    );
    if width == 0 || height == 0 {
        return Err(FfmpegError::Unsupported(
            "the video stream has no picture size".into(),
        ));
    }
    let pix_fmt = str_field(stream, "pix_fmt").unwrap_or("").to_owned();
    let bit_depth = num_field(stream, "bits_per_raw_sample")
        .map(|b| b as u8)
        .filter(|b| (1..=16).contains(b))
        .unwrap_or_else(|| depth_from_pix_fmt(&pix_fmt));
    let frame_rate = str_field(stream, "avg_frame_rate")
        .and_then(parse_rate)
        .or_else(|| str_field(stream, "r_frame_rate").and_then(parse_rate));
    let duration = num_field(stream, "duration").or_else(|| num_field(format, "duration"));
    let counted = num_field(stream, "nb_frames")
        .or_else(|| num_field(stream, "nb_read_packets"))
        .filter(|n| *n >= 1.0)
        .map(|n| n as u64);
    let (frames, frames_exact) = match counted {
        Some(n) => (n, true),
        None => (
            duration
                .zip(frame_rate)
                .map_or(0, |(d, r)| (d * r).round() as u64),
            false,
        ),
    };
    Ok(VideoInfo {
        format_name: str_field(format, "format_name").unwrap_or("").to_owned(),
        codec: str_field(stream, "codec_name")
            .unwrap_or("unknown")
            .to_owned(),
        profile: str_field(stream, "profile").map(str::to_owned),
        width,
        height,
        rotation: rotation_of(stream),
        frames,
        frames_exact,
        frame_rate,
        pix_fmt,
        bit_depth,
        duration,
    })
}

/// Absolute form of `path`, so that nothing in it can read as an option or a
/// protocol (`-x`, `http:`, `concat:`).
fn absolute_file(path: &Path) -> std::result::Result<PathBuf, FfmpegError> {
    let abs = std::path::absolute(path)
        .map_err(|e| FfmpegError::Probe(format!("{}: {e}", path.display())))?;
    if !abs.is_file() {
        return Err(FfmpegError::Probe(format!(
            "{} is not a file",
            path.display()
        )));
    }
    Ok(abs)
}

/// Arguments for the ffprobe call that describes the first video stream.
#[must_use]
pub fn probe_args(input: &Path, count_packets: bool) -> Vec<OsString> {
    let mut a: Vec<OsString> = ["-v", "error", "-hide_banner", "-protocol_whitelist", "file"]
        .into_iter()
        .map(Into::into)
        .collect();
    a.extend(
        [
            "-select_streams",
            "v:0",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ]
        .into_iter()
        .map(OsString::from),
    );
    if count_packets {
        a.push("-count_packets".into());
    }
    a.push("-i".into());
    a.push(input.as_os_str().to_owned());
    a
}

fn run_probe(
    tools: &Tools,
    input: &Path,
    count_packets: bool,
) -> std::result::Result<String, FfmpegError> {
    let out = Command::new(&tools.ffprobe)
        .args(probe_args(input, count_packets))
        .stdin(Stdio::null())
        .output()
        .map_err(|e| FfmpegError::Probe(format!("cannot run {}: {e}", tools.ffprobe.display())))?;
    if !out.status.success() {
        return Err(classify_stderr(&String::from_utf8_lossy(&out.stderr)));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A truncated file shows up as particular ffmpeg messages.
fn classify_stderr(stderr: &str) -> FfmpegError {
    let tail = tail_of(stderr, 600);
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("moov atom not found")
        || lower.contains("truncated")
        || lower.contains("end of file")
        || lower.contains("partial file")
    {
        FfmpegError::Truncated(tail)
    } else {
        FfmpegError::Probe(tail)
    }
}

fn tail_of(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.len() <= max {
        return s.to_owned();
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    format!("...{}", &s[start..])
}

/// Describe a video without decoding it.
///
/// # Errors
/// Any [`FfmpegError`] from locating the tools or reading the file.
pub fn inspect(path: &Path) -> std::result::Result<VideoInfo, FfmpegError> {
    inspect_with(&Tools::locate()?, path)
}

/// [`inspect`] with the tools already located.
///
/// # Errors
/// Any [`FfmpegError`] from reading the file.
pub fn inspect_with(tools: &Tools, path: &Path) -> std::result::Result<VideoInfo, FfmpegError> {
    let input = absolute_file(path)?;
    let mut info = parse_probe(&run_probe(tools, &input, false)?)?;
    if !info.frames_exact {
        // The container does not say; count the packets (reads, never decodes).
        if let Ok(counted) = run_probe(tools, &input, true).and_then(|j| parse_probe(&j))
            && counted.frames_exact
        {
            info.frames = counted.frames;
            info.frames_exact = true;
        }
    }
    if info.frames == 0 {
        return Err(FfmpegError::Truncated(
            "no frames could be counted in the video stream".into(),
        ));
    }
    Ok(info)
}

// ---------------------------------------------------------------- decoding

/// What `ffmpeg` is asked to produce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    /// Output pixel format, `gray` or `gray16le`.
    pub pix_fmt: &'static str,
    /// Bits per sample of the SER file: 8 or 16.
    pub depth: u8,
    /// Frame width after rotation.
    pub width: usize,
    /// Frame height after rotation.
    pub height: usize,
    /// Plane to extract from the RGB picture; `None` is luminance.
    pub plane: Option<char>,
}

impl Plan {
    /// Bytes per decoded frame.
    #[must_use]
    pub fn frame_bytes(&self) -> usize {
        self.width * self.height * usize::from(self.depth / 8)
    }
}

/// Choose the output for `info` and the user's channel.
///
/// Eight-bit video is kept at 8 bits and anything deeper at 16, so no space is
/// spent on precision the source does not have. A colour channel is extracted
/// by `ffmpeg` from RGB, so the cache holds one plane, not three.
#[must_use]
pub fn plan(info: &VideoInfo, channel: Channel) -> Plan {
    let (width, height) = info.output_size();
    let deep = info.bit_depth > 8;
    Plan {
        pix_fmt: if deep { "gray16le" } else { "gray" },
        depth: if deep { 16 } else { 8 },
        width,
        height,
        plane: match channel {
            Channel::Red => Some('r'),
            Channel::Green => Some('g'),
            Channel::Blue => Some('b'),
            Channel::Luminance => None,
        },
    }
}

/// Arguments for the decoding `ffmpeg` call: raw frames of `plan` on stdout.
///
/// `ffmpeg` applies the display rotation itself. `max_frames` stops early.
#[must_use]
pub fn decode_args(
    input: &Path,
    plan: &Plan,
    max_frames: Option<u64>,
    numeric: Option<(u32, u32)>,
) -> Vec<OsString> {
    let mut a: Vec<OsString> = [
        "-nostdin",
        "-hide_banner",
        "-nostats",
        "-loglevel",
        "error",
        "-protocol_whitelist",
        "file",
        "-i",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    a.push(input.as_os_str().to_owned());
    a.extend(
        ["-map", "0:v:0", "-an", "-sn", "-dn"]
            .into_iter()
            .map(OsString::from),
    );
    if let Some(p) = plan.plane {
        let rgb = if plan.depth > 8 { "rgb48le" } else { "rgb24" };
        a.push("-vf".into());
        a.push(format!("format={rgb},extractplanes={p}").into());
    }
    if let Some(n) = max_frames {
        a.push("-frames:v".into());
        a.push(n.to_string().into());
    }
    // Every frame, none dropped or repeated for timing; the flag was renamed.
    if numeric.is_some_and(|v| v < (5, 1)) {
        a.extend(["-vsync", "0"].into_iter().map(OsString::from));
    } else {
        a.extend(["-fps_mode", "passthrough"].into_iter().map(OsString::from));
    }
    a.extend(
        ["-f", "rawvideo", "-pix_fmt", plan.pix_fmt, "pipe:1"]
            .into_iter()
            .map(OsString::from),
    );
    a
}

/// Arguments that decode only frame `index` (counting from 0) of `plan`'s
/// stream. The index is a number, never text from the user.
#[must_use]
pub fn frame_args(
    input: &Path,
    plan: &Plan,
    index: usize,
    numeric: Option<(u32, u32)>,
) -> Vec<OsString> {
    let mut a = decode_args(input, plan, Some(1), numeric);
    // Put the selection first in the filter chain, before any plane extraction.
    let select = format!("select=eq(n\\,{index})");
    if let Some(at) = a.iter().position(|s| s == "-vf") {
        let chain = a[at + 1].to_string_lossy().into_owned();
        a[at + 1] = format!("{select},{chain}").into();
    } else if let Some(at) = a.iter().position(|s| s == "-frames:v") {
        a.insert(at, select.into());
        a.insert(at, "-vf".into());
    }
    a
}

/// The colour plane to stack from a colour video, chosen so that a clipped
/// plane is not used (see [`crate::clip`]): one early frame is decoded as each
/// of red, green and blue and the planes are checked for a flat plateau. A
/// Seestar video of the Sun, for one, has its red plane clipped and its blue
/// empty, so only the green holds detail. `Luminance` when nothing is clipped.
///
/// # Errors
/// As [`read_frame`].
pub fn choose_channel(path: &Path) -> Result<crate::clip::ChannelChoice> {
    use crate::debayer::Channel;
    let r = read_frame(path, 2, Channel::Red)?;
    let g = read_frame(path, 2, Channel::Green)?;
    let b = read_frame(path, 2, Channel::Blue)?;
    Ok(match crate::clip::probe_planes([&r, &g, &b]) {
        Some(p) => crate::clip::choose(Channel::Luminance, &p),
        None => crate::clip::ChannelChoice {
            channel: Channel::Luminance,
            note: None,
        },
    })
}

/// Decode a single frame without decoding the whole video: the frames before
/// it are decoded and dropped, so it takes time in proportion to `index`.
///
/// # Errors
/// [`Error::Ffmpeg`] for tools and decoding problems, [`Error::Invalid`] for an
/// index past the end.
pub fn read_frame(path: &Path, index: usize, channel: Channel) -> Result<crate::frame::Frame> {
    let tools = Tools::locate()?;
    let abs = absolute_file(path)?;
    let info = inspect_with(&tools, &abs)?;
    if index as u64 >= info.frames {
        return Err(Error::Invalid(format!(
            "frame {index} out of range ({})",
            info.frames
        )));
    }
    let plan = plan(&info, channel);
    let args = frame_args(&abs, &plan, index, tools.numeric);
    let child = Command::new(&tools.ffmpeg)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| FfmpegError::Failed {
            status: "could not start".into(),
            stderr: format!("{}: {e}", tools.ffmpeg.display()),
        })?;
    let out = child.wait_with_output()?;
    let want = plan.frame_bytes();
    if out.stdout.len() < want {
        return Err(classify_decode_failure(
            &out.status.to_string(),
            &String::from_utf8_lossy(&out.stderr),
        )
        .into());
    }
    let data = if plan.depth > 8 {
        out.stdout[..want]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| f32::from(u16::from_le_bytes(*c)) / 65535.0)
            .collect()
    } else {
        out.stdout[..want]
            .iter()
            .map(|&v| f32::from(v) / 255.0)
            .collect()
    };
    Ok(crate::frame::Frame {
        width: plan.width,
        height: plan.height,
        data,
        bayer: None,
    })
}

/// Kills the child on drop, so no path leaves `ffmpeg` running.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Options for opening a video.
#[derive(Clone, Default)]
pub struct VideoOptions {
    /// Directory for the decoded file; the OS cache directory by default.
    pub cache_dir: Option<PathBuf>,
    /// Decode only the first N frames.
    pub max_frames: Option<usize>,
    /// Set to stop decoding; the `ffmpeg` child is killed.
    pub cancel: Option<Arc<AtomicBool>>,
    /// Called with (frames decoded, frames expected) as decoding goes.
    pub progress: Option<Arc<dyn Fn(u64, u64) + Send + Sync>>,
}

impl std::fmt::Debug for VideoOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoOptions")
            .field("cache_dir", &self.cache_dir)
            .field("max_frames", &self.max_frames)
            .finish_non_exhaustive()
    }
}

/// The directory decoded video goes in unless told otherwise: `HELIA_CACHE_DIR`,
/// else the operating system's cache directory, else the temporary directory.
#[must_use]
pub fn default_cache_dir() -> PathBuf {
    if let Some(d) = std::env::var_os(ENV_CACHE_DIR).filter(|v| !v.is_empty()) {
        return PathBuf::from(d);
    }
    let var = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let base = if cfg!(windows) {
        var("LOCALAPPDATA")
    } else if cfg!(target_os = "macos") {
        var("HOME").map(|h| h.join("Library").join("Caches"))
    } else {
        var("XDG_CACHE_HOME").or_else(|| var("HOME").map(|h| h.join(".cache")))
    };
    base.unwrap_or_else(std::env::temp_dir)
        .join("helia")
        .join("video")
}

/// A decoded video on disk; deleted when the last handle drops.
#[derive(Debug)]
pub struct CacheFile {
    path: PathBuf,
}

impl CacheFile {
    /// Where the SER file is.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for CacheFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

type Registry = Mutex<HashMap<String, Weak<CacheFile>>>;

fn registry() -> &'static Registry {
    static R: OnceLock<Registry> = OnceLock::new();
    R.get_or_init(Mutex::default)
}

/// Serialises decodes, so two threads opening the same video decode it once.
fn decode_lock() -> &'static Mutex<()> {
    static L: Mutex<()> = Mutex::new(());
    &L
}

/// Remove `helia-video-*.ser` files in `dir` older than [`STALE_AFTER`] (left
/// by a run that was killed).
fn sweep_stale(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("helia-video-") && name.ends_with(".ser")) {
            continue;
        }
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age > STALE_AFTER);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// The result of decoding: the cache handle and what was in the file.
#[derive(Clone, Debug)]
pub struct Decoded {
    /// The decoded SER file; keep it alive while reading.
    pub cache: Arc<CacheFile>,
    /// The video's description.
    pub info: VideoInfo,
    /// Frames in the cache (can be fewer than `info.frames` with `max_frames`).
    pub frames: usize,
}

fn cache_key(
    abs: &Path,
    info_len: u64,
    mtime: SystemTime,
    plan: &Plan,
    max: Option<usize>,
) -> String {
    let secs = mtime
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(
        "{}|{info_len}|{secs}|{:?}|{max:?}|{}",
        abs.display(),
        plan.plane,
        plan.depth
    )
}

/// Decode `path` into a cache file with `ffmpeg`, or reuse the file a handle
/// elsewhere in this process is already holding.
///
/// # Errors
/// [`Error::Ffmpeg`] for tools, file and decoding problems,
/// [`Error::Cancelled`] when `opts.cancel` was set, I/O errors.
pub fn decode(path: &Path, channel: Channel, opts: &VideoOptions) -> Result<Decoded> {
    let tools = Tools::locate()?;
    decode_with(&tools, path, channel, opts)
}

/// [`decode`] with the tools already located.
///
/// # Errors
/// As [`decode`].
pub fn decode_with(
    tools: &Tools,
    path: &Path,
    channel: Channel,
    opts: &VideoOptions,
) -> Result<Decoded> {
    let abs = absolute_file(path)?;
    let info = inspect_with(tools, &abs)?;
    let plan = plan(&info, channel);
    let wanted = opts
        .max_frames
        .map_or(info.frames, |m| info.frames.min(m as u64));
    let meta = std::fs::metadata(&abs)?;
    let key = cache_key(
        &abs,
        meta.len(),
        meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        &plan,
        opts.max_frames,
    );
    let _serial = decode_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(cache) = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)
        .and_then(Weak::upgrade)
    {
        let frames = SerFrames::count(cache.path()).unwrap_or(wanted as usize);
        return Ok(Decoded {
            cache,
            info,
            frames,
        });
    }

    let dir = opts.cache_dir.clone().unwrap_or_else(default_cache_dir);
    std::fs::create_dir_all(&dir)?;
    sweep_stale(&dir);
    let needed = plan.frame_bytes() as u64 * wanted + 4096;
    let available = fs4::available_space(&dir)?;
    if available < needed + needed / 20 {
        return Err(FfmpegError::DiskSpace {
            needed,
            available,
            dir,
        }
        .into());
    }
    let out_path = dir.join(format!(
        "helia-video-{}-{:016x}.ser",
        std::process::id(),
        fnv(&key)
    ));
    let cache = Arc::new(CacheFile {
        path: out_path.clone(),
    });
    let frames = decode_into(tools, &abs, &info, &plan, wanted, &out_path, opts)?;
    registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(key, Arc::downgrade(&cache));
    Ok(Decoded {
        cache,
        info,
        frames,
    })
}

/// FNV-1a of a string, for file names.
fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Frame count of a cache SER file, from its header.
struct SerFrames;

impl SerFrames {
    fn count(path: &Path) -> Option<usize> {
        let mut f = File::open(path).ok()?;
        let mut head = [0u8; 178];
        f.read_exact(&mut head).ok()?;
        let n = i32::from_le_bytes([head[38], head[39], head[40], head[41]]);
        usize::try_from(n).ok()
    }
}

fn decode_into(
    tools: &Tools,
    input: &Path,
    info: &VideoInfo,
    plan: &Plan,
    wanted: u64,
    out_path: &Path,
    opts: &VideoOptions,
) -> Result<usize> {
    let cancelled = || {
        opts.cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
    };
    let header = SerHeader::new(
        u32::try_from(plan.width).unwrap_or(u32::MAX),
        u32::try_from(plan.height).unwrap_or(u32::MAX),
        ColorId::Mono,
        u32::from(plan.depth),
    );
    let mut writer = SerWriter::new(
        BufWriter::with_capacity(1 << 22, File::create(out_path)?),
        header,
    )?;
    let args = decode_args(
        input,
        plan,
        opts.max_frames.map(|m| m as u64),
        tools.numeric,
    );
    let mut child = ChildGuard(
        Command::new(&tools.ffmpeg)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| FfmpegError::Failed {
                status: "could not start".into(),
                stderr: format!("{}: {e}", tools.ffmpeg.display()),
            })?,
    );
    let mut stdout = child.0.stdout.take().ok_or_else(|| FfmpegError::Failed {
        status: "no output pipe".into(),
        stderr: String::new(),
    })?;
    let stderr = child.0.stderr.take();
    // Drain stderr on its own thread so a chatty ffmpeg never blocks on it.
    let err_thread = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut s) = stderr {
            let mut raw = Vec::new();
            let _ = s.read_to_end(&mut raw);
            text = String::from_utf8_lossy(&raw).into_owned();
        }
        text
    });

    let mut buf = vec![0u8; plan.frame_bytes()];
    let mut decoded: u64 = 0;
    let result = loop {
        if cancelled() {
            break Err(Error::Cancelled);
        }
        match read_full(&mut stdout, &mut buf) {
            Ok(true) => {}
            Ok(false) => break Ok(()),
            Err(e) => break Err(Error::Io(e)),
        }
        if let Err(e) = writer.write_raw_frame(&buf) {
            break Err(e.into());
        }
        decoded += 1;
        if let Some(p) = &opts.progress
            && (decoded.is_multiple_of(8) || decoded == wanted)
        {
            p(decoded, wanted);
        }
        if decoded >= wanted && opts.max_frames.is_some() {
            break Ok(());
        }
    };
    if result.is_err() {
        drop(child);
        let _ = err_thread.join();
        drop(writer);
        let _ = std::fs::remove_file(out_path);
        return result.map(|()| 0);
    }
    drop(stdout);
    let status = child.0.wait()?;
    let stderr_text = err_thread.join().unwrap_or_default();
    let mut file = writer.finish()?;
    file.flush()?;

    // A decoder can drop a frame or two at the very end of a stream (open-GOP
    // B-frames, a cut file); anything beyond 1% is a truncated or damaged video.
    let missing = wanted.saturating_sub(decoded);
    let tolerated = missing <= (wanted / 100).max(2);
    let short = !tolerated && (info.frames_exact || !status.success());
    if decoded == 0 || short {
        let _ = std::fs::remove_file(out_path);
        let tail = tail_of(&stderr_text, 600);
        return Err(if decoded == 0 && !status.success() {
            classify_decode_failure(&status.to_string(), &stderr_text)
        } else if decoded == 0 {
            FfmpegError::Unsupported("ffmpeg produced no frames".into())
        } else {
            FfmpegError::Truncated(format!("decoded {decoded} of {wanted} frames. {tail}"))
        }
        .into());
    }
    Ok(usize::try_from(decoded).unwrap_or(usize::MAX))
}

fn classify_decode_failure(status: &str, stderr: &str) -> FfmpegError {
    match classify_stderr(stderr) {
        FfmpegError::Truncated(t) => FfmpegError::Truncated(t),
        _ => FfmpegError::Failed {
            status: status.to_owned(),
            stderr: tail_of(stderr, 600),
        },
    }
}

/// Fill `buf`; false when the stream ended before the first byte, an error
/// when it ended part-way (a cut frame).
fn read_full(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<bool> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(&mut buf[got..]) {
            Ok(0) => return Ok(false),
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const H264_MP4: &str = r#"{
      "streams": [{
        "index": 0, "codec_name": "h264", "profile": "High", "codec_type": "video",
        "width": 3840, "height": 2160, "pix_fmt": "yuv420p",
        "r_frame_rate": "25/1", "avg_frame_rate": "25/1",
        "duration": "10.000000", "bits_per_raw_sample": "8", "nb_frames": "250"
      }],
      "format": { "format_name": "mov,mp4,m4a,3gp,3g2,mj2", "duration": "10.000000" }
    }"#;

    #[test]
    fn parses_an_h264_mp4() {
        let i = parse_probe(H264_MP4).expect("parse");
        assert_eq!(
            (i.width, i.height, i.frames, i.frames_exact),
            (3840, 2160, 250, true)
        );
        assert_eq!(i.codec, "h264");
        assert_eq!(i.bit_depth, 8);
        assert_eq!(i.frame_rate, Some(25.0));
        assert_eq!(i.chroma(), Some("4:2:0"));
        assert!(i.is_lossy());
        let note = i.quality_note().expect("lossy note");
        assert!(note.starts_with("lossy 8-bit 4:2:0 video"), "{note}");
        assert!(note.contains("SER/AVI raw captures keep more detail"));
    }

    #[test]
    fn counts_come_from_packets_then_duration() {
        let packets = H264_MP4.replace("\"nb_frames\": \"250\"", "\"nb_read_packets\": \"240\"");
        let i = parse_probe(&packets).expect("parse");
        assert_eq!((i.frames, i.frames_exact), (240, true));
        let none = H264_MP4.replace(", \"nb_frames\": \"250\"", "");
        let i = parse_probe(&none).expect("parse");
        assert_eq!((i.frames, i.frames_exact), (250, false), "duration * rate");
    }

    #[test]
    fn rotation_from_display_matrix_and_old_tag() {
        let side = H264_MP4.replace(
            "\"nb_frames\": \"250\"",
            "\"nb_frames\": \"250\", \"side_data_list\": [{\"side_data_type\": \"Display Matrix\", \"rotation\": -90}]",
        );
        let i = parse_probe(&side).expect("parse");
        assert_eq!(i.rotation, 270);
        assert_eq!(
            i.output_size(),
            (2160, 3840),
            "a quarter turn swaps the size"
        );
        let tag = H264_MP4.replace(
            "\"nb_frames\": \"250\"",
            "\"nb_frames\": \"250\", \"tags\": {\"rotate\": \"90\"}",
        );
        assert_eq!(parse_probe(&tag).expect("parse").rotation, 90);
        let flip = H264_MP4.replace(
            "\"nb_frames\": \"250\"",
            "\"nb_frames\": \"250\", \"side_data_list\": [{\"rotation\": 180}]",
        );
        let i = parse_probe(&flip).expect("parse");
        assert_eq!((i.rotation, i.output_size()), (180, (3840, 2160)));
    }

    #[test]
    fn deep_and_lossless_video() {
        let j = H264_MP4
            .replace("h264", "ffv1")
            .replace("yuv420p", "gray16le")
            .replace("\"bits_per_raw_sample\": \"8\", ", "");
        let i = parse_probe(&j).expect("parse");
        assert_eq!(i.bit_depth, 16);
        assert!(!i.is_lossy());
        assert_eq!(i.quality_note(), None);
        let ten = H264_MP4
            .replace("yuv420p", "yuv420p10le")
            .replace("\"8\"", "\"10\"");
        assert_eq!(parse_probe(&ten).expect("parse").bit_depth, 10);
    }

    #[test]
    fn rejects_files_without_a_picture() {
        assert!(matches!(
            parse_probe(r#"{"streams":[{"codec_type":"audio"}],"format":{}}"#),
            Err(FfmpegError::NoVideoStream)
        ));
        assert!(matches!(
            parse_probe("not json"),
            Err(FfmpegError::Probe(_))
        ));
        assert!(matches!(
            parse_probe(r#"{"streams":[{"codec_type":"video","codec_name":"h264"}]}"#),
            Err(FfmpegError::Unsupported(_))
        ));
    }

    #[test]
    fn version_strings() {
        assert_eq!(
            parse_version("ffmpeg version 6.1.1-3ubuntu5 Copyright"),
            Some((6, 1))
        );
        assert_eq!(
            parse_version("ffmpeg version n8.1.3-18-g46d899d7a0-20261018"),
            Some((8, 1))
        );
        assert_eq!(
            parse_version("ffmpeg version 4.4.2-0ubuntu0.22.04.1"),
            Some((4, 4))
        );
        assert_eq!(parse_version("ffmpeg version 7 Copyright"), Some((7, 0)));
        assert_eq!(parse_version("ffmpeg version N-110000-gabcdef0123"), None);
        assert_eq!(parse_version("something else"), None);
    }

    #[test]
    fn plan_keeps_8_bit_at_8_bit_and_extracts_one_plane() {
        let mut i = parse_probe(H264_MP4).expect("parse");
        let p = plan(&i, Channel::Luminance);
        assert_eq!((p.pix_fmt, p.depth, p.plane), ("gray", 8, None));
        assert_eq!(p.frame_bytes(), 3840 * 2160);
        let p = plan(&i, Channel::Green);
        assert_eq!(p.plane, Some('g'));
        i.bit_depth = 10;
        let p = plan(&i, Channel::Luminance);
        assert_eq!((p.pix_fmt, p.depth), ("gray16le", 16));
        assert_eq!(p.frame_bytes(), 3840 * 2160 * 2);
    }

    fn strings(a: &[OsString]) -> Vec<String> {
        a.iter().map(|s| s.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn decode_arguments_are_a_plain_vector() {
        let i = parse_probe(H264_MP4).expect("parse");
        let p = plan(&i, Channel::Luminance);
        let a = strings(&decode_args(
            Path::new("/data/sun.mp4"),
            &p,
            None,
            Some((6, 1)),
        ));
        let at = a.iter().position(|s| s == "-i").expect("-i");
        assert_eq!(a[at + 1], "/data/sun.mp4");
        assert_eq!(a[at - 2..at], ["-protocol_whitelist", "file"]);
        assert!(a.windows(2).any(|w| w == ["-pix_fmt", "gray"]));
        assert!(a.windows(2).any(|w| w == ["-f", "rawvideo"]));
        assert!(a.windows(2).any(|w| w == ["-fps_mode", "passthrough"]));
        assert_eq!(a.last().map(String::as_str), Some("pipe:1"));
        assert!(!a.iter().any(|s| s == "-frames:v"));
        let old = strings(&decode_args(Path::new("/x.mp4"), &p, Some(5), Some((4, 4))));
        assert!(old.windows(2).any(|w| w == ["-vsync", "0"]));
        assert!(old.windows(2).any(|w| w == ["-frames:v", "5"]));
        let g = plan(&i, Channel::Blue);
        let a = strings(&decode_args(Path::new("/x.mp4"), &g, None, None));
        assert!(
            a.windows(2)
                .any(|w| w == ["-vf", "format=rgb24,extractplanes=b"])
        );
    }

    #[test]
    fn single_frame_arguments_select_by_number() {
        let i = parse_probe(H264_MP4).expect("parse");
        let lum = strings(&frame_args(
            Path::new("/x.mp4"),
            &plan(&i, Channel::Luminance),
            42,
            Some((6, 1)),
        ));
        assert!(
            lum.windows(2).any(|w| w == ["-vf", "select=eq(n\\,42)"]),
            "{lum:?}"
        );
        assert!(lum.windows(2).any(|w| w == ["-frames:v", "1"]));
        let red = strings(&frame_args(
            Path::new("/x.mp4"),
            &plan(&i, Channel::Red),
            7,
            Some((6, 1)),
        ));
        assert!(
            red.windows(2)
                .any(|w| w == ["-vf", "select=eq(n\\,7),format=rgb24,extractplanes=r"]),
            "{red:?}"
        );
    }

    #[test]
    fn a_hostile_file_name_stays_one_argument() {
        let i = parse_probe(H264_MP4).expect("parse");
        let p = plan(&i, Channel::Luminance);
        for name in [
            "/tmp/-i evil.mp4",
            "/tmp/a; rm -rf ~ #.mp4",
            "/tmp/$(touch pwned).mp4",
            "/tmp/concat:a|b.mp4",
            "/tmp/-f lavfi.mp4",
            "/tmp/x\" -y \"y.mp4",
        ] {
            let a = decode_args(Path::new(name), &p, None, None);
            let at = a.iter().position(|s| s == "-i").expect("-i");
            assert_eq!(a[at + 1], OsString::from(name), "{name}");
            // The name is one argument, once, and nothing else carries it.
            assert_eq!(
                a.iter()
                    .filter(|s| s.to_string_lossy().contains("/tmp/"))
                    .count(),
                1,
                "{name}"
            );
            let pr = probe_args(Path::new(name), false);
            assert_eq!(pr.last().map(OsString::as_os_str), Some(OsStr::new(name)));
            assert_eq!(pr[pr.len() - 2], OsString::from("-i"));
        }
    }

    #[test]
    fn relative_and_option_like_paths_become_absolute() {
        let dir = std::env::temp_dir().join(format!("helia-ffmpeg-abs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let file = dir.join("-dash.mp4");
        std::fs::write(&file, b"x").expect("write");
        let abs = absolute_file(&file).expect("absolute");
        assert!(abs.is_absolute());
        assert!(
            !abs.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("http"))
        );
        assert!(absolute_file(&dir.join("missing.mp4")).is_err());
        assert!(absolute_file(&dir).is_err(), "a directory is not a video");
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn missing_tools_say_how_to_install_them() {
        let e = Tools::locate_with(None, None, Some(OsString::from("/nonexistent-dir")))
            .expect_err("no ffmpeg");
        let text = e.to_string();
        assert!(matches!(e, FfmpegError::NotFound { .. }));
        for needle in [
            "winget",
            "brew",
            "apt",
            "dnf",
            "HELIA_FFMPEG",
            "user-guide/installation",
        ] {
            assert!(text.contains(needle), "{needle} in {text}");
        }
    }

    #[test]
    fn truncated_files_are_recognised() {
        let e = classify_stderr("[mov,mp4] moov atom not found\nError opening input");
        assert!(matches!(e, FfmpegError::Truncated(_)));
        assert!(matches!(
            classify_stderr("Invalid data found"),
            FfmpegError::Probe(_)
        ));
    }

    #[test]
    fn video_extensions() {
        for p in ["a.mp4", "a.MOV", "a.mkv", "a.WebM", "a.m4v"] {
            assert!(is_video_path(Path::new(p)), "{p}");
        }
        for p in ["a.ser", "a.avi", "a.mp4.txt", "mp4"] {
            assert!(!is_video_path(Path::new(p)), "{p}");
        }
    }

    #[test]
    fn full_reads_and_cut_frames() {
        let mut full = std::io::Cursor::new(vec![1u8; 8]);
        let mut buf = [0u8; 4];
        assert!(read_full(&mut full, &mut buf).expect("read"));
        assert!(read_full(&mut full, &mut buf).expect("read"));
        assert!(!read_full(&mut full, &mut buf).expect("eof"));
    }

    #[test]
    fn cache_dir_defaults_somewhere_named_helia() {
        let d = default_cache_dir();
        if std::env::var_os(ENV_CACHE_DIR).is_none() {
            assert!(
                d.ends_with(Path::new("helia").join("video")),
                "{}",
                d.display()
            );
        }
    }
}
