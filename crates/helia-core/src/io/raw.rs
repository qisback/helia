// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Stored sample bytes to normalised `f32` rows, and positional file reads.
//!
//! [`RowLayout`] is the one place that knows how a container stores its
//! pixels. The readers in [`super::ser`] and [`super::avi`] and the concurrent
//! [`FileFrames`] all decode through it, so they agree bit for bit.
//!
//! [`FileFrames`] reads with positional I/O (`pread` / `ReadFile` with an
//! offset) instead of a shared seekable stream, so any number of threads can
//! decode different frames of one file at once. That is safe `std` code; the
//! alternative, a memory map, would need `unsafe` and can fault when the
//! capture is truncated while mapped.

#![allow(
    clippy::many_single_char_names,
    reason = "x, y, w, h, l are the natural names in image and layout maths"
)]

use std::any::Any;
use std::cell::RefCell;
use std::fs::File;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use crate::debayer::Debayer;
use crate::error::{Error, Result};
use crate::frame::{BayerPattern, Frame};
use crate::profile::{Category, Profile};

/// Storage of one sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sample {
    /// Unsigned 8 bit, divided by 255.
    U8,
    /// Unsigned 16 bit little-endian, divided by 65535.
    U16Le,
    /// Unsigned 16 bit big-endian, divided by 65535.
    U16Be,
    /// Little-endian `f32`, used as is (in-memory frames).
    F32,
}

impl Sample {
    /// Bytes per sample.
    #[must_use]
    pub fn bytes(self) -> usize {
        match self {
            Self::U8 => 1,
            Self::U16Le | Self::U16Be => 2,
            Self::F32 => 4,
        }
    }
}

/// How planes collapse to the single plane of a [`Frame`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Planes {
    /// One plane, used as is.
    One,
    /// Three interleaved planes; each is normalised, then the mean taken (SER RGB).
    MeanNormalised,
    /// Three interleaved 8-bit planes; the integer sum is divided by `3 * 255` (AVI BGR).
    MeanInteger,
}

/// Geometry and sample format of a stored frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowLayout {
    /// Pixels per row.
    pub width: usize,
    /// Rows.
    pub height: usize,
    /// Sample storage.
    pub sample: Sample,
    /// Plane handling.
    pub planes: Planes,
    /// Bytes from one stored row to the next.
    pub stride: usize,
    /// True when the first stored row is the bottom of the image.
    pub bottom_up: bool,
}

impl RowLayout {
    /// Bytes in one stored frame.
    #[must_use]
    pub fn frame_bytes(&self) -> usize {
        self.stride * self.height
    }

    /// Decode image row `y` (0 = top) of the stored frame `raw` into `out`
    /// (`width` samples).
    ///
    /// # Panics
    /// If `raw` is shorter than [`Self::frame_bytes`], `out` is not `width`
    /// long or `y` is out of range (caller bugs).
    pub fn row(&self, raw: &[u8], y: usize, out: &mut [f32]) {
        let w = self.width;
        assert_eq!(out.len(), w);
        let sy = if self.bottom_up {
            self.height - 1 - y
        } else {
            y
        };
        let src = &raw[sy * self.stride..(sy + 1) * self.stride];
        match (self.planes, self.sample) {
            (Planes::One, Sample::U8) => {
                for (o, &v) in out.iter_mut().zip(&src[..w]) {
                    *o = f32::from(v) / 255.0;
                }
            }
            (Planes::One, Sample::U16Le) => {
                for (o, c) in out.iter_mut().zip(src[..w * 2].as_chunks::<2>().0) {
                    *o = f32::from(u16::from_le_bytes(*c)) / 65535.0;
                }
            }
            (Planes::One, Sample::F32) => {
                for (o, c) in out.iter_mut().zip(src[..w * 4].as_chunks::<4>().0) {
                    *o = f32::from_le_bytes(*c);
                }
            }
            (Planes::One, Sample::U16Be) => {
                for (o, c) in out.iter_mut().zip(src[..w * 2].as_chunks::<2>().0) {
                    *o = f32::from(u16::from_be_bytes(*c)) / 65535.0;
                }
            }
            (_, Sample::F32) => unreachable!("f32 frames have a single plane"),
            (Planes::MeanNormalised, Sample::U8) => {
                for (o, c) in out.iter_mut().zip(src[..w * 3].as_chunks::<3>().0) {
                    let n = |v: u8| f32::from(v) / 255.0;
                    *o = (n(c[0]) + n(c[1]) + n(c[2])) / 3.0;
                }
            }
            (Planes::MeanNormalised, s) => {
                let get = |c: &[u8]| {
                    let p = [c[0], c[1]];
                    f32::from(if s == Sample::U16Le {
                        u16::from_le_bytes(p)
                    } else {
                        u16::from_be_bytes(p)
                    }) / 65535.0
                };
                for (o, c) in out.iter_mut().zip(src[..w * 6].as_chunks::<6>().0) {
                    *o = (get(&c[0..2]) + get(&c[2..4]) + get(&c[4..6])) / 3.0;
                }
            }
            (Planes::MeanInteger, _) => {
                for (o, c) in out.iter_mut().zip(src[..w * 3].as_chunks::<3>().0) {
                    *o = (f32::from(c[0]) + f32::from(c[1]) + f32::from(c[2])) / (3.0 * 255.0);
                }
            }
        }
    }

    /// Decode the whole frame (rows top to bottom) without demosaicing.
    #[must_use]
    pub fn decode(&self, raw: &[u8]) -> Vec<f32> {
        let mut data = vec![0.0f32; self.width * self.height];
        for (y, row) in data.chunks_exact_mut(self.width).enumerate() {
            self.row(raw, y, row);
        }
        data
    }
}

/// Where the frames of a file start.
#[derive(Clone, Debug)]
pub enum FrameOffsets {
    /// Equally spaced frames: `start + index * step`.
    Regular {
        /// Offset of frame 0.
        start: u64,
        /// Bytes between frames.
        step: u64,
        /// Frame count.
        count: usize,
    },
    /// Explicit offsets (AVI chunks).
    List(Arc<[u64]>),
}

impl FrameOffsets {
    fn count(&self) -> usize {
        match self {
            Self::Regular { count, .. } => *count,
            Self::List(l) => l.len(),
        }
    }

    fn get(&self, index: usize) -> Option<u64> {
        match self {
            Self::Regular { start, step, count } => {
                (index < *count).then(|| start + index as u64 * step)
            }
            Self::List(l) => l.get(index).copied(),
        }
    }
}

/// A place for a backend to keep, with a source, what it holds on a device
/// for that source's frames (raw frames resident in video memory).
///
/// The cache lives exactly as long as the source, so it can never serve one
/// capture's frames for another's, and the device memory is released with the
/// source. Core never looks inside.
#[derive(Default)]
pub struct DeviceCache {
    slot: Mutex<Option<Arc<dyn Any + Send + Sync>>>,
}

impl std::fmt::Debug for DeviceCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DeviceCache")
    }
}

impl DeviceCache {
    /// Drop what is cached, releasing the device memory it holds.
    pub fn clear(&self) {
        *self.slot.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// The cached value if it is a `T`, else the result of `make` (kept for
    /// next time when it is a `T` and the slot was empty). `None` when the
    /// slot holds something of another type or `make` declines.
    pub fn get_or_insert_with<T: Any + Send + Sync>(
        &self,
        make: impl FnOnce() -> Option<T>,
    ) -> Option<Arc<T>> {
        let mut slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(existing) = slot.as_ref() {
            return Arc::clone(existing).downcast::<T>().ok();
        }
        let value = Arc::new(make()?);
        *slot = Some(Arc::clone(&value) as Arc<dyn Any + Send + Sync>);
        Some(value)
    }
}

/// Frames readable from several threads at once.
pub trait SharedFrames: Send + Sync {
    /// Number of frames.
    fn frame_count(&self) -> usize;

    /// Geometry and sample format of the stored bytes.
    fn layout(&self) -> RowLayout;

    /// Mosaic pattern of the frames, when they are raw Bayer data.
    fn bayer(&self) -> Option<BayerPattern>;

    /// Read the stored bytes of frame `index` (a [`RowLayout::frame_bytes`]
    /// long buffer) into `buf`, which is resized as needed.
    ///
    /// # Errors
    /// Out-of-range index or I/O failure.
    fn read_raw(&self, index: usize, buf: &mut Vec<u8>) -> Result<()>;

    /// Where a backend may keep device-resident copies of this source's raw
    /// frames, or `None` when the source cannot promise that `read_raw`
    /// always returns the same bytes for an index.
    fn device_cache(&self) -> Option<&DeviceCache> {
        None
    }

    /// Decode frame `index` to a mono frame: stored samples are converted and,
    /// for a Bayer capture, demosaiced by `debayer`, in one pass over the
    /// bytes. Charges `profile` for the decode and debayer time.
    ///
    /// # Errors
    /// Out-of-range index or I/O failure.
    fn read_mono(&self, index: usize, debayer: &Debayer, profile: &Profile) -> Result<Frame> {
        RAW.with(|cell| {
            let mut raw = cell.borrow_mut();
            let t0 = Instant::now();
            self.read_raw(index, &mut raw)?;
            profile.add(Category::Decode, t0.elapsed());
            Ok(decode_mono(
                &self.layout(),
                self.bayer(),
                &raw,
                debayer,
                profile,
            ))
        })
    }
}

/// Convert the stored bytes of one frame to mono, demosaicing a Bayer mosaic.
#[must_use]
pub fn decode_mono(
    l: &RowLayout,
    bayer: Option<BayerPattern>,
    raw: &[u8],
    debayer: &Debayer,
    profile: &Profile,
) -> Frame {
    let (w, h) = (l.width, l.height);
    let mut out = vec![0.0f32; w * h];
    let mut decode = std::time::Duration::ZERO;
    match bayer {
        None => {
            let t = Instant::now();
            for (y, row) in out.chunks_exact_mut(w).enumerate() {
                l.row(raw, y, row);
            }
            decode += t.elapsed();
        }
        Some(pattern) => {
            let t = Instant::now();
            let mut spent = std::time::Duration::ZERO;
            debayer.apply_rows(w, h, pattern, &mut out, &mut |y, buf| {
                let s = Instant::now();
                l.row(raw, y, buf);
                spent += s.elapsed();
            });
            decode += spent;
            profile.add(Category::Debayer, t.elapsed().saturating_sub(spent));
        }
    }
    profile.add(Category::Decode, decode);
    Frame {
        width: w,
        height: h,
        data: out,
        bayer: None,
    }
}

thread_local! {
    static RAW: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// A capture file read with positional I/O.
#[derive(Debug)]
pub struct FileFrames {
    file: File,
    layout: RowLayout,
    offsets: FrameOffsets,
    bayer: Option<BayerPattern>,
    cache: DeviceCache,
}

#[cfg(unix)]
fn pread_exact(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
}

#[cfg(windows)]
fn pread_exact(file: &File, mut buf: &mut [u8], mut offset: u64) -> std::io::Result<()> {
    while !buf.is_empty() {
        let n = std::os::windows::fs::FileExt::seek_read(file, buf, offset)?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buf = &mut buf[n..];
        offset += n as u64;
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn pread_exact(_: &File, _: &mut [u8], _: u64) -> std::io::Result<()> {
    Err(std::io::ErrorKind::Unsupported.into())
}

impl FileFrames {
    /// Wrap an open file whose frames are described by `layout` and `offsets`.
    /// `bayer` tags decoded frames that need demosaicing.
    #[must_use]
    pub fn new(
        file: File,
        layout: RowLayout,
        offsets: FrameOffsets,
        bayer: Option<BayerPattern>,
    ) -> Self {
        Self {
            file,
            layout,
            offsets,
            bayer,
            cache: DeviceCache::default(),
        }
    }

    /// Replace the Bayer tag.
    pub fn set_bayer(&mut self, bayer: Option<BayerPattern>) {
        self.bayer = bayer;
    }
}

impl SharedFrames for FileFrames {
    fn frame_count(&self) -> usize {
        self.offsets.count()
    }

    fn layout(&self) -> RowLayout {
        self.layout
    }

    fn bayer(&self) -> Option<BayerPattern> {
        self.bayer
    }

    fn device_cache(&self) -> Option<&DeviceCache> {
        Some(&self.cache)
    }

    fn read_raw(&self, index: usize, buf: &mut Vec<u8>) -> Result<()> {
        let offset = self.offsets.get(index).ok_or_else(|| {
            Error::Invalid(format!(
                "frame {index} out of range ({})",
                self.offsets.count()
            ))
        })?;
        buf.resize(self.layout.frame_bytes(), 0);
        pread_exact(&self.file, buf, offset)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_decode_every_format() {
        let bytes: Vec<u8> = (0..64u8).collect();
        let mut out = vec![0.0f32; 3];
        let mut l = RowLayout {
            width: 3,
            height: 2,
            sample: Sample::U8,
            planes: Planes::One,
            stride: 12,
            bottom_up: false,
        };
        l.row(&bytes, 1, &mut out);
        assert_eq!(out, [12.0 / 255.0, 13.0 / 255.0, 14.0 / 255.0]);
        l.bottom_up = true;
        l.row(&bytes, 1, &mut out);
        assert_eq!(out[0], 0.0);
        l.sample = Sample::U16Le;
        l.row(&bytes, 0, &mut out);
        assert_eq!(out[0], f32::from(u16::from_le_bytes([12, 13])) / 65535.0);
        l.sample = Sample::U16Be;
        l.row(&bytes, 0, &mut out);
        assert_eq!(out[1], f32::from(u16::from_be_bytes([14, 15])) / 65535.0);
        l.planes = Planes::MeanInteger;
        l.sample = Sample::U8;
        l.row(&bytes, 1, &mut out);
        assert_eq!(out[0], (0.0 + 1.0 + 2.0) / (3.0 * 255.0));
    }
}
