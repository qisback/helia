// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! A capture made of FITS frames: a folder or pattern of single-frame files
//! (a Seestar's `Light_*.fit` subs), one file holding a cube of frames, or a
//! file with one image per extension.
//!
//! The container is read as defined by the FITS standard (Pence et al. 2010,
//! "Definition of the Flexible Image Transport System (FITS), version 3.0",
//! A&A 524, A42, doi:10.1051/0004-6361/201015362): 2880-byte blocks, an
//! 80-character card header per unit, `BITPIX`, `NAXISn`, `BZERO` and
//! `BSCALE` giving the physical value `BZERO + BSCALE * stored`, and the first
//! stored row being the bottom of the image. `BAYERPAT` and `ROWORDER` are
//! the de-facto conventions of capture programs, not part of the standard:
//! `BAYERPAT` names the 2x2 mosaic of the frame as stored (first stored row
//! first), and `ROWORDER = TOP-DOWN` says the first stored row is already the
//! top; without it the standard's bottom-up order applies and the rows are
//! flipped on reading, which also flips the pattern of an even-height mosaic.
//!
//! Only the headers are read when the capture is opened. A frame's samples
//! are read, converted and released one at a time, so memory stays at a few
//! frames however long the sequence.

#![allow(
    clippy::many_single_char_names,
    clippy::too_many_lines,
    reason = "x, y, w, h are the natural names in image maths; the header walks are one sequence each"
)]

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::{Error, Result};
use crate::frame::{BayerPattern, Frame};
use crate::io::fits::{
    FitsError, MAX_DIMENSION, bayer_pattern, decode_samples, float_or, int, parse_header,
};
use crate::io::raw::{DeviceCache, Planes, RowLayout, Sample, SharedFrames, pread_exact};
use crate::io::solarnet::unix_from_iso;
use crate::io::source::{FrameSource, RowOrder};

const BLOCK: u64 = 2880;
/// Axes read from one header before it is refused.
const MAX_AXES: i64 = 16;

/// Extensions taken as FITS frames.
const EXTENSIONS: [&str; 3] = ["fit", "fits", "fts"];

/// One image unit that holds frames.
#[derive(Clone, Debug, PartialEq)]
pub struct Hdu {
    /// Byte offset of the data.
    pub offset: u64,
    /// `BITPIX`.
    pub bitpix: i64,
    /// Width in pixels (`NAXIS1`).
    pub width: usize,
    /// Height in pixels (`NAXIS2`).
    pub height: usize,
    /// Planes stored (`NAXIS3`, else 1).
    pub planes: usize,
    /// Three planes that are red, green and blue of one frame (`CTYPE3 = RGB`).
    pub colour: bool,
    /// `BZERO`.
    pub bzero: f64,
    /// `BSCALE`.
    pub bscale: f64,
    /// `BAYERPAT` as written.
    pub bayerpat: Option<String>,
    /// `ROWORDER` as written.
    pub roworder: Option<String>,
    /// `INSTRUME` as written.
    pub instrument: Option<String>,
    /// `TELESCOP` as written.
    pub telescope: Option<String>,
    /// The first of `DATE-AVG`, `DATE-OBS`, `DATE-BEG` this unit's header has.
    pub date: Option<String>,
    /// `EXPTIME`, else `EXPOSURE`, in seconds.
    pub exposure_s: Option<f64>,
    /// Where the camera pointed and stood, when the header says.
    pub pointing: Option<Pointing>,
}

/// Where a FITS frame was taken and what it points at, from `RA`, `DEC`,
/// `SITELAT`, `SITELONG` and `EQMODE` (the keywords of a Seestar sub). The
/// field rotation of an altazimuth mount follows from these and the time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pointing {
    /// `RA`, degrees.
    pub ra_deg: f64,
    /// `DEC`, degrees.
    pub dec_deg: f64,
    /// `SITELAT`, degrees north, when given.
    pub site_lat_deg: Option<f64>,
    /// `SITELONG`, degrees east, when given.
    pub site_lon_deg: Option<f64>,
    /// `EQMODE`: `Some(0)` is an altazimuth mount, `Some(1)` an equatorial one.
    pub eqmode: Option<i64>,
}

impl Pointing {
    /// The observing site, when the header gave both coordinates.
    #[must_use]
    pub fn site(&self) -> Option<crate::io::solarnet::Site> {
        Some(crate::io::solarnet::Site {
            lat_deg: self.site_lat_deg?,
            lon_deg: self.site_lon_deg?,
            alt_m: 0.0,
        })
    }

    /// The frames turn with the sky: an altazimuth mount.
    #[must_use]
    pub fn is_altazimuth(&self) -> bool {
        self.eqmode == Some(0)
    }
}

fn pointing_of(pick: &dyn Fn(&str) -> Option<String>) -> Option<Pointing> {
    let num = |k: &str| {
        pick(k)
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|v| v.is_finite())
    };
    let (ra_deg, dec_deg) = (num("RA")?, num("DEC")?);
    if !(0.0..=360.0).contains(&ra_deg) || !(-90.0..=90.0).contains(&dec_deg) {
        return None;
    }
    Some(Pointing {
        ra_deg,
        dec_deg,
        site_lat_deg: num("SITELAT").filter(|v| v.abs() <= 90.0),
        site_lon_deg: num("SITELONG").filter(|v| v.abs() <= 360.0),
        eqmode: pick("EQMODE").and_then(|v| v.trim().parse().ok()),
    })
}

impl Hdu {
    /// Frames this unit holds: its planes, or one colour frame.
    #[must_use]
    pub fn frames(&self) -> usize {
        if self.colour { 1 } else { self.planes }
    }

    fn bytes_per_sample(&self) -> u64 {
        self.bitpix.unsigned_abs() / 8
    }

    fn plane_bytes(&self) -> u64 {
        self.width as u64 * self.height as u64 * self.bytes_per_sample()
    }
}

fn date_of(map: &BTreeMap<String, String>) -> Option<String> {
    ["DATE-AVG", "DATE-OBS", "DATE-BEG"]
        .iter()
        .find_map(|k| map.get(*k).filter(|v| unix_from_iso(v).is_some()).cloned())
}

/// The image units of a FITS stream that hold 2-D or 3-D integer or float
/// images, in file order. Other units (tables, empty primary) are skipped.
///
/// # Errors
/// [`FitsError`] for a malformed header, a unit longer than the stream or an
/// image of an unsupported type.
pub fn scan_hdus<R: Read + Seek>(mut r: R) -> std::result::Result<Vec<Hdu>, FitsError> {
    let total = r.seek(SeekFrom::End(0))?;
    let mut pos = 0u64;
    let mut out = Vec::new();
    let mut primary: Option<BTreeMap<String, String>> = None;
    loop {
        if primary.is_some() && total.saturating_sub(pos) < BLOCK {
            break;
        }
        r.seek(SeekFrom::Start(pos))?;
        let is_primary = primary.is_none();
        let map = parse_header(&mut r, is_primary)?;
        let data_start = r.stream_position()?;
        let bitpix = int(&map, "BITPIX")?;
        if !matches!(bitpix, 8 | 16 | 32 | 64 | -32 | -64) {
            return Err(FitsError::UnsupportedBitpix(bitpix));
        }
        let naxis = int(&map, "NAXIS")?;
        if !(0..=MAX_AXES).contains(&naxis) {
            return Err(FitsError::BadKeyword("NAXIS"));
        }
        let mut count: u64 = u64::from(naxis > 0);
        let mut dims = Vec::new();
        for i in 1..=naxis {
            let key: &'static str = [
                "NAXIS1", "NAXIS2", "NAXIS3", "NAXIS4", "NAXIS5", "NAXIS6", "NAXIS7", "NAXIS8",
                "NAXIS9", "NAXIS10", "NAXIS11", "NAXIS12", "NAXIS13", "NAXIS14", "NAXIS15",
                "NAXIS16",
            ][(i - 1) as usize];
            let n = int(&map, key)?;
            if n < 0 {
                return Err(FitsError::BadKeyword(key));
            }
            dims.push(n);
            count = count
                .checked_mul(n as u64)
                .ok_or(FitsError::BadKeyword(key))?;
        }
        let image = is_primary || map.get("XTENSION").is_some_and(|x| x.trim() == "IMAGE");
        let pcount = if is_primary {
            0.0
        } else {
            float_or(&map, "PCOUNT", 0.0)
        };
        let gcount = if is_primary {
            1.0
        } else {
            float_or(&map, "GCOUNT", 1.0)
        };
        if !(pcount >= 0.0 && gcount >= 0.0 && pcount.is_finite() && gcount.is_finite()) {
            return Err(FitsError::BadKeyword("PCOUNT"));
        }
        let data_len = (gcount * (pcount + count as f64) * (bitpix.unsigned_abs() / 8) as f64)
            .min(1.0e19) as u64;
        let available = total.saturating_sub(data_start);
        if data_len > available {
            return Err(FitsError::TruncatedData {
                expected: data_len,
                actual: available,
            });
        }
        if image && matches!(naxis, 2 | 3) && count > 0 {
            if !matches!(bitpix, 8 | 16 | -32) {
                return Err(FitsError::UnsupportedBitpix(bitpix));
            }
            let (w, h) = (dims[0], dims[1]);
            if !(1..=MAX_DIMENSION).contains(&w) || !(1..=MAX_DIMENSION).contains(&h) {
                return Err(FitsError::BadDimensions {
                    width: w,
                    height: h,
                });
            }
            let planes = if naxis == 3 { dims[2] } else { 1 };
            let pm = primary.as_ref().unwrap_or(&map);
            let pick = |k: &str| map.get(k).or_else(|| pm.get(k)).cloned();
            out.push(Hdu {
                offset: data_start,
                bitpix,
                width: w as usize,
                height: h as usize,
                planes: planes as usize,
                colour: planes == 3
                    && map
                        .get("CTYPE3")
                        .is_some_and(|c| c.trim().eq_ignore_ascii_case("RGB")),
                bzero: float_or(&map, "BZERO", 0.0),
                bscale: float_or(&map, "BSCALE", 1.0),
                bayerpat: pick("BAYERPAT"),
                roworder: pick("ROWORDER"),
                instrument: pick("INSTRUME"),
                telescope: pick("TELESCOP"),
                date: date_of(&map),
                exposure_s: ["EXPTIME", "EXPOSURE"]
                    .iter()
                    .find_map(|k| pick(k).and_then(|v| v.parse::<f64>().ok()))
                    .filter(|e| e.is_finite() && *e > 0.0),
                pointing: pointing_of(&pick),
            });
        }
        pos = data_start + data_len.div_ceil(BLOCK) * BLOCK;
        if primary.is_none() {
            primary = Some(map);
        }
    }
    Ok(out)
}

fn has_wildcard(name: &str) -> bool {
    name.contains(['*', '?'])
}

/// Shell-style match of `name` against `pattern` (`*` any run, `?` one
/// character), ignoring case.
fn wildcard_match(pattern: &[char], name: &[char]) -> bool {
    match pattern.split_first() {
        None => name.is_empty(),
        Some(('*', rest)) => (0..=name.len()).any(|i| wildcard_match(rest, &name[i..])),
        Some((p, rest)) => name
            .split_first()
            .is_some_and(|(n, tail)| (*p == '?' || *p == *n) && wildcard_match(rest, tail)),
    }
}

fn is_fits_name(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// True when `path` names FITS frames: a FITS file, a folder, or a pattern
/// with `*` or `?` in its last part that is not itself a file.
#[must_use]
pub fn is_fits_input(path: &Path) -> bool {
    if path.is_dir() {
        return true;
    }
    if is_fits_name(path) {
        return true;
    }
    !path.exists()
        && path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(has_wildcard)
}

/// Order for file names where `frame_2` comes before `frame_10`.
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (a, b) = (a.to_lowercase(), b.to_lowercase());
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(&b),
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
            (Some(p), Some(q)) if p.is_ascii_digit() && q.is_ascii_digit() => {
                let run = |it: &mut std::iter::Peekable<std::str::Chars<'_>>| {
                    let mut s = String::new();
                    while let Some(c) = it.next_if(char::is_ascii_digit) {
                        s.push(c);
                    }
                    s.trim_start_matches('0').to_string()
                };
                let (m, n) = (run(&mut x), run(&mut y));
                let ord = m.len().cmp(&n.len()).then_with(|| m.cmp(&n));
                if ord.is_ne() {
                    return ord;
                }
            }
            (Some(p), Some(q)) => {
                if p != q {
                    return p.cmp(&q);
                }
                x.next();
                y.next();
            }
        }
    }
}

/// The FITS files `path` names: the file itself, the FITS files in a folder,
/// or the files in its folder that match a `*`/`?` pattern. Sorted by name,
/// numbers compared as numbers.
///
/// # Errors
/// I/O failure, or nothing that matches.
pub fn expand(path: &Path) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = if path.is_dir() {
        list_dir(path, is_fits_name)?
    } else if path.exists() {
        vec![path.to_path_buf()]
    } else if let Some(pattern) = path
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| has_wildcard(n))
    {
        let pat: Vec<char> = pattern.to_lowercase().chars().collect();
        let dir = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        list_dir(dir, |p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                let name: Vec<char> = n.to_lowercase().chars().collect();
                wildcard_match(&pat, &name)
            })
        })?
    } else {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{} does not exist", path.display()),
        )));
    };
    if files.is_empty() {
        return Err(Error::Invalid(if path.is_dir() {
            format!(
                "no FITS frames (.fit, .fits or .fts) in the folder {}",
                path.display()
            )
        } else {
            format!("no files match {}", path.display())
        }));
    }
    files.sort_by(|a, b| {
        natural_cmp(
            &a.file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
            &b.file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
        )
    });
    Ok(files)
}

fn list_dir(dir: &Path, keep: impl Fn(&Path) -> bool) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let p = entry?.path();
        if p.is_file() && keep(&p) {
            out.push(p);
        }
    }
    Ok(out)
}

/// One frame of the sequence.
#[derive(Clone, Debug)]
struct Item {
    file: u32,
    offset: u64,
    bitpix: i64,
    bzero: f64,
    bscale: f64,
    colour: bool,
}

/// What the headers of a FITS capture say, in stack order.
#[derive(Clone, Debug)]
pub struct Scan {
    /// The files, in stack order.
    pub files: Vec<PathBuf>,
    /// Frame count.
    pub frames: usize,
    /// Pixels per row.
    pub width: usize,
    /// Rows.
    pub height: usize,
    /// `BITPIX` of the first frame.
    pub bitpix: i64,
    /// `BZERO` of the first frame.
    pub bzero: f64,
    /// `BSCALE` of the first frame.
    pub bscale: f64,
    /// `BAYERPAT` as written, if any.
    pub bayerpat: Option<String>,
    /// `ROWORDER` as written, if any.
    pub roworder: Option<String>,
    /// The first stored row is the bottom of the image (the rows are flipped
    /// on reading).
    pub bottom_up: bool,
    /// Why the rows are read that way.
    pub row_rule: RowRule,
    /// The files were put in `DATE-OBS` order (else by name).
    pub by_time: bool,
    /// Time of each frame, unix seconds UTC, where the header gives one.
    pub times: Vec<Option<f64>>,
    /// Header text of the time of the first and last frame.
    pub first_date: Option<String>,
    /// See `first_date`.
    pub last_date: Option<String>,
    /// Exposure of one frame in seconds, from the first frame's header.
    pub exposure_s: Option<f64>,
    /// Files that hold more than one frame (cubes, extensions).
    pub multi_frame_files: usize,
    /// What the first frame's header says it points at and where it stood.
    pub pointing: Option<Pointing>,
    items: Vec<Item>,
}

/// Why the rows of a FITS capture are read bottom-up or top-down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowRule {
    /// The user asked for it (`--row-order`).
    Requested,
    /// The header's `ROWORDER` card.
    Card,
    /// A Seestar S50 header (`INSTRUME` or `TELESCOP` names an S50) with no
    /// `ROWORDER` card: the camera stores the top row first.
    Seestar,
    /// No card and not a Seestar: the FITS standard's bottom-up order.
    Standard,
}

impl RowRule {
    /// A short phrase for tables.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Requested => "requested with --row-order",
            Self::Card => "ROWORDER card",
            Self::Seestar => "Seestar S50 header, stored top row first",
            Self::Standard => "no ROWORDER card, so bottom-up as the FITS standard says",
        }
    }
}

/// Seestar S50 frames name the camera in `INSTRUME` ("Seestar S50") and the
/// unit in `TELESCOP` ("S50_" and a serial).
fn is_seestar_s50(h: &Hdu) -> bool {
    [&h.instrument, &h.telescope]
        .into_iter()
        .flatten()
        .any(|v| v.to_ascii_uppercase().contains("S50"))
}

/// Whether frame header `h` is stored bottom-up, and why.
fn resolve_rows(h: &Hdu, order: RowOrder) -> (bool, RowRule) {
    match order {
        RowOrder::TopDown => return (false, RowRule::Requested),
        RowOrder::BottomUp => return (true, RowRule::Requested),
        RowOrder::Auto => {}
    }
    match h.roworder.as_deref().map(|r| r.trim().to_ascii_uppercase()) {
        Some(r) if r == "TOP-DOWN" => (false, RowRule::Card),
        Some(r) if r == "BOTTOM-UP" => (true, RowRule::Card),
        _ if is_seestar_s50(h) => (false, RowRule::Seestar),
        _ => (true, RowRule::Standard),
    }
}

fn flip_pattern(p: BayerPattern) -> BayerPattern {
    match p {
        BayerPattern::Rggb => BayerPattern::Gbrg,
        BayerPattern::Gbrg => BayerPattern::Rggb,
        BayerPattern::Grbg => BayerPattern::Bggr,
        BayerPattern::Bggr => BayerPattern::Grbg,
    }
}

fn name_of(p: &Path) -> String {
    p.file_name().map_or_else(
        || p.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

impl Scan {
    /// Read the headers of the FITS frames `path` names (see [`expand`]).
    ///
    /// # Errors
    /// I/O failure, a file that is not valid FITS, frames of different sizes
    /// or mosaic patterns, or no frames at all. The message says which file.
    pub fn read(path: &Path) -> Result<Self> {
        Self::read_with(path, RowOrder::Auto)
    }

    /// As [`Scan::read`], with the row order the user asked for.
    ///
    /// # Errors
    /// As [`Scan::read`].
    pub fn read_with(path: &Path, order: RowOrder) -> Result<Self> {
        let mut found: Vec<(PathBuf, Vec<Hdu>)> = Vec::new();
        for f in expand(path)? {
            let file = File::open(&f)?;
            let hdus = scan_hdus(BufReader::new(file)).map_err(|e| {
                Error::Invalid(format!(
                    "{} is not a FITS file Helia can read ({e}); every file in a FITS capture must be a valid FITS image",
                    name_of(&f)
                ))
            })?;
            if hdus.is_empty() {
                return Err(Error::Invalid(format!(
                    "{} holds no 2-D image (its primary unit is empty and it has no image extension)",
                    name_of(&f)
                )));
            }
            found.push((f, hdus));
        }
        let times = |h: &[Hdu]| {
            h.first()
                .filter(|_| h.len() == 1 && h[0].frames() == 1)
                .and_then(|u| u.date.as_deref().and_then(unix_from_iso))
        };
        let by_time = found.len() > 1 && found.iter().all(|(_, h)| times(h).is_some());
        if by_time {
            // Stable: ties keep the name order.
            found.sort_by(|a, b| {
                times(&a.1)
                    .partial_cmp(&times(&b.1))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        let first = found[0].1[0].clone();
        let first = &first;
        let (width, height) = (first.width, first.height);
        let (bottom_up, row_rule) = resolve_rows(first, order);
        let mosaic = |h: &Hdu| {
            bayer_pattern(&BTreeMap::from_iter(
                h.bayerpat
                    .iter()
                    .map(|p| ("BAYERPAT".to_string(), p.clone())),
            ))
        };
        let pattern = mosaic(first);
        let (mut items, mut frame_times) = (Vec::new(), Vec::new());
        let mut multi = 0;
        let (mut first_date, mut last_date) = (None, None);
        for (i, (f, hdus)) in found.iter().enumerate() {
            if hdus.iter().map(Hdu::frames).sum::<usize>() > 1 {
                multi += 1;
            }
            for h in hdus {
                if (h.width, h.height) != (width, height) {
                    return Err(Error::Invalid(format!(
                        "the frames are not all the same size: {} is {}x{} pixels but {} is {width}x{height}; stack frames of one size, or remove the odd one out",
                        name_of(f),
                        h.width,
                        h.height,
                        name_of(&found[0].0)
                    )));
                }
                let order_up = resolve_rows(h, order).0;
                if mosaic(h) != pattern || order_up != bottom_up {
                    return Err(Error::Invalid(format!(
                        "the frames disagree on their Bayer pattern or ROWORDER: {} has BAYERPAT {} and ROWORDER {}, but {} has BAYERPAT {} and ROWORDER {}; stack frames from one camera setup",
                        name_of(f),
                        h.bayerpat.as_deref().unwrap_or("(none)"),
                        h.roworder.as_deref().unwrap_or("(none)"),
                        name_of(&found[0].0),
                        first.bayerpat.as_deref().unwrap_or("(none)"),
                        first.roworder.as_deref().unwrap_or("(none)"),
                    )));
                }
                let n = h.frames();
                let t = (n == 1 && hdus.len() == 1)
                    .then(|| h.date.as_deref().and_then(unix_from_iso))
                    .flatten();
                for k in 0..n {
                    let off = if h.colour {
                        0
                    } else {
                        k as u64 * h.plane_bytes()
                    };
                    items.push(Item {
                        file: u32::try_from(i).map_err(|_| {
                            Error::Invalid("too many FITS files in one capture".into())
                        })?,
                        offset: h.offset + off,
                        bitpix: h.bitpix,
                        bzero: h.bzero,
                        bscale: h.bscale,
                        colour: h.colour,
                    });
                    frame_times.push(t);
                }
                if first_date.is_none() {
                    first_date.clone_from(&h.date);
                }
                if h.date.is_some() {
                    last_date.clone_from(&h.date);
                }
            }
        }
        Ok(Self {
            files: found.into_iter().map(|(f, _)| f).collect(),
            frames: items.len(),
            width,
            height,
            bitpix: first.bitpix,
            bzero: first.bzero,
            bscale: first.bscale,
            bayerpat: first.bayerpat.clone(),
            roworder: first.roworder.clone(),
            bottom_up,
            row_rule,
            by_time,
            times: frame_times,
            first_date,
            last_date,
            exposure_s: first.exposure_s,
            multi_frame_files: multi,
            pointing: first.pointing,
            items,
        })
    }

    /// The mosaic named by the header, as stored (first stored row first).
    #[must_use]
    pub fn stored_pattern(&self) -> Option<BayerPattern> {
        let m = self
            .bayerpat
            .as_ref()
            .map(|p| BTreeMap::from([("BAYERPAT".to_string(), p.clone())]))?;
        bayer_pattern(&m)
    }

    /// The pattern of the frames as read (top row first): the stored pattern,
    /// flipped vertically when the rows are flipped and the height is even.
    #[must_use]
    pub fn read_pattern(&self) -> Option<BayerPattern> {
        let p = self.stored_pattern()?;
        Some(if self.bottom_up && self.height.is_multiple_of(2) {
            flip_pattern(p)
        } else {
            p
        })
    }

    /// `ROWORDER` is present but neither `TOP-DOWN` nor `BOTTOM-UP`.
    #[must_use]
    pub fn roworder_unrecognised(&self) -> bool {
        self.roworder.as_deref().is_some_and(|r| {
            !r.trim().eq_ignore_ascii_case("TOP-DOWN")
                && !r.trim().eq_ignore_ascii_case("BOTTOM-UP")
        })
    }

    /// `BAYERPAT` is present but not RGGB, GRBG, GBRG or BGGR.
    #[must_use]
    pub fn bayerpat_unrecognised(&self) -> bool {
        self.bayerpat.is_some() && self.stored_pattern().is_none()
    }

    /// Open the frames for reading with `bayer` as their mosaic.
    #[must_use]
    pub fn into_source(self, bayer: Option<BayerPattern>) -> FitsSource {
        let desc = self.describe();
        FitsSource {
            frames: Arc::new(FitsFrames {
                files: self.files,
                items: self.items,
                width: self.width,
                height: self.height,
                flip: self.bottom_up,
                bayer,
                cache: DeviceCache::default(),
            }),
            desc,
        }
    }

    fn describe(&self) -> String {
        let depth = match self.bitpix {
            8 => "8-bit".to_string(),
            16 => "16-bit".to_string(),
            _ => "32-bit float".to_string(),
        };
        let files = if self.files.len() == 1 {
            "1 file".to_string()
        } else {
            format!("{} files", self.files.len())
        };
        format!(
            "FITS {}x{} {depth}, {} frames in {files}",
            self.width, self.height, self.frames
        )
    }
}

/// Frames of a FITS capture, read on demand from their files.
#[derive(Debug)]
pub struct FitsFrames {
    files: Vec<PathBuf>,
    items: Vec<Item>,
    width: usize,
    height: usize,
    flip: bool,
    bayer: Option<BayerPattern>,
    cache: DeviceCache,
}

impl FitsFrames {
    /// Frame `index` as normalised samples, top row first.
    fn load(&self, index: usize) -> Result<Vec<f32>> {
        let it = self.items.get(index).ok_or_else(|| {
            Error::Invalid(format!("frame {index} out of range ({})", self.items.len()))
        })?;
        let path = &self.files[it.file as usize];
        let planes = if it.colour { 3 } else { 1 };
        let n = self.width * self.height;
        let mut raw = vec![0u8; n * planes * (it.bitpix.unsigned_abs() / 8) as usize];
        let read = File::open(path).and_then(|f| pread_exact(&f, &mut raw, it.offset));
        read.map_err(|e| {
            Error::Invalid(format!(
                "cannot read frame {index} from {}: {e}; the file may have been moved, cut short or changed since it was opened",
                name_of(path)
            ))
        })?;
        let all = decode_samples(&raw, it.bitpix, it.bzero, it.bscale);
        let mut data = if planes == 3 {
            let third = 1.0 / 3.0;
            (0..n)
                .map(|i| (all[i] + all[n + i] + all[2 * n + i]) * third)
                .collect()
        } else {
            all
        };
        if self.flip {
            let w = self.width;
            for y in 0..self.height / 2 {
                let (a, b) = data.split_at_mut((self.height - 1 - y) * w);
                a[y * w..(y + 1) * w].swap_with_slice(&mut b[..w]);
            }
        }
        Ok(data)
    }
}

impl SharedFrames for FitsFrames {
    fn frame_count(&self) -> usize {
        self.items.len()
    }

    fn layout(&self) -> RowLayout {
        RowLayout {
            width: self.width,
            height: self.height,
            sample: Sample::F32,
            planes: Planes::One,
            stride: self.width * 4,
            bottom_up: false,
        }
    }

    fn bayer(&self) -> Option<BayerPattern> {
        self.bayer
    }

    fn device_cache(&self) -> Option<&DeviceCache> {
        Some(&self.cache)
    }

    fn read_raw(&self, index: usize, buf: &mut Vec<u8>) -> Result<()> {
        let data = self.load(index)?;
        buf.clear();
        buf.extend(data.iter().flat_map(|v| v.to_le_bytes()));
        Ok(())
    }
}

/// A FITS capture as a [`FrameSource`].
pub struct FitsSource {
    frames: Arc<FitsFrames>,
    desc: String,
}

impl FrameSource for FitsSource {
    fn dimensions(&self) -> (usize, usize) {
        (self.frames.width, self.frames.height)
    }
    fn frame_count(&self) -> usize {
        self.frames.items.len()
    }
    fn read(&mut self, index: usize) -> Result<Frame> {
        Ok(Frame {
            width: self.frames.width,
            height: self.frames.height,
            data: self.frames.load(index)?,
            bayer: self.frames.bayer,
        })
    }
    fn describe(&self) -> String {
        self.desc.clone()
    }
    fn shared(&self) -> Option<&dyn SharedFrames> {
        Some(self.frames.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::io::fits::{FitsFormat, HeaderCard, write_fits_cards, write_fits_planes};

    /// A unique folder under the temp dir, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let p = std::env::temp_dir()
                .join(format!("helia-fitsseq-test-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).expect("create temp dir");
            Self(p)
        }
        fn file(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn frame(w: usize, h: usize, k: usize) -> Frame {
        Frame {
            width: w,
            height: h,
            data: (0..w * h)
                .map(|i| ((i * 7 + k * 13) % 251) as f32 / 251.0)
                .collect(),
            bayer: None,
        }
    }

    fn write(path: &Path, f: &Frame, fmt: FitsFormat, cards: &[HeaderCard]) {
        let mut v = Vec::new();
        write_fits_cards(&mut v, f, fmt, cards, &[]).expect("encode");
        std::fs::write(path, v).expect("write");
    }

    fn date(t: &str) -> HeaderCard {
        HeaderCard::string("DATE-OBS", t, "")
    }

    fn read_all(path: &Path) -> Vec<Frame> {
        let scan = Scan::read(path).unwrap_or_else(|e| panic!("{e}"));
        let mut src = scan.clone().into_source(scan.read_pattern());
        (0..src.frame_count())
            .map(|i| src.read(i).unwrap_or_else(|e| panic!("{e}")))
            .collect()
    }

    /// An image extension appended by hand: the writer makes single units.
    fn image_extension(f: &Frame, extra: &str) -> Vec<u8> {
        let mut cards = vec![
            "XTENSION= 'IMAGE   '".to_string(),
            "BITPIX  =                  -32".to_string(),
            "NAXIS   =                    2".to_string(),
            format!("NAXIS1  = {:>20}", f.width),
            format!("NAXIS2  = {:>20}", f.height),
            "PCOUNT  =                    0".to_string(),
            "GCOUNT  =                    1".to_string(),
        ];
        if !extra.is_empty() {
            cards.push(extra.to_string());
        }
        cards.push("END".to_string());
        let mut out = Vec::new();
        for c in cards {
            out.extend(format!("{c:<80}").bytes());
        }
        out.resize(out.len().div_ceil(2880) * 2880, b' ');
        for y in (0..f.height).rev() {
            for v in f.row(y) {
                out.extend(v.to_be_bytes());
            }
        }
        out.resize(out.len().div_ceil(2880) * 2880, 0);
        out
    }

    #[test]
    fn a_folder_of_subs_reads_in_natural_name_order_with_exact_samples() {
        let d = TempDir::new("folder");
        for k in [10usize, 2, 1] {
            write(
                &d.file(&format!("sub_{k}.fit")),
                &frame(14, 9, k),
                FitsFormat::Float32,
                &[],
            );
        }
        std::fs::write(d.file("notes.txt"), b"ignored").expect("write");
        let got = read_all(&d.0);
        assert_eq!(got.len(), 3);
        for (g, k) in got.iter().zip([1, 2, 10]) {
            assert_eq!(g.data, frame(14, 9, k).data, "sub {k}");
        }
    }

    #[test]
    fn a_pattern_matches_only_its_files() {
        let d = TempDir::new("glob");
        for name in ["Light_a.fit", "Light_b.FIT", "Dark_a.fit"] {
            write(&d.file(name), &frame(8, 8, 1), FitsFormat::Float32, &[]);
        }
        let got = read_all(&d.file("light_*.fit"));
        assert_eq!(got.len(), 2);
        let err = Scan::read(&d.file("nothing_*.fit")).expect_err("no match");
        assert!(err.to_string().contains("no files match"), "{err}");
    }

    #[test]
    fn date_obs_orders_the_files_before_the_name_does() {
        let d = TempDir::new("order");
        // Names run the wrong way round; the times run a, b, c.
        for (name, k, t) in [
            ("c.fit", 3, "2026-03-21T00:00:01"),
            ("b.fit", 2, "2026-03-21T00:00:02"),
            ("a.fit", 1, "2026-03-21T00:00:03"),
        ] {
            write(
                &d.file(name),
                &frame(8, 8, k),
                FitsFormat::Float32,
                &[date(t)],
            );
        }
        let scan = Scan::read(&d.0).expect("scan");
        assert!(scan.by_time);
        let order: Vec<_> = scan.files.iter().map(|f| name_of(f)).collect();
        assert_eq!(order, ["c.fit", "b.fit", "a.fit"]);
        assert!(scan.times.windows(2).all(|w| w[0] < w[1]));
        // One file without a time puts the whole set back in name order.
        write(&d.file("d.fit"), &frame(8, 8, 4), FitsFormat::Float32, &[]);
        let scan = Scan::read(&d.0).expect("scan");
        assert!(!scan.by_time);
        assert_eq!(name_of(&scan.files[0]), "a.fit");
    }

    #[test]
    fn a_cube_gives_one_frame_per_plane_and_an_rgb_cube_one_colour_frame() {
        let d = TempDir::new("cube");
        let planes: Vec<Frame> = (0..5).map(|k| frame(10, 7, k)).collect();
        let refs: Vec<&Frame> = planes.iter().collect();
        let mut v = Vec::new();
        write_fits_planes(&mut v, &refs, FitsFormat::Float32, &[], &[]).expect("encode");
        std::fs::write(d.file("cube.fits"), v).expect("write");
        let got = read_all(&d.file("cube.fits"));
        assert_eq!(got.len(), 5);
        for (g, p) in got.iter().zip(&planes) {
            assert_eq!(g.data, p.data);
        }
        // Three planes with CTYPE3 = RGB are one colour frame, averaged.
        let mut v = Vec::new();
        write_fits_planes(&mut v, &refs[..3], FitsFormat::Float32, &[], &[]).expect("encode");
        std::fs::write(d.file("rgb.fits"), v).expect("write");
        let got = read_all(&d.file("rgb.fits"));
        assert_eq!(got.len(), 1);
        let want = (planes[0].data[5] + planes[1].data[5] + planes[2].data[5]) / 3.0;
        assert!((got[0].data[5] - want).abs() < 1e-6);
    }

    #[test]
    fn image_extensions_are_frames_and_tables_are_skipped() {
        let d = TempDir::new("hdu");
        let mut v = Vec::new();
        // A primary image, then two image extensions.
        write_fits_cards(&mut v, &frame(12, 6, 0), FitsFormat::Float32, &[], &[]).expect("encode");
        v.extend(image_extension(&frame(12, 6, 1), ""));
        v.extend(image_extension(&frame(12, 6, 2), ""));
        std::fs::write(d.file("multi.fits"), v).expect("write");
        let scan = Scan::read(&d.file("multi.fits")).expect("scan");
        assert_eq!(scan.frames, 3);
        let got = read_all(&d.file("multi.fits"));
        for (k, g) in got.iter().enumerate() {
            assert_eq!(g.data, frame(12, 6, k).data, "unit {k}");
        }
    }

    #[test]
    fn bzero_scaling_and_bayer_order_follow_the_header() {
        let d = TempDir::new("bayer");
        let f = frame(16, 10, 3);
        // 16-bit with BZERO 32768, as the Seestar writes.
        let cards = [HeaderCard::string("BAYERPAT", "GRBG", "")];
        write(&d.file("a.fit"), &f, FitsFormat::Int16, &cards);
        let scan = Scan::read(&d.file("a.fit")).expect("scan");
        assert_eq!(scan.bzero, 32768.0);
        // Bottom-up, even height: the stored GRBG reads as BGGR.
        assert_eq!(scan.read_pattern(), Some(BayerPattern::Bggr));
        let got = read_all(&d.file("a.fit"));
        let err = f
            .data
            .iter()
            .zip(&got[0].data)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(err <= 1.0 / 65535.0 + 1e-7, "{err}");
        assert_eq!(got[0].bayer, Some(BayerPattern::Bggr));
        // TOP-DOWN keeps the pattern; an odd height keeps it too.
        let cards = [
            HeaderCard::string("BAYERPAT", "GRBG", ""),
            HeaderCard::string("ROWORDER", "TOP-DOWN", ""),
        ];
        write(&d.file("b.fit"), &f, FitsFormat::Int16, &cards);
        let scan = Scan::read(&d.file("b.fit")).expect("scan");
        assert_eq!(scan.read_pattern(), Some(BayerPattern::Grbg));
        write(
            &d.file("c.fit"),
            &frame(15, 9, 0),
            FitsFormat::Int16,
            &[HeaderCard::string("BAYERPAT", "RGGB", "")],
        );
        assert_eq!(
            Scan::read(&d.file("c.fit")).expect("scan").read_pattern(),
            Some(BayerPattern::Rggb)
        );
    }

    fn flipped(f: &Frame) -> Vec<f32> {
        f.data
            .chunks(f.width)
            .rev()
            .flat_map(<[f32]>::to_vec)
            .collect()
    }

    /// The writer stores the bottom row first, so `flipped` is the file as stored.
    /// Rows as `Scan::read_with` hands them to the reader.
    fn read_rows(path: &Path, order: RowOrder) -> (Scan, Vec<f32>) {
        let scan = Scan::read_with(path, order).unwrap_or_else(|e| panic!("{e}"));
        let mut src = scan.clone().into_source(scan.read_pattern());
        let f = src.read(0).unwrap_or_else(|e| panic!("{e}"));
        (scan, f.data)
    }

    #[test]
    fn row_order_auto_reads_seestar_top_down_and_the_rest_bottom_up() {
        let d = TempDir::new("roworder");
        let f = frame(16, 10, 5);
        let seestar = [
            HeaderCard::string("INSTRUME", "Seestar S50", ""),
            HeaderCard::string("TELESCOP", "S50_64dbf865", ""),
            HeaderCard::string("BAYERPAT", "GRBG", ""),
        ];
        write(&d.file("s.fit"), &f, FitsFormat::Float32, &seestar);
        // A Seestar header with no ROWORDER card: nothing is flipped.
        let (scan, got) = read_rows(&d.file("s.fit"), RowOrder::Auto);
        assert!(!scan.bottom_up);
        assert_eq!(scan.row_rule, RowRule::Seestar);
        assert_eq!(scan.read_pattern(), Some(BayerPattern::Grbg));
        assert_eq!(got, flipped(&f));
        // Only the telescope name says S50: same result.
        write(
            &d.file("t.fit"),
            &f,
            FitsFormat::Float32,
            &[HeaderCard::string("TELESCOP", "S50_0123abcd", "")],
        );
        let (scan, got) = read_rows(&d.file("t.fit"), RowOrder::Auto);
        assert_eq!((scan.bottom_up, scan.row_rule), (false, RowRule::Seestar));
        assert_eq!(got, flipped(&f));
        // Any other camera: bottom-up per the FITS standard, rows flipped.
        write(
            &d.file("o.fit"),
            &f,
            FitsFormat::Float32,
            &[HeaderCard::string("INSTRUME", "ZWO ASI533MC", "")],
        );
        let (scan, got) = read_rows(&d.file("o.fit"), RowOrder::Auto);
        assert_eq!((scan.bottom_up, scan.row_rule), (true, RowRule::Standard));
        assert_eq!(got, f.data);
        // No camera named at all is the same.
        write(&d.file("n.fit"), &f, FitsFormat::Float32, &[]);
        let (scan, got) = read_rows(&d.file("n.fit"), RowOrder::Auto);
        assert!(scan.bottom_up);
        assert_eq!(got, f.data);
    }

    #[test]
    fn row_order_card_beats_the_seestar_rule_and_explicit_values_beat_both() {
        let d = TempDir::new("roworder2");
        let f = frame(16, 10, 6);
        let seestar = |order: &str| {
            vec![
                HeaderCard::string("INSTRUME", "Seestar S50", ""),
                HeaderCard::string("ROWORDER", order, ""),
            ]
        };
        // The card wins in auto, either way.
        write(
            &d.file("up.fit"),
            &f,
            FitsFormat::Float32,
            &seestar("BOTTOM-UP"),
        );
        let (scan, got) = read_rows(&d.file("up.fit"), RowOrder::Auto);
        assert_eq!((scan.bottom_up, scan.row_rule), (true, RowRule::Card));
        assert_eq!(got, f.data);
        write(
            &d.file("down.fit"),
            &f,
            FitsFormat::Float32,
            &[
                HeaderCard::string("INSTRUME", "ZWO ASI533MC", ""),
                HeaderCard::string("ROWORDER", "top-down", ""),
            ],
        );
        let (scan, got) = read_rows(&d.file("down.fit"), RowOrder::Auto);
        assert_eq!((scan.bottom_up, scan.row_rule), (false, RowRule::Card));
        assert_eq!(got, flipped(&f));
        // An explicit value overrides the card, the Seestar rule and the standard.
        let (scan, got) = read_rows(&d.file("up.fit"), RowOrder::TopDown);
        assert_eq!((scan.bottom_up, scan.row_rule), (false, RowRule::Requested));
        assert_eq!(got, flipped(&f));
        let (scan, got) = read_rows(&d.file("down.fit"), RowOrder::BottomUp);
        assert_eq!((scan.bottom_up, scan.row_rule), (true, RowRule::Requested));
        assert_eq!(got, f.data);
        write(
            &d.file("s.fit"),
            &f,
            FitsFormat::Float32,
            &[HeaderCard::string("INSTRUME", "Seestar S50", "")],
        );
        let (_, got) = read_rows(&d.file("s.fit"), RowOrder::BottomUp);
        assert_eq!(got, f.data);
        write(&d.file("n.fit"), &f, FitsFormat::Float32, &[]);
        let (_, got) = read_rows(&d.file("n.fit"), RowOrder::TopDown);
        assert_eq!(got, flipped(&f));
        // An unrecognised card is ignored, so the Seestar rule still applies.
        write(
            &d.file("odd.fit"),
            &f,
            FitsFormat::Float32,
            &seestar("SIDEWAYS"),
        );
        let (scan, _) = read_rows(&d.file("odd.fit"), RowOrder::Auto);
        assert_eq!((scan.bottom_up, scan.row_rule), (false, RowRule::Seestar));
        assert!(scan.roworder_unrecognised());
    }

    #[test]
    fn row_order_parses_the_words_the_cli_and_api_use() {
        for (s, want) in [
            ("auto", RowOrder::Auto),
            ("Top-Down", RowOrder::TopDown),
            ("bottom-up", RowOrder::BottomUp),
        ] {
            assert_eq!(s.parse::<RowOrder>().expect("parse"), want);
            assert_eq!(want.label().parse::<RowOrder>().expect("label"), want);
        }
        let err = "sideways".parse::<RowOrder>().expect_err("unknown");
        assert!(err.to_string().contains("auto, top-down or bottom-up"));
    }

    #[test]
    fn mixed_sizes_and_patterns_are_refused_in_plain_words() {
        let d = TempDir::new("mixed");
        write(&d.file("a.fit"), &frame(8, 8, 1), FitsFormat::Float32, &[]);
        write(&d.file("b.fit"), &frame(9, 8, 1), FitsFormat::Float32, &[]);
        let err = Scan::read(&d.0).expect_err("sizes differ").to_string();
        assert!(err.contains("not all the same size"), "{err}");
        assert!(err.contains("b.fit") && err.contains("9x8"), "{err}");
        write(
            &d.file("b.fit"),
            &frame(8, 8, 1),
            FitsFormat::Float32,
            &[HeaderCard::string("BAYERPAT", "RGGB", "")],
        );
        let err = Scan::read(&d.0).expect_err("patterns differ").to_string();
        assert!(err.contains("BAYERPAT"), "{err}");
    }

    #[test]
    fn damaged_files_are_refused_not_panicked_on() {
        let d = TempDir::new("bad");
        let mut good = Vec::new();
        write_fits_cards(&mut good, &frame(8, 8, 1), FitsFormat::Float32, &[], &[])
            .expect("encode");
        // Every prefix and a few corruptions: an error or a result, never a panic.
        for cut in (0..good.len()).step_by(97) {
            let _ = scan_hdus(Cursor::new(&good[..cut]));
        }
        for at in (0..2880).step_by(37) {
            let mut v = good.clone();
            v[at] = v[at].wrapping_add(0x55);
            let _ = scan_hdus(Cursor::new(v));
        }
        std::fs::write(d.file("junk.fit"), vec![0xAAu8; 5000]).expect("write");
        let err = Scan::read(&d.file("junk.fit"))
            .expect_err("junk")
            .to_string();
        assert!(
            err.contains("junk.fit") && err.contains("not a FITS file"),
            "{err}"
        );
        std::fs::write(d.file("cut.fit"), &good[..3000]).expect("write");
        let err = Scan::read(&d.file("cut.fit")).expect_err("cut").to_string();
        assert!(
            err.contains("cut.fit") && err.contains("truncated"),
            "{err}"
        );
        // An empty folder.
        let e = TempDir::new("empty");
        let err = Scan::read(&e.0).expect_err("empty").to_string();
        assert!(err.contains("no FITS frames"), "{err}");
    }

    #[test]
    fn natural_order_compares_numbers_as_numbers() {
        use std::cmp::Ordering::{Greater, Less};
        assert_eq!(natural_cmp("a2.fit", "a10.fit"), Less);
        assert_eq!(natural_cmp("a010.fit", "a9.fit"), Greater);
        assert_eq!(natural_cmp("A1.fit", "a2.fit"), Less);
    }
}
