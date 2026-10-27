// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Granulation statistics as a quality report (issue #56).
//!
//! On a flattened quiet-Sun region of a finished stack ([`analyse`]):
//!
//! * the **RMS contrast**, standard deviation over mean of the image divided by
//!   its own slow background, so brightness scale and gradients drop out;
//! * the azimuthally averaged **power spectrum** of that region, with the
//!   noise floor taken from the highest frequencies, the frequency where the
//!   noise-subtracted power has fallen to 10% of its peak (the **roll-off**,
//!   which seeing and sharpening move) and to 1% (the highest **resolved**
//!   frequency), and the telescope's diffraction **cut-off** `D / lambda` when
//!   the aperture, wavelength and image scale are known;
//! * the **cell size**: the mean spacing of granules and their mean diameter,
//!   from a segmentation of the band-passed region into bright cells.
//!
//! Only for white-light and continuum-like data (the photospheric presets);
//! H-alpha has no granulation. Report it on the unsharpened stack, since
//! sharpening raises the contrast and the high-frequency power. Contrast
//! depends on bit depth, noise and processing: compare like with like.

#![allow(
    clippy::many_single_char_names,
    reason = "x, y, w, h and the filter scales are the natural names in image maths"
)]

use realfft::num_complex::Complex32;
use rustfft::FftPlanner;

use crate::colour::{GRANULE_ARCSEC, Preset};
use crate::filter::gaussian_blur;
use crate::frame::{Frame, Rect};
use crate::measure::{DEFAULT_CROP, centre_crop};

/// Side of the analysed region, the same crop as `helia measure`.
pub const REGION: usize = DEFAULT_CROP;

/// The highest frequency a sampled image holds, cycles per pixel.
pub const NYQUIST: f32 = 0.5;

/// Granules smaller than this, in pixels, are not resolved well enough for a
/// cell size.
pub const MIN_GRANULE_PX: f32 = 3.0;

/// What is known about the optics, for the diffraction cut-off and arcseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Optics {
    /// Aperture in millimetres.
    pub aperture_mm: Option<f32>,
    /// Wavelength in nanometres.
    pub wavelength_nm: Option<f32>,
    /// Image scale in arcseconds per pixel.
    pub arcsec_per_px: Option<f32>,
}

/// One bin of the radially averaged power spectrum.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpectrumBin {
    /// Spatial frequency in cycles per pixel (0 to 0.5).
    pub freq: f32,
    /// Mean power in the bin, in units of the squared RMS contrast.
    pub power: f32,
}

/// The granulation report of one region.
#[derive(Clone, Debug, PartialEq)]
pub struct Granulation {
    /// The region analysed.
    pub region: Rect,
    /// Standard deviation over mean of the flattened region (a fraction).
    pub rms_contrast: f32,
    /// Radially averaged power spectrum.
    pub spectrum: Vec<SpectrumBin>,
    /// Mean power of the highest frequencies, taken as the noise floor.
    pub noise_floor: f32,
    /// Frequency (cycles per pixel) of the largest noise-subtracted power,
    /// leaving out the lowest bins that the flattening has emptied.
    pub peak_freq: f32,
    /// First frequency past the peak where the noise-subtracted power is down
    /// to 10% of the peak's.
    pub rolloff_freq: f32,
    /// Highest frequency whose noise-subtracted power is above 1% of the peak's.
    pub resolved_freq: f32,
    /// Diffraction cut-off `D / lambda` in cycles per pixel, with the optics known.
    pub cutoff_freq: Option<f32>,
    /// Mean spacing of granules: the square root of the area per cell, pixels.
    pub cell_pitch_px: f32,
    /// Mean diameter of the cells that lie wholly inside the region, pixels.
    pub cell_diameter_px: f32,
    /// Cells found (those cut by the region's edge count half).
    pub cells: f32,
    /// Image scale in arcseconds per pixel, when known.
    pub arcsec_per_px: Option<f32>,
}

impl Granulation {
    /// Convert pixels to arcseconds when the scale is known.
    #[must_use]
    pub fn arcsec(&self, px: f32) -> Option<f32> {
        self.arcsec_per_px.map(|s| px * s)
    }

    /// A solar granule in pixels at this scale, when it is known.
    #[must_use]
    pub fn granule_px(&self) -> Option<f32> {
        self.arcsec_per_px.map(|s| GRANULE_ARCSEC / s)
    }

    /// Resolution of the roll-off as a period in pixels (`1 / f`).
    #[must_use]
    pub fn rolloff_period_px(&self) -> f32 {
        1.0 / self.rolloff_freq.max(1e-6)
    }

    /// How much of the frequency range the data can carry it reaches:
    /// resolved frequency over the lower of the diffraction cut-off and the
    /// sampling limit (Nyquist, 0.5 cycles per pixel). `None` without the optics.
    #[must_use]
    pub fn resolved_fraction(&self) -> Option<f32> {
        self.cutoff_freq
            .map(|c| self.resolved_freq / c.min(NYQUIST))
    }

    /// The report as lines for the command line.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let as_arc = |px: f32| {
            self.arcsec(px)
                .map_or(String::new(), |a| format!(" ({a:.2} arcsec)"))
        };
        let mut v = vec![
            format!(
                "region:         {} x {} at ({}, {})",
                self.region.width(),
                self.region.height(),
                self.region.x0,
                self.region.y0
            ),
            format!("RMS contrast:   {:.3}%", self.rms_contrast * 100.0),
            format!(
                "cell size:      pitch {:.2} px{}, diameter {:.2} px{} ({:.0} cells)",
                self.cell_pitch_px,
                as_arc(self.cell_pitch_px),
                self.cell_diameter_px,
                as_arc(self.cell_diameter_px),
                self.cells
            ),
            format!(
                "power spectrum: roll-off {:.3} cycles/px (period {:.2} px{}), \
                 resolved to {:.3} (period {:.2} px{})",
                self.rolloff_freq,
                self.rolloff_period_px(),
                self.arcsec(self.rolloff_period_px())
                    .map_or(String::new(), |a| format!(", {a:.2} arcsec")),
                self.resolved_freq,
                1.0 / self.resolved_freq.max(1e-6),
                self.arcsec(1.0 / self.resolved_freq.max(1e-6))
                    .map_or(String::new(), |a| format!(", {a:.2} arcsec")),
            ),
        ];
        if let (Some(c), Some(f)) = (self.cutoff_freq, self.resolved_fraction()) {
            if c > NYQUIST {
                v.push(format!(
                    "telescope:      diffraction cut-off {c:.3} cycles/px lies above the sampling \
                     limit ({NYQUIST}): the pixel scale, not the telescope, limits the detail; \
                     resolved {:.0}% of the sampling limit",
                    f * 100.0
                ));
            } else {
                v.push(format!(
                    "telescope:      diffraction cut-off {c:.3} cycles/px; resolved {:.0}% of it",
                    f * 100.0
                ));
            }
        }
        v.push(format!("noise floor:    {:.3e}", self.noise_floor));
        if let Some(px) = self.granule_px()
            && px < MIN_GRANULE_PX
        {
            v.push(format!(
                "note:           a granule ({GRANULE_ARCSEC} arcsec) spans {px:.1} px at this scale; below \
                 {MIN_GRANULE_PX} px the cell size measures larger structure and noise, not granules. \
                 The contrast and the spectrum still compare runs of the same capture."
            ));
        }
        v
    }
}

/// Diffraction cut-off in cycles per pixel for aperture `d_mm`, wavelength
/// `nm` and image scale `arcsec_per_px`: `D / lambda` per radian, times the
/// radians per pixel.
#[must_use]
pub fn cutoff_cycles_per_px(d_mm: f32, nm: f32, arcsec_per_px: f32) -> f32 {
    let rad_per_px = f64::from(arcsec_per_px) * std::f64::consts::PI / (180.0 * 3600.0);
    (f64::from(d_mm) * 1e-3 / (f64::from(nm) * 1e-9) * rad_per_px) as f32
}

/// Wavelength in nanometres for the diffraction cut-off of a photospheric
/// preset: sodium D, and 550 nm for white light (broad-band, so an assumption).
#[must_use]
pub fn preset_wavelength_nm(preset: Preset) -> Option<f32> {
    match preset {
        Preset::Na => Some(589.3),
        Preset::White => Some(550.0),
        _ => None,
    }
}

/// The report for a finished stack, or `None` when the filter is not a
/// photospheric one (H-alpha and the other lines have no granulation to
/// measure) or the region is unusable. `disc_radius_px` is the fitted limb,
/// which gives the image scale.
#[must_use]
pub fn for_stack(
    image: &Frame,
    preset: Option<Preset>,
    disc_radius_px: Option<f32>,
    aperture_mm: Option<f32>,
) -> Option<Granulation> {
    let preset = preset.filter(|p| p.is_photospheric())?;
    granulation(
        image,
        &Optics {
            aperture_mm,
            wavelength_nm: preset_wavelength_nm(preset),
            arcsec_per_px: disc_radius_px.and_then(crate::autoap::arcsec_per_px),
        },
    )
}

/// Report on the disc-centre [`REGION`] of `frame`; `None` when it is empty
/// or dark.
#[must_use]
pub fn granulation(frame: &Frame, optics: &Optics) -> Option<Granulation> {
    analyse(frame, centre_crop(frame, REGION), optics)
}

/// Report on `area` of `frame`; `None` when it is smaller than 32 pixels a
/// side or has no signal.
#[must_use]
pub fn analyse(frame: &Frame, area: Rect, optics: &Optics) -> Option<Granulation> {
    let area = area.clamp(frame.width, frame.height);
    let (w, h) = (area.width(), area.height());
    if w < 32 || h < 32 {
        return None;
    }
    let flat = flatten(frame, area)?;
    let n = (w * h) as f64;
    let mean = flat.iter().map(|&v| f64::from(v)).sum::<f64>() / n;
    let var = flat
        .iter()
        .map(|&v| (f64::from(v) - mean).powi(2))
        .sum::<f64>()
        / n;
    let rms_contrast = var.sqrt() as f32;
    if rms_contrast <= 0.0 || !rms_contrast.is_finite() {
        return None;
    }

    let spectrum = power_spectrum(&flat, w, h, mean as f32);
    let shape = SpectrumShape::of(&spectrum);

    let (pitch, diameter, cells) = segment(&flat, w, h);
    let cutoff_freq = match (
        optics.aperture_mm,
        optics.wavelength_nm,
        optics.arcsec_per_px,
    ) {
        (Some(d), Some(l), Some(s)) if d > 0.0 && l > 0.0 && s > 0.0 => {
            Some(cutoff_cycles_per_px(d, l, s))
        }
        _ => None,
    };
    Some(Granulation {
        region: area,
        rms_contrast,
        spectrum,
        noise_floor: shape.floor,
        peak_freq: shape.peak_freq,
        rolloff_freq: shape.rolloff,
        resolved_freq: shape.resolved,
        cutoff_freq,
        cell_pitch_px: pitch,
        cell_diameter_px: diameter,
        cells,
        arcsec_per_px: optics.arcsec_per_px,
    })
}

/// `area / background - 1`, the contrast field. The background is the area
/// blurred with a Gaussian of an eighth of its side, which follows limb
/// darkening and vignetting but not granules.
fn flatten(frame: &Frame, area: Rect) -> Option<Vec<f32>> {
    let (w, h) = (area.width(), area.height());
    let mut data = Vec::with_capacity(w * h);
    for y in area.y0..area.y1 {
        data.extend_from_slice(&frame.row(y)[area.x0..area.x1]);
    }
    let crop = Frame::new(w, h, data).ok()?;
    let mean = crop.mean();
    if !(mean.is_finite() && mean > 0.0) {
        return None;
    }
    let bg = gaussian_blur(&crop, w.min(h) as f32 / 8.0);
    let floor = 0.05 * mean;
    Some(
        crop.data
            .iter()
            .zip(&bg.data)
            .map(|(&v, &b)| if b > floor { v / b - 1.0 } else { 0.0 })
            .collect(),
    )
}

/// Radially averaged power spectrum of the Hann-windowed `flat` (mean `mean`
/// removed), in bins one frequency step wide. Normalised so that white noise
/// of variance `s` has power `s` in every bin.
fn power_spectrum(flat: &[f32], w: usize, h: usize, mean: f32) -> Vec<SpectrumBin> {
    let hann = |i: usize, n: usize| {
        0.5 - 0.5 * (std::f32::consts::TAU * (i as f32 + 0.5) / n as f32).cos()
    };
    let mut buf: Vec<Complex32> = Vec::with_capacity(w * h);
    let mut wsum = 0.0f64;
    for y in 0..h {
        let wy = hann(y, h);
        for x in 0..w {
            let win = wy * hann(x, w);
            wsum += f64::from(win * win);
            buf.push(Complex32::new((flat[y * w + x] - mean) * win, 0.0));
        }
    }
    fft2(&mut buf, w, h, false);
    let side = w.max(h);
    let bins = side / 2;
    let df = 1.0 / side as f32;
    let (mut sum, mut cnt) = (vec![0.0f64; bins], vec![0u32; bins]);
    for ky in 0..h {
        let fy = if ky <= h / 2 {
            ky as f32
        } else {
            ky as f32 - h as f32
        } / h as f32;
        for kx in 0..w {
            let fx = if kx <= w / 2 {
                kx as f32
            } else {
                kx as f32 - w as f32
            } / w as f32;
            let r = fx.hypot(fy);
            let b = (r / df).round() as usize;
            if r < 0.5 && b < bins {
                sum[b] += f64::from(buf[ky * w + kx].norm_sqr());
                cnt[b] += 1;
            }
        }
    }
    (0..bins)
        .filter(|&b| cnt[b] > 0)
        .map(|b| SpectrumBin {
            freq: b as f32 * df,
            power: (sum[b] / f64::from(cnt[b]) / wsum) as f32,
        })
        .collect()
}

/// 2-D FFT in place of a row-major `w` x `h` buffer (unnormalised both ways).
pub(crate) fn fft2(buf: &mut [Complex32], w: usize, h: usize, inverse: bool) {
    let mut planner = FftPlanner::<f32>::new();
    let (row, col) = if inverse {
        (planner.plan_fft_inverse(w), planner.plan_fft_inverse(h))
    } else {
        (planner.plan_fft_forward(w), planner.plan_fft_forward(h))
    };
    for r in buf.chunks_mut(w) {
        row.process(r);
    }
    let mut line = vec![Complex32::new(0.0, 0.0); h];
    for x in 0..w {
        for y in 0..h {
            line[y] = buf[y * w + x];
        }
        col.process(&mut line);
        for y in 0..h {
            buf[y * w + x] = line[y];
        }
    }
}

/// The landmarks of a radially averaged spectrum.
struct SpectrumShape {
    floor: f32,
    peak_freq: f32,
    rolloff: f32,
    resolved: f32,
}

impl SpectrumShape {
    fn of(s: &[SpectrumBin]) -> Self {
        let fallback = Self {
            floor: 0.0,
            peak_freq: 0.1,
            rolloff: 0.1,
            resolved: 0.1,
        };
        if s.len() < 8 {
            return fallback;
        }
        // Noise floor from the top 12% of the frequencies.
        let top = s.len() - (s.len() / 8).max(2);
        let floor = s[top..].iter().map(|b| b.power).sum::<f32>() / (s.len() - top) as f32;
        // Noise-subtracted, smoothed over three bins.
        let sig: Vec<f32> = (0..s.len())
            .map(|i| {
                let lo = i.saturating_sub(1);
                let hi = (i + 1).min(s.len() - 1);
                let m = s[lo..=hi].iter().map(|b| b.power).sum::<f32>() / (hi - lo + 1) as f32;
                (m - floor).max(0.0)
            })
            .collect();
        // Skip the lowest bins, which the flattening has already emptied.
        let first = 2;
        let (mut peak, mut best) = (first, 0.0f32);
        for (i, &v) in sig.iter().enumerate().skip(first) {
            if v > best {
                best = v;
                peak = i;
            }
        }
        if best <= 0.0 {
            return Self { floor, ..fallback };
        }
        let p = sig[peak];
        let rolloff = (peak..s.len())
            .find(|&i| sig[i] <= 0.1 * p)
            .map_or(s[s.len() - 1].freq, |i| s[i].freq);
        let resolved = (peak..s.len())
            .rev()
            .find(|&i| sig[i] > 0.01 * p)
            .map_or(s[peak].freq, |i| s[i].freq);
        Self {
            floor,
            peak_freq: s[peak].freq,
            rolloff,
            resolved,
        }
    }
}

/// Segment the bright cells of `flat` and return the mean spacing, the mean
/// diameter of the whole cells and the cell count.
fn segment(flat: &[f32], w: usize, h: usize) -> (f32, f32, f32) {
    let frame = Frame {
        width: w,
        height: h,
        data: flat.to_vec(),
        bayer: None,
    };
    // The band-pass scale where the variance peaks is the size of the
    // dominant structure; the cells separate best at 0.6 of it (on Voronoi
    // mosaics from 8 to 24 pixels across, this recovers the count within 7%).
    let mut best = (SCALES[0], 0.0f32);
    for &s in &SCALES {
        let (a, b) = (gaussian_blur(&frame, s), gaussian_blur(&frame, 2.0 * s));
        let band: Vec<f32> = a.data.iter().zip(&b.data).map(|(&p, &q)| p - q).collect();
        let m = band.iter().sum::<f32>() / band.len() as f32;
        let v = band.iter().map(|x| (x - m).powi(2)).sum::<f32>() / band.len() as f32;
        if v > best.1 {
            best = (s, v);
        }
    }
    let (cells, pitch, diameter) = segment_at(flat, w, h, (0.6 * best.0).max(0.7), 2.0);
    (pitch, diameter, cells)
}

/// Band-pass scales tried, pixels (a ladder of steps of the square root of two).
const SCALES: [f32; 8] = [0.7, 1.0, 1.4, 2.0, 2.8, 4.0, 5.6, 8.0];

fn segment_at(flat: &[f32], w: usize, h: usize, s1: f32, ratio: f32) -> (f32, f32, f32) {
    let frame = Frame {
        width: w,
        height: h,
        data: flat.to_vec(),
        bayer: None,
    };
    let (a, b) = (gaussian_blur(&frame, s1), gaussian_blur(&frame, ratio * s1));
    let band: Vec<f32> = a.data.iter().zip(&b.data).map(|(&p, &q)| p - q).collect();
    let m = band.iter().sum::<f32>() / band.len() as f32;
    let sd = (band.iter().map(|v| (v - m).powi(2)).sum::<f32>() / band.len() as f32).sqrt();
    let thr = m + 0.25 * sd;
    let mut seen = vec![false; w * h];
    let mut stack: Vec<usize> = Vec::new();
    let (mut whole, mut cut, mut diam) = (0u32, 0u32, 0.0f64);
    for start in 0..w * h {
        if seen[start] || band[start] <= thr {
            continue;
        }
        let (mut area, mut border) = (0u32, false);
        seen[start] = true;
        stack.push(start);
        while let Some(i) = stack.pop() {
            area += 1;
            let (x, y) = (i % w, i / w);
            border |= x == 0 || y == 0 || x == w - 1 || y == h - 1;
            let mut visit = |j: usize| {
                if !seen[j] && band[j] > thr {
                    seen[j] = true;
                    stack.push(j);
                }
            };
            if x > 0 {
                visit(i - 1);
            }
            if x + 1 < w {
                visit(i + 1);
            }
            if y > 0 {
                visit(i - w);
            }
            if y + 1 < h {
                visit(i + w);
            }
        }
        if area < 3 {
            continue;
        }
        if border {
            cut += 1;
        } else {
            whole += 1;
            diam += (4.0 * f64::from(area) / std::f64::consts::PI).sqrt();
        }
    }
    let cells = whole as f32 + 0.5 * cut as f32;
    if cells <= 0.0 {
        return (0.0, 0.0, 0.0);
    }
    let pitch = ((w * h) as f32 / cells).sqrt();
    let diameter = if whole > 0 {
        (diam / f64::from(whole)) as f32
    } else {
        0.0
    };
    (cells, pitch, diameter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Rng;

    /// A Poisson-Voronoi mosaic with `pitch` pixels per cell on average (the
    /// mean cell area is `pitch^2`): every cell has its own brightness
    /// (relative spread `contrast`) and a dark lane of width `lane` between cells.
    fn voronoi(w: usize, h: usize, pitch: f32, contrast: f32, lane: f32, seed: u64) -> Frame {
        let mut rng = Rng::new(seed);
        let count = ((w * h) as f32 / (pitch * pitch)).round() as usize;
        // Seeds over a margin so that the edges are not starved of neighbours.
        let m = 2.0 * pitch;
        let seeds: Vec<(f32, f32, f32)> = (0..(count as f32 * 1.6) as usize + 8)
            .map(|_| {
                (
                    rng.uniform() * (w as f32 + 2.0 * m) - m,
                    rng.uniform() * (h as f32 + 2.0 * m) - m,
                    1.0 + contrast * rng.gauss(),
                )
            })
            .collect();
        let mut data = vec![0.0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let (mut d1, mut d2, mut v1) = (f32::MAX, f32::MAX, 1.0);
                for &(sx, sy, v) in &seeds {
                    let d = ((sx - px).powi(2) + (sy - py).powi(2)).sqrt();
                    if d < d1 {
                        d2 = d1;
                        d1 = d;
                        v1 = v;
                    } else if d < d2 {
                        d2 = d;
                    }
                }
                // Distance to the cell boundary is about (d2 - d1) / 2.
                let edge = 0.5 * (d2 - d1);
                let lane_dark = (-(edge * edge) / (2.0 * lane * lane)).exp();
                data[y * w + x] = v1 * (1.0 - 0.35 * lane_dark);
            }
        }
        Frame::new(w, h, data).unwrap_or_else(|e| panic!("{e}"))
    }

    fn std_over_mean(f: &Frame) -> f32 {
        let m = f.mean();
        (f.data.iter().map(|v| (v - m).powi(2)).sum::<f32>() / f.data.len() as f32).sqrt() / m
    }

    fn noisy(f: &Frame, sigma: f32, seed: u64) -> Frame {
        let mut rng = Rng::new(seed);
        let mut g = f.clone();
        g.data.iter_mut().for_each(|v| *v += sigma * rng.gauss());
        g
    }

    fn optics() -> Optics {
        Optics::default()
    }

    #[test]
    fn cell_size_is_recovered_within_ten_percent() {
        for (pitch, seed) in [(8.0f32, 1u64), (12.0, 2), (16.0, 3)] {
            let img = gaussian_blur(&voronoi(300, 300, pitch, 0.08, 0.9, seed), 1.0);
            let g = analyse(&img, Rect::new(30, 30, 240, 240), &optics()).expect("report");
            eprintln!(
                "pitch {pitch}: found {:.2} diameter {:.2} cells {} (peak period {:.1})",
                g.cell_pitch_px,
                g.cell_diameter_px,
                g.cells,
                1.0 / g.peak_freq
            );
            let err = (g.cell_pitch_px - pitch).abs() / pitch;
            assert!(
                err < 0.10,
                "pitch {pitch}: {} ({:.1}%)",
                g.cell_pitch_px,
                err * 100.0
            );
        }
    }

    #[test]
    fn contrast_follows_the_blurred_truth_and_falls_with_blur() {
        let sharp = voronoi(300, 300, 12.0, 0.08, 0.9, 5);
        let mut last = f32::MAX;
        for sigma in [0.0f32, 1.0, 2.0, 3.0] {
            let img = gaussian_blur(&sharp, sigma);
            let area = Rect::new(30, 30, 240, 240);
            let g = analyse(&img, area, &optics()).expect("report");
            // The truth: std over mean of the same region of the blurred image.
            let mut crop = Frame::zeros(240, 240);
            for y in 0..240 {
                crop.data[y * 240..(y + 1) * 240].copy_from_slice(&img.row(y + 30)[30..270]);
            }
            let truth = std_over_mean(&crop);
            eprintln!(
                "blur {sigma}: contrast {:.4} truth {truth:.4}",
                g.rms_contrast
            );
            assert!(
                (g.rms_contrast - truth).abs() / truth < 0.05,
                "blur {sigma}: {} vs {truth}",
                g.rms_contrast
            );
            assert!(g.rms_contrast < last, "contrast must fall with blur");
            last = g.rms_contrast;
        }
    }

    #[test]
    fn brightness_scale_and_a_flat_gradient_do_not_change_the_report() {
        let base = gaussian_blur(&voronoi(300, 300, 12.0, 0.08, 0.9, 6), 1.0);
        let area = Rect::new(30, 30, 240, 240);
        let a = analyse(&base, area, &optics()).expect("a");
        let mut other = base.clone();
        for y in 0..300 {
            for x in 0..300 {
                other.data[y * 300 + x] *=
                    4.0 * (1.0 + 0.25 * x as f32 / 300.0 - 0.15 * y as f32 / 300.0);
            }
        }
        let b = analyse(&other, area, &optics()).expect("b");
        assert!(
            (a.rms_contrast - b.rms_contrast).abs() / a.rms_contrast < 0.03,
            "{} {}",
            a.rms_contrast,
            b.rms_contrast
        );
        assert!((a.cell_pitch_px - b.cell_pitch_px).abs() / a.cell_pitch_px < 0.05);
        assert!((a.rolloff_freq - b.rolloff_freq).abs() < 0.02);
    }

    #[test]
    fn a_band_limited_image_resolves_to_its_cutoff() {
        let img = voronoi(256, 256, 10.0, 0.1, 0.9, 7);
        let mut buf: Vec<Complex32> = img.data.iter().map(|&v| Complex32::new(v, 0.0)).collect();
        fft2(&mut buf, 256, 256, false);
        for ky in 0..256usize {
            for kx in 0..256usize {
                let fy = if ky <= 128 {
                    ky as f32
                } else {
                    ky as f32 - 256.0
                } / 256.0;
                let fx = if kx <= 128 {
                    kx as f32
                } else {
                    kx as f32 - 256.0
                } / 256.0;
                if fx.hypot(fy) > 0.2 {
                    buf[ky * 256 + kx] = Complex32::new(0.0, 0.0);
                }
            }
        }
        fft2(&mut buf, 256, 256, true);
        let data: Vec<f32> = buf.iter().map(|c| c.re / (256.0 * 256.0)).collect();
        let band = Frame::new(256, 256, data).unwrap_or_else(|e| panic!("{e}"));
        let g = analyse(
            &noisy(&band, 0.002, 9),
            Rect::new(8, 8, 240, 240),
            &optics(),
        )
        .expect("report");
        eprintln!(
            "cut at 0.2: rolloff {:.3} resolved {:.3}",
            g.rolloff_freq, g.resolved_freq
        );
        assert!((g.resolved_freq - 0.2).abs() < 0.03, "{}", g.resolved_freq);
    }

    #[test]
    fn roll_off_moves_down_with_blur_and_up_with_a_bigger_telescope() {
        let sharp = voronoi(300, 300, 10.0, 0.1, 0.9, 8);
        let area = Rect::new(30, 30, 240, 240);
        let mut last = f32::MAX;
        for sigma in [0.6f32, 1.0, 1.6, 2.4] {
            let g = analyse(
                &noisy(&gaussian_blur(&sharp, sigma), 0.002, 4),
                area,
                &optics(),
            )
            .expect("g");
            eprintln!(
                "blur {sigma}: rolloff {:.3} resolved {:.3}",
                g.rolloff_freq, g.resolved_freq
            );
            assert!(
                g.rolloff_freq <= last + 1e-6,
                "rolloff {} after {last}",
                g.rolloff_freq
            );
            last = g.rolloff_freq;
        }
        // 100 mm at 550 nm and 1 arcsec per pixel: 100e-3/550e-9 * 4.848e-6.
        let c = cutoff_cycles_per_px(100.0, 550.0, 1.0);
        assert!((c - 0.8815).abs() < 0.001, "{c}");
        let small = cutoff_cycles_per_px(50.0, 550.0, 1.0);
        assert!((c / small - 2.0).abs() < 1e-4);
    }

    #[test]
    fn the_report_says_when_the_scale_or_the_optics_limit_it() {
        let img = gaussian_blur(&voronoi(300, 300, 12.0, 0.08, 0.9, 12), 1.0);
        let area = Rect::new(30, 30, 240, 240);
        let coarse = Optics {
            aperture_mm: Some(80.0),
            wavelength_nm: Some(589.3),
            arcsec_per_px: Some(2.16),
        };
        let text = analyse(&img, area, &coarse)
            .expect("coarse")
            .lines()
            .concat();
        assert!(text.contains("above the sampling limit"), "{text}");
        assert!(text.contains("spans 0.7 px"), "{text}");
        let fine = Optics {
            aperture_mm: Some(150.0),
            arcsec_per_px: Some(0.2),
            ..coarse
        };
        let g = analyse(&img, area, &fine).expect("fine");
        let text = g.lines().concat();
        assert!(
            text.contains("of it") && !text.contains("sampling limit"),
            "{text}"
        );
        assert!(!text.contains("spans"), "{text}");
        assert!((g.cutoff_freq.expect("cutoff") - 0.2468).abs() < 0.001);
        assert!(g.resolved_fraction().expect("fraction") > 0.0);
        // Nothing is said about the optics when they are not given.
        let bare = analyse(&img, area, &Optics::default()).expect("none");
        assert!(!bare.lines().concat().contains("telescope"));
    }

    #[test]
    fn noise_is_not_mistaken_for_granulation_and_flat_images_give_nothing() {
        let img = gaussian_blur(&voronoi(300, 300, 12.0, 0.08, 0.9, 11), 1.0);
        let area = Rect::new(30, 30, 240, 240);
        let clean = analyse(&img, area, &optics()).expect("clean");
        let noisy = analyse(&noisy(&img, 0.05, 3), area, &optics()).expect("noisy");
        assert!(noisy.noise_floor > 20.0 * clean.noise_floor.max(1e-9));
        assert!(noisy.resolved_freq <= clean.resolved_freq + 0.02);
        assert!(analyse(&Frame::zeros(300, 300), area, &optics()).is_none());
        let flat = Frame::new(300, 300, vec![0.5; 300 * 300]).unwrap_or_else(|e| panic!("{e}"));
        assert!(analyse(&flat, area, &optics()).is_none());
        assert!(analyse(&img, Rect::new(0, 0, 20, 20), &optics()).is_none());
    }
}
