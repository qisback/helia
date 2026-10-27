// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! SER v3 capture files.
//!
//! Layout: 178-byte header, `frame_count` frames, optional trailer of
//! `frame_count` little-endian i64 timestamps. Header integers are always
//! little-endian; the `little_endian` header flag applies to 16-bit pixel data
//! only and is honoured as written (1 = little, 0 = big).
//!
//! Samples are normalised by the container range (255 for 1 byte, 65535 for
//! 2 bytes), not by `pixel_depth`, because cameras differ on whether 12-bit
//! data is stored justified low or high.
//!
//! Every size is validated against the real stream length before any buffer is
//! allocated, so a hostile header cannot trigger a large allocation.

use std::io::{Read, Seek, SeekFrom, Write};

use crate::frame::{BayerPattern, Frame};
use crate::io::raw::{FrameOffsets, Planes, RowLayout, Sample};

/// Header length in bytes.
pub const HEADER_LEN: u64 = 178;
/// Largest accepted width or height.
pub const MAX_DIMENSION: u32 = 65_535;
const MAGIC: &[u8; 14] = b"LUCAM-RECORDER";
const FRAME_COUNT_OFFSET: u64 = 38;

/// SER parsing and writing failures.
#[derive(Debug, thiserror::Error)]
pub enum SerError {
    /// Stream ended inside the header.
    #[error("file is shorter than the {HEADER_LEN}-byte SER header")]
    TruncatedHeader,
    /// Header magic mismatch.
    #[error("not a SER file (bad signature)")]
    BadSignature,
    /// Colour ID not handled.
    #[error("unsupported SER colour id {0}")]
    UnsupportedColor(i32),
    /// Width or height outside `1..=MAX_DIMENSION`.
    #[error("invalid image dimensions {width}x{height}")]
    BadDimensions {
        /// Declared width.
        width: i64,
        /// Declared height.
        height: i64,
    },
    /// Pixel depth outside 1..=16.
    #[error("invalid pixel depth {0}")]
    BadDepth(i64),
    /// Negative frame count.
    #[error("invalid frame count {0}")]
    BadFrameCount(i64),
    /// Header promises more frame data than the stream holds.
    #[error("file truncated: header needs {expected} bytes, stream has {actual}")]
    Truncated {
        /// Bytes the header implies.
        expected: u64,
        /// Bytes available.
        actual: u64,
    },
    /// Frame index past the end.
    #[error("frame {index} out of range (file has {count})")]
    FrameOutOfRange {
        /// Requested index.
        index: usize,
        /// Frames in file.
        count: usize,
    },
    /// Frame passed to the writer does not match the header.
    #[error("frame is {got_w}x{got_h}, writer expects {want_w}x{want_h}")]
    FrameSizeMismatch {
        /// Frame width.
        got_w: usize,
        /// Frame height.
        got_h: usize,
        /// Expected width.
        want_w: usize,
        /// Expected height.
        want_h: usize,
    },
    /// Writer asked for a layout it cannot produce.
    #[error("cannot write SER colour mode {0:?}")]
    UnwritableColor(ColorId),
    /// I/O failure.
    #[error("SER I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Pixel arrangement (SER `ColorID`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorId {
    /// Single plane.
    Mono,
    /// Raw Bayer mosaic.
    Bayer(BayerPattern),
    /// Interleaved R, G, B per pixel.
    Rgb,
    /// Interleaved B, G, R per pixel.
    Bgr,
}

impl ColorId {
    /// The colour mode for a stored `ColorID`, if Helia handles it.
    #[must_use]
    pub fn from_id(raw: i32) -> Option<Self> {
        Self::from_raw(raw).ok()
    }

    fn from_raw(v: i32) -> Result<Self, SerError> {
        Ok(match v {
            0 => Self::Mono,
            8 => Self::Bayer(BayerPattern::Rggb),
            9 => Self::Bayer(BayerPattern::Grbg),
            10 => Self::Bayer(BayerPattern::Gbrg),
            11 => Self::Bayer(BayerPattern::Bggr),
            100 => Self::Rgb,
            101 => Self::Bgr,
            other => return Err(SerError::UnsupportedColor(other)),
        })
    }

    /// The `ColorID` integer stored in the header.
    #[must_use]
    pub fn to_raw(self) -> i32 {
        match self {
            Self::Mono => 0,
            Self::Bayer(BayerPattern::Rggb) => 8,
            Self::Bayer(BayerPattern::Grbg) => 9,
            Self::Bayer(BayerPattern::Gbrg) => 10,
            Self::Bayer(BayerPattern::Bggr) => 11,
            Self::Rgb => 100,
            Self::Bgr => 101,
        }
    }

    fn planes(self) -> usize {
        match self {
            Self::Rgb | Self::Bgr => 3,
            _ => 1,
        }
    }
}

impl ColorId {
    /// What the `ColorID` means, for display.
    #[must_use]
    pub fn meaning(self) -> &'static str {
        match self {
            Self::Mono => "MONO",
            Self::Bayer(BayerPattern::Rggb) => "BAYER_RGGB",
            Self::Bayer(BayerPattern::Grbg) => "BAYER_GRBG",
            Self::Bayer(BayerPattern::Gbrg) => "BAYER_GBRG",
            Self::Bayer(BayerPattern::Bggr) => "BAYER_BGGR",
            Self::Rgb => "RGB",
            Self::Bgr => "BGR",
        }
    }
}

/// Things the user may say about a file that its header gets wrong. The
/// reader decodes with these instead of the header's claims.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SerOverrides {
    /// Bits per sample; 1 to 8 read one byte per pixel, 9 to 16 two bytes.
    pub bit_depth: Option<u32>,
    /// Byte order of 16-bit samples (`true` = little-endian).
    pub little_endian: Option<bool>,
    /// Read the rows bottom to top.
    pub flip_rows: bool,
}

/// Ticks (100 ns) from 0001-01-01 to the Unix epoch.
const UNIX_EPOCH_TICKS: i64 = 621_355_968_000_000_000;

/// Decode a SER timestamp (100 ns ticks since 0001-01-01) as
/// `YYYY-MM-DD HH:MM:SS.mmm`. `None` for zero, negative or out-of-range values
/// (recorders that leave the field unset write zero).
#[must_use]
pub fn ticks_to_text(ticks: i64) -> Option<String> {
    if ticks <= 0 {
        return None;
    }
    let since_unix = ticks.checked_sub(UNIX_EPOCH_TICKS)?;
    let secs = since_unix.div_euclid(10_000_000);
    let millis = since_unix.rem_euclid(10_000_000) / 10_000;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    if !(1..=9999).contains(&year) {
        return None;
    }
    Some(format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}.{millis:03}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    ))
}

/// Parsed SER header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SerHeader {
    /// `FileID` text (`LUCAM-RECORDER` in every valid file).
    pub file_id: String,
    /// `LuID`, the camera series number some recorders store.
    pub lu_id: i32,
    /// Pixel arrangement.
    pub color: ColorId,
    /// Byte order of 16-bit pixel data.
    pub little_endian: bool,
    /// Frame width.
    pub width: u32,
    /// Frame height.
    pub height: u32,
    /// Bits per plane, 1..=16.
    pub pixel_depth: u32,
    /// Frames in the file.
    pub frame_count: u32,
    /// Observer name.
    pub observer: String,
    /// Instrument name.
    pub instrument: String,
    /// Telescope name.
    pub telescope: String,
    /// Local capture start, 100 ns ticks since 0001-01-01.
    pub date_time: i64,
    /// UTC capture start, same units.
    pub date_time_utc: i64,
}

impl SerHeader {
    /// Header for a new mono 16-bit little-endian file.
    #[must_use]
    pub fn new(width: u32, height: u32, color: ColorId, pixel_depth: u32) -> Self {
        Self {
            file_id: String::from_utf8_lossy(MAGIC).into_owned(),
            lu_id: 0,
            color,
            little_endian: true,
            width,
            height,
            pixel_depth,
            frame_count: 0,
            observer: String::new(),
            instrument: String::new(),
            telescope: String::new(),
            date_time: 0,
            date_time_utc: 0,
        }
    }

    /// Bytes per sample: 1 up to 8 bits, otherwise 2.
    #[must_use]
    pub fn bytes_per_sample(&self) -> usize {
        if self.pixel_depth <= 8 { 1 } else { 2 }
    }

    /// Bytes per frame.
    #[must_use]
    pub fn frame_bytes(&self) -> u64 {
        u64::from(self.width)
            * u64::from(self.height)
            * self.color.planes() as u64
            * self.bytes_per_sample() as u64
    }
}

fn le_i32(b: &[u8], at: usize) -> i32 {
    i32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn le_i64(b: &[u8], at: usize) -> i64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[at..at + 8]);
    i64::from_le_bytes(a)
}

fn text(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).trim().to_string()
}

/// Random-access SER reader.
pub struct SerReader<R> {
    r: R,
    header: SerHeader,
    stream_len: u64,
    /// Byte order actually used for 16-bit data.
    data_little: bool,
    /// Rows are read bottom to top.
    flip_rows: bool,
    /// The header as written in the file, before any override.
    declared: SerHeader,
    /// Overrides in force.
    overrides: SerOverrides,
}

/// Sum of absolute differences between samples two apart under both byte
/// orders, over up to 64 rows of a 16-bit frame. Returns `(little, big)`.
fn byte_order_costs(buf: &[u8], width: usize, height: usize, planes: usize) -> (f64, f64) {
    let row_samples = width * planes;
    let row_bytes = row_samples * 2;
    let stride = (height / 64).max(1);
    let (mut le, mut be) = (0.0f64, 0.0f64);
    for y in (0..height).step_by(stride) {
        let row = &buf[y * row_bytes..(y + 1) * row_bytes];
        let get = |i: usize, little: bool| -> f64 {
            let p = [row[2 * i], row[2 * i + 1]];
            f64::from(if little {
                u16::from_le_bytes(p)
            } else {
                u16::from_be_bytes(p)
            })
        };
        let step = 2 * planes;
        for i in 0..row_samples.saturating_sub(step) {
            le += (get(i, true) - get(i + step, true)).abs();
            be += (get(i, false) - get(i + step, false)).abs();
        }
    }
    (le, be)
}

impl<R: Read + Seek> SerReader<R> {
    /// Parse and validate the header. Fails if the stream is shorter than the
    /// header claims.
    ///
    /// # Errors
    /// [`SerError`] for malformed or truncated input.
    pub fn new(r: R) -> Result<Self, SerError> {
        Self::open(r, false, SerOverrides::default())
    }

    /// Like [`Self::new`], decoding with `overrides` instead of what the
    /// header claims.
    ///
    /// # Errors
    /// [`SerError`] for malformed or truncated input, including a file too
    /// short for the overridden bit depth.
    pub fn with_overrides(r: R, overrides: SerOverrides) -> Result<Self, SerError> {
        Self::open(r, false, overrides)
    }

    /// Like [`Self::new`] but clamps a too-large frame count to the frames
    /// actually present, to recover captures that were cut short.
    ///
    /// # Errors
    /// [`SerError`] for malformed input.
    pub fn new_lenient(r: R) -> Result<Self, SerError> {
        Self::open(r, true, SerOverrides::default())
    }

    fn open(mut r: R, lenient: bool, overrides: SerOverrides) -> Result<Self, SerError> {
        let stream_len = r.seek(SeekFrom::End(0))?;
        r.seek(SeekFrom::Start(0))?;
        if stream_len < HEADER_LEN {
            return Err(SerError::TruncatedHeader);
        }
        let mut b = [0u8; HEADER_LEN as usize];
        r.read_exact(&mut b)?;
        if &b[0..14] != MAGIC {
            return Err(SerError::BadSignature);
        }
        let color = ColorId::from_raw(le_i32(&b, 18))?;
        let little_endian = le_i32(&b, 22) != 0;
        let (w, h) = (i64::from(le_i32(&b, 26)), i64::from(le_i32(&b, 30)));
        let max = i64::from(MAX_DIMENSION);
        if !(1..=max).contains(&w) || !(1..=max).contains(&h) {
            return Err(SerError::BadDimensions {
                width: w,
                height: h,
            });
        }
        let depth = i64::from(le_i32(&b, 34));
        if !(1..=16).contains(&depth) {
            return Err(SerError::BadDepth(depth));
        }
        let count = i64::from(le_i32(&b, 38));
        if count < 0 {
            return Err(SerError::BadFrameCount(count));
        }
        let mut header = SerHeader {
            file_id: String::from_utf8_lossy(MAGIC).into_owned(),
            lu_id: le_i32(&b, 14),
            color,
            little_endian,
            width: w as u32,
            height: h as u32,
            pixel_depth: depth as u32,
            frame_count: count as u32,
            observer: text(&b[42..82]),
            instrument: text(&b[82..122]),
            telescope: text(&b[122..162]),
            date_time: le_i64(&b, 162),
            date_time_utc: le_i64(&b, 170),
        };
        let declared = header.clone();
        if let Some(d) = overrides.bit_depth {
            if !(1..=16).contains(&d) {
                return Err(SerError::BadDepth(i64::from(d)));
            }
            header.pixel_depth = d;
        }
        let fb = header.frame_bytes();
        let expected = HEADER_LEN.saturating_add(u64::from(header.frame_count).saturating_mul(fb));
        if expected > stream_len {
            if lenient {
                header.frame_count = ((stream_len - HEADER_LEN) / fb) as u32;
            } else {
                return Err(SerError::Truncated {
                    expected,
                    actual: stream_len,
                });
            }
        }
        let mut data_little = overrides.little_endian.unwrap_or(header.little_endian);
        if overrides.little_endian.is_none()
            && !data_little
            && header.bytes_per_sample() == 2
            && header.frame_count > 0
        {
            let mut buf = vec![0u8; fb as usize];
            r.seek(SeekFrom::Start(HEADER_LEN))?;
            r.read_exact(&mut buf)?;
            let (le, be) = byte_order_costs(
                &buf,
                header.width as usize,
                header.height as usize,
                header.color.planes(),
            );
            data_little = le < be;
        }
        Ok(Self {
            r,
            header,
            stream_len,
            data_little,
            flip_rows: overrides.flip_rows,
            declared,
            overrides,
        })
    }

    /// Byte order used to decode 16-bit samples (see the module notes).
    #[must_use]
    pub fn data_is_little_endian(&self) -> bool {
        self.data_little
    }

    /// Header as used for decoding: the file's own values with any
    /// [`SerOverrides`] applied.
    #[must_use]
    pub fn header(&self) -> &SerHeader {
        &self.header
    }

    /// Header exactly as written in the file.
    #[must_use]
    pub fn declared_header(&self) -> &SerHeader {
        &self.declared
    }

    /// Overrides this reader decodes with.
    #[must_use]
    pub fn overrides(&self) -> SerOverrides {
        self.overrides
    }

    /// Number of frames.
    #[must_use]
    pub fn frame_count(&self) -> usize {
        self.header.frame_count as usize
    }

    /// Read one frame. Bayer files return the raw mosaic with
    /// [`Frame::bayer`] set; RGB files are reduced to the channel mean.
    ///
    /// # Errors
    /// [`SerError::FrameOutOfRange`] or I/O errors.
    pub fn read_frame(&mut self, index: usize) -> Result<Frame, SerError> {
        let count = self.frame_count();
        if index >= count {
            return Err(SerError::FrameOutOfRange { index, count });
        }
        let fb = self.header.frame_bytes();
        // Safe to allocate: open() proved count * fb fits inside the stream.
        let mut buf = vec![0u8; fb as usize];
        self.r
            .seek(SeekFrom::Start(HEADER_LEN + index as u64 * fb))?;
        self.r.read_exact(&mut buf)?;
        let layout = self.row_layout();
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
        let (w, h) = (self.header.width as usize, self.header.height as usize);
        let planes = self.header.color.planes();
        let sample = match (self.header.bytes_per_sample(), self.data_little) {
            (1, _) => Sample::U8,
            (_, true) => Sample::U16Le,
            (_, false) => Sample::U16Be,
        };
        RowLayout {
            width: w,
            height: h,
            sample,
            planes: if planes == 3 {
                Planes::MeanNormalised
            } else {
                Planes::One
            },
            stride: w * planes * self.header.bytes_per_sample(),
            bottom_up: self.flip_rows,
        }
    }

    /// Offsets of the frames within the file.
    #[must_use]
    pub fn frame_offsets(&self) -> FrameOffsets {
        FrameOffsets::Regular {
            start: HEADER_LEN,
            step: self.header.frame_bytes(),
            count: self.frame_count(),
        }
    }

    /// Mosaic pattern of the decoded frames, when the header says the file
    /// stores raw Bayer data (adjusted when the rows are read upside down).
    #[must_use]
    pub fn bayer_pattern(&self) -> Option<BayerPattern> {
        match self.header.color {
            ColorId::Bayer(p) if self.flip_rows => {
                Some(p.flipped_vertically(self.header.height as usize))
            }
            ColorId::Bayer(p) => Some(p),
            _ => None,
        }
    }

    /// Streaming iterator over all frames in order.
    pub fn frames(&mut self) -> Frames<'_, R> {
        Frames {
            reader: self,
            next: 0,
        }
    }

    /// Per-frame timestamps if the trailer is present.
    ///
    /// # Errors
    /// I/O failures while reading the trailer.
    pub fn timestamps(&mut self) -> Result<Option<Vec<i64>>, SerError> {
        let n = u64::from(self.header.frame_count);
        let start = HEADER_LEN + n * self.header.frame_bytes();
        if n == 0 || self.stream_len < start + n * 8 {
            return Ok(None);
        }
        let mut buf = vec![0u8; (n * 8) as usize];
        self.r.seek(SeekFrom::Start(start))?;
        self.r.read_exact(&mut buf)?;
        Ok(Some(
            buf.as_chunks::<8>()
                .0
                .iter()
                .map(|c| i64::from_le_bytes(*c))
                .collect(),
        ))
    }
}

/// Iterator returned by [`SerReader::frames`].
pub struct Frames<'a, R> {
    reader: &'a mut SerReader<R>,
    next: usize,
}

impl<R: Read + Seek> Iterator for Frames<'_, R> {
    type Item = Result<Frame, SerError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.reader.frame_count() {
            return None;
        }
        let item = self.reader.read_frame(self.next);
        // Stop after the first error rather than looping on a broken stream.
        self.next = if item.is_err() {
            usize::MAX
        } else {
            self.next + 1
        };
        Some(item)
    }
}

/// SER writer, mainly for tests and synthetic captures.
pub struct SerWriter<W> {
    w: W,
    header: SerHeader,
    count: u32,
    timestamps: Vec<i64>,
    all_stamped: bool,
}

impl<W: Write + Seek> SerWriter<W> {
    /// Write the header (frame count patched in [`Self::finish`]).
    ///
    /// # Errors
    /// Invalid header values, unsupported colour mode or I/O failure.
    pub fn new(mut w: W, mut header: SerHeader) -> Result<Self, SerError> {
        let max = MAX_DIMENSION;
        if !(1..=max).contains(&header.width) || !(1..=max).contains(&header.height) {
            return Err(SerError::BadDimensions {
                width: i64::from(header.width),
                height: i64::from(header.height),
            });
        }
        if !(1..=16).contains(&header.pixel_depth) {
            return Err(SerError::BadDepth(i64::from(header.pixel_depth)));
        }
        if header.color.planes() != 1 {
            return Err(SerError::UnwritableColor(header.color));
        }
        header.frame_count = 0;
        let mut b = [0u8; HEADER_LEN as usize];
        b[0..14].copy_from_slice(MAGIC);
        b[18..22].copy_from_slice(&header.color.to_raw().to_le_bytes());
        b[22..26].copy_from_slice(&i32::from(header.little_endian).to_le_bytes());
        b[26..30].copy_from_slice(&i32::from(header.width as u16).to_le_bytes());
        b[30..34].copy_from_slice(&i32::from(header.height as u16).to_le_bytes());
        b[34..38].copy_from_slice(&i32::from(header.pixel_depth as u16).to_le_bytes());
        for (range, s) in [
            (42..82, &header.observer),
            (82..122, &header.instrument),
            (122..162, &header.telescope),
        ] {
            let bytes = s.as_bytes();
            let n = bytes.len().min(range.len());
            b[range.start..range.start + n].copy_from_slice(&bytes[..n]);
        }
        b[162..170].copy_from_slice(&header.date_time.to_le_bytes());
        b[170..178].copy_from_slice(&header.date_time_utc.to_le_bytes());
        w.write_all(&b)?;
        Ok(Self {
            w,
            header,
            count: 0,
            timestamps: Vec::new(),
            all_stamped: true,
        })
    }

    /// Append a frame, quantising 0..1 to the container range.
    ///
    /// # Errors
    /// Size mismatch or I/O failure.
    pub fn write_frame(&mut self, frame: &Frame, timestamp: Option<i64>) -> Result<(), SerError> {
        let (w, h) = (self.header.width as usize, self.header.height as usize);
        if frame.width != w || frame.height != h {
            return Err(SerError::FrameSizeMismatch {
                got_w: frame.width,
                got_h: frame.height,
                want_w: w,
                want_h: h,
            });
        }
        let mut out = Vec::with_capacity(w * h * self.header.bytes_per_sample());
        if self.header.bytes_per_sample() == 1 {
            out.extend(
                frame
                    .data
                    .iter()
                    .map(|&v| (v.clamp(0.0, 1.0) * 255.0).round() as u8),
            );
        } else {
            for &v in &frame.data {
                let q = (v.clamp(0.0, 1.0) * 65535.0).round() as u16;
                out.extend_from_slice(&if self.header.little_endian {
                    q.to_le_bytes()
                } else {
                    q.to_be_bytes()
                });
            }
        }
        self.w.write_all(&out)?;
        self.count += 1;
        match timestamp {
            Some(t) => self.timestamps.push(t),
            None => self.all_stamped = false,
        }
        Ok(())
    }

    /// Append one frame already in the file's storage format (`width * height`
    /// samples of 1 or 2 bytes, in the header's byte order). No timestamp, so
    /// the file gets no trailer.
    ///
    /// # Errors
    /// A wrong length or an I/O failure.
    pub fn write_raw_frame(&mut self, bytes: &[u8]) -> Result<(), SerError> {
        let want = self.header.frame_bytes() as usize;
        if bytes.len() != want {
            return Err(SerError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("raw frame is {} bytes, the header says {want}", bytes.len()),
            )));
        }
        self.w.write_all(bytes)?;
        self.count += 1;
        self.all_stamped = false;
        Ok(())
    }

    /// Write the trailer (when every frame had a timestamp), patch the frame
    /// count and return the underlying writer.
    ///
    /// # Errors
    /// I/O failure.
    pub fn finish(mut self) -> Result<W, SerError> {
        if self.all_stamped && self.count > 0 {
            for t in &self.timestamps {
                self.w.write_all(&t.to_le_bytes())?;
            }
        }
        let end = self.w.stream_position()?;
        self.w.seek(SeekFrom::Start(FRAME_COUNT_OFFSET))?;
        self.w
            .write_all(&i32::try_from(self.count).unwrap_or(i32::MAX).to_le_bytes())?;
        self.w.seek(SeekFrom::Start(end))?;
        self.w.flush()?;
        Ok(self.w)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn gradient(w: usize, h: usize, phase: f32) -> Frame {
        let data = (0..w * h)
            .map(|i| {
                (((i % w) as f32 / w as f32) + phase + (i / w) as f32 / (2.0 * h as f32)).fract()
            })
            .collect();
        Frame {
            width: w,
            height: h,
            data,
            bayer: None,
        }
    }

    fn make(depth: u32, little: bool, color: ColorId, stamps: bool) -> (Vec<u8>, Vec<Frame>) {
        let mut hdr = SerHeader::new(13, 9, color, depth);
        hdr.little_endian = little;
        hdr.observer = "Tester".into();
        let mut wtr =
            SerWriter::new(Cursor::new(Vec::new()), hdr).unwrap_or_else(|e| panic!("{e}"));
        let frames: Vec<Frame> = (0..5).map(|i| gradient(13, 9, i as f32 * 0.1)).collect();
        for (i, f) in frames.iter().enumerate() {
            wtr.write_frame(f, stamps.then(|| i64::try_from(1000 + i).unwrap_or(0)))
                .unwrap_or_else(|e| panic!("{e}"));
        }
        (
            wtr.finish().unwrap_or_else(|e| panic!("{e}")).into_inner(),
            frames,
        )
    }

    #[test]
    fn round_trip_all_depths_and_endians() {
        for (depth, little) in [(8, true), (16, true), (16, false), (12, true)] {
            let (bytes, frames) = make(depth, little, ColorId::Mono, true);
            let mut r = SerReader::new(Cursor::new(bytes)).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(r.header().observer, "Tester");
            assert_eq!(r.frame_count(), 5);
            let tol = if depth == 8 {
                1.0 / 255.0
            } else {
                1.0 / 65535.0
            };
            for (got, want) in r.frames().zip(&frames) {
                let got = got.unwrap_or_else(|e| panic!("{e}"));
                assert_eq!((got.width, got.height), (13, 9));
                let err = got
                    .data
                    .iter()
                    .zip(&want.data)
                    .map(|(a, b)| (a - b).abs())
                    .fold(0.0, f32::max);
                assert!(err <= tol, "depth {depth} little {little} err {err}");
            }
            assert_eq!(
                r.timestamps().unwrap_or(None),
                Some((0..5).map(|i| 1000 + i).collect())
            );
        }
    }

    #[test]
    fn mislabelled_big_endian_flag_is_corrected() {
        // Little-endian smooth data with the flag cleared, as SharpCap writes.
        let (mut bytes, frames) = make(16, true, ColorId::Mono, false);
        bytes[22..26].copy_from_slice(&0i32.to_le_bytes());
        let mut r = SerReader::new(Cursor::new(bytes)).expect("open");
        assert!(r.data_is_little_endian());
        let got = r.read_frame(1).expect("frame");
        assert!((got.data[5] - frames[1].data[5]).abs() < 1e-4);
        // Genuinely big-endian data with the flag cleared stays big-endian.
        let (bytes, frames) = make(16, false, ColorId::Mono, false);
        let mut r = SerReader::new(Cursor::new(bytes)).expect("open");
        assert!(!r.data_is_little_endian());
        let got = r.read_frame(1).expect("frame");
        assert!((got.data[5] - frames[1].data[5]).abs() < 1e-4);
    }

    #[test]
    fn bayer_frames_keep_pattern_and_no_trailer_is_none() {
        let (bytes, _) = make(16, true, ColorId::Bayer(BayerPattern::Gbrg), false);
        let mut r = SerReader::new(Cursor::new(bytes)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            r.read_frame(2).map(|f| f.bayer).ok(),
            Some(Some(BayerPattern::Gbrg))
        );
        assert_eq!(r.timestamps().unwrap_or(Some(vec![])), None);
    }

    #[test]
    fn out_of_range_frame_errors() {
        let (bytes, _) = make(8, true, ColorId::Mono, false);
        let mut r = SerReader::new(Cursor::new(bytes)).unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(
            r.read_frame(5),
            Err(SerError::FrameOutOfRange { .. })
        ));
    }

    #[test]
    fn rgb_reduces_to_mean() {
        let mut hdr = Vec::from(&make(8, true, ColorId::Mono, false).0[..178]);
        hdr[18..22].copy_from_slice(&100i32.to_le_bytes());
        hdr[26..30].copy_from_slice(&2i32.to_le_bytes());
        hdr[30..34].copy_from_slice(&1i32.to_le_bytes());
        hdr[38..42].copy_from_slice(&1i32.to_le_bytes());
        hdr.extend_from_slice(&[255, 0, 0, 0, 255, 255]);
        let mut r = SerReader::new(Cursor::new(hdr)).unwrap_or_else(|e| panic!("{e}"));
        let f = r.read_frame(0).unwrap_or_else(|e| panic!("{e}"));
        assert!((f.data[0] - 1.0 / 3.0).abs() < 1e-6 && (f.data[1] - 2.0 / 3.0).abs() < 1e-6);
    }

    fn header_bytes() -> Vec<u8> {
        make(16, true, ColorId::Mono, false).0[..178].to_vec()
    }

    #[test]
    fn truncated_header_is_error() {
        let b = header_bytes();
        for n in [0, 5, 14, 100, 177] {
            assert!(matches!(
                SerReader::new(Cursor::new(b[..n].to_vec())).err(),
                Some(SerError::TruncatedHeader)
            ));
        }
    }

    #[test]
    fn bad_signature_and_color_are_errors() {
        let mut b = header_bytes();
        b[0] = b'X';
        assert!(matches!(
            SerReader::new(Cursor::new(b)).err(),
            Some(SerError::BadSignature)
        ));
        let mut b = header_bytes();
        b[18..22].copy_from_slice(&17i32.to_le_bytes());
        assert!(matches!(
            SerReader::new(Cursor::new(b)).err(),
            Some(SerError::UnsupportedColor(17))
        ));
    }

    #[test]
    fn absurd_dimensions_rejected_without_allocating() {
        for (w, h) in [
            (0i32, 5i32),
            (5, 0),
            (-1, 5),
            (i32::MAX, i32::MAX),
            (70_000, 70_000),
        ] {
            let mut b = header_bytes();
            b[26..30].copy_from_slice(&w.to_le_bytes());
            b[30..34].copy_from_slice(&h.to_le_bytes());
            assert!(matches!(
                SerReader::new(Cursor::new(b)).err(),
                Some(SerError::BadDimensions { .. })
            ));
        }
        // In range but far larger than the stream: caught by the length check.
        let mut b = header_bytes();
        b[26..30].copy_from_slice(&60_000i32.to_le_bytes());
        b[30..34].copy_from_slice(&60_000i32.to_le_bytes());
        b[38..42].copy_from_slice(&1i32.to_le_bytes());
        assert!(matches!(
            SerReader::new(Cursor::new(b)).err(),
            Some(SerError::Truncated { .. })
        ));
    }

    #[test]
    fn bad_depth_and_negative_count_are_errors() {
        let mut b = header_bytes();
        b[34..38].copy_from_slice(&0i32.to_le_bytes());
        assert!(matches!(
            SerReader::new(Cursor::new(b)).err(),
            Some(SerError::BadDepth(0))
        ));
        let mut b = header_bytes();
        b[38..42].copy_from_slice(&(-3i32).to_le_bytes());
        assert!(matches!(
            SerReader::new(Cursor::new(b)).err(),
            Some(SerError::BadFrameCount(-3))
        ));
    }

    #[test]
    fn frame_count_larger_than_file_is_error_or_clamped() {
        let (mut bytes, _) = make(16, true, ColorId::Mono, false);
        bytes[38..42].copy_from_slice(&i32::MAX.to_le_bytes());
        assert!(matches!(
            SerReader::new(Cursor::new(bytes.clone())).err(),
            Some(SerError::Truncated { .. })
        ));
        let r = SerReader::new_lenient(Cursor::new(bytes)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(r.frame_count(), 5);
    }

    #[test]
    fn truncated_frame_data_is_error() {
        let (bytes, _) = make(16, true, ColorId::Mono, false);
        let cut = bytes[..bytes.len() - 10].to_vec();
        assert!(SerReader::new(Cursor::new(cut)).is_err());
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        let mut state = 0x1234_5678_u32;
        for len in [0usize, 1, 50, 178, 179, 500, 4000] {
            for _ in 0..20 {
                let b: Vec<u8> = (0..len)
                    .map(|_| {
                        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                        (state >> 24) as u8
                    })
                    .collect();
                if let Ok(mut r) = SerReader::new(Cursor::new(b)) {
                    for f in r.frames() {
                        let _ = f;
                    }
                }
            }
        }
    }

    #[test]
    fn ticks_decode_to_a_date() {
        // 2022-07-17 15:32:09.500 UTC.
        let unix = 1_658_071_929i64;
        let ticks = UNIX_EPOCH_TICKS + unix * 10_000_000 + 5_000_000;
        assert_eq!(
            ticks_to_text(ticks).as_deref(),
            Some("2022-07-17 15:32:09.500")
        );
        assert_eq!(ticks_to_text(0), None);
        assert_eq!(ticks_to_text(-5), None);
        assert_eq!(
            ticks_to_text(UNIX_EPOCH_TICKS).as_deref(),
            Some("1970-01-01 00:00:00.000")
        );
    }

    #[test]
    fn overrides_change_how_the_samples_are_read() {
        // A 16-bit file whose header claims 8 bits reads as garbage until told.
        let (bytes, frames) = make(16, true, ColorId::Mono, false);
        let mut lying = bytes.clone();
        lying[34..38].copy_from_slice(&8i32.to_le_bytes());
        assert!(SerReader::new(Cursor::new(lying.clone())).is_ok());
        let mut r = SerReader::with_overrides(
            Cursor::new(lying),
            SerOverrides {
                bit_depth: Some(16),
                ..SerOverrides::default()
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(r.declared_header().pixel_depth, 8);
        assert_eq!(r.header().pixel_depth, 16);
        let f = r.read_frame(1).unwrap_or_else(|e| panic!("{e}"));
        for (a, b) in f.data.iter().zip(&frames[1].data) {
            assert!((a - b).abs() < 1e-4);
        }
        // Flipped rows come back bottom to top.
        let mut flip = SerReader::with_overrides(
            Cursor::new(bytes.clone()),
            SerOverrides {
                flip_rows: true,
                ..SerOverrides::default()
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let g = flip.read_frame(0).unwrap_or_else(|e| panic!("{e}"));
        assert!((g.data[0] - frames[0].data[13 * 8]).abs() < 1e-4);
        // Forcing big-endian on little-endian data scrambles it.
        let mut be = SerReader::with_overrides(
            Cursor::new(bytes),
            SerOverrides {
                little_endian: Some(false),
                ..SerOverrides::default()
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(!be.data_is_little_endian());
        let h = be.read_frame(0).unwrap_or_else(|e| panic!("{e}"));
        assert!((h.data[5] - frames[0].data[5]).abs() > 1e-3);
    }
}
