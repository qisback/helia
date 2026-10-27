// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The `hlpx` pixel buffer file and the buffer references used in messages.
//!
//! Layout (all little-endian): magic `HLPX`, `u32` layout version (1), `u32` width, `u32` height,
//! `u32` channels (1 to 4), `u32` reserved (0), then `width * height * channels` `f32` samples,
//! row-major, channels interleaved. The file is exactly `24 + 4 * width * height * channels` bytes.
//!
//! Every size is checked before anything is allocated or read, with overflow-safe arithmetic, so a
//! hostile or damaged file cannot make a reader allocate more than the limit it passes.

use std::fmt;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File magic.
pub const MAGIC: [u8; 4] = *b"HLPX";
/// Layout version written and accepted.
pub const LAYOUT_VERSION: u32 = 1;
/// Header size in bytes.
pub const HEADER_LEN: usize = 24;
/// Largest width or height accepted.
pub const MAX_DIM: u32 = 65_536;
/// Most channels accepted.
pub const MAX_CHANNELS: u32 = 4;
/// Default limit on one buffer file, in bytes.
pub const DEFAULT_MAX_BYTES: u64 = 4 << 30;
/// Longest buffer path, in bytes.
pub const MAX_PATH_LEN: usize = 128;

/// Why a buffer or path was refused. The text is for logs; Helia explains in its own words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HlpxError {
    /// The path breaks the rules for buffer paths.
    BadPath(String),
    /// The file is not a valid `hlpx` file.
    BadFile(String),
    /// The file or the message disagree about the size.
    SizeMismatch(String),
    /// The buffer is larger than the limit.
    TooLarge(String),
    /// A file system failure.
    Io(String),
}

impl fmt::Display for HlpxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadPath(s) => write!(f, "bad buffer path: {s}"),
            Self::BadFile(s) => write!(f, "bad buffer file: {s}"),
            Self::SizeMismatch(s) => write!(f, "buffer size mismatch: {s}"),
            Self::TooLarge(s) => write!(f, "buffer too large: {s}"),
            Self::Io(s) => write!(f, "buffer file error: {s}"),
        }
    }
}

impl std::error::Error for HlpxError {}

impl From<std::io::Error> for HlpxError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

fn default_format() -> String {
    "hlpx".into()
}

fn one() -> u32 {
    1
}

/// A reference to a pixel file in the working folder, as it appears in a message.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct BufferRef {
    /// Path relative to the working folder.
    pub path: String,
    /// `hlpx` (default) or, where a call allows it, `fits`.
    #[serde(default = "default_format")]
    pub format: String,
    /// Width in pixels.
    #[serde(default)]
    pub width: u32,
    /// Height in pixels.
    #[serde(default)]
    pub height: u32,
    /// Channels per pixel.
    #[serde(default = "one")]
    pub channels: u32,
}

/// Where the host wants a result written.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct OutputSpec {
    /// Path relative to the working folder.
    pub path: String,
    /// `hlpx` (default) or `fits` where allowed.
    #[serde(default = "default_format")]
    pub format: String,
}

/// Number of bytes in a file of these dimensions, or `None` on overflow or a bad dimension.
#[must_use]
pub fn file_len(width: u32, height: u32, channels: u32) -> Option<u64> {
    if width == 0
        || height == 0
        || channels == 0
        || width > MAX_DIM
        || height > MAX_DIM
        || channels > MAX_CHANNELS
    {
        return None;
    }
    let samples = u64::from(width)
        .checked_mul(u64::from(height))?
        .checked_mul(u64::from(channels))?;
    samples.checked_mul(4)?.checked_add(HEADER_LEN as u64)
}

/// The parsed 24-byte header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Channels per pixel.
    pub channels: u32,
}

impl Header {
    /// The whole file length this header describes.
    ///
    /// # Errors
    /// [`HlpxError::BadFile`] if a dimension is zero or out of range.
    pub fn file_len(&self) -> Result<u64, HlpxError> {
        file_len(self.width, self.height, self.channels)
            .ok_or_else(|| HlpxError::BadFile("dimensions out of range".into()))
    }

    /// Parse and validate `bytes` (at least [`HEADER_LEN`] long).
    ///
    /// # Errors
    /// [`HlpxError::BadFile`] on a short header, wrong magic or version, or bad dimensions.
    pub fn parse(bytes: &[u8]) -> Result<Self, HlpxError> {
        if bytes.len() < HEADER_LEN {
            return Err(HlpxError::BadFile("shorter than the 24-byte header".into()));
        }
        if bytes[..4] != MAGIC {
            return Err(HlpxError::BadFile("wrong magic".into()));
        }
        let u = |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
        if u(4) != LAYOUT_VERSION {
            return Err(HlpxError::BadFile(format!("layout version {}", u(4))));
        }
        let h = Self {
            width: u(8),
            height: u(12),
            channels: u(16),
        };
        h.file_len()?;
        Ok(h)
    }
}

/// A decoded image: `f32` samples, row-major, channels interleaved.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// Channels per pixel (1 mono, 3 RGB, 4 RGBA).
    pub channels: usize,
    /// The samples.
    pub data: Vec<f32>,
}

impl Image {
    /// Wrap samples.
    ///
    /// # Errors
    /// [`HlpxError::SizeMismatch`] if the length does not match, or a dimension is out of range.
    pub fn new(
        width: usize,
        height: usize,
        channels: usize,
        data: Vec<f32>,
    ) -> Result<Self, HlpxError> {
        let (w, h, c) = (
            u32::try_from(width).unwrap_or(u32::MAX),
            u32::try_from(height).unwrap_or(u32::MAX),
            u32::try_from(channels).unwrap_or(u32::MAX),
        );
        if file_len(w, h, c).is_none() {
            return Err(HlpxError::BadFile("dimensions out of range".into()));
        }
        if width * height * channels != data.len() {
            return Err(HlpxError::SizeMismatch(format!(
                "{width}x{height}x{channels} needs {} samples, got {}",
                width * height * channels,
                data.len()
            )));
        }
        Ok(Self {
            width,
            height,
            channels,
            data,
        })
    }

    /// Smallest and largest finite sample (0, 0 if there is none).
    #[must_use]
    pub fn range(&self) -> (f32, f32) {
        let mut lo = f32::INFINITY;
        let mut hi = f32::NEG_INFINITY;
        for &v in &self.data {
            if v.is_finite() {
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
        if lo > hi { (0.0, 0.0) } else { (lo, hi) }
    }

    /// The file bytes.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.data.len() * 4);
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&LAYOUT_VERSION.to_le_bytes());
        for v in [self.width, self.height, self.channels] {
            out.extend_from_slice(&u32::try_from(v).unwrap_or(u32::MAX).to_le_bytes());
        }
        out.extend_from_slice(&0u32.to_le_bytes());
        for v in &self.data {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }

    /// Decode file bytes, refusing a file over `max_bytes` or whose length is not exactly what its
    /// header says. NaN and infinity become 0.
    ///
    /// # Errors
    /// See [`HlpxError`].
    pub fn decode(bytes: &[u8], max_bytes: u64) -> Result<Self, HlpxError> {
        let h = Header::parse(bytes)?;
        let want = h.file_len()?;
        if want > max_bytes {
            return Err(HlpxError::TooLarge(format!(
                "{want} bytes, limit {max_bytes}"
            )));
        }
        if bytes.len() as u64 != want {
            return Err(HlpxError::SizeMismatch(format!(
                "header says {want} bytes, file has {}",
                bytes.len()
            )));
        }
        let data = bytes[HEADER_LEN..]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| {
                let v = f32::from_le_bytes(*c);
                if v.is_finite() { v } else { 0.0 }
            })
            .collect();
        Ok(Self {
            width: h.width as usize,
            height: h.height as usize,
            channels: h.channels as usize,
            data,
        })
    }

    /// Read a file. The size is checked against the header and `max_bytes` before the data is read.
    ///
    /// # Errors
    /// See [`HlpxError`].
    pub fn read_file(path: &Path, max_bytes: u64) -> Result<Self, HlpxError> {
        let meta = std::fs::symlink_metadata(path)?;
        if !meta.is_file() {
            return Err(HlpxError::BadFile("not a regular file".into()));
        }
        let mut f = std::fs::File::open(path)?;
        let mut head = [0u8; HEADER_LEN];
        f.read_exact(&mut head)
            .map_err(|_| HlpxError::BadFile("shorter than the 24-byte header".into()))?;
        let h = Header::parse(&head)?;
        let want = h.file_len()?;
        if want > max_bytes {
            return Err(HlpxError::TooLarge(format!(
                "{want} bytes, limit {max_bytes}"
            )));
        }
        if meta.len() != want {
            return Err(HlpxError::SizeMismatch(format!(
                "header says {want} bytes, file has {}",
                meta.len()
            )));
        }
        let mut body = vec![0u8; usize::try_from(want).unwrap_or(usize::MAX) - HEADER_LEN];
        f.read_exact(&mut body)?;
        let mut all = Vec::with_capacity(HEADER_LEN + body.len());
        all.extend_from_slice(&head);
        all.extend_from_slice(&body);
        drop(body);
        Self::decode(&all, max_bytes)
    }

    /// Write the file completely under a temporary name, then rename it into place.
    ///
    /// # Errors
    /// [`HlpxError::Io`] on a file system failure.
    pub fn write_file(&self, path: &Path) -> Result<(), HlpxError> {
        let tmp = path.with_extension("hlpx.part");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&self.encode())?;
            f.flush()?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// Check the rules for a path in a message: relative, no `..`, no drive letter or colon, no
/// backslash, no NUL, not empty, at most [`MAX_PATH_LEN`] bytes.
///
/// # Errors
/// [`HlpxError::BadPath`] naming the rule.
pub fn check_relative_path(rel: &str) -> Result<(), HlpxError> {
    let bad = |why: &str| Err(HlpxError::BadPath(why.into()));
    if rel.is_empty() {
        return bad("empty");
    }
    if rel.len() > MAX_PATH_LEN {
        return bad("too long");
    }
    if rel.contains('\0') {
        return bad("contains NUL");
    }
    if rel.contains('\\') || rel.contains(':') {
        return bad("contains a backslash or colon");
    }
    if rel.starts_with('/') {
        return bad("absolute");
    }
    for part in rel.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return bad("empty, . or .. component");
        }
    }
    // Belt and braces: the platform's own parser must agree there is nothing but names.
    if Path::new(rel)
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return bad("not a plain relative path");
    }
    Ok(())
}

/// The full path of `rel` inside `workdir`, after [`check_relative_path`] and a check that the
/// result, with links resolved, is still inside `workdir`. For a file that does not exist yet
/// (an output) the parent folder is resolved instead.
///
/// # Errors
/// [`HlpxError::BadPath`] or [`HlpxError::Io`].
pub fn resolve_in(workdir: &Path, rel: &str, must_exist: bool) -> Result<PathBuf, HlpxError> {
    check_relative_path(rel)?;
    let root = std::fs::canonicalize(workdir)?;
    let full = root.join(rel);
    let probe = if must_exist {
        full.clone()
    } else {
        full.parent()
            .map_or_else(|| root.clone(), Path::to_path_buf)
    };
    let resolved = std::fs::canonicalize(&probe)?;
    if !resolved.starts_with(&root) {
        return Err(HlpxError::BadPath("leaves the working folder".into()));
    }
    if !must_exist {
        // A link already at the output name would redirect the write.
        if let Ok(m) = std::fs::symlink_metadata(&full)
            && m.file_type().is_symlink()
        {
            return Err(HlpxError::BadPath("is a link".into()));
        }
    }
    Ok(full)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let img = Image::new(3, 2, 1, vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0]).unwrap();
        let back = Image::decode(&img.encode(), DEFAULT_MAX_BYTES).unwrap();
        assert_eq!(img, back);
    }

    #[test]
    fn nan_becomes_zero() {
        let img = Image::new(2, 1, 1, vec![f32::NAN, f32::INFINITY]).unwrap();
        let back = Image::decode(&img.encode(), DEFAULT_MAX_BYTES).unwrap();
        assert_eq!(back.data, vec![0.0, 0.0]);
    }

    #[test]
    fn size_lies_are_refused() {
        let mut b = Image::new(2, 2, 1, vec![0.0; 4]).unwrap().encode();
        b.truncate(b.len() - 1);
        assert!(matches!(
            Image::decode(&b, DEFAULT_MAX_BYTES),
            Err(HlpxError::SizeMismatch(_))
        ));
        // Header claims 65536 x 65536 x 4 on a 24-byte file.
        let mut h = Vec::new();
        h.extend_from_slice(&MAGIC);
        for v in [1u32, 65_536, 65_536, 4, 0] {
            h.extend_from_slice(&v.to_le_bytes());
        }
        assert!(matches!(
            Image::decode(&h, DEFAULT_MAX_BYTES),
            Err(HlpxError::TooLarge(_))
        ));
    }

    #[test]
    fn paths() {
        for ok in ["a.hlpx", "d/a.hlpx", "in-0001.hlpx"] {
            assert!(check_relative_path(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../b",
            "a//b",
            "C:x",
            "a\\b",
            "a\0b",
            "./a",
            "a/",
            &"x".repeat(129),
        ] {
            assert!(check_relative_path(bad).is_err(), "{bad:?}");
        }
    }
}
