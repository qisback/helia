// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Uncompressed AVI capture files (`SharpCap`, `FireCapture`, ...).
//!
//! Supported video: `BI_RGB`/`DIB ` or `Y800`/`GREY` 8-bit, `Y16 ` 16-bit
//! little-endian, and 24-bit BGR (reduced to the channel mean). Rows are padded
//! to 4 bytes; a positive `biHeight` means bottom-up and is flipped so frame
//! row 0 is the top.
//!
//! Frames are located by walking the `movi` lists of the first `RIFF AVI ` and of
//! every following `RIFF AVIX ` (`OpenDML`, files beyond 1 GB) instead of
//! trusting `idx1` or the `indx`/`ix##` indexes, which writers routinely get
//! wrong or leave out and which only duplicate what the chunks already say.
//! A walk reads one 8-byte header per frame.
//!
//! Raw Bayer 8-bit video is indistinguishable from mono in the container; the
//! caller supplies the pattern with [`AviReader::with_bayer`]. Because a
//! bottom-up mosaic is flipped on read, the pattern refers to the upright image.
//!
//! Sizes are validated against the stream length before allocation.

use std::io::{Read, Seek, SeekFrom};

use crate::frame::{BayerPattern, Frame};
use crate::io::raw::{FrameOffsets, Planes, RowLayout, Sample};

/// Largest accepted width or height.
pub const MAX_DIMENSION: i64 = 65_535;

/// AVI parsing failures.
#[derive(Debug, thiserror::Error)]
pub enum AviError {
    /// Not a RIFF/AVI stream.
    #[error("not an AVI file")]
    NotAvi,
    /// No video stream header found before the frame data.
    #[error("AVI has no video stream description")]
    NoVideoStream,
    /// Video format not handled.
    #[error("unsupported AVI video format: {0}")]
    UnsupportedFormat(String),
    /// Stream header too short or inconsistent.
    #[error("malformed AVI stream header")]
    BadHeader,
    /// Width or height outside `1..=MAX_DIMENSION`.
    #[error("invalid image dimensions {width}x{height}")]
    BadDimensions {
        /// Declared width.
        width: i64,
        /// Declared height.
        height: i64,
    },
    /// Frame index past the end.
    #[error("frame {index} out of range (file has {count})")]
    FrameOutOfRange {
        /// Requested index.
        index: usize,
        /// Frames in file.
        count: usize,
    },
    /// I/O failure.
    #[error("AVI I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Sample layout of the video stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// 8-bit single plane.
    Gray8,
    /// 16-bit little-endian single plane.
    Gray16,
    /// 24-bit B, G, R.
    Bgr24,
}

/// Parsed video stream description.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AviInfo {
    /// Frame width.
    pub width: u32,
    /// Frame height.
    pub height: u32,
    /// True when stored bottom-up (positive `biHeight`).
    pub bottom_up: bool,
    /// Sample layout.
    pub format: PixelFormat,
    /// Compression `FourCC` as written (`"DIB "` for `BI_RGB` fourcc 0).
    pub fourcc: String,
}

impl AviInfo {
    /// Bytes per padded row.
    #[must_use]
    pub fn stride(&self) -> u64 {
        let bits = match self.format {
            PixelFormat::Gray8 => 8,
            PixelFormat::Gray16 => 16,
            PixelFormat::Bgr24 => 24,
        };
        (u64::from(self.width) * bits).div_ceil(32) * 4
    }

    /// Bytes per frame.
    #[must_use]
    pub fn frame_bytes(&self) -> u64 {
        self.stride() * u64::from(self.height)
    }
}

type FourCc = [u8; 4];

fn rd_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn rd_u16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

struct Header {
    id: FourCc,
    size: u32,
    /// Offset of the payload.
    data: u64,
}

impl Header {
    /// Payload end clamped to `limit`.
    fn end(&self, limit: u64) -> u64 {
        (self.data + u64::from(self.size)).min(limit)
    }

    /// Offset of the next sibling chunk (payloads are padded to even size).
    fn next(&self, limit: u64) -> u64 {
        let e = self.data + u64::from(self.size);
        (e + (e & 1)).min(limit.max(e))
    }
}

fn header_at<R: Read + Seek>(r: &mut R, pos: u64, limit: u64) -> Result<Option<Header>, AviError> {
    if pos + 8 > limit {
        return Ok(None);
    }
    r.seek(SeekFrom::Start(pos))?;
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(Some(Header {
        id: [b[0], b[1], b[2], b[3]],
        size: rd_u32(&b, 4),
        data: pos + 8,
    }))
}

fn form_type<R: Read + Seek>(r: &mut R, h: &Header) -> Result<Option<FourCc>, AviError> {
    if h.size < 4 {
        return Ok(None);
    }
    r.seek(SeekFrom::Start(h.data))?;
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(Some(b))
}

#[derive(Default)]
struct Walk {
    info: Option<AviInfo>,
    video_stream: Option<u32>,
    streams_seen: u32,
    want_strf: bool,
    frames: Vec<(u64, u32)>,
}

impl Walk {
    fn hdrl<R: Read + Seek>(&mut self, r: &mut R, start: u64, end: u64) -> Result<(), AviError> {
        let mut p = start;
        while let Some(h) = header_at(r, p, end)? {
            if &h.id == b"LIST" && form_type(r, &h)? == Some(*b"strl") {
                self.strl(r, h.data + 4, h.end(end))?;
                self.streams_seen += 1;
            }
            p = h.next(end);
        }
        Ok(())
    }

    fn strl<R: Read + Seek>(&mut self, r: &mut R, start: u64, end: u64) -> Result<(), AviError> {
        let mut p = start;
        while let Some(h) = header_at(r, p, end)? {
            let n = h.size.min(512) as usize;
            if &h.id == b"strh" && self.video_stream.is_none() && n >= 4 {
                r.seek(SeekFrom::Start(h.data))?;
                let mut b = vec![0u8; n];
                r.read_exact(&mut b)?;
                if &b[0..4] == b"vids" {
                    self.video_stream = Some(self.streams_seen);
                    self.want_strf = true;
                }
            } else if &h.id == b"strf" && self.want_strf {
                self.want_strf = false;
                if h.size < 40 {
                    return Err(AviError::BadHeader);
                }
                r.seek(SeekFrom::Start(h.data))?;
                let mut b = [0u8; 40];
                r.read_exact(&mut b)?;
                self.info = Some(parse_bitmap_info(&b)?);
            }
            p = h.next(end);
        }
        Ok(())
    }

    fn movi<R: Read + Seek>(
        &mut self,
        r: &mut R,
        start: u64,
        end: u64,
        depth: u32,
    ) -> Result<(), AviError> {
        let (Some(info), Some(stream)) = (self.info.as_ref(), self.video_stream) else {
            return Err(AviError::NoVideoStream);
        };
        let need = info.frame_bytes();
        let mut p = start;
        while let Some(h) = header_at(r, p, end)? {
            if &h.id == b"LIST" && depth < 2 && form_type(r, &h)? == Some(*b"rec ") {
                self.movi(r, h.data + 4, h.end(end), depth + 1)?;
            } else if h.id[2] == b'd' && (h.id[3] == b'b' || h.id[3] == b'c') {
                let id = std::str::from_utf8(&h.id[..2])
                    .ok()
                    .and_then(|s| u32::from_str_radix(s, 16).ok());
                // Zero-length chunks are dropped frames; short ones are damaged.
                if id == Some(stream) && u64::from(h.size) >= need && h.data + need <= end {
                    self.frames.push((h.data, h.size));
                }
            }
            p = h.next(end);
        }
        Ok(())
    }
}

fn parse_bitmap_info(b: &[u8; 40]) -> Result<AviInfo, AviError> {
    let width = i64::from(i32::from_le_bytes([b[4], b[5], b[6], b[7]]));
    let height = i64::from(i32::from_le_bytes([b[8], b[9], b[10], b[11]]));
    let bits = rd_u16(b, 14);
    let tag: FourCc = [b[16], b[17], b[18], b[19]];
    let w_abs = width;
    let h_abs = height.abs();
    if !(1..=MAX_DIMENSION).contains(&w_abs) || !(1..=MAX_DIMENSION).contains(&h_abs) {
        return Err(AviError::BadDimensions { width, height });
    }
    let fourcc = if tag == [0; 4] {
        "DIB ".to_string()
    } else {
        String::from_utf8_lossy(&tag).into_owned()
    };
    let format = match (fourcc.as_str(), bits) {
        ("DIB " | "RGB " | "Y800" | "GREY" | "Y8  ", 8) => PixelFormat::Gray8,
        ("Y16 ", 16) => PixelFormat::Gray16,
        ("DIB " | "RGB ", 24) => PixelFormat::Bgr24,
        _ => {
            return Err(AviError::UnsupportedFormat(format!(
                "{fourcc:?} at {bits} bits per pixel"
            )));
        }
    };
    Ok(AviInfo {
        width: w_abs as u32,
        height: h_abs as u32,
        bottom_up: height > 0,
        format,
        fourcc,
    })
}

/// Random-access reader for uncompressed AVI video.
pub struct AviReader<R> {
    r: R,
    info: AviInfo,
    frames: Vec<(u64, u32)>,
    bayer: Option<BayerPattern>,
}

impl<R: Read + Seek> AviReader<R> {
    /// Parse the container and index the video frames.
    ///
    /// # Errors
    /// [`AviError`] for malformed or unsupported files.
    pub fn new(mut r: R) -> Result<Self, AviError> {
        let len = r.seek(SeekFrom::End(0))?;
        let mut walk = Walk::default();
        let mut pos = 0u64;
        let mut riffs = 0u32;
        while let Some(h) = header_at(&mut r, pos, len)? {
            if &h.id != b"RIFF" {
                break;
            }
            let form = form_type(&mut r, &h)?;
            let expect: &[u8; 4] = if riffs == 0 { b"AVI " } else { b"AVIX" };
            if form.as_ref() != Some(expect) {
                if riffs == 0 {
                    return Err(AviError::NotAvi);
                }
                break;
            }
            let end = h.end(len);
            let mut p = h.data + 4;
            while let Some(c) = header_at(&mut r, p, end)? {
                if &c.id == b"LIST" {
                    match form_type(&mut r, &c)?.as_ref() {
                        Some(b"hdrl") => walk.hdrl(&mut r, c.data + 4, c.end(end))?,
                        Some(b"movi") => walk.movi(&mut r, c.data + 4, c.end(end), 0)?,
                        _ => {}
                    }
                }
                p = c.next(end);
            }
            riffs += 1;
            pos = h.next(len);
        }
        if riffs == 0 {
            return Err(AviError::NotAvi);
        }
        let info = walk.info.ok_or(AviError::NoVideoStream)?;
        Ok(Self {
            r,
            info,
            frames: walk.frames,
            bayer: None,
        })
    }

    /// Tag decoded 8/16-bit frames with a Bayer pattern (or clear it).
    #[must_use]
    pub fn with_bayer(mut self, pattern: Option<BayerPattern>) -> Self {
        self.bayer = pattern;
        self
    }

    /// Video stream description.
    #[must_use]
    pub fn info(&self) -> &AviInfo {
        &self.info
    }

    /// Number of video frames found.
    #[must_use]
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Decode one frame.
    ///
    /// # Errors
    /// [`AviError::FrameOutOfRange`] or I/O errors.
    pub fn read_frame(&mut self, index: usize) -> Result<Frame, AviError> {
        let count = self.frames.len();
        let &(offset, _) = self
            .frames
            .get(index)
            .ok_or(AviError::FrameOutOfRange { index, count })?;
        let layout = self.row_layout();
        // Safe to allocate: the walk only kept chunks at least this large.
        let mut buf = vec![0u8; layout.frame_bytes()];
        self.r.seek(SeekFrom::Start(offset))?;
        self.r.read_exact(&mut buf)?;
        Ok(Frame {
            width: layout.width,
            height: layout.height,
            data: layout.decode(&buf),
            bayer: self.bayer_pattern(),
        })
    }

    /// How the stored frames are laid out, for [`crate::io::raw`] decoding.
    #[must_use]
    pub fn row_layout(&self) -> RowLayout {
        let (sample, planes) = match self.info.format {
            PixelFormat::Gray8 => (Sample::U8, Planes::One),
            PixelFormat::Gray16 => (Sample::U16Le, Planes::One),
            PixelFormat::Bgr24 => (Sample::U8, Planes::MeanInteger),
        };
        RowLayout {
            width: self.info.width as usize,
            height: self.info.height as usize,
            sample,
            planes,
            stride: self.info.stride() as usize,
            bottom_up: self.info.bottom_up,
        }
    }

    /// Offsets of the video frames within the file.
    #[must_use]
    pub fn frame_offsets(&self) -> FrameOffsets {
        FrameOffsets::List(self.frames.iter().map(|f| f.0).collect())
    }

    /// Mosaic pattern given to decoded frames (never for colour video).
    #[must_use]
    pub fn bayer_pattern(&self) -> Option<BayerPattern> {
        if self.info.format == PixelFormat::Bgr24 {
            None
        } else {
            self.bayer
        }
    }
}

/// Minimal AVI writer for tests: single `RIFF AVI ` with `idx1` omitted.
/// With `avix_after`, frames after that count go into a following `RIFF AVIX `.
///
/// # Panics
/// On an empty frame list or dimensions beyond `i32`.
#[cfg(any(test, feature = "testutil"))]
#[must_use]
pub fn write_test_avi(
    frames: &[Frame],
    fourcc: &[u8; 4],
    bits: u16,
    bottom_up: bool,
    avix_after: Option<usize>,
) -> Vec<u8> {
    fn chunk(id: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut v = id.to_vec();
        v.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        v.extend_from_slice(payload);
        if payload.len() % 2 == 1 {
            v.push(0);
        }
        v
    }
    fn list(form: &[u8], id: &[u8], body: &[u8]) -> Vec<u8> {
        let mut p = form.to_vec();
        p.extend_from_slice(body);
        chunk(id, &p)
    }
    let (w, h) = (frames[0].width, frames[0].height);
    let stride = (w * usize::from(bits)).div_ceil(32) * 4;
    let mut hdr_stream = vec![0u8; 56];
    hdr_stream[0..4].copy_from_slice(b"vids");
    let mut hdr_format = vec![0u8; 40];
    hdr_format[0..4].copy_from_slice(&40u32.to_le_bytes());
    let (wi, hi) = (
        i32::try_from(w).expect("test width"),
        i32::try_from(h).expect("test height"),
    );
    hdr_format[4..8].copy_from_slice(&wi.to_le_bytes());
    let signed_height = if bottom_up { hi } else { -hi };
    hdr_format[8..12].copy_from_slice(&signed_height.to_le_bytes());
    hdr_format[12..14].copy_from_slice(&1u16.to_le_bytes());
    hdr_format[14..16].copy_from_slice(&bits.to_le_bytes());
    hdr_format[16..20].copy_from_slice(fourcc);
    let stream_list = list(
        b"strl",
        b"LIST",
        &[chunk(b"strh", &hdr_stream), chunk(b"strf", &hdr_format)].concat(),
    );
    let hdrl = list(
        b"hdrl",
        b"LIST",
        &[chunk(b"avih", &[0u8; 56]), stream_list].concat(),
    );
    let encode = |f: &Frame| {
        let mut buf = vec![0u8; stride * h];
        for y in 0..h {
            let dy = if bottom_up { h - 1 - y } else { y };
            for x in 0..w {
                let v = f.get(x, y).clamp(0.0, 1.0);
                match bits {
                    8 => buf[dy * stride + x] = (v * 255.0).round() as u8,
                    16 => buf[dy * stride + 2 * x..dy * stride + 2 * x + 2]
                        .copy_from_slice(&((v * 65535.0).round() as u16).to_le_bytes()),
                    _ => {
                        let q = (v * 255.0).round() as u8;
                        buf[dy * stride + 3 * x..dy * stride + 3 * x + 3]
                            .copy_from_slice(&[q, q, q]);
                    }
                }
            }
        }
        chunk(b"00db", &buf)
    };
    let split = avix_after.unwrap_or(frames.len()).min(frames.len());
    let movi1: Vec<u8> = [
        chunk(b"JUNK", &[0u8; 5]),
        frames[..split].iter().flat_map(encode).collect(),
    ]
    .concat();
    let mut out = list(
        b"AVI ",
        b"RIFF",
        &[hdrl, list(b"movi", b"LIST", &movi1)].concat(),
    );
    if split < frames.len() {
        let movi2: Vec<u8> = frames[split..].iter().flat_map(encode).collect();
        out.extend(list(b"AVIX", b"RIFF", &list(b"movi", b"LIST", &movi2)));
    }
    out
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn frames(n: usize, w: usize, h: usize) -> Vec<Frame> {
        (0..n)
            .map(|k| {
                let data = (0..w * h)
                    .map(|i| {
                        (((i % w) as f32 * 0.9 + (i / w) as f32 * 0.3 + k as f32 * 7.0) % 100.0)
                            / 100.0
                    })
                    .collect();
                Frame {
                    width: w,
                    height: h,
                    data,
                    bayer: None,
                }
            })
            .collect()
    }

    fn max_err(a: &Frame, b: &Frame) -> f32 {
        a.data
            .iter()
            .zip(&b.data)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0, f32::max)
    }

    #[test]
    fn round_trips_both_orientations_with_row_padding() {
        // Width 7 gives a 1-byte row pad at 8 bits and 3 at 24.
        for (fourcc, bits, tol) in [
            (b"DIB ", 8u16, 1.0 / 255.0),
            (b"Y800", 8, 1.0 / 255.0),
            (b"Y16 ", 16, 1.0 / 65535.0),
            (b"DIB ", 24, 1.0 / 255.0),
        ] {
            for bottom_up in [false, true] {
                let src = frames(4, 7, 5);
                let bytes = write_test_avi(&src, fourcc, bits, bottom_up, None);
                let mut r = AviReader::new(Cursor::new(bytes)).expect("open");
                assert_eq!(r.frame_count(), 4);
                assert_eq!(r.info().bottom_up, bottom_up);
                for (i, want) in src.iter().enumerate() {
                    let got = r.read_frame(i).expect("frame");
                    assert!(
                        max_err(&got, want) <= tol * 0.51 + 1e-6,
                        "{fourcc:?} {bits} {bottom_up}"
                    );
                }
            }
        }
    }

    #[test]
    fn opendml_avix_chunks_are_followed() {
        let src = frames(6, 8, 6);
        let bytes = write_test_avi(&src, b"DIB ", 8, true, Some(2));
        let mut r = AviReader::new(Cursor::new(bytes)).expect("open");
        assert_eq!(r.frame_count(), 6);
        let got = r.read_frame(5).expect("last");
        assert!(max_err(&got, &src[5]) <= 0.51 / 255.0 + 1e-6);
    }

    #[test]
    fn bayer_tag_is_applied_to_gray_only() {
        let src = frames(1, 8, 6);
        let b = write_test_avi(&src, b"DIB ", 8, false, None);
        let mut r = AviReader::new(Cursor::new(b))
            .expect("open")
            .with_bayer(Some(BayerPattern::Rggb));
        assert_eq!(
            r.read_frame(0).expect("frame").bayer,
            Some(BayerPattern::Rggb)
        );
        assert!(matches!(
            r.read_frame(1),
            Err(AviError::FrameOutOfRange { .. })
        ));
    }

    #[test]
    fn rejects_non_avi_and_unsupported_codecs() {
        assert!(matches!(
            AviReader::new(Cursor::new(Vec::new())).err(),
            Some(AviError::NotAvi)
        ));
        assert!(matches!(
            AviReader::new(Cursor::new(vec![0u8; 100])).err(),
            Some(AviError::NotAvi)
        ));
        let src = frames(1, 8, 6);
        let b = write_test_avi(&src, b"MJPG", 24, false, None);
        assert!(matches!(
            AviReader::new(Cursor::new(b)).err(),
            Some(AviError::UnsupportedFormat(_))
        ));
    }

    #[test]
    fn hostile_dimensions_and_truncation_are_safe() {
        let src = frames(3, 8, 6);
        let good = write_test_avi(&src, b"DIB ", 8, false, None);
        // Locate biWidth in strf and corrupt it.
        let pos = good.windows(4).position(|w| w == b"strf").expect("strf") + 8 + 4;
        for w in [0i32, -5, 70_000, i32::MAX] {
            let mut b = good.clone();
            b[pos..pos + 4].copy_from_slice(&w.to_le_bytes());
            assert!(matches!(
                AviReader::new(Cursor::new(b)).err(),
                Some(AviError::BadDimensions { .. })
            ));
        }
        // Huge but legal dimensions: no frame chunk is that large, so zero frames, no allocation.
        let mut b = good.clone();
        b[pos..pos + 4].copy_from_slice(&60_000i32.to_le_bytes());
        b[pos + 4..pos + 8].copy_from_slice(&60_000i32.to_le_bytes());
        assert_eq!(
            AviReader::new(Cursor::new(b)).expect("open").frame_count(),
            0
        );
        // Truncation drops the cut frame but keeps earlier ones.
        let cut = good[..good.len() - 20].to_vec();
        assert_eq!(
            AviReader::new(Cursor::new(cut))
                .expect("open")
                .frame_count(),
            2
        );
        // Every prefix parses or errors, never panics.
        for n in (0..good.len()).step_by(7) {
            let _ = AviReader::new(Cursor::new(good[..n].to_vec()));
        }
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        let mut state = 4242_u32;
        let good = write_test_avi(&frames(2, 8, 6), b"DIB ", 8, false, None);
        for round in 0..200 {
            let mut b = good.clone();
            for _ in 0..=(round % 6) {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let at = (state >> 8) as usize % b.len();
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                b[at] = (state >> 24) as u8;
            }
            if let Ok(mut r) = AviReader::new(Cursor::new(b)) {
                for i in 0..r.frame_count() {
                    let _ = r.read_frame(i);
                }
            }
        }
    }
}
