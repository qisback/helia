// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Demosaicing of colour-filter-array frames to a mono image.
//!
//! Two interpolators are provided: plain bilinear, and Malvar-He-Cutler
//! (MHC), a 5x5 gradient-corrected linear filter that keeps edges sharper.
//! The mono output is either one colour channel (solar H-alpha users usually
//! want red only) or the mean of all three.
//!
//! # References
//!
//! - Malvar, H. S., He, L.-W. and Cutler, R. (2004), High-quality linear interpolation for demosaicing of
//!   Bayer-patterned color images, Proc. IEEE ICASSP 2004, vol. 3, iii-485, doi:10.1109/ICASSP.2004.1326587.

#![allow(
    clippy::cast_possible_wrap,
    reason = "pixel coordinates are bounded by the 65535 dimension limit and signed offsets are needed for clamping"
)]
#![allow(
    clippy::many_single_char_names,
    reason = "x, y, w, h, dx, dy and r, g, b are the natural names in image and kernel maths"
)]

use crate::frame::{BayerPattern, Frame};

/// Interpolation method.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Method {
    /// Mean of the nearest same-colour samples.
    Bilinear,
    /// Malvar-He-Cutler gradient-corrected interpolation.
    #[default]
    Mhc,
}

/// Which plane becomes the mono image.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Channel {
    /// Red plane only.
    Red,
    /// Green plane only.
    Green,
    /// Blue plane only.
    Blue,
    /// Mean of red, green and blue.
    #[default]
    Luminance,
}

impl Channel {
    fn index(self) -> Option<usize> {
        match self {
            Self::Red => Some(0),
            Self::Green => Some(1),
            Self::Blue => Some(2),
            Self::Luminance => None,
        }
    }
}

/// Mono conversion settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Debayer {
    /// Interpolator.
    pub method: Method,
    /// Output channel.
    pub channel: Channel,
}

impl Debayer {
    /// Convert to mono. Frames without a Bayer tag are returned unchanged.
    #[must_use]
    pub fn apply(&self, frame: &Frame) -> Frame {
        let Some(pattern) = frame.bayer else {
            return frame.clone();
        };
        let (w, h) = (frame.width, frame.height);
        let mut data = vec![0.0f32; w * h];
        self.apply_rows(w, h, pattern, &mut data, &mut |y, buf| {
            buf.copy_from_slice(frame.row(y));
        });
        Frame {
            width: w,
            height: h,
            data,
            bayer: None,
        }
    }

    /// Demosaic a `w` x `h` mosaic into `out` (`w * h` samples), reading the
    /// source through `fetch(y, row)`, which must fill `row` with source row
    /// `y`. Rows may be requested more than once and in any order. This lets a
    /// reader convert stored bytes straight into the demosaic without first
    /// materialising the whole raw frame.
    ///
    /// Each output sample is one fixed 5x5 linear stencil per Bayer phase, so
    /// the work is a handful of contiguous multiply-adds per sample.
    ///
    /// # Panics
    /// If `out.len() != w * h` (caller bug).
    pub fn apply_rows(
        &self,
        w: usize,
        h: usize,
        pattern: BayerPattern,
        out: &mut [f32],
        fetch: &mut dyn FnMut(usize, &mut [f32]),
    ) {
        assert_eq!(out.len(), w * h);
        if w < 5 || h < 5 {
            // Smaller than the stencil: gather and use the border-safe path.
            let mut data = vec![0.0f32; w * h];
            for (y, row) in data.chunks_exact_mut(w).enumerate() {
                fetch(y, row);
            }
            let f = Frame {
                width: w,
                height: h,
                data,
                bayer: Some(pattern),
            };
            out.copy_from_slice(&self.apply_generic(&f).data);
            return;
        }
        let stencils = Stencils::new(*self, pattern);
        // Even-width padded rows, split into even and odd lanes.
        let pw = (w + 4).next_multiple_of(2);
        let half = pw / 2;
        let n_even = w.div_ceil(2);
        let n_odd = w / 2;
        let mut rowbuf = vec![0.0f32; pw];
        let mut lanes = vec![0.0f32; (BAND + 4) * pw];
        let mut acc = [vec![0.0f32; n_even], vec![0.0f32; n_odd]];
        for y0 in (0..h).step_by(BAND) {
            let y1 = (y0 + BAND).min(h);
            for r in 0..y1 - y0 + 4 {
                let sy = reflect(y0 as isize + r as isize - 2, h);
                fetch(sy, &mut rowbuf[2..w + 2]);
                rowbuf[1] = rowbuf[3];
                rowbuf[0] = rowbuf[4];
                rowbuf[w + 2] = rowbuf[w];
                rowbuf[w + 3] = rowbuf[w - 1];
                let (even, odd) = lanes[r * pw..(r + 1) * pw].split_at_mut(half);
                for ((e, o), pair) in even.iter_mut().zip(odd).zip(rowbuf.as_chunks::<2>().0) {
                    *e = pair[0];
                    *o = pair[1];
                }
            }
            for y in y0..y1 {
                let r0 = y - y0;
                for (px, acc) in acc.iter_mut().enumerate() {
                    acc.fill(0.0);
                    for t in &stencils.terms[y & 1][px] {
                        let base = (r0 + t.row) * pw + t.lane * half + t.off;
                        let src = &lanes[base..base + acc.len()];
                        let c = t.coeff;
                        for (a, &v) in acc.iter_mut().zip(src) {
                            *a += c * v;
                        }
                    }
                }
                let out_row = &mut out[y * w..(y + 1) * w];
                for (i, pair) in out_row.chunks_mut(2).enumerate() {
                    pair[0] = acc[0][i];
                    if pair.len() == 2 {
                        pair[1] = acc[1][i];
                    }
                }
            }
        }
    }

    /// Border-safe reference implementation, one channel plane at a time.
    fn apply_generic(self, frame: &Frame) -> Frame {
        let Some(pattern) = frame.bayer else {
            return frame.clone();
        };
        let data = if let Some(c) = self.channel.index() {
            plane(frame, pattern, self.method, c)
        } else {
            let (r, g, b) = (
                plane(frame, pattern, self.method, 0),
                plane(frame, pattern, self.method, 1),
                plane(frame, pattern, self.method, 2),
            );
            r.iter()
                .zip(&g)
                .zip(&b)
                .map(|((r, g), b)| (r + g + b) / 3.0)
                .collect()
        };
        Frame {
            width: frame.width,
            height: frame.height,
            data,
            bayer: None,
        }
    }
}

impl Debayer {
    /// Like [`Self::apply`] but avoids a copy for mono frames.
    #[must_use]
    pub fn apply_owned(&self, frame: Frame) -> Frame {
        if frame.bayer.is_none() {
            frame
        } else {
            self.apply(&frame)
        }
    }
}

/// Output rows demosaiced per block of fetched source rows.
const BAND: usize = 32;

/// Mirror `i` into `0..n` by reflection about the end samples (`-1` maps to
/// `1`, `n` to `n - 2`). Reflecting by an even distance never changes a
/// sample's Bayer parity. Valid for `n >= 3` and `i` within two of the range.
fn reflect(i: isize, n: usize) -> usize {
    let n = n as isize;
    (if i < 0 {
        -i
    } else if i >= n {
        2 * (n - 1) - i
    } else {
        i
    }) as usize
}

/// One multiply-add of a stencil: source row `row` (0..5 = offset -2..=2),
/// lane (0 even, 1 odd) and offset within the lane, scaled by `coeff`.
#[derive(Clone, Copy, Debug)]
struct Term {
    row: usize,
    lane: usize,
    off: usize,
    coeff: f32,
}

/// Stencil terms per output phase `[y & 1][x & 1]`.
struct Stencils {
    terms: [[Vec<Term>; 2]; 2],
}

/// The 5x5 coefficients (`[dy + 2][dx + 2]`) producing the mono value at a
/// site of phase `(px, py)`.
fn stencil_grid(d: Debayer, pattern: BayerPattern, py: usize, px: usize) -> [[f32; 5]; 5] {
    let channels: &[usize] = match d.channel.index() {
        Some(0) => &[0],
        Some(1) => &[1],
        Some(_) => &[2],
        None => &[0, 1, 2],
    };
    let scale = 1.0 / channels.len() as f32;
    let mut grid = [[0.0f32; 5]; 5];
    for &target in channels {
        for (dx, dy, c) in taps(d.method, pattern, target, px, py) {
            grid[(dy + 2) as usize][(dx + 2) as usize] += c * scale;
        }
    }
    grid
}

impl Debayer {
    /// The demosaic as dense 5x5 stencils, one per Bayer phase `y % 2 * 2 + x % 2`,
    /// each `[dy + 2][dx + 2]` row-major (25 coefficients). This is exactly what
    /// [`Self::apply_rows`] evaluates, for backends that run it elsewhere.
    #[must_use]
    pub fn stencil_table(&self, pattern: BayerPattern) -> [[f32; 25]; 4] {
        let mut out = [[0.0f32; 25]; 4];
        for (phase, table) in out.iter_mut().enumerate() {
            let grid = stencil_grid(*self, pattern, phase / 2, phase % 2);
            for (i, c) in grid.iter().flatten().enumerate() {
                table[i] = *c;
            }
        }
        out
    }
}

impl Stencils {
    fn new(d: Debayer, pattern: BayerPattern) -> Self {
        let build = |py: usize, px: usize| -> Vec<Term> {
            let grid = stencil_grid(d, pattern, py, px);
            let mut v = Vec::new();
            for (row, grid_row) in grid.iter().enumerate() {
                for (col, &coeff) in grid_row.iter().enumerate() {
                    if coeff != 0.0 {
                        // The padded sample feeding output x = px + 2i sits at
                        // 2i + px + 2 + dx = 2i + px + col.
                        let t = px + col;
                        v.push(Term {
                            row,
                            lane: t & 1,
                            off: t >> 1,
                            coeff,
                        });
                    }
                }
            }
            v
        };
        Self {
            terms: [[build(0, 0), build(0, 1)], [build(1, 0), build(1, 1)]],
        }
    }
}

/// Interpolation taps `(dx, dy, weight)` for `target` at a site of phase `(x, y)`.
fn taps(
    method: Method,
    pattern: BayerPattern,
    target: usize,
    x: usize,
    y: usize,
) -> Vec<(i32, i32, f32)> {
    type Sites = [(i32, i32)];
    let here = pattern.channel_at(x, y);
    let cross: &Sites = &[(-1, 0), (1, 0), (0, -1), (0, 1)];
    let diag: &Sites = &[(-1, -1), (1, -1), (-1, 1), (1, 1)];
    let far: &Sites = &[(-2, 0), (2, 0), (0, -2), (0, 2)];
    let mut v: Vec<(i32, i32, f32)> = Vec::new();
    let mut add = |sites: &Sites, w: f32| v.extend(sites.iter().map(|&(a, b)| (a, b, w)));
    if here == target {
        add(&[(0, 0)], 1.0);
    } else if target == 1 {
        match method {
            Method::Bilinear => add(cross, 0.25),
            Method::Mhc => {
                add(&[(0, 0)], 0.5);
                add(cross, 0.25);
                add(far, -0.125);
            }
        }
    } else if here == 1 {
        // The wanted colour sits on this row (horizontal) or column.
        let horizontal = pattern.channel_at(x + 1, y) == target;
        let (along, along_far, across_far): (&Sites, &Sites, &Sites) = if horizontal {
            (&[(-1, 0), (1, 0)], &[(-2, 0), (2, 0)], &[(0, -2), (0, 2)])
        } else {
            (&[(0, -1), (0, 1)], &[(0, -2), (0, 2)], &[(-2, 0), (2, 0)])
        };
        match method {
            Method::Bilinear => add(along, 0.5),
            Method::Mhc => {
                add(&[(0, 0)], 5.0 / 8.0);
                add(along, 0.5);
                add(along_far, -0.125);
                add(diag, -0.125);
                add(across_far, 0.5 / 8.0);
            }
        }
    } else {
        match method {
            Method::Bilinear => add(diag, 0.25),
            Method::Mhc => {
                add(&[(0, 0)], 6.0 / 8.0);
                add(diag, 0.25);
                add(far, -1.5 / 8.0);
            }
        }
    }
    v
}

const PAD: usize = 2;

/// Copy with a 2-pixel mirrored border. Reflection by an even distance keeps
/// the Bayer parity of every border sample.
fn padded(frame: &Frame) -> (Vec<f32>, usize) {
    let (w, h) = (frame.width, frame.height);
    let pw = w + 2 * PAD;
    let reflect = |i: i64, n: usize| -> usize {
        let n = n as i64;
        let mut i = i;
        if n == 1 {
            return 0;
        }
        // Bounce until inside; handles frames smaller than the border.
        while i < 0 || i >= n {
            if i < 0 {
                i = -i;
            }
            if i >= n {
                i = 2 * (n - 1) - i;
            }
        }
        i as usize
    };
    let mut out = vec![0.0f32; pw * (h + 2 * PAD)];
    for y in 0..h + 2 * PAD {
        let sy = reflect(y as i64 - PAD as i64, h);
        for x in 0..pw {
            let sx = reflect(x as i64 - PAD as i64, w);
            out[y * pw + x] = frame.data[sy * w + sx];
        }
    }
    (out, pw)
}

fn plane(frame: &Frame, pattern: BayerPattern, method: Method, target: usize) -> Vec<f32> {
    let (w, h) = (frame.width, frame.height);
    let (pad, pw) = padded(frame);
    let mut out = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let (cx, cy) = ((x + PAD) as i64, (y + PAD) as i64);
            let p = |dx: i64, dy: i64| pad[((cy + dy) as usize) * pw + (cx + dx) as usize];
            let here = pattern.channel_at(x, y);
            out[y * w + x] = if here == target {
                p(0, 0)
            } else if target == 1 {
                // Green at a red or blue site.
                match method {
                    Method::Bilinear => 0.25 * (p(-1, 0) + p(1, 0) + p(0, -1) + p(0, 1)),
                    Method::Mhc => {
                        (4.0 * p(0, 0) + 2.0 * (p(-1, 0) + p(1, 0) + p(0, -1) + p(0, 1))
                            - (p(-2, 0) + p(2, 0) + p(0, -2) + p(0, 2)))
                            / 8.0
                    }
                }
            } else if here == 1 {
                // Red or blue at a green site: the wanted colour is on the
                // row (horizontal) or the column (vertical).
                let horizontal = pattern.channel_at(x + 1, y) == target;
                match (method, horizontal) {
                    (Method::Bilinear, true) => p(-1, 0).midpoint(p(1, 0)),
                    (Method::Bilinear, false) => p(0, -1).midpoint(p(0, 1)),
                    (Method::Mhc, true) => {
                        (5.0 * p(0, 0) + 4.0 * (p(-1, 0) + p(1, 0))
                            - (p(-2, 0) + p(2, 0))
                            - (p(-1, -1) + p(1, -1) + p(-1, 1) + p(1, 1))
                            + p(0, -2).midpoint(p(0, 2)))
                            / 8.0
                    }
                    (Method::Mhc, false) => {
                        (5.0 * p(0, 0) + 4.0 * (p(0, -1) + p(0, 1))
                            - (p(0, -2) + p(0, 2))
                            - (p(-1, -1) + p(1, -1) + p(-1, 1) + p(1, 1))
                            + p(-2, 0).midpoint(p(2, 0)))
                            / 8.0
                    }
                }
            } else {
                // Red at blue or blue at red: diagonal neighbours.
                match method {
                    Method::Bilinear => 0.25 * (p(-1, -1) + p(1, -1) + p(-1, 1) + p(1, 1)),
                    Method::Mhc => {
                        (6.0 * p(0, 0) + 2.0 * (p(-1, -1) + p(1, -1) + p(-1, 1) + p(1, 1))
                            - 1.5 * (p(-2, 0) + p(2, 0) + p(0, -2) + p(0, 2)))
                            / 8.0
                    }
                }
            };
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATTERNS: [BayerPattern; 4] = [
        BayerPattern::Rggb,
        BayerPattern::Grbg,
        BayerPattern::Gbrg,
        BayerPattern::Bggr,
    ];

    /// Sample a smooth RGB scene through a CFA.
    fn mosaic(w: usize, h: usize, p: BayerPattern, rgb: impl Fn(f32, f32) -> [f32; 3]) -> Frame {
        let data = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                rgb(x as f32, y as f32)[p.channel_at(x, y)]
            })
            .collect();
        Frame {
            width: w,
            height: h,
            data,
            bayer: Some(p),
        }
    }

    fn interior_err(a: &Frame, truth: impl Fn(f32, f32) -> f32) -> f32 {
        let mut worst = 0.0f32;
        for y in 4..a.height - 4 {
            for x in 4..a.width - 4 {
                worst = worst.max((a.get(x, y) - truth(x as f32, y as f32)).abs());
            }
        }
        worst
    }

    #[test]
    fn flat_field_is_exact_for_every_pattern_method_channel() {
        for p in PATTERNS {
            for method in [Method::Bilinear, Method::Mhc] {
                for channel in [
                    Channel::Red,
                    Channel::Green,
                    Channel::Blue,
                    Channel::Luminance,
                ] {
                    let f = mosaic(11, 9, p, |_, _| [0.2, 0.5, 0.8]);
                    let m = Debayer { method, channel }.apply(&f);
                    let want = match channel {
                        Channel::Red => 0.2,
                        Channel::Green | Channel::Luminance => 0.5,
                        Channel::Blue => 0.8,
                    };
                    assert!(m.bayer.is_none());
                    assert!(
                        m.data.iter().all(|&v| (v - want).abs() < 1e-5),
                        "{p:?} {method:?} {channel:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn linear_ramps_are_reproduced_by_both_methods() {
        // Bilinear and MHC are both exact on planar scenes away from borders.
        for p in PATTERNS {
            for method in [Method::Bilinear, Method::Mhc] {
                let scene = |x: f32, y: f32| [0.01 * x + 0.02 * y, 0.02 * x + 0.01 * y, 0.015 * x];
                let f = mosaic(24, 20, p, scene);
                for (c, channel) in [Channel::Red, Channel::Green, Channel::Blue]
                    .into_iter()
                    .enumerate()
                {
                    let m = Debayer { method, channel }.apply(&f);
                    let err = interior_err(&m, |x, y| scene(x, y)[c]);
                    assert!(err < 1e-4, "{p:?} {method:?} {channel:?} err {err}");
                }
            }
        }
    }

    #[test]
    fn mhc_beats_bilinear_on_a_sharp_edge() {
        let scene = |x: f32, _y: f32| {
            let v = if x < 11.5 { 0.1 } else { 0.9 };
            [v, v, v]
        };
        let f = mosaic(24, 16, BayerPattern::Rggb, scene);
        let rms = |m: Method| {
            let o = Debayer {
                method: m,
                channel: Channel::Green,
            }
            .apply(&f);
            let mut s = 0.0;
            for y in 4..12 {
                for x in 4..20 {
                    s += (o.get(x, y) - scene(x as f32, 0.0)[1]).powi(2);
                }
            }
            s
        };
        assert!(rms(Method::Mhc) < rms(Method::Bilinear));
    }

    /// Deterministic noise so every stencil tap carries a distinct value.
    fn noisy_mosaic(w: usize, h: usize, p: BayerPattern) -> Frame {
        let mut state = 0x9e37_79b9u32 ^ (w * 31 + h) as u32;
        let data = (0..w * h)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 8) as f32 / (1u32 << 24) as f32
            })
            .collect();
        Frame {
            width: w,
            height: h,
            data,
            bayer: Some(p),
        }
    }

    #[test]
    fn stencil_path_matches_the_plane_reference() {
        // Odd and even sizes, narrower and wider than one band, every
        // pattern, method and channel.
        for p in PATTERNS {
            for method in [Method::Bilinear, Method::Mhc] {
                for channel in [
                    Channel::Red,
                    Channel::Green,
                    Channel::Blue,
                    Channel::Luminance,
                ] {
                    for (w, h) in [(5, 5), (8, 6), (13, 71), (40, 33), (64, 100)] {
                        let f = noisy_mosaic(w, h, p);
                        let d = Debayer { method, channel };
                        let fast = d.apply(&f);
                        let slow = d.apply_generic(&f);
                        let worst = fast
                            .data
                            .iter()
                            .zip(&slow.data)
                            .map(|(a, b)| (a - b).abs())
                            .fold(0.0f32, f32::max);
                        assert!(
                            worst < 2e-6,
                            "{p:?} {method:?} {channel:?} {w}x{h}: worst difference {worst}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn rows_are_fetched_from_the_source_as_asked() {
        let f = noisy_mosaic(20, 17, BayerPattern::Grbg);
        let d = Debayer::default();
        let mut out = vec![0.0; 20 * 17];
        let mut fetched = 0usize;
        d.apply_rows(20, 17, BayerPattern::Grbg, &mut out, &mut |y, row| {
            fetched += 1;
            row.copy_from_slice(f.row(y));
        });
        assert!(fetched >= 17);
        assert_eq!(out, d.apply(&f).data);
    }

    #[test]
    fn tiny_frames_do_not_panic() {
        for (w, h) in [(1, 1), (2, 2), (3, 1), (1, 5)] {
            let f = Frame {
                width: w,
                height: h,
                data: vec![0.3; w * h],
                bayer: Some(BayerPattern::Rggb),
            };
            let m = Debayer::default().apply(&f);
            assert_eq!(m.data.len(), w * h);
        }
    }

    #[test]
    fn mono_passes_through() {
        let f = Frame {
            width: 2,
            height: 2,
            data: vec![0.1, 0.2, 0.3, 0.4],
            bayer: None,
        };
        assert_eq!(Debayer::default().apply(&f), f);
    }
}
