// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Minimal FITS support: one 2-D image in the primary HDU.
//!
//! Reads `BITPIX` 8, 16 and -32 with `BZERO`/`BSCALE`. Integer data is mapped
//! to 0..1 by dividing the physical value by the unsigned full scale (255 or
//! 65535); floating point data is passed through unchanged. The first FITS row
//! is the bottom of the image, so rows are flipped on read and write.
//!
//! Sizes are checked against the stream length before allocating.

#![allow(
    clippy::many_single_char_names,
    reason = "x, y, w, h, dx, dy and r, g, b are the natural names in image and kernel maths"
)]

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom, Write};

use crate::frame::{BayerPattern, Frame};

const BLOCK: usize = 2880;
const CARD: usize = 80;
/// Header blocks accepted before giving up on finding `END`.
const MAX_HEADER_BLOCKS: usize = 256;
/// Largest accepted axis length.
pub const MAX_DIMENSION: i64 = 65_535;

/// FITS parsing failures.
#[derive(Debug, thiserror::Error)]
pub enum FitsError {
    /// Stream ended before a complete header block.
    #[error("FITS header truncated")]
    TruncatedHeader,
    /// First card is not `SIMPLE  = T`.
    #[error("not a FITS file (missing SIMPLE = T)")]
    NotFits,
    /// No `END` card within the allowed header size.
    #[error("FITS header has no END card within {MAX_HEADER_BLOCKS} blocks")]
    NoEnd,
    /// Required keyword missing or malformed.
    #[error("missing or invalid FITS keyword {0}")]
    BadKeyword(&'static str),
    /// Data type not handled.
    #[error("unsupported BITPIX {0}; supported: 8, 16, -32")]
    UnsupportedBitpix(i64),
    /// Not a single 2-D image.
    #[error("unsupported image shape (NAXIS = {0}); need a single 2-D image")]
    UnsupportedShape(i64),
    /// Axis length outside `1..=MAX_DIMENSION`.
    #[error("invalid image dimensions {width}x{height}")]
    BadDimensions {
        /// NAXIS1.
        width: i64,
        /// NAXIS2.
        height: i64,
    },
    /// Data unit shorter than the header implies.
    #[error("FITS data truncated: need {expected} bytes, stream has {actual}")]
    TruncatedData {
        /// Bytes required.
        expected: u64,
        /// Bytes available.
        actual: u64,
    },
    /// I/O failure.
    #[error("FITS I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Sample format for [`write_fits`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FitsFormat {
    /// Unsigned 16-bit via `BITPIX = 16`, `BZERO = 32768`.
    Int16,
    /// IEEE single precision, `BITPIX = -32`.
    Float32,
}

fn parse_header<R: Read>(r: &mut R) -> Result<BTreeMap<String, String>, FitsError> {
    let mut map = BTreeMap::new();
    let mut first = true;
    let mut block = [0u8; BLOCK];
    for _ in 0..MAX_HEADER_BLOCKS {
        r.read_exact(&mut block).map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                FitsError::TruncatedHeader
            } else {
                FitsError::Io(e)
            }
        })?;
        for card in block.as_chunks::<CARD>().0 {
            let key = String::from_utf8_lossy(&card[..8]).trim().to_string();
            if first {
                let ok = key == "SIMPLE" && card_value(card).as_deref() == Some("T");
                if !ok {
                    return Err(FitsError::NotFits);
                }
                first = false;
            }
            if key == "END" {
                return Ok(map);
            }
            if let Some(v) = card_value(card) {
                map.entry(key).or_insert(v);
            }
        }
    }
    Err(FitsError::NoEnd)
}

/// Value field of a card, or `None` for commentary cards.
fn card_value(card: &[u8]) -> Option<String> {
    if card.len() < 10 || &card[8..10] != b"= " {
        return None;
    }
    let s = String::from_utf8_lossy(&card[10..]);
    let s = s.trim_start();
    if let Some(rest) = s.strip_prefix('\'') {
        // Quoted string; '' is an escaped quote.
        let mut out = String::new();
        let mut chars = rest.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\'' {
                if chars.peek() == Some(&'\'') {
                    out.push('\'');
                    chars.next();
                } else {
                    break;
                }
            } else {
                out.push(c);
            }
        }
        return Some(out.trim_end().to_string());
    }
    Some(s.split('/').next().unwrap_or("").trim().to_string())
}

fn int(map: &BTreeMap<String, String>, key: &'static str) -> Result<i64, FitsError> {
    map.get(key)
        .and_then(|v| v.parse().ok())
        .ok_or(FitsError::BadKeyword(key))
}

fn float_or(map: &BTreeMap<String, String>, key: &str, default: f64) -> f64 {
    map.get(key)
        .and_then(|v| v.replace(['D', 'd'], "E").parse().ok())
        .unwrap_or(default)
}

/// Read the primary-HDU image.
///
/// # Errors
/// [`FitsError`] for malformed, unsupported or truncated input.
pub fn read_fits<R: Read + Seek>(r: R) -> Result<Frame, FitsError> {
    read_fits_header(r).map(|(f, _)| f)
}

/// Read the primary-HDU image and the value of every keyword that has one
/// (the first occurrence of each; strings unquoted, comments dropped).
///
/// # Errors
/// [`FitsError`] for malformed, unsupported or truncated input, including a
/// cube of more than one plane (see [`read_fits_planes`]).
pub fn read_fits_header<R: Read + Seek>(
    r: R,
) -> Result<(Frame, BTreeMap<String, String>), FitsError> {
    let (mut planes, map) = read_fits_planes(r)?;
    if planes.len() != 1 {
        return Err(FitsError::UnsupportedShape(3));
    }
    Ok((planes.remove(0), map))
}

/// Read the primary-HDU image as its planes: one for a plain image, three for
/// a colour cube (`NAXIS` 3 with `NAXIS3` 3: red, green, blue), with the value
/// of every keyword that has one.
///
/// # Errors
/// [`FitsError`] for malformed, unsupported or truncated input.
pub fn read_fits_planes<R: Read + Seek>(
    mut r: R,
) -> Result<(Vec<Frame>, BTreeMap<String, String>), FitsError> {
    let total = r.seek(SeekFrom::End(0))?;
    r.seek(SeekFrom::Start(0))?;
    let map = parse_header(&mut r)?;
    let bitpix = int(&map, "BITPIX")?;
    if !matches!(bitpix, 8 | 16 | -32) {
        return Err(FitsError::UnsupportedBitpix(bitpix));
    }
    let naxis = int(&map, "NAXIS")?;
    let (w, h, count) = match naxis {
        2 => (int(&map, "NAXIS1")?, int(&map, "NAXIS2")?, 1),
        3 => match int(&map, "NAXIS3")? {
            n @ (1 | 3) => (int(&map, "NAXIS1")?, int(&map, "NAXIS2")?, n),
            _ => return Err(FitsError::UnsupportedShape(3)),
        },
        n => return Err(FitsError::UnsupportedShape(n)),
    };
    if !(1..=MAX_DIMENSION).contains(&w) || !(1..=MAX_DIMENSION).contains(&h) {
        return Err(FitsError::BadDimensions {
            width: w,
            height: h,
        });
    }
    let bytes_per = bitpix.unsigned_abs() / 8;
    let data_len = w as u64 * h as u64 * count as u64 * bytes_per;
    let data_start = r.stream_position()?;
    let available = total.saturating_sub(data_start);
    if data_len > available {
        return Err(FitsError::TruncatedData {
            expected: data_len,
            actual: available,
        });
    }
    let mut raw = vec![0u8; data_len as usize];
    r.read_exact(&mut raw)?;

    let bzero = float_or(&map, "BZERO", 0.0);
    let bscale = float_or(&map, "BSCALE", 1.0);
    let (w, h) = (w as usize, h as usize);
    let convert = |phys: f64, full: f64| (phys / full) as f32;
    let all: Vec<f32> = match bitpix {
        8 => raw
            .iter()
            .map(|&v| convert(bzero + bscale * f64::from(v), 255.0))
            .collect(),
        16 => raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| convert(bzero + bscale * f64::from(i16::from_be_bytes(*c)), 65535.0))
            .collect(),
        _ => raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| {
                let v = f32::from_be_bytes(*c);
                if v.is_finite() {
                    (bzero + bscale * f64::from(v)) as f32
                } else {
                    0.0
                }
            })
            .collect(),
    };
    let bayer = match map
        .get("BAYERPAT")
        .map(|s| s.trim().to_ascii_uppercase())
        .as_deref()
    {
        Some("RGGB") => Some(BayerPattern::Rggb),
        Some("GRBG") => Some(BayerPattern::Grbg),
        Some("GBRG") => Some(BayerPattern::Gbrg),
        Some("BGGR") => Some(BayerPattern::Bggr),
        _ => None,
    };
    let planes = all
        .chunks_exact(w * h)
        .map(|plane| {
            let mut data = plane.to_vec();
            // FITS stores the bottom row first.
            for y in 0..h / 2 {
                let (a, b) = data.split_at_mut((h - 1 - y) * w);
                a[y * w..(y + 1) * w].swap_with_slice(&mut b[..w]);
            }
            Frame {
                width: w,
                height: h,
                data,
                // A mosaic pattern describes a single plane.
                bayer: if count == 1 { bayer } else { None },
            }
        })
        .collect();
    Ok((planes, map))
}

fn card(key: &str, value: &str, out: &mut Vec<u8>) {
    let mut c = format!("{key:<8}= {value:>20}").into_bytes();
    c.resize(CARD, b' ');
    out.extend_from_slice(&c);
}

/// A header value.
#[derive(Clone, Debug, PartialEq)]
pub enum CardValue {
    /// `T` or `F`.
    Bool(bool),
    /// Integer.
    Int(i64),
    /// Real number (finite; anything else is written as an empty value).
    Float(f64),
    /// Text: printable ASCII, at most 68 characters after quote doubling.
    Str(String),
}

/// One header card: a keyword with a value and an optional comment, or (with
/// no value) a commentary card such as `COMMENT`.
#[derive(Clone, Debug, PartialEq)]
pub struct HeaderCard {
    /// Keyword, upper case, at most 8 characters.
    pub key: String,
    /// Value; `None` for commentary cards, whose text is `comment`.
    pub value: Option<CardValue>,
    /// Comment after the value, or the text of a commentary card.
    pub comment: String,
}

impl HeaderCard {
    /// Text value.
    #[must_use]
    pub fn string(key: &str, value: impl Into<String>, comment: &str) -> Self {
        Self::with(key, CardValue::Str(value.into()), comment)
    }

    /// Logical value.
    #[must_use]
    pub fn boolean(key: &str, value: bool, comment: &str) -> Self {
        Self::with(key, CardValue::Bool(value), comment)
    }

    /// Integer value.
    #[must_use]
    pub fn int(key: &str, value: i64, comment: &str) -> Self {
        Self::with(key, CardValue::Int(value), comment)
    }

    /// Real value.
    #[must_use]
    pub fn float(key: &str, value: f64, comment: &str) -> Self {
        Self::with(key, CardValue::Float(value), comment)
    }

    /// A `COMMENT` card (long text is split over several cards on writing).
    #[must_use]
    pub fn commentary(text: &str) -> Self {
        Self {
            key: "COMMENT".into(),
            value: None,
            comment: text.into(),
        }
    }

    fn with(key: &str, value: CardValue, comment: &str) -> Self {
        Self {
            key: key.into(),
            value: Some(value),
            comment: comment.into(),
        }
    }
}

/// Printable ASCII only; anything else becomes `?`.
fn ascii(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii() && !c.is_ascii_control() {
                c
            } else {
                '?'
            }
        })
        .collect()
}

/// Real number the way FITS wants it: shortest round-trip digits, an upper-case
/// exponent and always a decimal point or an exponent so it is not an integer.
fn float_text(v: f64) -> String {
    let t = format!("{v:?}").to_ascii_uppercase();
    if t.contains(['.', 'E']) {
        t
    } else {
        format!("{t}.0")
    }
}

/// Break `text` into lines of at most `width` characters at spaces; a word
/// longer than a line is split. Always at least one (possibly empty) line.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = vec![String::new()];
    for word in text.split_whitespace() {
        let mut rest = word;
        loop {
            let (piece, tail) = rest.split_at(rest.len().min(width));
            let fits = lines
                .last()
                .is_some_and(|l| l.is_empty() || l.len() + 1 + piece.len() <= width);
            if !fits {
                lines.push(String::new());
            }
            if let Some(last) = lines.last_mut() {
                if !last.is_empty() {
                    last.push(' ');
                }
                last.push_str(piece);
            }
            if tail.is_empty() {
                break;
            }
            lines.push(String::new());
            rest = tail;
        }
    }
    lines
}

/// Encode `c` as one or more 80-character cards.
fn encode_card(c: &HeaderCard, out: &mut Vec<u8>) {
    let key: String = ascii(&c.key).to_ascii_uppercase().chars().take(8).collect();
    let Some(value) = &c.value else {
        for chunk in wrap(&ascii(&c.comment), 72) {
            let mut line = format!("{key:<8}{chunk}").into_bytes();
            line.resize(CARD, b' ');
            out.extend_from_slice(&line);
        }
        return;
    };
    let field = match value {
        CardValue::Bool(b) => format!("{:>20}", if *b { "T" } else { "F" }),
        CardValue::Int(i) => format!("{i:>20}"),
        CardValue::Float(f) if f.is_finite() => format!("{:>20}", float_text(*f)),
        CardValue::Float(_) => format!("{:>20}", ""),
        CardValue::Str(s) => {
            let mut q = String::new();
            for ch in ascii(s).chars() {
                let add = if ch == '\'' { 2 } else { 1 };
                if q.len() + add > 68 {
                    break;
                }
                if ch == '\'' {
                    q.push('\'');
                }
                q.push(ch);
            }
            format!("'{q:<8}'")
        }
    };
    let mut line = format!("{key:<8}= {field}");
    let comment = ascii(&c.comment);
    if !comment.is_empty() && line.len() + 3 < CARD {
        line.push_str(" / ");
        line.push_str(&comment);
    }
    let mut bytes = line.into_bytes();
    bytes.truncate(CARD);
    bytes.resize(CARD, b' ');
    out.extend_from_slice(&bytes);
}

/// Append `text` as `HISTORY` cards of up to 72 characters each, ASCII only.
fn history_cards(text: &str, out: &mut Vec<u8>) {
    let ascii: Vec<char> = ascii(text).chars().collect();
    for chunk in ascii.chunks(72) {
        let mut c = format!("HISTORY {}", chunk.iter().collect::<String>()).into_bytes();
        c.resize(CARD, b' ');
        out.extend_from_slice(&c);
    }
}

/// Write `frame` as a single-HDU FITS file.
///
/// # Errors
/// I/O failure.
pub fn write_fits<W: Write>(w: &mut W, frame: &Frame, format: FitsFormat) -> std::io::Result<()> {
    write_fits_with_history(w, frame, format, &[])
}

/// As [`write_fits`], with each of `history` as `HISTORY` cards (long lines
/// are split to fit the 80-character card). The pixel data is not touched.
///
/// # Errors
/// I/O failure.
pub fn write_fits_with_history<W: Write>(
    w: &mut W,
    frame: &Frame,
    format: FitsFormat,
    history: &[String],
) -> std::io::Result<()> {
    write_fits_cards(w, frame, format, &[], history)
}

/// As [`write_fits_with_history`], with `cards` written between the structural
/// keywords and `CREATOR`. Cards for keywords the writer owns (`SIMPLE`,
/// `BITPIX`, `NAXIS*`, `BZERO`, `BSCALE`, `END`) are dropped.
///
/// # Errors
/// I/O failure.
pub fn write_fits_cards<W: Write>(
    w: &mut W,
    frame: &Frame,
    format: FitsFormat,
    cards: &[HeaderCard],
    history: &[String],
) -> std::io::Result<()> {
    write_fits_planes(w, &[frame], format, cards, history)
}

/// As [`write_fits_cards`] for one plane or a cube of planes of the same size
/// (three for colour: red, green, blue, with `CTYPE3` saying so).
///
/// # Errors
/// I/O failure, or [`std::io::ErrorKind::InvalidInput`] for no planes or
/// planes of different sizes.
pub fn write_fits_planes<W: Write>(
    w: &mut W,
    planes: &[&Frame],
    format: FitsFormat,
    cards: &[HeaderCard],
    history: &[String],
) -> std::io::Result<()> {
    let Some(frame) = planes.first() else {
        return Err(std::io::ErrorKind::InvalidInput.into());
    };
    if planes
        .iter()
        .any(|p| p.width != frame.width || p.height != frame.height)
    {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    let mut hdr = Vec::with_capacity(BLOCK);
    card("SIMPLE", "T", &mut hdr);
    card(
        "BITPIX",
        if format == FitsFormat::Int16 {
            "16"
        } else {
            "-32"
        },
        &mut hdr,
    );
    card("NAXIS", if planes.len() > 1 { "3" } else { "2" }, &mut hdr);
    card("NAXIS1", &frame.width.to_string(), &mut hdr);
    card("NAXIS2", &frame.height.to_string(), &mut hdr);
    if planes.len() > 1 {
        card("NAXIS3", &planes.len().to_string(), &mut hdr);
    }
    if format == FitsFormat::Int16 {
        card("BZERO", "32768", &mut hdr);
        card("BSCALE", "1", &mut hdr);
    }
    card("CREATOR", "'Helia'", &mut hdr);
    if planes.len() == 3 {
        card("CTYPE3", "'RGB'", &mut hdr);
        let mut note = b"COMMENT plane 1 is red, plane 2 green and plane 3 blue".to_vec();
        note.resize(CARD, b' ');
        hdr.extend_from_slice(&note);
    }
    for c in cards {
        let k = c.key.trim().to_ascii_uppercase();
        let owned = matches!(
            k.as_str(),
            "SIMPLE"
                | "BITPIX"
                | "NAXIS"
                | "NAXIS1"
                | "NAXIS2"
                | "NAXIS3"
                | "BZERO"
                | "BSCALE"
                | "END"
        ) || (planes.len() == 3 && k == "CTYPE3");
        if !owned && !k.is_empty() {
            encode_card(c, &mut hdr);
        }
    }
    for line in history {
        history_cards(line, &mut hdr);
    }
    let mut end = b"END".to_vec();
    end.resize(CARD, b' ');
    hdr.extend_from_slice(&end);
    hdr.resize(hdr.len().div_ceil(BLOCK) * BLOCK, b' ');
    w.write_all(&hdr)?;

    let mut data = Vec::with_capacity(frame.data.len() * planes.len() * 4);
    for plane in planes {
        for y in (0..plane.height).rev() {
            for &v in plane.row(y) {
                match format {
                    FitsFormat::Int16 => {
                        let q = (v.clamp(0.0, 1.0) * 65535.0).round() as i32 - 32768;
                        data.extend_from_slice(&(q as i16).to_be_bytes());
                    }
                    FitsFormat::Float32 => data.extend_from_slice(&v.to_be_bytes()),
                }
            }
        }
    }
    data.resize(data.len().div_ceil(BLOCK) * BLOCK, 0);
    w.write_all(&data)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn pattern(w: usize, h: usize) -> Frame {
        let data = (0..w * h)
            .map(|i| (i as f32 * 0.37).sin() * 0.5 + 0.5)
            .collect();
        Frame {
            width: w,
            height: h,
            data,
            bayer: None,
        }
    }

    fn encode(f: &Frame, fmt: FitsFormat) -> Vec<u8> {
        let mut v = Vec::new();
        write_fits(&mut v, f, fmt).unwrap_or_else(|e| panic!("{e}"));
        v
    }

    #[test]
    fn float32_round_trip_is_exact() {
        let f = pattern(21, 14);
        let bytes = encode(&f, FitsFormat::Float32);
        assert_eq!(bytes.len() % BLOCK, 0);
        let g = read_fits(Cursor::new(bytes)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(g, f);
    }

    #[test]
    fn int16_round_trip_within_one_lsb() {
        let f = pattern(17, 11);
        let g =
            read_fits(Cursor::new(encode(&f, FitsFormat::Int16))).unwrap_or_else(|e| panic!("{e}"));
        let err = f
            .data
            .iter()
            .zip(&g.data)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(err <= 0.5 / 65535.0 + 1e-7, "{err}");
    }

    #[test]
    fn row_order_is_top_first() {
        let mut f = Frame::zeros(4, 3);
        f.data[0] = 1.0; // top-left
        let g = read_fits(Cursor::new(encode(&f, FitsFormat::Float32)))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(g.get(0, 0), 1.0);
        assert_eq!(g.get(0, 2), 0.0);
    }

    fn handmade(bitpix: i64, extra: &[(&str, &str)], data: &[u8], w: usize, h: usize) -> Vec<u8> {
        let mut hdr = Vec::new();
        card("SIMPLE", "T", &mut hdr);
        card("BITPIX", &bitpix.to_string(), &mut hdr);
        card("NAXIS", "2", &mut hdr);
        card("NAXIS1", &w.to_string(), &mut hdr);
        card("NAXIS2", &h.to_string(), &mut hdr);
        for (k, v) in extra {
            card(k, v, &mut hdr);
        }
        let mut end = b"END".to_vec();
        end.resize(CARD, b' ');
        hdr.extend_from_slice(&end);
        hdr.resize(BLOCK, b' ');
        hdr.extend_from_slice(data);
        hdr
    }

    #[test]
    fn eight_bit_and_bzero_bscale() {
        let b = handmade(8, &[], &[0, 255, 51, 102], 2, 2);
        let f = read_fits(Cursor::new(b)).unwrap_or_else(|e| panic!("{e}"));
        // Bottom row first: file row 0 = [0, 255] is the image's bottom row.
        assert_eq!(f.get(1, 1), 1.0);
        assert!((f.get(1, 0) - 102.0 / 255.0).abs() < 1e-6);
        // Signed 16-bit with BZERO 32768 maps to 0..1.
        let raw: Vec<u8> = [-32768i16, 32767, 0, 0]
            .iter()
            .flat_map(|v| v.to_be_bytes())
            .collect();
        let b = handmade(16, &[("BZERO", "32768"), ("BSCALE", "1")], &raw, 2, 2);
        let f = read_fits(Cursor::new(b)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(f.get(0, 1), 0.0);
        assert_eq!(f.get(1, 1), 1.0);
        assert!((f.get(0, 0) - 32768.0 / 65535.0).abs() < 1e-6);
    }

    #[test]
    fn rejects_malformed_input() {
        assert!(matches!(
            read_fits(Cursor::new(Vec::new())).err(),
            Some(FitsError::TruncatedHeader)
        ));
        assert!(matches!(
            read_fits(Cursor::new(vec![b'x'; BLOCK])).err(),
            Some(FitsError::NotFits)
        ));
        let good = encode(&pattern(8, 8), FitsFormat::Float32);
        assert!(matches!(
            read_fits(Cursor::new(good[..BLOCK + 10].to_vec())).err(),
            Some(FitsError::TruncatedData { .. })
        ));
        assert!(matches!(
            read_fits(Cursor::new(handmade(32, &[], &[0; 16], 2, 2))).err(),
            Some(FitsError::UnsupportedBitpix(32))
        ));
        // Huge claimed dimensions with a tiny data unit must not allocate.
        assert!(matches!(
            read_fits(Cursor::new(handmade(-32, &[], &[0; 16], 60_000, 60_000))).err(),
            Some(FitsError::TruncatedData { .. })
        ));
        assert!(matches!(
            read_fits(Cursor::new(handmade(8, &[], &[0; 4], 0, 2))).err(),
            Some(FitsError::BadDimensions { .. })
        ));
        // No END card at all.
        let mut noend = Vec::new();
        card("SIMPLE", "T", &mut noend);
        noend.resize(BLOCK, b' ');
        let long: Vec<u8> = noend
            .iter()
            .cycle()
            .take(BLOCK * (MAX_HEADER_BLOCKS + 1))
            .copied()
            .collect();
        assert!(read_fits(Cursor::new(long)).is_err());
    }

    fn encoded(c: &HeaderCard) -> String {
        let mut v = Vec::new();
        encode_card(c, &mut v);
        assert_eq!(v.len() % CARD, 0);
        String::from_utf8(v).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn cards_are_eighty_columns_in_fixed_format() {
        let t = encoded(&HeaderCard::string("TELESCOP", "Bob's 80/600", "[x] note"));
        assert_eq!(t.len(), 80);
        assert!(t.starts_with("TELESCOP= 'Bob''s 80/600' / [x] note"), "{t}");
        // Short strings are padded to eight characters inside the quotes.
        assert!(encoded(&HeaderCard::string("A", "UTC", "")).starts_with("A       = 'UTC     '"));
        let f = encoded(&HeaderCard::float("CDELT1", 11.5, "arcsec"));
        assert_eq!(&f[..30], "CDELT1  =                 11.5");
        assert!(encoded(&HeaderCard::float("X", 3.0, "")).contains("3.0"));
        assert!(encoded(&HeaderCard::float("X", 1e-7, "")).contains("1E-7"));
        assert_eq!(
            &encoded(&HeaderCard::int("NSUMEXP", -4, ""))[..30],
            "NSUMEXP =                   -4"
        );
        assert_eq!(
            &encoded(&HeaderCard::boolean("FLAG", true, ""))[..30],
            "FLAG    =                    T"
        );
        // Not finite: an empty (undefined) value, never NaN.
        assert!(!encoded(&HeaderCard::float("X", f64::NAN, "")).contains("NAN"));
    }

    #[test]
    fn long_text_is_cut_and_commentary_is_split() {
        let long = "x".repeat(200);
        let t = encoded(&HeaderCard::string("K", long, "comment"));
        assert_eq!(t.len(), 80);
        assert!(!t.contains("comment"), "no room left for the comment");
        // Quote doubling never pushes the closing quote off the card.
        let quotes = encoded(&HeaderCard::string("K", "\u{27}".repeat(100), ""));
        assert!(quotes.trim_end().ends_with('\u{27}'), "{quotes}");
        let c = encoded(&HeaderCard::commentary(&"y".repeat(150)));
        assert_eq!(c.len(), 240);
        assert!(c.starts_with("COMMENT yyyy"));
        // Words are not cut when a space is available.
        let words = encoded(&HeaderCard::commentary(&"word ".repeat(30)));
        assert!(words.as_bytes().chunks(80).all(|l| {
            let t = String::from_utf8_lossy(&l[8..]).trim().to_owned();
            t.split(' ').all(|w| w == "word")
        }));
        // Non-ASCII becomes '?'.
        assert!(encoded(&HeaderCard::string("K", "caf\u{e9}", "")).contains("caf?"));
    }

    #[test]
    fn extra_cards_do_not_disturb_the_image() {
        let f = pattern(9, 7);
        let mut plain = Vec::new();
        write_fits(&mut plain, &f, FitsFormat::Float32).unwrap_or_else(|e| panic!("{e}"));
        let cards: Vec<HeaderCard> = (0..60)
            .map(|i| HeaderCard::int(&format!("KEY{i}"), i, "filler"))
            .chain([HeaderCard::int("NAXIS1", 99, "must be ignored")])
            .collect();
        let mut rich = Vec::new();
        write_fits_cards(&mut rich, &f, FitsFormat::Float32, &cards, &["h".into()])
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(rich.len() > plain.len(), "the header grew to more blocks");
        assert_eq!(rich.len() % BLOCK, 0);
        let g = read_fits(Cursor::new(rich)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(g, f);
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        let mut state = 99_u32;
        for len in [0usize, 80, 2880, 3000, 6000] {
            for _ in 0..10 {
                let mut b: Vec<u8> = (0..len)
                    .map(|_| {
                        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                        (state >> 24) as u8
                    })
                    .collect();
                if len >= 80 {
                    b[..80].copy_from_slice(&{
                        let mut c = Vec::new();
                        card("SIMPLE", "T", &mut c);
                        c
                    });
                }
                let _ = read_fits(Cursor::new(b));
            }
        }
    }

    #[test]
    fn a_colour_cube_round_trips_plane_by_plane() {
        let planes = [pattern(21, 14), pattern(21, 14), pattern(21, 14)];
        let mut p1 = planes[1].clone();
        p1.data.iter_mut().for_each(|v| *v *= 0.5);
        let mut p2 = planes[2].clone();
        p2.data.iter_mut().for_each(|v| *v = 1.0 - *v);
        let cube = [planes[0].clone(), p1, p2];
        let mut bytes = Vec::new();
        write_fits_planes(
            &mut bytes,
            &[&cube[0], &cube[1], &cube[2]],
            FitsFormat::Float32,
            &[],
            &["three planes".to_owned()],
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(bytes.len() % BLOCK, 0);
        let (got, map) =
            read_fits_planes(Cursor::new(bytes.clone())).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(got, cube);
        assert_eq!(map.get("NAXIS3").map(String::as_str), Some("3"));
        assert_eq!(map.get("CTYPE3").map(String::as_str), Some("RGB"));
        // The single-plane reader refuses a cube rather than read one plane of it.
        assert!(matches!(
            read_fits(Cursor::new(bytes)),
            Err(FitsError::UnsupportedShape(_))
        ));
    }

    #[test]
    fn planes_of_different_sizes_are_not_written() {
        let (a, b) = (pattern(4, 4), pattern(5, 4));
        let mut v = Vec::new();
        assert!(write_fits_planes(&mut v, &[&a, &b], FitsFormat::Float32, &[], &[]).is_err());
        assert!(write_fits_planes(&mut v, &[], FitsFormat::Float32, &[], &[]).is_err());
    }
}
