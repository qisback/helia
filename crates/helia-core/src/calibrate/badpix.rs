// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Hot, cold and stuck pixels: finding them and replacing them.
//!
//! A pixel is judged against the median of its neighbours of the same colour
//! (the 8 pixels around it, or at a 2 pixel spacing in a raw colour mosaic).

#![allow(
    clippy::many_single_char_names,
    reason = "x, y, w, h, dx, dy are the natural names in image maths"
)]
#![allow(
    clippy::cast_possible_wrap,
    reason = "pixel coordinates are far below isize::MAX"
)]

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;

use rayon::prelude::*;

use crate::error::{Error, Result};
use crate::frame::Frame;
use crate::io::fits::{self, FitsFormat, HeaderCard};
use crate::io::source::FrameSource;

use super::master::median;

/// Thresholds for finding bad pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DetectOptions {
    /// A pixel of a dark that stands out from its neighbours by more than this
    /// many robust standard deviations is hot (or, below, cold).
    pub hot_sigma: f32,
    /// Smallest deviation (on the 0 to 1 scale) that can count as bad, so a
    /// dark that is almost noise-free does not condemn pixels that differ by one
    /// grey level.
    pub min_deviation: f32,
    /// A pixel of a flat that reads below this fraction of its neighbours is
    /// cold or dead.
    pub cold_fraction: f32,
}

impl Default for DetectOptions {
    fn default() -> Self {
        Self {
            hot_sigma: 6.0,
            min_deviation: 0.012,
            cold_fraction: 0.5,
        }
    }
}

/// Pixels to replace.
#[derive(Clone, Debug, PartialEq)]
pub struct BadPixelMap {
    /// Width of the frames it applies to.
    pub width: usize,
    /// Height of the frames it applies to.
    pub height: usize,
    /// Row-major indices of the bad pixels, ascending.
    pub indices: Vec<u32>,
    /// How many were found as hot (in a dark).
    pub hot: usize,
    /// How many were found as cold or dead (in a flat, or low in a dark).
    pub cold: usize,
    /// How many were found as stuck (in light frames).
    pub stuck: usize,
    /// Where the list came from.
    pub source: String,
}

impl BadPixelMap {
    /// An empty map.
    #[must_use]
    pub fn new(width: usize, height: usize, source: impl Into<String>) -> Self {
        Self {
            width,
            height,
            indices: Vec::new(),
            hot: 0,
            cold: 0,
            stuck: 0,
            source: source.into(),
        }
    }

    /// Add the pixels of `other` (same size).
    ///
    /// # Panics
    /// If the sizes differ (caller bug).
    pub fn merge(&mut self, other: &Self) {
        assert_eq!((self.width, self.height), (other.width, other.height));
        self.indices.extend_from_slice(&other.indices);
        self.indices.sort_unstable();
        self.indices.dedup();
        self.hot += other.hot;
        self.cold += other.cold;
        self.stuck += other.stuck;
        if !other.source.is_empty() && self.source != other.source {
            if self.source.is_empty() {
                self.source.clone_from(&other.source);
            } else {
                self.source = format!("{} + {}", self.source, other.source);
            }
        }
    }

    /// One line for logs and headers.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        for (n, what) in [
            (self.hot, "hot"),
            (self.cold, "cold"),
            (self.stuck, "stuck"),
        ] {
            if n > 0 {
                parts.push(format!("{n} {what}"));
            }
        }
        let detail = if parts.is_empty() {
            String::new()
        } else {
            format!(" ({})", parts.join(", "))
        };
        let src = if self.source.is_empty() {
            String::new()
        } else {
            format!(", from {}", self.source)
        };
        format!("{} pixel(s){detail}{src}", self.indices.len())
    }

    /// Read a map from a FITS image: any pixel above 0.5 is bad.
    ///
    /// # Errors
    /// Unreadable or malformed file.
    pub fn read(path: &Path) -> Result<Self> {
        let source = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let f = fits::read_fits(BufReader::new(File::open(path)?))?;
        let mut m = Self::new(f.width, f.height, source);
        m.indices = f
            .data
            .iter()
            .enumerate()
            .filter(|&(_, &v)| v > 0.5)
            .map(|(i, _)| i as u32)
            .collect();
        m.hot = m.indices.len();
        Ok(m)
    }

    /// Read an `x y` text list for frames of `width` x `height`.
    ///
    /// # Errors
    /// Unreadable file, a line that is not two integers, or a pixel outside
    /// the frame.
    pub fn read_list(path: &Path, width: usize, height: usize) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let mut m = Self::new(
            width,
            height,
            path.file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
        );
        for (n, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let v: Vec<usize> = line
                .split([' ', ',', '\t'])
                .filter(|s| !s.is_empty())
                .map(str::parse::<usize>)
                .collect::<std::result::Result<_, _>>()
                .map_err(|_| {
                    Error::Invalid(format!(
                        "{}:{}: expected two whole numbers, x and y",
                        path.display(),
                        n + 1
                    ))
                })?;
            let [x, y] = v[..] else {
                return Err(Error::Invalid(format!(
                    "{}:{}: expected two whole numbers, x and y",
                    path.display(),
                    n + 1
                )));
            };
            if x >= width || y >= height {
                return Err(Error::Invalid(format!(
                    "{}:{}: pixel ({x}, {y}) is outside the {width}x{height} frame",
                    path.display(),
                    n + 1
                )));
            }
            m.indices.push((y * width + x) as u32);
        }
        m.indices.sort_unstable();
        m.indices.dedup();
        m.hot = m.indices.len();
        Ok(m)
    }

    /// Load a map from `path`: FITS directly, anything else as an `x y` list
    /// for frames of the given size.
    ///
    /// # Errors
    /// As [`Self::read`] and [`Self::read_list`].
    pub fn load(path: &Path, width: usize, height: usize) -> Result<Self> {
        let fits = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "fit" | "fits" | "fts"));
        if fits {
            Self::read(path)
        } else {
            Self::read_list(path, width, height)
        }
    }

    /// Write as a float FITS image, 1 for a bad pixel and 0 elsewhere, and
    /// print-friendly counts in the header.
    ///
    /// # Errors
    /// I/O failure.
    pub fn write(&self, path: &Path) -> Result<()> {
        let mut data = vec![0.0f32; self.width * self.height];
        for &i in &self.indices {
            data[i as usize] = 1.0;
        }
        let frame = Frame {
            width: self.width,
            height: self.height,
            data,
            bayer: None,
        };
        let cards = [
            HeaderCard::string("HELIAKND", "badpix", "Helia bad-pixel map"),
            HeaderCard::int("HELIAHOT", count(self.hot), "Hot pixels"),
            HeaderCard::int("HELIACLD", count(self.cold), "Cold and dead pixels"),
            HeaderCard::int("HELIASTK", count(self.stuck), "Stuck pixels"),
        ];
        let history = [format!("Helia bad-pixel map: {}", self.describe())];
        let mut w = BufWriter::new(File::create(path)?);
        fits::write_fits_cards(&mut w, &frame, FitsFormat::Float32, &cards, &history)?;
        std::io::Write::flush(&mut w)?;
        Ok(())
    }
}

fn count(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// Median of the neighbours of every pixel: the 8 around it at distance
/// `step`, those that exist. Edge pixels use fewer.
fn neighbour_median(data: &[f32], w: usize, h: usize, step: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let mut buf = [0.0f32; 8];
        for (x, o) in row.iter_mut().enumerate() {
            let mut n = 0;
            for dy in [-1isize, 0, 1] {
                for dx in [-1isize, 0, 1] {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let (nx, ny) = (
                        x as isize + dx * step as isize,
                        y as isize + dy * step as isize,
                    );
                    if nx >= 0 && ny >= 0 && (nx as usize) < w && (ny as usize) < h {
                        buf[n] = data[ny as usize * w + nx as usize];
                        n += 1;
                    }
                }
            }
            *o = median(&mut buf[..n]);
        }
    });
    out
}

/// Smallest and largest of the neighbours of every pixel, as for
/// [`neighbour_median`].
fn neighbour_range(data: &[f32], w: usize, h: usize, step: usize) -> (Vec<f32>, Vec<f32>) {
    let mut lo = vec![0.0f32; w * h];
    let mut hi = vec![0.0f32; w * h];
    lo.par_chunks_mut(w)
        .zip(hi.par_chunks_mut(w))
        .enumerate()
        .for_each(|(y, (lo_row, hi_row))| {
            for x in 0..w {
                let (mut a, mut b) = (f32::INFINITY, f32::NEG_INFINITY);
                for dy in [-1isize, 0, 1] {
                    for dx in [-1isize, 0, 1] {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let (nx, ny) = (
                            x as isize + dx * step as isize,
                            y as isize + dy * step as isize,
                        );
                        if nx >= 0 && ny >= 0 && (nx as usize) < w && (ny as usize) < h {
                            let v = data[ny as usize * w + nx as usize];
                            a = a.min(v);
                            b = b.max(v);
                        }
                    }
                }
                lo_row[x] = a;
                hi_row[x] = b;
            }
        });
    (lo, hi)
}

/// Robust spread (1.4826 x MAD) of `r`.
fn robust_sigma(r: &[f32]) -> f32 {
    let mut v = r.to_vec();
    let m = median(&mut v);
    let mut dev: Vec<f32> = r.iter().map(|&x| (x - m).abs()).collect();
    1.4826 * median(&mut dev)
}

/// Find hot pixels in a master `dark` and cold or dead pixels in a master
/// `flat` (normalised), either of which may be absent. `mosaic` says the
/// frames are a raw colour mosaic.
///
/// # Panics
/// If both are given and differ in size (caller bug).
#[must_use]
pub fn detect(
    dark: Option<&Frame>,
    flat: Option<&Frame>,
    mosaic: bool,
    opts: &DetectOptions,
    source: &str,
) -> BadPixelMap {
    let step = if mosaic { 2 } else { 1 };
    let (w, h) = dark.or(flat).map_or((0, 0), |f| (f.width, f.height));
    let mut map = BadPixelMap::new(w, h, source);
    let mut flags = vec![0u8; w * h];
    if let Some(d) = dark {
        let lm = neighbour_median(&d.data, w, h, step);
        let r: Vec<f32> = d.data.iter().zip(&lm).map(|(&a, &b)| a - b).collect();
        let sigma = robust_sigma(&r);
        let lim = (opts.hot_sigma * sigma).max(opts.min_deviation);
        for (i, &v) in r.iter().enumerate() {
            if v > lim {
                flags[i] = 1;
            } else if v < -lim {
                flags[i] = 2;
            }
        }
    }
    if let Some(f) = flat {
        assert_eq!((f.width, f.height), (w, h));
        let lm = neighbour_median(&f.data, w, h, step);
        for (i, (&v, &m)) in f.data.iter().zip(&lm).enumerate() {
            if m > 0.0 && v < opts.cold_fraction * m {
                flags[i] = 2;
            }
        }
    }
    for (i, &fl) in flags.iter().enumerate() {
        match fl {
            1 => {
                map.hot += 1;
                map.indices.push(i as u32);
            }
            2 => {
                map.cold += 1;
                map.indices.push(i as u32);
            }
            _ => {}
        }
    }
    map
}

/// Mean and temporal spread of every pixel over a sample of light frames.
#[derive(Clone, Debug)]
pub struct LightStats {
    /// Width.
    pub width: usize,
    /// Height.
    pub height: usize,
    /// Per-pixel mean over the sampled frames.
    pub mean: Vec<f32>,
    /// Per-pixel standard deviation over the sampled frames.
    pub std: Vec<f32>,
    /// Frames sampled.
    pub frames: usize,
}

/// Sample up to `want` frames of `src`, evenly spread, and measure each pixel's
/// mean and spread.
///
/// # Errors
/// Read failure.
pub fn sample_lights(src: &mut dyn FrameSource, want: usize) -> Result<LightStats> {
    let (w, h) = src.dimensions();
    let n = src.frame_count();
    let take = want.clamp(1, n.max(1)).min(n);
    let mut sum = vec![0.0f64; w * h];
    let mut sq = vec![0.0f64; w * h];
    for k in 0..take {
        let f = src.read(k * n / take)?;
        for ((s, q), &v) in sum.iter_mut().zip(sq.iter_mut()).zip(&f.data) {
            *s += f64::from(v);
            *q += f64::from(v) * f64::from(v);
        }
    }
    let t = take as f64;
    let mean: Vec<f32> = sum.iter().map(|&s| (s / t) as f32).collect();
    let std: Vec<f32> = sum
        .iter()
        .zip(&sq)
        .map(|(&s, &q)| ((q / t - (s / t) * (s / t)).max(0.0)).sqrt() as f32)
        .collect();
    Ok(LightStats {
        width: w,
        height: h,
        mean,
        std,
        frames: take,
    })
}

/// Stuck pixels of a light capture: samples up to `want` frames of `src`,
/// finds the pixels of [`detect_stuck`], and leaves out those inside burned-in
/// text (a timestamp is as stuck as a bad pixel, and is masked separately).
///
/// # Errors
/// Read failure.
pub fn find_stuck(
    src: &mut dyn FrameSource,
    want: usize,
    mosaic: bool,
    opts: &DetectOptions,
    source: &str,
) -> Result<BadPixelMap> {
    let n = src.frame_count();
    let probe: Vec<Frame> = crate::mask::sample_indices(n, crate::mask::OVERLAY_SAMPLES.min(n))
        .into_iter()
        .map(|i| src.read(i))
        .collect::<Result<_>>()?;
    let text = crate::mask::detect_overlay(&probe);
    let stats = sample_lights(src, want)?;
    Ok(detect_stuck(&stats, mosaic, opts, source, &text))
}

/// Find pixels that stay off from their neighbours in every light frame:
/// those whose mean stands out from their neighbours' by more than `hot_sigma`
/// local robust deviations and by more than `hot_sigma` standard errors of a
/// mean of the sampled frames, lies outside the range of the neighbours (so a
/// pixel on the slope of the limb, which lies between them, is never one), and
/// fluctuates no more than the neighbours do. The Sun moves over the sensor, a
/// bad pixel does not. Pixels inside `exclude` (burned-in text, which is as
/// stuck as a bad pixel) are left out, with a margin of 4 pixels.
#[must_use]
pub fn detect_stuck(
    stats: &LightStats,
    mosaic: bool,
    opts: &DetectOptions,
    source: &str,
    exclude: &[crate::frame::Rect],
) -> BadPixelMap {
    let (w, h) = (stats.width, stats.height);
    let step = if mosaic { 2 } else { 1 };
    let lm = neighbour_median(&stats.mean, w, h, step);
    let lsd = neighbour_median(&stats.std, w, h, step);
    let (nlo, nhi) = neighbour_range(&stats.mean, w, h, step);
    let r: Vec<f32> = stats.mean.iter().zip(&lm).map(|(&a, &b)| a - b).collect();
    let abs_r: Vec<f32> = r.iter().map(|v| v.abs()).collect();
    // The local scale of the residual: the median over the neighbours.
    let local = neighbour_median(&abs_r, w, h, step);
    let mut map = BadPixelMap::new(w, h, source);
    let excluded = |i: usize| {
        let (x, y) = (i % w, i / w);
        exclude
            .iter()
            .any(|r| x + 4 >= r.x0 && x < r.x1 + 4 && y + 4 >= r.y0 && y < r.y1 + 4)
    };
    for (i, &v) in r.iter().enumerate() {
        if excluded(i) {
            continue;
        }
        // Beyond what the noise of a mean of `frames` frames explains.
        let error_of_mean = lsd[i] / (stats.frames.max(1) as f32).sqrt();
        let lim = (opts.hot_sigma * 1.4826 * local[i])
            .max(opts.hot_sigma * error_of_mean)
            .max(opts.min_deviation);
        let m = stats.mean[i];
        let outside = m > nhi[i] + 0.25 * lim || m < nlo[i] - 0.25 * lim;
        if v.abs() > lim && outside && stats.std[i] <= 1.5 * lsd[i].max(1e-4) {
            if v > 0.0 {
                map.hot += 1;
            } else {
                map.cold += 1;
            }
            map.stuck += 1;
            map.indices.push(i as u32);
        }
    }
    map
}

/// Neighbour tables for replacing a fixed set of pixels.
#[derive(Clone, Debug)]
pub struct Cosmetic {
    targets: Vec<u32>,
    /// `targets[k]` takes the median of `nbr[start[k]..start[k + 1]]`.
    start: Vec<u32>,
    nbr: Vec<u32>,
}

impl Cosmetic {
    /// Tables for `bad` (ascending pixel indices) in a `width` x `height`
    /// frame. The neighbours are good pixels of the same colour: the ring at
    /// 1 step (2 pixels in a mosaic), widened to 2 and 3 steps while fewer
    /// than four are found.
    #[must_use]
    pub fn new(width: usize, height: usize, mosaic: bool, bad: &[u32]) -> Self {
        let step = if mosaic { 2isize } else { 1 };
        let mut is_bad = vec![false; width * height];
        for &i in bad {
            is_bad[i as usize] = true;
        }
        let mut targets = Vec::with_capacity(bad.len());
        let mut start = vec![0u32];
        let mut nbr = Vec::new();
        for &i in bad {
            let (x, y) = ((i as usize % width) as isize, (i as usize / width) as isize);
            let first = nbr.len();
            for ring in 1..=3isize {
                nbr.truncate(first);
                for dy in -ring..=ring {
                    for dx in -ring..=ring {
                        if dx.abs().max(dy.abs()) != ring {
                            continue;
                        }
                        let (nx, ny) = (x + dx * step, y + dy * step);
                        if nx >= 0 && ny >= 0 && (nx as usize) < width && (ny as usize) < height {
                            let j = ny as usize * width + nx as usize;
                            if !is_bad[j] {
                                nbr.push(j as u32);
                            }
                        }
                    }
                }
                if nbr.len() - first >= 4 {
                    break;
                }
            }
            if nbr.len() > first {
                targets.push(i);
                start.push(nbr.len() as u32);
            }
        }
        Self {
            targets,
            start,
            nbr,
        }
    }

    /// Pixels that will be replaced.
    #[must_use]
    pub fn len(&self) -> usize {
        self.targets.len()
    }

    /// True when nothing will be replaced.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }

    /// Replace the bad pixels of `data` by the median of their neighbours.
    pub fn apply(&self, data: &mut [f32]) {
        let mut buf: Vec<f32> = Vec::with_capacity(32);
        for (k, &t) in self.targets.iter().enumerate() {
            buf.clear();
            let (a, b) = (self.start[k] as usize, self.start[k + 1] as usize);
            buf.extend(self.nbr[a..b].iter().map(|&j| data[j as usize]));
            data[t as usize] = median(&mut buf);
        }
    }
}
