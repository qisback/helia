// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Uniform random access to the frames of a capture.

use std::any::Any;
use std::io::{Read, Seek};
use std::path::Path;
use std::sync::Arc;

use crate::debayer::Channel;
use crate::error::{Error, Result};
use crate::frame::{BayerPattern, Frame};
use crate::io::avi::AviReader;
use crate::io::ffmpeg::{self, VideoInfo, VideoOptions};
use crate::io::raw::{FileFrames, Planes, RowLayout, Sample, SharedFrames};
use crate::io::ser::SerReader;

/// Frames of one capture. `read` returns the raw frame as stored (Bayer
/// frames keep their mosaic and [`Frame::bayer`] tag); demosaicing happens
/// later so it can run in parallel.
pub trait FrameSource: Send + Sync {
    /// Width and height shared by every frame.
    fn dimensions(&self) -> (usize, usize);
    /// Number of frames.
    fn frame_count(&self) -> usize;
    /// Decode frame `index`.
    ///
    /// # Errors
    /// Out-of-range index, I/O or format failure.
    fn read(&mut self, index: usize) -> Result<Frame>;
    /// Short human-readable description for logs.
    fn describe(&self) -> String;
    /// Concurrent decoder for this capture, when the container supports
    /// reading frames from several threads at once.
    fn shared(&self) -> Option<&dyn SharedFrames> {
        None
    }
    /// A caveat about the quality of the input (lossy video), for users.
    fn quality_note(&self) -> Option<String> {
        None
    }
    /// A handle that keeps what this source depends on (a decoded video on
    /// disk) alive while it is held, so a later open of the same capture can
    /// reuse it. `None` for ordinary files.
    fn keep_alive(&self) -> Option<Arc<dyn Any + Send + Sync>> {
        None
    }
}

impl FrameSource for Box<dyn FrameSource> {
    fn dimensions(&self) -> (usize, usize) {
        (**self).dimensions()
    }
    fn frame_count(&self) -> usize {
        (**self).frame_count()
    }
    fn read(&mut self, index: usize) -> Result<Frame> {
        (**self).read(index)
    }
    fn describe(&self) -> String {
        (**self).describe()
    }
    fn shared(&self) -> Option<&dyn SharedFrames> {
        (**self).shared()
    }
    fn quality_note(&self) -> Option<String> {
        (**self).quality_note()
    }
    fn keep_alive(&self) -> Option<Arc<dyn Any + Send + Sync>> {
        (**self).keep_alive()
    }
}

/// Bayer handling requested by the user.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BayerSetting {
    /// Use the file's own colour ID, or the sidecar hint for AVI.
    #[default]
    Auto,
    /// Treat frames as mono even if the file says otherwise.
    Mono,
    /// Force this pattern on single-plane frames.
    Pattern(BayerPattern),
}

impl std::str::FromStr for BayerSetting {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "none" | "mono" => Ok(Self::Mono),
            "rggb" => Ok(Self::Pattern(BayerPattern::Rggb)),
            "grbg" => Ok(Self::Pattern(BayerPattern::Grbg)),
            "gbrg" => Ok(Self::Pattern(BayerPattern::Gbrg)),
            "bggr" => Ok(Self::Pattern(BayerPattern::Bggr)),
            other => Err(Error::Invalid(format!(
                "unknown Bayer setting {other:?}; use auto, none, rggb, grbg, gbrg or bggr"
            ))),
        }
    }
}

/// Byte order of 16-bit samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endian {
    /// Least significant byte first.
    Little,
    /// Most significant byte first.
    Big,
}

impl Endian {
    /// `little-endian` or `big-endian`.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Little => "little-endian",
            Self::Big => "big-endian",
        }
    }
}

impl std::str::FromStr for Endian {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "little" | "le" => Ok(Self::Little),
            "big" | "be" => Ok(Self::Big),
            other => Err(Error::Invalid(format!(
                "unknown byte order {other:?}; use little or big"
            ))),
        }
    }
}

/// Options for [`open_capture`]: what the user says about a file, to be used
/// instead of what its header says. The default trusts the file, checking a
/// mono-labelled SER file for a hidden Bayer mosaic.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OpenOptions {
    /// Bayer handling.
    pub bayer: BayerSetting,
    /// Bits per sample, instead of the SER header's (1 to 8 read one byte per
    /// pixel, 9 to 16 two). SER only.
    pub bit_depth: Option<u32>,
    /// Byte order of 16-bit samples, instead of the header flag (and the
    /// check of the data that backs it up). SER only.
    pub endian: Option<Endian>,
    /// Read the rows bottom to top. SER only.
    pub flip_rows: bool,
    /// Colour channel to extract from compressed video (`ffmpeg` does it, so
    /// the decoded copy holds one plane). Other containers ignore it: their
    /// channel is chosen later, when the frames are converted to mono.
    pub channel: Channel,
}

/// In-memory frames, mainly for tests.
pub struct MemorySource(pub Vec<Frame>);

impl FrameSource for MemorySource {
    fn dimensions(&self) -> (usize, usize) {
        self.0.first().map_or((0, 0), |f| (f.width, f.height))
    }
    fn frame_count(&self) -> usize {
        self.0.len()
    }
    fn read(&mut self, index: usize) -> Result<Frame> {
        self.0
            .get(index)
            .cloned()
            .ok_or_else(|| Error::Invalid(format!("frame {index} out of range ({})", self.0.len())))
    }
    fn describe(&self) -> String {
        format!("{} in-memory frames", self.0.len())
    }
    fn shared(&self) -> Option<&dyn SharedFrames> {
        (!self.0.is_empty()).then_some(self)
    }
}

impl SharedFrames for MemorySource {
    fn frame_count(&self) -> usize {
        self.0.len()
    }
    fn layout(&self) -> RowLayout {
        let (width, height) = self.dimensions();
        RowLayout {
            width,
            height,
            sample: Sample::F32,
            planes: Planes::One,
            stride: width * 4,
            bottom_up: false,
        }
    }
    fn bayer(&self) -> Option<BayerPattern> {
        self.0.first().and_then(|f| f.bayer)
    }
    fn read_raw(&self, index: usize, buf: &mut Vec<u8>) -> Result<()> {
        let f = self.0.get(index).ok_or_else(|| {
            Error::Invalid(format!("frame {index} out of range ({})", self.0.len()))
        })?;
        let (w, h) = self.dimensions();
        if (f.width, f.height) != (w, h) {
            return Err(Error::Invalid(
                "frames in the capture differ in size".into(),
            ));
        }
        buf.clear();
        buf.extend(f.data.iter().flat_map(|v| v.to_le_bytes()));
        Ok(())
    }
    fn read_mono(
        &self,
        index: usize,
        debayer: &crate::debayer::Debayer,
        profile: &crate::profile::Profile,
    ) -> Result<Frame> {
        let t = std::time::Instant::now();
        let f = self.0.get(index).ok_or_else(|| {
            Error::Invalid(format!("frame {index} out of range ({})", self.0.len()))
        })?;
        let (w, h) = self.dimensions();
        if (f.width, f.height) != (w, h) {
            return Err(Error::Invalid(
                "frames in the capture differ in size".into(),
            ));
        }
        let copy = f.clone();
        profile.add(crate::profile::Category::Decode, t.elapsed());
        Ok(profile.time(crate::profile::Category::Debayer, || {
            debayer.apply_owned(copy)
        }))
    }
}

/// A window `start..start + count` onto another source.
pub struct LimitedSource<S> {
    inner: S,
    start: usize,
    count: usize,
    cache: crate::io::raw::DeviceCache,
}

impl<S: FrameSource> LimitedSource<S> {
    /// Expose at most `count` frames beginning at `start`.
    #[must_use]
    pub fn new(inner: S, start: usize, count: usize) -> Self {
        let avail = inner.frame_count().saturating_sub(start);
        Self {
            inner,
            start,
            count: count.min(avail),
            cache: crate::io::raw::DeviceCache::default(),
        }
    }
}

impl<S: FrameSource> FrameSource for LimitedSource<S> {
    fn dimensions(&self) -> (usize, usize) {
        self.inner.dimensions()
    }
    fn frame_count(&self) -> usize {
        self.count
    }
    fn read(&mut self, index: usize) -> Result<Frame> {
        if index >= self.count {
            return Err(Error::Invalid(format!(
                "frame {index} out of range ({})",
                self.count
            )));
        }
        self.inner.read(self.start + index)
    }
    fn describe(&self) -> String {
        format!(
            "{} (frames {}..{})",
            self.inner.describe(),
            self.start,
            self.start + self.count
        )
    }
    fn shared(&self) -> Option<&dyn SharedFrames> {
        self.inner.shared().map(|_| self as &dyn SharedFrames)
    }
    fn quality_note(&self) -> Option<String> {
        self.inner.quality_note()
    }
    fn keep_alive(&self) -> Option<Arc<dyn Any + Send + Sync>> {
        self.inner.keep_alive()
    }
}

impl<S: FrameSource> LimitedSource<S> {
    fn inner_shared(&self) -> Result<&dyn SharedFrames> {
        self.inner
            .shared()
            .ok_or_else(|| Error::Unsupported("source cannot be read concurrently".into()))
    }

    fn inner_index(&self, index: usize) -> Result<usize> {
        if index >= self.count {
            return Err(Error::Invalid(format!(
                "frame {index} out of range ({})",
                self.count
            )));
        }
        Ok(self.start + index)
    }
}

impl<S: FrameSource> SharedFrames for LimitedSource<S> {
    fn frame_count(&self) -> usize {
        self.count
    }
    fn layout(&self) -> RowLayout {
        self.inner_shared().map_or_else(
            |_| RowLayout {
                width: 0,
                height: 0,
                sample: Sample::U8,
                planes: Planes::One,
                stride: 0,
                bottom_up: false,
            },
            SharedFrames::layout,
        )
    }
    fn bayer(&self) -> Option<BayerPattern> {
        self.inner.shared().and_then(SharedFrames::bayer)
    }
    fn device_cache(&self) -> Option<&crate::io::raw::DeviceCache> {
        Some(&self.cache)
    }
    fn read_raw(&self, index: usize, buf: &mut Vec<u8>) -> Result<()> {
        self.inner_shared()?.read_raw(self.inner_index(index)?, buf)
    }
    fn read_mono(
        &self,
        index: usize,
        debayer: &crate::debayer::Debayer,
        profile: &crate::profile::Profile,
    ) -> Result<Frame> {
        self.inner_shared()?
            .read_mono(self.inner_index(index)?, debayer, profile)
    }
}

/// SER file source.
pub struct SerSource<R> {
    reader: SerReader<R>,
    bayer: Option<BayerPattern>,
    shared: Option<FileFrames>,
}

impl<R: Read + Seek> SerSource<R> {
    /// Wrap a reader; decoded frames are tagged with `bayer`.
    #[must_use]
    pub fn new(reader: SerReader<R>, bayer: Option<BayerPattern>) -> Self {
        Self {
            reader,
            bayer,
            shared: None,
        }
    }

    /// Attach a concurrent decoder for the same file.
    #[must_use]
    pub fn with_shared(mut self, shared: FileFrames) -> Self {
        self.shared = Some(shared);
        self
    }

    /// Underlying reader (header, timestamps).
    pub fn reader_mut(&mut self) -> &mut SerReader<R> {
        &mut self.reader
    }
}

impl<R: Read + Seek + Send + Sync> FrameSource for SerSource<R> {
    fn dimensions(&self) -> (usize, usize) {
        (
            self.reader.header().width as usize,
            self.reader.header().height as usize,
        )
    }
    fn frame_count(&self) -> usize {
        self.reader.frame_count()
    }
    fn read(&mut self, index: usize) -> Result<Frame> {
        let mut f = self.reader.read_frame(index)?;
        f.bayer = self.bayer;
        Ok(f)
    }
    fn describe(&self) -> String {
        let h = self.reader.header();
        format!(
            "SER {}x{} {}-bit, {} frames",
            h.width, h.height, h.pixel_depth, h.frame_count
        )
    }
    fn shared(&self) -> Option<&dyn SharedFrames> {
        self.shared.as_ref().map(|s| s as &dyn SharedFrames)
    }
}

/// AVI file source.
pub struct AviSource<R> {
    reader: AviReader<R>,
    shared: Option<FileFrames>,
}

impl<R: Read + Seek> AviSource<R> {
    /// Wrap a reader (set its Bayer pattern first).
    #[must_use]
    pub fn new(reader: AviReader<R>) -> Self {
        Self {
            reader,
            shared: None,
        }
    }

    /// Attach a concurrent decoder for the same file.
    #[must_use]
    pub fn with_shared(mut self, shared: FileFrames) -> Self {
        self.shared = Some(shared);
        self
    }
}

impl<R: Read + Seek + Send + Sync> FrameSource for AviSource<R> {
    fn dimensions(&self) -> (usize, usize) {
        (
            self.reader.info().width as usize,
            self.reader.info().height as usize,
        )
    }
    fn frame_count(&self) -> usize {
        self.reader.frame_count()
    }
    fn read(&mut self, index: usize) -> Result<Frame> {
        Ok(self.reader.read_frame(index)?)
    }
    fn describe(&self) -> String {
        let i = self.reader.info();
        format!(
            "AVI {}x{} {:?} ({}), {} frames",
            i.width,
            i.height,
            i.format,
            i.fourcc,
            self.reader.frame_count()
        )
    }
    fn shared(&self) -> Option<&dyn SharedFrames> {
        self.shared.as_ref().map(|s| s as &dyn SharedFrames)
    }
}

/// A compressed video decoded to a cache file by `ffmpeg`, read as SER.
///
/// The cache file goes when the last handle to it drops (this source, or the
/// [`FrameSource::keep_alive`] handle).
pub struct VideoSource {
    inner: Box<dyn FrameSource>,
    cache: Arc<ffmpeg::CacheFile>,
    info: VideoInfo,
    label: String,
}

impl VideoSource {
    /// What `ffprobe` reported about the video.
    #[must_use]
    pub fn info(&self) -> &VideoInfo {
        &self.info
    }
}

impl FrameSource for VideoSource {
    fn dimensions(&self) -> (usize, usize) {
        self.inner.dimensions()
    }
    fn frame_count(&self) -> usize {
        self.inner.frame_count()
    }
    fn read(&mut self, index: usize) -> Result<Frame> {
        self.inner.read(index)
    }
    fn describe(&self) -> String {
        let (w, h) = self.inner.dimensions();
        format!(
            "{} {w}x{h} {}-bit {}, {} frames, decoded with ffmpeg",
            self.label,
            self.info.bit_depth,
            self.info.codec,
            self.inner.frame_count()
        )
    }
    fn shared(&self) -> Option<&dyn SharedFrames> {
        self.inner.shared()
    }
    fn quality_note(&self) -> Option<String> {
        self.info.quality_note()
    }
    fn keep_alive(&self) -> Option<Arc<dyn Any + Send + Sync>> {
        Some(Arc::clone(&self.cache) as Arc<dyn Any + Send + Sync>)
    }
}

/// Decode a compressed video with `ffmpeg` and open the result.
pub(crate) fn open_video(
    path: &Path,
    channel: Channel,
    video: &VideoOptions,
) -> Result<Box<dyn FrameSource>> {
    let decoded = ffmpeg::decode(path, channel, video)?;
    let inner = open_capture(decoded.cache.path(), &OpenOptions::default())?;
    let label = decoded.info.container_label(path);
    Ok(Box::new(VideoSource {
        inner,
        cache: decoded.cache,
        info: decoded.info,
        label,
    }))
}

/// Open a `.ser` or `.avi` capture, or a compressed video through `ffmpeg`.
///
/// For SER the Bayer pattern is the user's, else the header's, else (for a
/// file labelled mono) what the pixels show, else the `SharpCap` sidecar's;
/// see [`crate::io::capture`]. For AVI it comes from `opts`, else from the
/// sidecar hint; 8-bit AVI with neither is treated as mono. Videos (`.mp4`,
/// `.mov`, `.m4v`, `.mkv`, `.webm`, and AVI whose codec the built-in reader
/// cannot handle) need the optional `ffmpeg`; see [`ffmpeg`].
///
/// # Errors
/// I/O, format or unsupported-extension errors, or [`Error::Ffmpeg`].
pub fn open_capture(path: &Path, opts: &OpenOptions) -> Result<Box<dyn FrameSource>> {
    open_capture_with(path, opts, &VideoOptions::default())
}

/// [`open_capture`] with the settings that apply to videos: the cache
/// directory, a frame limit, progress and cancellation (the colour channel is
/// in `opts`).
///
/// # Errors
/// As [`open_capture`]; [`Error::Cancelled`] when `video.cancel` was set.
pub fn open_capture_with(
    path: &Path,
    opts: &OpenOptions,
    video: &VideoOptions,
) -> Result<Box<dyn FrameSource>> {
    crate::io::capture::open_reported_with(path, opts, video).map(|(src, _)| src)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::debayer::Debayer;
    use crate::io::avi::write_test_avi;
    use crate::io::ser::{ColorId, SerHeader, SerWriter};
    use crate::profile::Profile;

    fn mosaic(n: usize, w: usize, h: usize) -> Vec<Frame> {
        (0..n)
            .map(|k| Frame {
                width: w,
                height: h,
                data: (0..w * h)
                    .map(|i| (((i * 7 + k * 13) % 251) as f32) / 251.0)
                    .collect(),
                bayer: None,
            })
            .collect()
    }

    /// A unique file under the temp dir, removed on drop.
    struct TempFile(std::path::PathBuf);

    impl TempFile {
        fn new(name: &str, bytes: &[u8]) -> Self {
            let path = std::env::temp_dir()
                .join(format!("helia-source-test-{}-{name}", std::process::id()));
            std::fs::write(&path, bytes).expect("write temp capture");
            Self(path)
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn ser(color: ColorId, depth: u32, little: bool, frames: &[Frame]) -> Vec<u8> {
        let mut hdr = SerHeader::new(
            frames[0].width as u32,
            frames[0].height as u32,
            color,
            depth,
        );
        hdr.little_endian = little;
        let mut w = SerWriter::new(Cursor::new(Vec::new()), hdr).expect("header");
        for f in frames {
            w.write_frame(f, None).expect("frame");
        }
        w.finish().expect("finish").into_inner()
    }

    /// The concurrent decoder must agree with the sequential reader plus
    /// the standalone demosaic, to the last bit.
    fn assert_shared_matches(file: &TempFile, opts: OpenOptions) {
        let mut src = open_capture(&file.0, &opts).expect("open");
        let d = Debayer::default();
        let profile = Profile::new();
        let want: Vec<Frame> = (0..src.frame_count())
            .map(|i| d.apply_owned(src.read(i).expect("read")))
            .collect();
        let shared = src.shared().expect("shared decoder");
        assert_eq!(shared.frame_count(), want.len());
        for (i, want) in want.iter().enumerate().rev() {
            let got = shared.read_mono(i, &d, &profile).expect("shared read");
            assert_eq!(&got, want, "frame {i}");
        }
        assert!(shared.read_mono(want.len(), &d, &profile).is_err());
    }

    #[test]
    fn concurrent_ser_decoding_matches_sequential_for_every_layout() {
        let frames = mosaic(5, 19, 14);
        let rggb = ColorId::Bayer(BayerPattern::Rggb);
        for (name, color, depth, little) in [
            ("mono8", ColorId::Mono, 8, true),
            ("mono16le", ColorId::Mono, 16, true),
            ("mono16be", ColorId::Mono, 16, false),
            ("bayer8", rggb, 8, true),
            ("bayer16", rggb, 16, true),
        ] {
            let f = TempFile::new(&format!("{name}.ser"), &ser(color, depth, little, &frames));
            assert_shared_matches(&f, OpenOptions::default());
        }
        // A forced pattern on a mono file, and forcing mono on a Bayer file.
        let f = TempFile::new("forced.ser", &ser(ColorId::Mono, 16, true, &frames));
        assert_shared_matches(
            &f,
            OpenOptions {
                bayer: BayerSetting::Pattern(BayerPattern::Bggr),
                ..OpenOptions::default()
            },
        );
        let f = TempFile::new("unbayer.ser", &ser(rggb, 8, true, &frames));
        assert_shared_matches(
            &f,
            OpenOptions {
                bayer: BayerSetting::Mono,
                ..OpenOptions::default()
            },
        );
    }

    #[test]
    fn concurrent_avi_decoding_matches_sequential_for_every_layout() {
        let frames = mosaic(4, 21, 11);
        for (fourcc, bits) in [(b"DIB ", 8u16), (b"Y16 ", 16), (b"DIB ", 24)] {
            for bottom_up in [false, true] {
                let bytes = write_test_avi(&frames, fourcc, bits, bottom_up, Some(2));
                let f = TempFile::new(&format!("{bits}-{bottom_up}.avi"), &bytes);
                assert_shared_matches(&f, OpenOptions::default());
                assert_shared_matches(
                    &f,
                    OpenOptions {
                        bayer: BayerSetting::Pattern(BayerPattern::Grbg),
                        ..OpenOptions::default()
                    },
                );
            }
        }
    }
}
