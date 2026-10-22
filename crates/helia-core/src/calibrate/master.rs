// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Master dark, bias and flat frames: building them from calibration captures,
//! and storing them as float FITS files.

#![allow(
    clippy::cast_possible_wrap,
    reason = "frame counts are far below i64::MAX"
)]

use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use rayon::prelude::*;

use crate::error::{Error, Result};
use crate::frame::{BayerPattern, Frame};
use crate::io::fits::{self, FitsFormat, HeaderCard};
use crate::io::solarnet::CaptureInfo;
use crate::io::source::{FrameSource, OpenOptions, open_capture};

use super::diag;

/// What a master is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MasterKind {
    /// Signal with the lens capped, at the exposure and gain of the lights.
    Dark,
    /// Zero-length exposure: the electronic offset.
    Bias,
    /// An evenly lit field, normalised to a mean of 1.
    Flat,
}

impl MasterKind {
    /// Lower-case name, as stored in the `HELIAKND` card.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Bias => "bias",
            Self::Flat => "flat",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "dark" => Some(Self::Dark),
            "bias" => Some(Self::Bias),
            "flat" => Some(Self::Flat),
            _ => None,
        }
    }
}

/// How the frames of a stack of calibration frames are combined, per pixel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Combine {
    /// The median.
    Median,
    /// The mean after rejecting values more than `kappa` sigma from it,
    /// repeated until nothing more is rejected (at most five rounds).
    SigmaClip {
        /// Rejection threshold in standard deviations.
        kappa: f32,
    },
}

impl Default for Combine {
    fn default() -> Self {
        Self::SigmaClip { kappa: 3.0 }
    }
}

impl Combine {
    /// Text for a header or a log: `median` or `sigma-clip 3`.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::Median => "median".to_owned(),
            Self::SigmaClip { kappa } => format!("sigma-clip {kappa}"),
        }
    }
}

impl std::str::FromStr for Combine {
    type Err = Error;

    /// `median`, `sigma` (kappa 3) or `sigma:K`.
    fn from_str(s: &str) -> Result<Self> {
        let t = s.trim().to_ascii_lowercase();
        if t == "median" {
            return Ok(Self::Median);
        }
        let rest = t
            .strip_prefix("sigma-clip")
            .or_else(|| t.strip_prefix("sigma"));
        if let Some(rest) = rest {
            let rest = rest.trim_start_matches([':', '=', ' ']);
            if rest.is_empty() {
                return Ok(Self::default());
            }
            if let Ok(k) = rest.parse::<f32>()
                && k.is_finite()
                && k > 0.0
            {
                return Ok(Self::SigmaClip { kappa: k });
            }
        }
        Err(Error::Invalid(format!(
            "unknown combine method {s:?}; use median, sigma or sigma:K"
        )))
    }
}

/// Settings for building a master.
#[derive(Clone, Copy, Debug)]
pub struct BuildOptions {
    /// How the frames are combined.
    pub combine: Combine,
    /// Use at most this many frames, spread evenly over the input; 0 uses
    /// as many as `memory_bytes` allows.
    pub max_frames: usize,
    /// Memory the frames may occupy while they are combined.
    pub memory_bytes: u64,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            combine: Combine::default(),
            max_frames: 0,
            memory_bytes: 3 << 30,
        }
    }
}

/// Where a master came from.
#[derive(Clone, Debug, PartialEq)]
pub struct MasterInfo {
    /// What it is.
    pub kind: MasterKind,
    /// Frames combined.
    pub frames_used: usize,
    /// Frames the input held.
    pub frames_total: usize,
    /// How they were combined; empty for a master read from a file that does
    /// not say.
    pub combine: String,
    /// Exposure of the frames in seconds, when known.
    pub exposure_s: Option<f64>,
    /// Mosaic pattern of the frames, when they are a raw colour mosaic.
    pub bayer: Option<BayerPattern>,
    /// The input, for the record: a path or a file name.
    pub source: String,
    /// FNV-1a 64 fingerprint of the pixels, to tell masters apart in a log.
    /// It is not a cryptographic hash.
    pub fingerprint: u64,
}

/// A master calibration frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Master {
    /// The pixels, as decoded from the capture (`0..=1` scale), not
    /// demosaiced. A flat is normalised to a mean of 1 within each colour.
    pub frame: Frame,
    /// Where it came from.
    pub info: MasterInfo,
}

fn count(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

fn fnv1a(data: &[f32]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for v in data {
        for b in v.to_bits().to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

fn pattern_name(p: BayerPattern) -> &'static str {
    match p {
        BayerPattern::Rggb => "RGGB",
        BayerPattern::Grbg => "GRBG",
        BayerPattern::Gbrg => "GBRG",
        BayerPattern::Bggr => "BGGR",
    }
}

impl Master {
    /// Wrap a frame as a master.
    #[must_use]
    pub fn new(kind: MasterKind, frame: Frame, mut info: MasterInfo) -> Self {
        info.kind = kind;
        info.bayer = frame.bayer.or(info.bayer);
        info.fingerprint = fnv1a(&frame.data);
        Self { frame, info }
    }

    /// One line: kind, frames, exposure, source and fingerprint.
    #[must_use]
    pub fn describe(&self) -> String {
        let i = &self.info;
        let mut s = format!("master {}", i.kind.name());
        if i.frames_used > 0 {
            let _ = write!(s, " of {} frame(s)", i.frames_used);
            if !i.combine.is_empty() {
                let _ = write!(s, ", {}", i.combine);
            }
        }
        if let Some(e) = i.exposure_s {
            let _ = write!(s, ", exposure {}", fmt_seconds(e));
        }
        if !i.source.is_empty() {
            let _ = write!(s, ", from {}", i.source);
        }
        let _ = write!(s, ", fp:{:016x}", i.fingerprint);
        s
    }

    /// Write as a 32-bit float FITS file, with the record of where it came
    /// from in the header.
    ///
    /// # Errors
    /// I/O failure.
    pub fn write(&self, path: &Path) -> Result<()> {
        let i = &self.info;
        let mut cards = vec![
            HeaderCard::string("HELIAKND", i.kind.name(), "Helia master kind"),
            HeaderCard::int("HELIAFRM", count(i.frames_used), "Frames combined"),
            HeaderCard::int("HELIATOT", count(i.frames_total), "Frames in the input"),
            HeaderCard::string(
                "HELIACMB",
                i.combine.clone(),
                "How the frames were combined",
            ),
            HeaderCard::string(
                "HELIAFP",
                format!("{:016x}", i.fingerprint),
                "FNV-1a 64 of pixels",
            ),
            HeaderCard::string("HELIASRC", ascii_tail(&i.source, 60), "Input"),
        ];
        if let Some(e) = i.exposure_s {
            cards.push(HeaderCard::float("EXPTIME", e, "[s] Exposure of one frame"));
        }
        if let Some(p) = self.frame.bayer.or(i.bayer) {
            cards.push(HeaderCard::string(
                "BAYERPAT",
                pattern_name(p),
                "Mosaic pattern",
            ));
        }
        let history = [format!("Helia {}", self.describe())];
        let mut w = BufWriter::new(File::create(path)?);
        fits::write_fits_cards(&mut w, &self.frame, FitsFormat::Float32, &cards, &history)?;
        std::io::Write::flush(&mut w)?;
        Ok(())
    }

    /// Read a master from a FITS, TIFF or PNG file. `kind` says what the user
    /// expects it to be; the file's own `HELIAKND` card must agree. A flat
    /// from another program (no `HELIAKND` card) is normalised to a mean of 1
    /// within each colour.
    ///
    /// # Errors
    /// Unreadable file, or a Helia master of another kind.
    pub fn read(path: &Path, kind: MasterKind) -> Result<Self> {
        let label = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let is_fits = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "fit" | "fits" | "fts"));
        if !is_fits {
            let (img, _) = crate::io::any::read_any(path)?;
            let crate::io::any::Loaded::Mono(frame) = img else {
                return Err(Error::Invalid(format!(
                    "{}: a colour image cannot be a master; use a single-plane FITS or grey TIFF",
                    path.display()
                )));
            };
            return Ok(Self::finish_foreign(kind, frame, label, None));
        }
        let (mut frame, map) = fits::read_fits_header(BufReader::new(File::open(path)?))?;
        let exposure = map
            .get("EXPTIME")
            .or_else(|| map.get("EXPOSURE"))
            .and_then(|v| v.parse::<f64>().ok());
        match map.get("HELIAKND").and_then(|k| MasterKind::parse(k)) {
            Some(found) => {
                if found != kind {
                    return Err(Error::Invalid(format!(
                        "{} is a master {}, not a master {}",
                        path.display(),
                        found.name(),
                        kind.name()
                    )));
                }
                let n = |k: &str| {
                    map.get(k)
                        .and_then(|v| v.parse::<usize>().ok())
                        .unwrap_or(0)
                };
                let fp = map
                    .get("HELIAFP")
                    .and_then(|v| u64::from_str_radix(v.trim(), 16).ok());
                let info = MasterInfo {
                    kind,
                    frames_used: n("HELIAFRM"),
                    frames_total: n("HELIATOT"),
                    combine: map.get("HELIACMB").cloned().unwrap_or_default(),
                    exposure_s: exposure,
                    bayer: frame.bayer,
                    source: map.get("HELIASRC").cloned().unwrap_or(label),
                    fingerprint: 0,
                };
                frame.bayer = info.bayer;
                let mut m = Self::new(kind, frame, info);
                if let Some(fp) = fp
                    && fp != m.info.fingerprint
                {
                    return Err(Error::Invalid(format!(
                        "{} does not match the fingerprint in its header: the file has been changed or damaged",
                        path.display()
                    )));
                }
                m.info.frames_total = m.info.frames_total.max(m.info.frames_used);
                Ok(m)
            }
            None => Ok(Self::finish_foreign(kind, frame, label, exposure)),
        }
    }

    fn finish_foreign(
        kind: MasterKind,
        mut frame: Frame,
        label: String,
        exposure_s: Option<f64>,
    ) -> Self {
        if kind == MasterKind::Flat {
            normalise_per_colour(&mut frame);
        }
        let info = MasterInfo {
            kind,
            frames_used: 0,
            frames_total: 0,
            combine: String::new(),
            exposure_s,
            bayer: frame.bayer,
            source: label,
            fingerprint: 0,
        };
        Self::new(kind, frame, info)
    }
}

fn fmt_seconds(s: f64) -> String {
    if s < 1.0 {
        format!("{:.4} ms", s * 1000.0)
    } else {
        format!("{s:.3} s")
    }
}

/// The last `n` characters, for a header value that must fit a card.
fn ascii_tail(s: &str, n: usize) -> String {
    let t: String = s.chars().filter(char::is_ascii).collect();
    let skip = t.chars().count().saturating_sub(n);
    t.chars().skip(skip).collect()
}

/// Divide each colour of `f` by its own mean, so that every colour averages
/// 1 and the colour balance of the sensor is not touched.
pub fn normalise_per_colour(f: &mut Frame) {
    let channels = if f.bayer.is_some() { 3 } else { 1 };
    let mut sum = [0.0f64; 3];
    let mut count = [0u64; 3];
    for y in 0..f.height {
        for x in 0..f.width {
            let c = f.bayer.map_or(0, |p| p.channel_at(x, y));
            sum[c] += f64::from(f.data[y * f.width + x]);
            count[c] += 1;
        }
    }
    let mut scale = [1.0f32; 3];
    for c in 0..channels {
        if count[c] > 0 && sum[c] > 0.0 {
            #[allow(clippy::cast_possible_truncation, reason = "a gain near 1")]
            {
                scale[c] = (count[c] as f64 / sum[c]) as f32;
            }
        }
    }
    for y in 0..f.height {
        for x in 0..f.width {
            let c = f.bayer.map_or(0, |p| p.channel_at(x, y));
            f.data[y * f.width + x] *= scale[c];
        }
    }
}

/// One file or capture of calibration frames.
enum Entry {
    Capture {
        src: Box<dyn FrameSource>,
        count: usize,
    },
    Fits(PathBuf),
    Memory(Vec<Frame>),
}

/// The frames of one or more calibration captures: SER, AVI or video files,
/// FITS stills, or folders of them.
pub struct CalInput {
    entries: Vec<Entry>,
    width: usize,
    height: usize,
    total: usize,
    /// Exposure of one frame in seconds, from a sidecar or `EXPTIME`.
    pub exposure_s: Option<f64>,
    /// Mosaic pattern of the frames.
    pub bayer: Option<BayerPattern>,
    /// What the input was, for the record.
    pub label: String,
    /// Things worth telling the user (files skipped, exposures that differ).
    pub notes: Vec<String>,
}

const CAPTURE_EXT: &[&str] = &["ser", "avi", "mp4", "mov", "m4v", "mkv", "webm"];
const FITS_EXT: &[&str] = &["fit", "fits", "fts"];

fn ext_in(p: &Path, list: &[&str]) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| list.contains(&e.to_ascii_lowercase().as_str()))
}

impl CalInput {
    /// Open `paths`: each a capture file, a FITS file, or a folder whose
    /// capture and FITS files are all used (not its sub-folders), in name order.
    ///
    /// # Errors
    /// Nothing readable, files of different sizes, or a read failure.
    #[allow(
        clippy::too_many_lines,
        reason = "one pass over the files, then the checks"
    )]
    pub fn open(paths: &[PathBuf], opts: &OpenOptions) -> Result<Self> {
        let mut files: Vec<PathBuf> = Vec::new();
        let mut notes = Vec::new();
        for p in paths {
            if p.is_dir() {
                let mut inside: Vec<PathBuf> = std::fs::read_dir(p)?
                    .filter_map(std::result::Result::ok)
                    .map(|e| e.path())
                    .filter(|f| f.is_file() && (ext_in(f, CAPTURE_EXT) || ext_in(f, FITS_EXT)))
                    .collect();
                inside.sort();
                if inside.is_empty() {
                    notes.push(format!("{} holds no capture or FITS files", p.display()));
                }
                files.extend(inside);
            } else {
                files.push(p.clone());
            }
        }
        if files.is_empty() {
            return Err(Error::Invalid("no calibration frames were given".into()));
        }
        let mut entries = Vec::new();
        let (mut width, mut height, mut total) = (0usize, 0usize, 0usize);
        let mut bayer = None;
        let mut exposures: Vec<f64> = Vec::new();
        for f in &files {
            let (w, h, count, b, exp, entry) = if ext_in(f, FITS_EXT) {
                let (frame, map) = fits::read_fits_header(BufReader::new(File::open(f)?))
                    .map_err(|e| Error::Invalid(format!("{}: {e}", f.display())))?;
                let exp = map
                    .get("EXPTIME")
                    .or_else(|| map.get("EXPOSURE"))
                    .and_then(|v| v.parse::<f64>().ok());
                (
                    frame.width,
                    frame.height,
                    1,
                    frame.bayer,
                    exp,
                    Entry::Fits(f.clone()),
                )
            } else {
                let mut src = open_capture(f, opts)
                    .map_err(|e| Error::Invalid(format!("{}: {e}", f.display())))?;
                let (w, h) = src.dimensions();
                let count = src.frame_count();
                let b = src.read(0).ok().and_then(|fr| fr.bayer);
                let exp = CaptureInfo::read(f, None).exposure_s;
                (w, h, count, b, exp, Entry::Capture { src, count })
            };
            if total > 0 && (w, h) != (width, height) {
                return Err(Error::Invalid(format!(
                    "{} is {w}x{h} but the first file is {width}x{height}",
                    f.display()
                )));
            }
            if total > 0 && b != bayer {
                notes.push(format!(
                    "{} has a different mosaic pattern from the first file",
                    f.display()
                ));
            }
            (width, height) = (w, h);
            if total == 0 {
                bayer = b;
            }
            if let Some(e) = exp {
                exposures.push(e);
            }
            total += count;
            entries.push(entry);
        }
        if total == 0 {
            return Err(Error::Invalid("the calibration input has no frames".into()));
        }
        exposures.sort_by(f64::total_cmp);
        exposures.dedup_by(|a, b| (*a - *b).abs() <= 1e-6 * b.abs().max(1e-9));
        if exposures.len() > 1 {
            notes.push(format!(
                "the files have {} different exposures ({} to {}); a master is only right for lights of one exposure",
                exposures.len(),
                fmt_seconds(exposures[0]),
                fmt_seconds(exposures[exposures.len() - 1])
            ));
        }
        let label = match paths {
            [one] => one.file_name().map_or_else(
                || one.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            ),
            _ => format!("{} inputs", paths.len()),
        };
        Ok(Self {
            entries,
            width,
            height,
            total,
            exposure_s: exposures.first().copied(),
            bayer,
            label,
            notes,
        })
    }

    /// Frames already in memory (for tests and demonstrations).
    ///
    /// # Errors
    /// No frames, or frames of different sizes.
    pub fn from_frames(frames: Vec<Frame>, exposure_s: Option<f64>, label: &str) -> Result<Self> {
        let Some(first) = frames.first() else {
            return Err(Error::Invalid("no calibration frames were given".into()));
        };
        let (width, height, bayer) = (first.width, first.height, first.bayer);
        if frames
            .iter()
            .any(|f| (f.width, f.height) != (width, height))
        {
            return Err(Error::Invalid("the frames differ in size".into()));
        }
        Ok(Self {
            width,
            height,
            total: frames.len(),
            entries: vec![Entry::Memory(frames)],
            exposure_s,
            bayer,
            label: label.to_owned(),
            notes: Vec::new(),
        })
    }

    /// Frame size.
    #[must_use]
    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// Frames in all the files.
    #[must_use]
    pub fn frame_count(&self) -> usize {
        self.total
    }

    /// Read frame `index` of the whole input, undemosaiced.
    ///
    /// # Errors
    /// Out-of-range index or read failure.
    pub fn read(&mut self, index: usize) -> Result<Frame> {
        let mut rest = index;
        for e in &mut self.entries {
            match e {
                Entry::Fits(p) => {
                    if rest == 0 {
                        return Ok(fits::read_fits(BufReader::new(File::open(&*p)?))?);
                    }
                    rest -= 1;
                }
                Entry::Memory(frames) => {
                    if let Some(f) = frames.get(rest) {
                        return Ok(f.clone());
                    }
                    rest -= frames.len();
                }
                Entry::Capture { src, count } => {
                    if rest < *count {
                        return src.read(rest);
                    }
                    rest -= *count;
                }
            }
        }
        Err(Error::Invalid(format!(
            "calibration frame {index} out of range ({})",
            self.total
        )))
    }
}

/// How many of `total` frames to use, and which, spread evenly.
fn choose(total: usize, w: usize, h: usize, opts: &BuildOptions) -> Vec<usize> {
    let per = (w * h * 4) as u64;
    let by_memory = usize::try_from(opts.memory_bytes / per.max(1))
        .unwrap_or(usize::MAX)
        .max(1);
    let mut n = total.min(by_memory);
    if opts.max_frames > 0 {
        n = n.min(opts.max_frames);
    }
    (0..n).map(|i| i * total / n).collect()
}

/// Per-pixel combination of `frames` (each `w * h`), in parallel over bands of
/// pixels.
fn combine_frames(frames: &[Vec<f32>], method: Combine, cancel: &AtomicBool) -> Result<Vec<f32>> {
    let n = frames.len();
    let pixels = frames.first().map_or(0, Vec::len);
    let mut out = vec![0.0f32; pixels];
    out.par_chunks_mut(2048).enumerate().try_for_each_init(
        || Vec::with_capacity(n),
        |buf: &mut Vec<f32>, (c, chunk)| {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let base = c * 2048;
            for (j, o) in chunk.iter_mut().enumerate() {
                buf.clear();
                buf.extend(frames.iter().map(|f| f[base + j]));
                *o = combine_values(buf, method);
            }
            Ok(())
        },
    )?;
    Ok(out)
}

/// Combine one pixel's values; `v` is reordered.
#[must_use]
pub fn combine_values(v: &mut [f32], method: Combine) -> f32 {
    let n = v.len();
    if n == 0 {
        return 0.0;
    }
    if n < 3 {
        return v.iter().sum::<f32>() / n as f32;
    }
    let med = median(v);
    match method {
        Combine::Median => med,
        Combine::SigmaClip { kappa } => {
            let mut centre = med;
            let mut mad: Vec<f32> = v.iter().map(|&x| (x - med).abs()).collect();
            let mut sigma = 1.4826 * median(&mut mad);
            let mut keep: Vec<f32> = v.to_vec();
            for _ in 0..5 {
                let lim = kappa * sigma;
                let kept: Vec<f32> = v
                    .iter()
                    .copied()
                    .filter(|&x| (x - centre).abs() <= lim)
                    .collect();
                if kept.len() < 2 || kept.len() == keep.len() {
                    if kept.len() >= 2 {
                        keep = kept;
                    }
                    break;
                }
                let m = kept.iter().sum::<f32>() / kept.len() as f32;
                let var = kept.iter().map(|&x| (x - m) * (x - m)).sum::<f32>() / kept.len() as f32;
                centre = m;
                sigma = var.sqrt();
                keep = kept;
            }
            keep.iter().sum::<f32>() / keep.len() as f32
        }
    }
}

/// Median; reorders `v`. The mean of the two middle values for an even count.
#[must_use]
pub fn median(v: &mut [f32]) -> f32 {
    let n = v.len();
    if n == 0 {
        return 0.0;
    }
    let mid = n / 2;
    let (lo, m, _) = v.select_nth_unstable_by(mid, f32::total_cmp);
    if n % 2 == 1 {
        *m
    } else {
        let lower = lo.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        f32::midpoint(lower, *m)
    }
}

/// Progress of a build: `0..=1`.
pub type Progress<'a> = &'a dyn Fn(f32);

pub(crate) fn load(
    input: &mut CalInput,
    opts: &BuildOptions,
    progress: Progress<'_>,
    cancel: &AtomicBool,
    mut each: impl FnMut(&mut Vec<f32>),
) -> Result<(Vec<Vec<f32>>, usize)> {
    let (w, h) = input.size();
    let pick = choose(input.frame_count(), w, h, opts);
    let mut frames = Vec::with_capacity(pick.len());
    for (k, &i) in pick.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let f = input.read(i)?;
        if (f.width, f.height) != (w, h) {
            return Err(Error::Invalid(format!("frame {i} differs in size")));
        }
        let mut d = f.data;
        each(&mut d);
        frames.push(d);
        progress(0.8 * (k + 1) as f32 / pick.len() as f32);
    }
    Ok((frames, pick.len()))
}

/// Build a master dark or bias from `input`.
///
/// # Errors
/// Read failure or cancellation.
pub fn build_dark(
    kind: MasterKind,
    input: &mut CalInput,
    opts: &BuildOptions,
    progress: Progress<'_>,
    cancel: &AtomicBool,
) -> Result<Master> {
    debug_assert_ne!(kind, MasterKind::Flat);
    let (w, h) = input.size();
    let (frames, used) = load(input, opts, progress, cancel, |_| {})?;
    let data = combine_frames(&frames, opts.combine, cancel)?;
    progress(1.0);
    let frame = Frame {
        width: w,
        height: h,
        data,
        bayer: input.bayer,
    };
    Ok(Master::new(
        kind,
        frame,
        info(kind, input, used, opts.combine),
    ))
}

fn info(kind: MasterKind, input: &CalInput, used: usize, combine: Combine) -> MasterInfo {
    MasterInfo {
        kind,
        frames_used: used,
        frames_total: input.frame_count(),
        combine: combine.label(),
        exposure_s: input.exposure_s,
        bayer: input.bayer,
        source: input.label.clone(),
        fingerprint: 0,
    }
}

/// What a flat build found.
#[derive(Clone, Debug)]
pub struct FlatBuild {
    /// The normalised master flat.
    pub master: Master,
    /// Warnings for the user: Newton rings, etalon fringes, strong vignetting,
    /// a dark that does not match.
    pub warnings: Vec<String>,
    /// Numbers behind the warnings.
    pub diagnostics: diag::FlatDiagnostics,
}

/// Build a master flat from `input`. Each frame has `dark` (a master dark
/// taken at the flats' exposure, the "dark flat", or else a master bias) taken
/// off, is scaled to a mean of 1, and the frames are combined. The result is
/// normalised to a mean of 1 within each colour of the mosaic.
///
/// # Errors
/// Read failure, a `dark` of another size, or cancellation.
pub fn build_flat(
    input: &mut CalInput,
    dark: Option<&Master>,
    opts: &BuildOptions,
    progress: Progress<'_>,
    cancel: &AtomicBool,
) -> Result<FlatBuild> {
    let (w, h) = input.size();
    let mut warnings = Vec::new();
    if let Some(d) = dark {
        if (d.frame.width, d.frame.height) != (w, h) {
            return Err(Error::Invalid(format!(
                "the dark for the flats is {}x{} but the flats are {w}x{h}",
                d.frame.width, d.frame.height
            )));
        }
        if let (Some(a), Some(b)) = (d.info.exposure_s, input.exposure_s)
            && (a - b).abs() > 0.02 * b.max(a)
        {
            warnings.push(format!(
                "the dark for the flats was exposed for {} but the flats for {}: use a dark flat of the same exposure",
                fmt_seconds(a),
                fmt_seconds(b)
            ));
        }
    } else {
        warnings.push(
            "no dark flat given: the flats keep their offset, which makes the correction too strong in the dim parts. Capture darks at the exposure of the flats"
                .to_owned(),
        );
    }
    let bayer = input.bayer;
    let (frames, used) = load(input, opts, progress, cancel, |d| {
        if let Some(dk) = dark {
            for (v, &o) in d.iter_mut().zip(&dk.frame.data) {
                *v -= o;
            }
        }
        // Scale each frame to a mean of 1 per colour first, so a lamp or sky
        // that drifts while the flats are taken does not become a pattern.
        let mut f = Frame {
            width: w,
            height: h,
            data: std::mem::take(d),
            bayer,
        };
        normalise_per_colour(&mut f);
        *d = f.data;
    })?;
    let data = combine_frames(&frames, opts.combine, cancel)?;
    drop(frames);
    let mut frame = Frame {
        width: w,
        height: h,
        data,
        bayer,
    };
    normalise_per_colour(&mut frame);
    progress(1.0);
    let diagnostics = diag::analyse(&frame);
    warnings.extend(diagnostics.warnings.iter().cloned());
    let master = Master::new(
        MasterKind::Flat,
        frame,
        info(MasterKind::Flat, input, used, opts.combine),
    );
    Ok(FlatBuild {
        master,
        warnings,
        diagnostics,
    })
}
