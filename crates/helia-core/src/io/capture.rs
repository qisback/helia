// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Opening a capture and saying how it was understood.
//!
//! A capture's header is not always right. [`open_reported`] opens a file with
//! the user's [`OpenOptions`], checks the header against the data where that is
//! cheap (see [`super::detect`]), and returns a [`CaptureReport`] next to the
//! frame source: every header field as written, the values Helia will actually
//! use, where each came from ([`Origin`]) and the notices a person should read.
//! The command line, the GUI and the API all print this one report, so they
//! cannot disagree.
//!
//! Precedence for the Bayer pattern of a SER file: the user's choice, then a
//! Bayer `ColorID` in the header, then (only when the header says mono) what
//! the data shows, then the `SharpCap` sidecar when the data cannot say.

use std::fmt::Write as _;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use crate::debayer::Channel;
use crate::error::{Error, Result};
use crate::frame::{BayerPattern, Frame};
use crate::io::avi::{AviError, AviReader, PixelFormat};
use crate::io::detect::{self, Detection, Hints, Outcome, RbBasis};
use crate::io::ffmpeg::{self, VideoOptions};
use crate::io::raw::{FileFrames, Sample};
use crate::io::ser::{ColorId, SerOverrides, SerReader, ticks_to_text};
use crate::io::sidecar::CameraSettings;
use crate::io::source::{AviSource, BayerSetting, Endian, FrameSource, OpenOptions, SerSource};
use crate::recommend::capture_texts;

/// Where a value Helia uses came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The file's own header (or its container format).
    Header,
    /// A `SharpCap` `*.CameraSettings.txt` next to the file.
    Sidecar,
    /// Measured from the pixels.
    Detection,
    /// Chosen by the user.
    Override,
}

impl Origin {
    /// One word for tables and logs.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Header => "header",
            Self::Sidecar => "sidecar",
            Self::Detection => "detection",
            Self::Override => "override",
        }
    }
}

/// A value with its [`Origin`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sourced<T> {
    /// The value in force.
    pub value: T,
    /// Where it came from.
    pub origin: Origin,
}

/// How serious a [`Notice`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// Worth knowing.
    Info,
    /// The file disagrees with itself or with the user; Helia has chosen.
    Warning,
}

/// Something a person should read about how the capture was understood.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    /// Severity.
    pub level: Level,
    /// One short line, for a toast or a status bar.
    pub summary: String,
    /// The full explanation, in plain words.
    pub text: String,
}

/// Trailer timestamps of a SER file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StampFacts {
    /// Timestamps present.
    pub count: usize,
    /// First, decoded (UTC).
    pub first: Option<String>,
    /// Last, decoded (UTC).
    pub last: Option<String>,
}

/// A SER header as written in the file, decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SerFacts {
    /// `FileID`.
    pub file_id: String,
    /// `LuID`.
    pub lu_id: i32,
    /// `ColorID` as stored.
    pub colour_id: i32,
    /// What that `ColorID` means.
    pub colour_meaning: &'static str,
    /// `LittleEndian` flag as stored.
    pub little_endian_flag: bool,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Bits per sample.
    pub pixel_depth: u32,
    /// Frames the header claims.
    pub frame_count: u32,
    /// Observer.
    pub observer: String,
    /// Instrument.
    pub instrument: String,
    /// Telescope.
    pub telescope: String,
    /// `DateTime` (local), decoded; `None` when unset.
    pub date_time: Option<String>,
    /// `DateTimeUTC`, decoded; `None` when unset.
    pub date_time_utc: Option<String>,
    /// Trailer timestamps, when the file has them.
    pub stamps: Option<StampFacts>,
}

/// An AVI container's own description.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AviFacts {
    /// `FourCC` of the video stream.
    pub fourcc: String,
    /// Pixel format name.
    pub format: String,
    /// Frames are stored bottom row first.
    pub bottom_up: bool,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Frames.
    pub frames: usize,
}

/// The `SharpCap` sidecar, as far as Helia reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidecarFacts {
    /// Where it was found.
    pub path: PathBuf,
    /// Camera model (first section heading).
    pub camera: Option<String>,
    /// `Colour Space`.
    pub colour_space: Option<String>,
    /// `Debayer Preview`.
    pub debayer_preview: Option<String>,
    /// Pattern the sidecar implies.
    pub bayer_hint: Option<BayerPattern>,
}

/// The values Helia decodes the capture with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Effective {
    /// Bayer pattern of the decoded frames; `None` is mono (or colour already).
    pub bayer: Sourced<Option<BayerPattern>>,
    /// Bits per sample.
    pub bit_depth: Sourced<u32>,
    /// Byte order of 16-bit samples.
    pub endian: Sourced<Endian>,
    /// Rows are read bottom to top.
    pub flip_rows: Sourced<bool>,
}

/// Everything known about how a capture was opened.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptureReport {
    /// File opened.
    pub path: PathBuf,
    /// `"SER"` or `"AVI"`.
    pub container: &'static str,
    /// SER header as written, for a SER file.
    pub ser: Option<SerFacts>,
    /// AVI description, for an AVI file.
    pub avi: Option<AviFacts>,
    /// The sidecar, when there is one.
    pub sidecar: Option<SidecarFacts>,
    /// Result of the mosaic test, when it was run.
    pub detection: Option<Detection>,
    /// What Helia uses.
    pub effective: Effective,
    /// What the user overrode, one phrase each.
    pub overrides: Vec<String>,
    /// What to tell the user.
    pub notices: Vec<Notice>,
}

/// Name of a pattern, upper case.
#[must_use]
pub fn pattern_name(p: BayerPattern) -> &'static str {
    match p {
        BayerPattern::Rggb => "RGGB",
        BayerPattern::Grbg => "GRBG",
        BayerPattern::Gbrg => "GBRG",
        BayerPattern::Bggr => "BGGR",
    }
}

fn bayer_text(p: Option<BayerPattern>) -> String {
    p.map_or_else(|| "none (mono)".into(), |p| pattern_name(p).into())
}

fn percent(v: f64) -> String {
    format!("{:.1}%", v * 100.0)
}

fn spread_text(d: &Detection) -> String {
    format!("{:.0}%", d.spread * 100.0)
}

impl CaptureReport {
    /// The file as written, one `(label, value)` row per field.
    #[must_use]
    pub fn header_rows(&self) -> Vec<(String, String)> {
        let row = |k: &str, v: String| (k.to_string(), v);
        let text = |s: &str| {
            if s.is_empty() {
                "(empty)".into()
            } else {
                s.to_string()
            }
        };
        let mut out = Vec::new();
        if let Some(h) = &self.ser {
            out.push(row("FileID", h.file_id.clone()));
            out.push(row("LuID", h.lu_id.to_string()));
            out.push(row(
                "ColorID",
                format!("{} ({})", h.colour_id, h.colour_meaning),
            ));
            out.push(row(
                "LittleEndian",
                format!(
                    "{} ({})",
                    i32::from(h.little_endian_flag),
                    if h.little_endian_flag {
                        "little-endian"
                    } else {
                        "big-endian"
                    }
                ),
            ));
            out.push(row("Width", format!("{} px", h.width)));
            out.push(row("Height", format!("{} px", h.height)));
            out.push(row("PixelDepth", format!("{} bit", h.pixel_depth)));
            out.push(row("FrameCount", h.frame_count.to_string()));
            out.push(row("Observer", text(&h.observer)));
            out.push(row("Instrument", text(&h.instrument)));
            out.push(row("Telescope", text(&h.telescope)));
            out.push(row(
                "DateTime (local)",
                h.date_time.clone().unwrap_or_else(|| "not set".into()),
            ));
            out.push(row(
                "DateTimeUTC",
                h.date_time_utc.clone().unwrap_or_else(|| "not set".into()),
            ));
            match &h.stamps {
                None => out.push(row("Timestamps", "absent".into())),
                Some(s) => {
                    out.push(row("Timestamps", format!("present, {} (UTC)", s.count)));
                    out.push(row(
                        "First stamp",
                        s.first.clone().unwrap_or_else(|| "not set".into()),
                    ));
                    out.push(row(
                        "Last stamp",
                        s.last.clone().unwrap_or_else(|| "not set".into()),
                    ));
                }
            }
        }
        if let Some(a) = &self.avi {
            out.push(row("FourCC", a.fourcc.clone()));
            out.push(row("Format", a.format.clone()));
            out.push(row("Width", format!("{} px", a.width)));
            out.push(row("Height", format!("{} px", a.height)));
            out.push(row(
                "Row order",
                if a.bottom_up { "bottom-up" } else { "top-down" }.into(),
            ));
            out.push(row("Frames", a.frames.to_string()));
        }
        if let Some(s) = &self.sidecar {
            out.push(row("Sidecar", s.path.display().to_string()));
            if let Some(c) = &s.camera {
                out.push(row("Sidecar camera", c.clone()));
            }
            if let Some(c) = &s.colour_space {
                out.push(row("Colour Space", c.clone()));
            }
            if let Some(c) = &s.debayer_preview {
                out.push(row("Debayer Preview", c.clone()));
            }
        }
        out
    }

    /// What Helia uses: `(label, value, origin)`.
    #[must_use]
    pub fn effective_rows(&self) -> Vec<(&'static str, String, Origin)> {
        let e = &self.effective;
        let mut rows = vec![("Bayer pattern", bayer_text(e.bayer.value), e.bayer.origin)];
        if self.container == "SER" {
            rows.push((
                "Bit depth",
                format!("{} bit", e.bit_depth.value),
                e.bit_depth.origin,
            ));
            rows.push((
                "Byte order",
                if e.bit_depth.value <= 8 {
                    "not used (8-bit)".to_string()
                } else {
                    e.endian.value.label().to_string()
                },
                e.endian.origin,
            ));
        }
        rows.push((
            "Row order",
            if e.flip_rows.value {
                "bottom to top"
            } else {
                "top to bottom"
            }
            .to_string(),
            e.flip_rows.origin,
        ));
        rows
    }

    /// The Bayer pattern frames are decoded with.
    #[must_use]
    pub fn bayer(&self) -> Option<BayerPattern> {
        self.effective.bayer.value
    }

    /// Lines to record in an output file about how the capture was read.
    #[must_use]
    pub fn note_lines(&self) -> Vec<String> {
        let e = &self.effective;
        let mut line = format!(
            "Capture read as: Bayer {} ({})",
            bayer_text(e.bayer.value),
            e.bayer.origin.label()
        );
        if self.container == "SER" {
            let _ = write!(
                line,
                ", {}-bit ({})",
                e.bit_depth.value,
                e.bit_depth.origin.label()
            );
            if e.bit_depth.value > 8 {
                let _ = write!(
                    line,
                    ", {} ({})",
                    e.endian.value.label(),
                    e.endian.origin.label()
                );
            }
        }
        if e.flip_rows.value {
            let _ = write!(line, ", rows flipped ({})", e.flip_rows.origin.label());
        }
        let mut out = vec![line];
        if let Some(h) = &self.ser {
            out.push(format!(
                "SER header: ColorID {} ({}), {}-bit, {}",
                h.colour_id,
                h.colour_meaning,
                h.pixel_depth,
                if h.little_endian_flag {
                    "little-endian"
                } else {
                    "big-endian"
                }
            ));
        }
        if let Some(d) = self
            .detection
            .filter(|d| matches!(d.outcome, Outcome::Mosaic { .. }))
        {
            out.push(format!(
                "Bayer detection: 2x2 position means {} of full scale, spread {}, {} frames",
                d.means.map(percent).join(" / "),
                spread_text(&d),
                d.frames_used
            ));
        }
        if !self.overrides.is_empty() {
            out.push(format!("Overrides: {}", self.overrides.join(", ")));
        }
        out
    }
}

/// True when a camera model name looks like a colour sensor (ZWO `MC`, QHY
/// `...C`, `-C`).
#[must_use]
pub fn model_is_colour(name: &str) -> bool {
    let up = name.to_ascii_uppercase();
    let up = up.trim();
    (up.contains("ASI") && up.contains("MC"))
        || (up.contains("QHY") && up.ends_with('C'))
        || up.ends_with("-C")
}

/// The layout colour cameras of this model normally have. ZWO colour cameras
/// are RGGB; others say nothing.
fn model_default(name: &str) -> Option<BayerPattern> {
    let up = name.to_ascii_uppercase();
    (up.contains("ASI") && up.contains("MC")).then_some(BayerPattern::Rggb)
}

/// Colour the filter in use should pass, from the file name, folder, sidecar
/// and header texts.
fn filter_channel(path: &Path) -> Option<Channel> {
    crate::colour::detect_filter(&capture_texts(path))
        .map(|d| d.preset.info().channel)
        .filter(|c| matches!(c, Channel::Red | Channel::Blue))
}

fn sidecar_facts(path: &Path, s: &CameraSettings) -> SidecarFacts {
    SidecarFacts {
        path: CameraSettings::sidecar_path(path),
        camera: s.sections.first().cloned(),
        colour_space: s.get("Colour Space").map(str::to_owned),
        debayer_preview: s.get("Debayer Preview").map(str::to_owned),
        bayer_hint: s.bayer_hint(),
    }
}

fn warn(summary: String, text: String) -> Notice {
    Notice {
        level: Level::Warning,
        summary,
        text,
    }
}

fn note(text: String) -> Notice {
    Notice {
        level: Level::Info,
        summary: text.clone(),
        text,
    }
}

/// Words for the measured 2x2 means.
fn measurement(d: &Detection) -> String {
    format!(
        "the four pixel positions of a 2x2 cell (top-left, top-right, bottom-left, \
         bottom-right) average {} of full scale, a spread of {} (measured on the disc in {} \
         frame{})",
        d.means.map(percent).join(", "),
        spread_text(d),
        d.frames_used,
        if d.frames_used == 1 { "" } else { "s" }
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "one flat list of cases, easiest read in order"
)]
fn ser_notices(
    header: &SerFacts,
    opts: &OpenOptions,
    effective: &Effective,
    detection: Option<&Detection>,
    sidecar: Option<&SidecarFacts>,
) -> Vec<Notice> {
    let mut out = Vec::new();
    let colour = format!("ColorID {}", header.colour_id);
    let used = effective.bayer;
    match (opts.bayer, used.origin) {
        (BayerSetting::Auto, Origin::Detection) => {
            let d = detection;
            let how = if let Some(Outcome::Mosaic {
                basis: RbBasis::Brightness,
                ..
            }) = d.map(|d| d.outcome)
            {
                " Red and blue cannot be told apart from the pixels alone, so the dimmer of \
                 the two is taken to be blue; if the colours look swapped, choose the other \
                 pattern."
            } else {
                ""
            };
            out.push(warn(
                format!(
                    "The header says mono, but the data is a Bayer mosaic: debayering as {}",
                    bayer_text(used.value)
                ),
                format!(
                    "The SER header says mono ({colour}), but the frames hold a Bayer mosaic that \
                     looks like {pattern}: {}. Helia will debayer them as {pattern}.{how} To \
                     treat the file as mono, set the Bayer pattern to none; to use a different \
                     pattern, choose it yourself.",
                    d.map_or_else(String::new, measurement),
                    pattern = bayer_text(used.value),
                ),
            ));
        }
        (BayerSetting::Auto, Origin::Sidecar) => {
            let why = detection.and_then(|d| match d.outcome {
                Outcome::Inconclusive(r) => Some(r),
                _ => None,
            });
            out.push(warn(
                format!(
                    "The header says mono, the SharpCap sidecar says raw colour: debayering as {}",
                    bayer_text(used.value)
                ),
                format!(
                    "The SER header says mono ({colour}), but the SharpCap sidecar says the \
                     camera delivered raw colour ({}), and the pixels could not settle it{}. \
                     Helia will debayer as {}. If the file really is mono, set the Bayer \
                     pattern to none.",
                    sidecar
                        .and_then(|s| s.colour_space.clone())
                        .unwrap_or_default(),
                    why.map_or_else(String::new, |r| format!(" ({r})")),
                    bayer_text(used.value),
                ),
            ));
        }
        (BayerSetting::Auto, Origin::Header) if used.value.is_none() => {
            if let Some(d) = detection {
                let model = [
                    sidecar.and_then(|s| s.camera.as_deref()),
                    Some(header.instrument.as_str()),
                ]
                .into_iter()
                .flatten()
                .find(|m| model_is_colour(m));
                match (d.outcome, model) {
                    (Outcome::NoMosaic, Some(m)) => out.push(note(format!(
                        "The instrument name ({m}) suggests a colour camera, but the frames show \
                         no Bayer mosaic ({}), so they are treated as mono as the header says.",
                        measurement(d)
                    ))),
                    (Outcome::Inconclusive(r), Some(m)) => out.push(note(format!(
                        "The instrument name ({m}) suggests a colour camera, but the pixels \
                         could not confirm a mosaic ({r}); the file is treated as mono as the \
                         header says. If the stack shows a grid, set the Bayer pattern."
                    ))),
                    _ => {}
                }
            }
        }
        _ => {}
    }
    if opts.bayer != BayerSetting::Auto {
        let was = ColorId::from_id(header.colour_id).map_or_else(
            || colour.clone(),
            |c| match c {
                ColorId::Bayer(p) => format!("Bayer {}", pattern_name(p)),
                ColorId::Mono => "mono".into(),
                _ => "colour".into(),
            },
        );
        out.push(note(format!(
            "Bayer pattern set by you to {}; the header says {was}. The pixels are not checked.",
            bayer_text(used.value)
        )));
    }
    if let Some(d) = opts.bit_depth {
        out.push(note(format!(
            "Bit depth set by you to {d}; the header says {}.",
            header.pixel_depth
        )));
    }
    if let Some(e) = opts.endian {
        out.push(note(format!(
            "Byte order set by you to {}; the header flag says {}.",
            e.label(),
            if header.little_endian_flag {
                "little-endian"
            } else {
                "big-endian"
            }
        )));
    }
    if opts.flip_rows {
        out.push(note(
            "Rows are read bottom to top because you asked for it.".into(),
        ));
    }
    if effective.endian.origin == Origin::Detection {
        out.push(note(format!(
            "The header flag says {}, but the pixel data only makes sense as {}; Helia reads it \
             that way.",
            if header.little_endian_flag {
                "little-endian"
            } else {
                "big-endian"
            },
            effective.endian.value.label()
        )));
    }
    out
}

fn override_phrases(opts: &OpenOptions) -> Vec<String> {
    let mut v = Vec::new();
    match opts.bayer {
        BayerSetting::Auto => {}
        BayerSetting::Mono => v.push("--bayer none".to_string()),
        BayerSetting::Pattern(p) => {
            v.push(format!("--bayer {}", pattern_name(p).to_ascii_lowercase()));
        }
    }
    if let Some(d) = opts.bit_depth {
        v.push(format!("--bit-depth {d}"));
    }
    if let Some(e) = opts.endian {
        v.push(format!(
            "--endian {}",
            match e {
                Endian::Little => "little",
                Endian::Big => "big",
            }
        ));
    }
    if opts.flip_rows {
        v.push("--flip-rows".into());
    }
    v
}

fn stamp_facts(r: &mut SerReader<BufReader<File>>) -> Option<StampFacts> {
    let t = r.timestamps().ok().flatten()?;
    Some(StampFacts {
        count: t.len(),
        first: t.first().and_then(|&x| ticks_to_text(x)),
        last: t.last().and_then(|&x| ticks_to_text(x)),
    })
}

/// Choose the Bayer pattern of a SER file; see the module notes for the order.
fn ser_bayer(
    reader: &mut SerReader<BufReader<File>>,
    path: &Path,
    opts: &OpenOptions,
    sidecar: Option<&CameraSettings>,
    instrument: &str,
) -> (Option<BayerPattern>, Origin, Option<Detection>) {
    let header_pattern = reader.bayer_pattern();
    match opts.bayer {
        BayerSetting::Pattern(p) => (Some(p), Origin::Override, None),
        BayerSetting::Mono => (None, Origin::Override, None),
        BayerSetting::Auto if header_pattern.is_some() => (header_pattern, Origin::Header, None),
        BayerSetting::Auto if reader.header().color != ColorId::Mono => {
            (None, Origin::Header, None)
        }
        BayerSetting::Auto => {
            let model = sidecar
                .and_then(|s| s.sections.first().cloned())
                .into_iter()
                .chain([instrument.to_string()])
                .find(|m| model_is_colour(m));
            let hints = Hints {
                stated: sidecar.and_then(CameraSettings::forced_pattern),
                model_default: model.as_deref().and_then(model_default),
                bright: filter_channel(path),
            };
            let frames: Vec<Frame> = detect::sample_indices(reader.frame_count())
                .into_iter()
                .filter_map(|i| reader.read_frame(i).ok())
                .collect();
            let d = detect::detect(&frames, hints);
            let (pattern, origin) = match d.outcome {
                Outcome::Mosaic { pattern, .. } => (Some(pattern), Origin::Detection),
                Outcome::NoMosaic => (None, Origin::Header),
                Outcome::Inconclusive(_) => match sidecar.and_then(CameraSettings::bayer_hint) {
                    Some(p) => (Some(p), Origin::Sidecar),
                    None => (None, Origin::Header),
                },
            };
            (pattern, origin, Some(d))
        }
    }
}

fn open_ser(path: &Path, opts: &OpenOptions) -> Result<(Box<dyn FrameSource>, CaptureReport)> {
    let file = BufReader::with_capacity(1 << 20, File::open(path)?);
    let mut reader = SerReader::with_overrides(
        file,
        SerOverrides {
            bit_depth: opts.bit_depth,
            little_endian: opts.endian.map(|e| e == Endian::Little),
            flip_rows: opts.flip_rows,
        },
    )?;
    let h = reader.declared_header().clone();
    let sidecar = CameraSettings::for_capture(path);
    let sidecar_facts = sidecar.as_ref().map(|s| sidecar_facts(path, s));
    let (pattern, origin, detection) =
        ser_bayer(&mut reader, path, opts, sidecar.as_ref(), &h.instrument);

    let data_little = reader.data_is_little_endian();
    let effective = Effective {
        bayer: Sourced {
            value: pattern,
            origin,
        },
        bit_depth: Sourced {
            value: reader.header().pixel_depth,
            origin: if opts.bit_depth.is_some() {
                Origin::Override
            } else {
                Origin::Header
            },
        },
        endian: Sourced {
            value: if data_little {
                Endian::Little
            } else {
                Endian::Big
            },
            origin: if opts.endian.is_some() {
                Origin::Override
            } else if data_little != h.little_endian {
                Origin::Detection
            } else {
                Origin::Header
            },
        },
        flip_rows: Sourced {
            value: opts.flip_rows,
            origin: if opts.flip_rows {
                Origin::Override
            } else {
                Origin::Header
            },
        },
    };
    let facts = SerFacts {
        file_id: h.file_id.clone(),
        lu_id: h.lu_id,
        colour_id: h.color.to_raw(),
        colour_meaning: h.color.meaning(),
        little_endian_flag: h.little_endian,
        width: h.width,
        height: h.height,
        pixel_depth: h.pixel_depth,
        frame_count: h.frame_count,
        observer: h.observer.clone(),
        instrument: h.instrument.clone(),
        telescope: h.telescope.clone(),
        date_time: ticks_to_text(h.date_time),
        date_time_utc: ticks_to_text(h.date_time_utc),
        stamps: stamp_facts(&mut reader),
    };
    let notices = ser_notices(
        &facts,
        opts,
        &effective,
        detection.as_ref(),
        sidecar_facts.as_ref(),
    );
    let shared = FileFrames::new(
        File::open(path)?,
        reader.row_layout(),
        reader.frame_offsets(),
        pattern,
    );
    let report = CaptureReport {
        path: path.to_path_buf(),
        container: "SER",
        ser: Some(facts),
        avi: None,
        sidecar: sidecar_facts,
        detection,
        effective,
        overrides: override_phrases(opts),
        notices,
    };
    Ok((
        Box::new(SerSource::new(reader, pattern).with_shared(shared)),
        report,
    ))
}

/// Mean level (0 to 1 of full scale) of the disc below which an AVI with no
/// sidecar is not checked for a mosaic.
const AVI_MIN_DISC_LEVEL: f64 = 0.05;

/// Choose the Bayer pattern of an AVI: the user's choice, then the sidecar,
/// then (for a gray AVI) what the pixels show.
fn avi_bayer(
    reader: &mut AviReader<BufReader<File>>,
    path: &Path,
    opts: &OpenOptions,
    sidecar: Option<&CameraSettings>,
    gray: bool,
) -> (Option<BayerPattern>, Origin, Option<Detection>) {
    match opts.bayer {
        BayerSetting::Pattern(p) => (Some(p), Origin::Override, None),
        BayerSetting::Mono => (None, Origin::Override, None),
        BayerSetting::Auto => match sidecar.and_then(CameraSettings::bayer_hint) {
            Some(p) => (Some(p), Origin::Sidecar, None),
            // An AVI never says whether it holds raw colour. Without a sidecar
            // (a capture copied on its own, or from another recorder) the
            // pixels are the only evidence, as for a SER file labelled mono.
            None if gray => {
                let hints = Hints {
                    stated: None,
                    model_default: None,
                    bright: filter_channel(path),
                };
                let frames: Vec<Frame> = detect::sample_indices(reader.frame_count())
                    .into_iter()
                    .filter_map(|i| reader.read_frame(i).ok())
                    .collect();
                let d = detect::detect(&frames, hints);
                // A dim target (a planet on a mono camera) has phase means a
                // few counts apart that quantisation alone makes uneven: only
                // a bright disc, as the Sun is, is trusted to show a mosaic.
                let bright = d.means.iter().sum::<f64>() / 4.0 >= AVI_MIN_DISC_LEVEL;
                let (pattern, origin) = match d.outcome {
                    Outcome::Mosaic { pattern, .. } if bright => (Some(pattern), Origin::Detection),
                    _ => (None, Origin::Header),
                };
                (pattern, origin, Some(d))
            }
            None => (None, Origin::Header, None),
        },
    }
}

/// The warning for an AVI with no sidecar whose pixels show a mosaic.
fn avi_detection_notice(pattern: Option<BayerPattern>, detection: Option<&Detection>) -> Notice {
    let how = if let Some(Outcome::Mosaic {
        basis: RbBasis::Brightness,
        ..
    }) = detection.map(|d| d.outcome)
    {
        " Red and blue cannot be told apart from the pixels alone, so the dimmer of the \
             two is taken to be blue; if the colours look swapped, choose the other pattern."
    } else {
        ""
    };
    warn(
        format!(
            "The AVI has no sidecar, but the data is a Bayer mosaic: debayering as {}",
            bayer_text(pattern)
        ),
        format!(
            "An AVI does not say whether it holds raw colour, and there is no SharpCap \
             sidecar beside it, but the frames hold a Bayer mosaic that looks like {}: {}. \
             Helia will debayer them as {}.{how} To treat the file as mono, set the Bayer \
             pattern to none; to use a different pattern, choose it yourself.",
            bayer_text(pattern),
            detection.map_or_else(String::new, measurement),
            bayer_text(pattern),
        ),
    )
}

fn open_avi(path: &Path, opts: &OpenOptions) -> Result<(Box<dyn FrameSource>, CaptureReport)> {
    if opts.bit_depth.is_some() || opts.endian.is_some() || opts.flip_rows {
        return Err(Error::Invalid(
            "--bit-depth, --endian and --flip-rows apply to SER files; an AVI describes its \
             own samples"
                .into(),
        ));
    }
    let file = BufReader::with_capacity(1 << 20, File::open(path)?);
    let mut reader = AviReader::new(file)?;
    let info = reader.info().clone();
    let gray = info.format != PixelFormat::Bgr24;
    let sidecar = CameraSettings::for_capture(path);
    let sidecar_facts = sidecar.as_ref().map(|s| sidecar_facts(path, s));
    let (pattern, origin, detection) = avi_bayer(&mut reader, path, opts, sidecar.as_ref(), gray);
    let pattern = if gray { pattern } else { None };
    let reader = reader.with_bayer(pattern);
    let shared = FileFrames::new(
        File::open(path)?,
        reader.row_layout(),
        reader.frame_offsets(),
        reader.bayer_pattern(),
    );
    let mut notices = Vec::new();
    if opts.bayer != BayerSetting::Auto {
        notices.push(note(format!(
            "Bayer pattern set by you to {}; an AVI does not say.",
            bayer_text(pattern)
        )));
    } else if origin == Origin::Sidecar {
        notices.push(note(format!(
            "The AVI does not say whether it holds raw colour; the SharpCap sidecar does, so \
             Helia will debayer as {}.",
            bayer_text(pattern)
        )));
    } else if origin == Origin::Detection {
        notices.push(avi_detection_notice(pattern, detection.as_ref()));
    }
    let report = CaptureReport {
        path: path.to_path_buf(),
        container: "AVI",
        ser: None,
        avi: Some(AviFacts {
            fourcc: info.fourcc.trim().to_string(),
            format: format!("{:?}", info.format),
            bottom_up: info.bottom_up,
            width: info.width,
            height: info.height,
            frames: reader.frame_count(),
        }),
        sidecar: sidecar_facts,
        detection,
        effective: Effective {
            bayer: Sourced {
                value: pattern,
                origin,
            },
            bit_depth: Sourced {
                value: if info.format == PixelFormat::Gray16 {
                    16
                } else {
                    8
                },
                origin: Origin::Header,
            },
            endian: Sourced {
                value: Endian::Little,
                origin: Origin::Header,
            },
            flip_rows: Sourced {
                value: false,
                origin: Origin::Header,
            },
        },
        overrides: override_phrases(opts),
        notices,
    };
    Ok((Box::new(AviSource::new(reader).with_shared(shared)), report))
}

/// Open a `.ser` or `.avi` capture, or a compressed video through `ffmpeg`,
/// and report how it was understood.
///
/// # Errors
/// I/O, format or unsupported-extension errors; options that do not apply to
/// the container.
pub fn open_reported(
    path: &Path,
    opts: &OpenOptions,
) -> Result<(Box<dyn FrameSource>, CaptureReport)> {
    open_reported_with(path, opts, &VideoOptions::default())
}

/// [`open_reported`] with the settings that apply to videos.
///
/// # Errors
/// As [`open_reported`]; [`Error::Cancelled`] when `video.cancel` was set.
pub fn open_reported_with(
    path: &Path,
    opts: &OpenOptions,
    video: &VideoOptions,
) -> Result<(Box<dyn FrameSource>, CaptureReport)> {
    if ffmpeg::is_video_path(path) {
        return open_video_reported(path, opts, video);
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("ser") => open_ser(path, opts),
        Some("avi") => match open_avi(path, opts) {
            // A codec the built-in reader does not handle (MJPEG, H.264, ...).
            Err(Error::Avi(AviError::UnsupportedFormat(_))) => {
                open_video_reported(path, opts, video)
            }
            other => other,
        },
        _ => Err(Error::Unsupported(format!(
            "unrecognised capture type {}; expected .ser, .avi, .mp4, .mov, .mkv, .webm or .m4v",
            path.display()
        ))),
    }
}

/// A video is decoded to a mono cache by `ffmpeg`; nothing in it can be
/// overridden, so the report has no header to show.
fn from_header<T>(value: T) -> Sourced<T> {
    Sourced {
        value,
        origin: Origin::Header,
    }
}

fn open_video_reported(
    path: &Path,
    opts: &OpenOptions,
    video: &VideoOptions,
) -> Result<(Box<dyn FrameSource>, CaptureReport)> {
    if opts.bit_depth.is_some() || opts.endian.is_some() || opts.flip_rows {
        return Err(Error::Invalid(
            "--bit-depth, --endian and --flip-rows apply to SER files; a video is decoded by \
             ffmpeg"
                .into(),
        ));
    }
    let src = crate::io::source::open_video(path, opts.channel, video)?;
    let report = CaptureReport {
        path: path.to_path_buf(),
        container: "Video",
        ser: None,
        avi: None,
        sidecar: None,
        detection: None,
        effective: Effective {
            bayer: from_header(None),
            bit_depth: from_header(src.shared().map_or(8, |s| match s.layout().sample {
                Sample::U8 => 8,
                _ => 16,
            })),
            endian: from_header(Endian::Little),
            flip_rows: from_header(false),
        },
        overrides: override_phrases(opts),
        notices: Vec::new(),
    };
    Ok((src, report))
}

/// Describe how a `.ser` or `.avi` capture would be opened, without keeping it
/// open. A compressed video is not decoded: an AVI with a codec the built-in
/// reader cannot handle fails with [`AviError::UnsupportedFormat`], so the
/// caller can ask `ffprobe` instead.
///
/// # Errors
/// As [`open_reported`], without the video fallback.
pub fn inspect(path: &Path, opts: &OpenOptions) -> Result<CaptureReport> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("ser") => open_ser(path, opts),
        Some("avi") => open_avi(path, opts),
        _ => Err(Error::Unsupported(format!(
            "unrecognised capture type {}; expected .ser or .avi",
            path.display()
        ))),
    }
    .map(|(_, r)| r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::raw::SharedFrames;
    use crate::io::ser::SerHeader;
    use crate::testutil::{mosaic_disc, ser_bytes_with};

    /// A unique file under the temp dir, removed on drop.
    struct TempFile(PathBuf);

    impl TempFile {
        fn new(name: &str, bytes: &[u8]) -> Self {
            let path = std::env::temp_dir()
                .join(format!("helia-capture-test-{}-{name}", std::process::id()));
            std::fs::write(&path, bytes).unwrap_or_else(|e| panic!("{e}"));
            Self(path)
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
            let _ = std::fs::remove_file(CameraSettings::sidecar_path(&self.0));
        }
    }

    const PATTERNS: [BayerPattern; 4] = [
        BayerPattern::Rggb,
        BayerPattern::Bggr,
        BayerPattern::Grbg,
        BayerPattern::Gbrg,
    ];

    fn data(pattern: Option<BayerPattern>, gains: [f32; 3]) -> Vec<Frame> {
        (0..4)
            .map(|i| mosaic_disc(240, 200, pattern, gains, 11 + i))
            .collect()
    }

    fn ser(label: ColorId, depth: u32, frames: &[Frame]) -> Vec<u8> {
        ser_named("", label, depth, frames)
    }

    fn ser_named(instrument: &str, label: ColorId, depth: u32, frames: &[Frame]) -> Vec<u8> {
        let mut h = SerHeader::new(240, 200, label, depth);
        h.instrument = instrument.into();
        ser_bytes_with(frames, h)
    }

    fn report(f: &TempFile, opts: OpenOptions) -> CaptureReport {
        inspect(&f.0, &opts).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    #[cfg_attr(miri, ignore = "whole-frame pixel maths: far too slow under Miri")]
    fn mono_labelled_mosaics_are_found_for_all_four_patterns() {
        for (i, p) in PATTERNS.into_iter().enumerate() {
            for depth in [8, 16] {
                let f = TempFile::new(
                    &format!("m{i}-{depth}.ser"),
                    &ser(ColorId::Mono, depth, &data(Some(p), [0.75, 1.0, 0.55])),
                );
                let r = report(&f, OpenOptions::default());
                assert_eq!(
                    r.effective.bayer.value,
                    Some(p),
                    "{p:?} at {depth} bit: {:?}",
                    r.detection
                );
                assert_eq!(r.effective.bayer.origin, Origin::Detection);
                let n = &r.notices[0];
                assert_eq!(n.level, Level::Warning);
                assert!(n.text.contains("header says mono"), "{}", n.text);
                assert!(n.text.contains(pattern_name(p)), "{}", n.text);
                // The frames really come out tagged for the debayerer.
                let (mut src, _) =
                    open_reported(&f.0, &OpenOptions::default()).unwrap_or_else(|e| panic!("{e}"));
                assert_eq!(src.read(0).map(|f| f.bayer).ok(), Some(Some(p)));
                assert_eq!(src.shared().map(SharedFrames::bayer), Some(Some(p)));
            }
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "whole-frame pixel maths: far too slow under Miri")]
    fn true_mono_is_left_alone() {
        let f = TempFile::new(
            "mono.ser",
            &ser_named("ZWO ASI482MC", ColorId::Mono, 16, &data(None, [1.0; 3])),
        );
        let r = report(&f, OpenOptions::default());
        assert_eq!(r.effective.bayer.value, None);
        assert_eq!(r.effective.bayer.origin, Origin::Header);
        assert_eq!(r.detection.map(|d| d.outcome), Some(Outcome::NoMosaic));
        // A colour-sounding instrument name earns a note, not a debayer.
        assert!(
            r.notices.iter().all(|n| n.level == Level::Info),
            "{:?}",
            r.notices
        );
        assert!(r.notices[0].text.contains("ZWO ASI482MC"));
    }

    #[test]
    #[cfg_attr(miri, ignore = "whole-frame pixel maths: far too slow under Miri")]
    fn a_near_black_blue_channel_is_still_a_mosaic() {
        let f = TempFile::new(
            "ha.ser",
            &ser(
                ColorId::Mono,
                8,
                &data(Some(BayerPattern::Rggb), [0.94, 1.0, 0.005]),
            ),
        );
        let r = report(&f, OpenOptions::default());
        assert_eq!(r.effective.bayer.value, Some(BayerPattern::Rggb));
        assert!(r.detection.is_some_and(|d| d.spread > 1.0));
    }

    #[test]
    #[cfg_attr(miri, ignore = "whole-frame pixel maths: far too slow under Miri")]
    fn the_user_beats_the_header_and_the_data() {
        let frames = data(Some(BayerPattern::Rggb), [0.75, 1.0, 0.55]);
        let f = TempFile::new("prec.ser", &ser(ColorId::Mono, 16, &frames));
        let pick = |bayer| {
            report(
                &f,
                OpenOptions {
                    bayer,
                    ..OpenOptions::default()
                },
            )
        };
        let forced = pick(BayerSetting::Pattern(BayerPattern::Gbrg));
        assert_eq!(forced.effective.bayer.value, Some(BayerPattern::Gbrg));
        assert_eq!(forced.effective.bayer.origin, Origin::Override);
        assert!(forced.detection.is_none(), "an override skips the test");
        assert_eq!(forced.overrides, ["--bayer gbrg"]);
        let none = pick(BayerSetting::Mono);
        assert_eq!(none.effective.bayer.value, None);
        assert_eq!(none.effective.bayer.origin, Origin::Override);
        assert!(none.notices[0].text.contains("set by you"));
        assert!(
            none.note_lines()
                .iter()
                .any(|l| l.contains("Overrides: --bayer none")),
            "{:?}",
            none.note_lines()
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "whole-frame pixel maths: far too slow under Miri")]
    fn a_header_that_says_bayer_is_trusted_unless_overridden() {
        let frames = data(Some(BayerPattern::Rggb), [0.75, 1.0, 0.55]);
        let label = ColorId::Bayer(BayerPattern::Bggr);
        let f = TempFile::new("hdr.ser", &ser(label, 16, &frames));
        let r = report(&f, OpenOptions::default());
        assert_eq!(r.effective.bayer.value, Some(BayerPattern::Bggr));
        assert_eq!(r.effective.bayer.origin, Origin::Header);
        assert!(r.detection.is_none());
        let flipped = report(
            &f,
            OpenOptions {
                flip_rows: true,
                ..OpenOptions::default()
            },
        );
        // Reading an even-height mosaic upside down swaps the row parity.
        assert_eq!(flipped.effective.bayer.value, Some(BayerPattern::Grbg));
        assert!(flipped.effective.flip_rows.value);
        assert_eq!(flipped.effective.flip_rows.origin, Origin::Override);
    }

    #[test]
    #[cfg_attr(miri, ignore = "whole-frame pixel maths: far too slow under Miri")]
    fn the_sidecar_settles_what_the_pixels_cannot() {
        // Too dark to measure, but SharpCap says raw colour and names the layout.
        let dark: Vec<Frame> = (0..3)
            .map(|_| Frame {
                width: 240,
                height: 200,
                data: vec![0.0; 240 * 200],
                bayer: None,
            })
            .collect();
        let f = TempFile::new("side.ser", &ser(ColorId::Mono, 16, &dark));
        assert_eq!(
            report(&f, OpenOptions::default()).effective.bayer.value,
            None
        );
        std::fs::write(
            CameraSettings::sidecar_path(&f.0),
            "[ZWO ASI482MC]\nColour Space=RAW8\nDebayer Preview=Force GBRG\n",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let r = report(&f, OpenOptions::default());
        assert_eq!(r.effective.bayer.value, Some(BayerPattern::Gbrg));
        assert_eq!(r.effective.bayer.origin, Origin::Sidecar);
        assert!(r.notices[0].text.contains("SharpCap sidecar"));
        // Data that shows no mosaic wins over a sidecar that claims one.
        let mono = TempFile::new("side2.ser", &ser(ColorId::Mono, 16, &data(None, [1.0; 3])));
        std::fs::write(
            CameraSettings::sidecar_path(&mono.0),
            "Colour Space=RAW8\nDebayer Preview=Force RGGB\n",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            report(&mono, OpenOptions::default()).effective.bayer.value,
            None
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "whole-frame pixel maths: far too slow under Miri")]
    fn the_header_view_decodes_every_field() {
        let mut h = SerHeader::new(240, 200, ColorId::Mono, 16);
        h.observer = "Laurence".into();
        h.instrument = "ZWO ASI482MC".into();
        h.telescope = "Lunt 80".into();
        // 2022-07-17 15:32:09.500 UTC.
        h.date_time_utc = 637_936_687_295_000_000;
        let frames = data(None, [1.0; 3]);
        let f = TempFile::new("hdr2.ser", &ser_bytes_with(&frames, h));
        let r = report(&f, OpenOptions::default());
        let rows = r.header_rows();
        let get = |k: &str| {
            let (_, v) = rows
                .iter()
                .find(|(l, _)| l == k)
                .unwrap_or_else(|| panic!("no row {k}"));
            v.as_str()
        };
        assert_eq!(get("FileID"), "LUCAM-RECORDER");
        assert_eq!(get("ColorID"), "0 (MONO)");
        assert_eq!(get("LittleEndian"), "1 (little-endian)");
        assert_eq!(get("Width"), "240 px");
        assert_eq!(get("PixelDepth"), "16 bit");
        assert_eq!(get("FrameCount"), "4");
        assert_eq!(get("Observer"), "Laurence");
        assert_eq!(get("DateTime (local)"), "not set");
        assert_eq!(get("DateTimeUTC"), "2022-07-17 15:32:09.500");
        assert_eq!(get("Timestamps"), "absent");
        let eff = r.effective_rows();
        assert_eq!(
            eff[0],
            ("Bayer pattern", "none (mono)".to_string(), Origin::Header)
        );
        assert_eq!(eff[1].1, "16 bit");
    }

    #[test]
    #[cfg_attr(miri, ignore = "whole-frame pixel maths: far too slow under Miri")]
    fn bit_depth_and_byte_order_overrides_reach_the_pixels() {
        let frames = data(None, [1.0; 3]);
        let mut bytes = ser(ColorId::Mono, 16, &frames);
        // The header claims 8 bits for 16-bit data.
        bytes[34..38].copy_from_slice(&8i32.to_le_bytes());
        let f = TempFile::new("lie.ser", &bytes);
        let fixed = OpenOptions {
            bit_depth: Some(16),
            ..OpenOptions::default()
        };
        let (mut src, r) = open_reported(&f.0, &fixed).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(r.effective.bit_depth.origin, Origin::Override);
        assert_eq!(r.ser.as_ref().map(|s| s.pixel_depth), Some(8));
        let got = src.read(2).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            got.data
                .iter()
                .zip(&frames[2].data)
                .all(|(a, b)| (a - b).abs() < 1e-4)
        );
        // The concurrent decoder agrees.
        let shared = src.shared().unwrap_or_else(|| panic!("no shared decoder"));
        let d = crate::debayer::Debayer::default();
        let via = shared
            .read_mono(2, &d, &crate::profile::Profile::new())
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(via.data, got.data);
        // Big-endian forced on little-endian data reads differently.
        let be = OpenOptions {
            endian: Some(Endian::Big),
            ..OpenOptions::default()
        };
        let g = TempFile::new("be.ser", &ser(ColorId::Mono, 16, &frames));
        let (mut src, r) = open_reported(&g.0, &be).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(r.effective.endian.value, Endian::Big);
        assert_ne!(
            src.read(0).map(|f| f.data).ok(),
            Some(frames[0].data.clone())
        );
    }

    fn avi(frames: &[Frame]) -> Vec<u8> {
        crate::io::avi::write_test_avi(frames, b"DIB ", 8, false, None)
    }

    #[test]
    #[cfg_attr(miri, ignore = "whole-frame pixel maths: far too slow under Miri")]
    fn an_avi_without_a_sidecar_is_checked_for_a_mosaic() {
        for p in PATTERNS {
            let f = TempFile::new(
                &format!("det-{}.avi", pattern_name(p)),
                &avi(&data(Some(p), [0.75, 1.0, 0.55])),
            );
            let r = report(&f, OpenOptions::default());
            assert_eq!(r.effective.bayer.value, Some(p), "{p:?}: {:?}", r.detection);
            assert_eq!(r.effective.bayer.origin, Origin::Detection);
            assert_eq!(r.notices[0].level, Level::Warning);
            assert!(r.notices[0].text.contains("no SharpCap sidecar"));
            let (mut src, _) =
                open_reported(&f.0, &OpenOptions::default()).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(src.read(0).map(|f| f.bayer).ok(), Some(Some(p)));
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "whole-frame pixel maths: far too slow under Miri")]
    fn a_mono_avi_without_a_sidecar_stays_mono() {
        let f = TempFile::new("det-mono.avi", &avi(&data(None, [1.0, 1.0, 1.0])));
        let r = report(&f, OpenOptions::default());
        assert_eq!(r.effective.bayer.value, None);
        assert_eq!(r.effective.bayer.origin, Origin::Header);
        assert!(r.notices.is_empty(), "{:?}", r.notices);
    }

    #[test]
    #[cfg_attr(miri, ignore = "whole-frame pixel maths: far too slow under Miri")]
    fn a_dim_avi_is_not_guessed_at() {
        let dim: Vec<Frame> = data(Some(BayerPattern::Rggb), [0.75, 1.0, 0.55])
            .into_iter()
            .map(|mut f| {
                f.data.iter_mut().for_each(|v| *v *= 0.03);
                f
            })
            .collect();
        let f = TempFile::new("det-dim.avi", &avi(&dim));
        let r = report(&f, OpenOptions::default());
        assert_eq!(r.effective.bayer.value, None);
    }

    #[test]
    #[cfg_attr(miri, ignore = "whole-frame pixel maths: far too slow under Miri")]
    fn the_sidecar_still_comes_before_the_pixels_for_an_avi() {
        let f = TempFile::new("det-side.avi", &avi(&data(None, [1.0, 1.0, 1.0])));
        std::fs::write(
            CameraSettings::sidecar_path(&f.0),
            "[ZWO ASI482MC]\nColour Space=RAW8\nDebayer Preview=Force RGGB\n",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let r = report(&f, OpenOptions::default());
        assert_eq!(r.effective.bayer.value, Some(BayerPattern::Rggb));
        assert_eq!(r.effective.bayer.origin, Origin::Sidecar);
        assert!(r.detection.is_none());
    }

    #[test]
    fn serial_only_options_are_refused_for_avi() {
        let f = TempFile::new("x.avi", b"not read");
        for opts in [
            OpenOptions {
                bit_depth: Some(8),
                ..OpenOptions::default()
            },
            OpenOptions {
                endian: Some(Endian::Big),
                ..OpenOptions::default()
            },
            OpenOptions {
                flip_rows: true,
                ..OpenOptions::default()
            },
        ] {
            let Err(e) = open_reported(&f.0, &opts) else {
                panic!("accepted {opts:?}");
            };
            assert!(e.to_string().contains("apply to SER"), "{e}");
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "whole-frame pixel maths: far too slow under Miri")]
    fn how_the_capture_was_read_reaches_every_output_format() {
        use crate::io::{ImageNotes, save_image_with};
        let f = TempFile::new(
            "meta.ser",
            &ser(
                ColorId::Mono,
                16,
                &data(Some(BayerPattern::Grbg), [0.75, 1.0, 0.55]),
            ),
        );
        let opts = OpenOptions {
            endian: Some(Endian::Little),
            ..OpenOptions::default()
        };
        let r = report(&f, opts);
        let lines = r.note_lines();
        assert_eq!(
            lines[0],
            "Capture read as: Bayer GRBG (detection), 16-bit (header), little-endian (override)"
        );
        assert_eq!(
            lines[1],
            "SER header: ColorID 0 (MONO), 16-bit, little-endian"
        );
        assert!(
            lines[2].starts_with("Bayer detection: 2x2 position means "),
            "{lines:?}"
        );
        assert_eq!(lines[3], "Overrides: --endian little");
        let image = Frame::new(4, 2, vec![0.5; 8]).unwrap_or_else(|e| panic!("{e}"));
        let notes = ImageNotes {
            lines,
            ..ImageNotes::default()
        };
        for ext in ["tif", "png", "fits"] {
            let out = TempFile::new(&format!("out.{ext}"), b"");
            save_image_with(&out.0, &image, &notes).unwrap_or_else(|e| panic!("{e}"));
            let bytes = std::fs::read(&out.0).unwrap_or_default();
            let text = String::from_utf8_lossy(&bytes);
            // FITS wraps long cards at 72 characters, so look for the first words.
            assert!(
                text.contains("Capture read as: Bayer GRBG (detection)"),
                "{ext}"
            );
            assert!(text.contains("Overrides: --endian"), "{ext}");
        }
    }
}
