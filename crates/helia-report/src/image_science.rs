// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Image science: what the stack holds at each spatial frequency (the radially
//! averaged power spectrum and an MTF-like transfer curve of stack, best frame
//! and sharpened image), the limb-darkening profile against its fitted law, the
//! limb's measured blur, the histograms with their clipping, and the noise.

use std::fmt::Write as _;

use helia_core::granulation::{self, NYQUIST, Optics};
use helia_core::limbdark;
use helia_core::pipeline::StackOutput;
use helia_core::{Frame, Rect};
use plotters::prelude::*;
use serde::{Deserialize, Serialize};

use crate::bundle::{Bundle, Figure, Section, Table};
use crate::canvas::{self, FONT, Size, palette, plot_err, tick};
use crate::{Error, PlotKind, Result};

/// Frames and numbers the stack itself does not keep.
#[derive(Clone, Debug, Default)]
pub struct Extras {
    /// The best single frame (as the pipeline reads it: one colour plane).
    pub best_frame: Option<Frame>,
    /// The image after sharpening and adjustment, as it will be written.
    pub sharpened: Option<Frame>,
    /// Histograms of the raw capture, per colour plane.
    pub capture: Option<CaptureHistograms>,
    /// Aperture in millimetres, for the diffraction cut-off.
    pub aperture_mm: Option<f32>,
    /// Wavelength in nanometres, for the diffraction cut-off.
    pub wavelength_nm: Option<f32>,
    /// When the capture was taken and how the picture sits against the sky,
    /// for the heliographic grids and the sunspot table.
    pub solar: crate::solar::SolarInputs,
}

impl Extras {
    /// Read what the stack does not keep from the capture: the best frame as
    /// the pipeline reads it, and a few raw frames for the histograms.
    /// Failures leave the field empty. `plane` is the colour plane stacked
    /// and `debayer` how it is made from a mosaic.
    #[must_use]
    pub fn gather(
        src: &mut dyn helia_core::io::source::FrameSource,
        debayer: helia_core::debayer::Debayer,
        out: &StackOutput,
    ) -> Self {
        let d = helia_core::debayer::Debayer {
            channel: out.channel,
            ..debayer
        };
        let n = src.frame_count();
        // The pipeline flattens burned-in text and exclusions in every frame;
        // a raw frame is flattened the same way, or the text skews the spectra.
        let flatten = |mut f: Frame| {
            let level = f.mean();
            for r in &out.masked {
                let r = r.clamp(f.width, f.height);
                for y in r.y0..r.y1 {
                    let w = f.width;
                    f.data[y * w + r.x0..y * w + r.x1].fill(level);
                }
            }
            f
        };
        let best_frame = out
            .kept
            .first()
            .and_then(|&i| src.read(i).ok())
            .map(|f| d.apply_owned(flatten(f)));
        let sample: Vec<Frame> = helia_core::mask::sample_indices(n, 6.min(n))
            .into_iter()
            .filter_map(|i| src.read(i).ok())
            .map(flatten)
            .collect();
        Self {
            best_frame,
            capture: CaptureHistograms::from_raw(&sample),
            ..Self::default()
        }
    }
}

/// One curve of the spectrum plots.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpectrumCurve {
    /// What the curve is: `stack`, `best frame` or `sharpened`.
    pub name: String,
    /// Spatial frequency, cycles per pixel.
    pub freq: Vec<f32>,
    /// Radially averaged power (of the contrast image, so comparable between images).
    pub power: Vec<f32>,
    /// Noise floor of this image, same units as `power`.
    pub noise_floor: f32,
    /// Highest frequency still above 1% of the peak and the noise.
    pub resolved_freq: f32,
}

/// Power spectra and the marks drawn on them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpectrumData {
    /// Curves, stack first.
    pub curves: Vec<SpectrumCurve>,
    /// Frequency of the granulation peak of the stack, cycles per pixel.
    pub granulation_peak: f32,
    /// Where the stack's power has fallen to 10% of its peak.
    pub rolloff: f32,
    /// The telescope's diffraction cut-off, when aperture, wavelength and scale are known.
    pub diffraction_cutoff: Option<f32>,
    /// Aperture, millimetres.
    pub aperture_mm: Option<f32>,
    /// Wavelength, nanometres.
    pub wavelength_nm: Option<f32>,
    /// Image scale, arcseconds per pixel.
    pub arcsec_per_px: Option<f32>,
    /// The square of the disc centre the spectra are of.
    pub region: [usize; 4],
}

/// The measured limb profile and the law fitted to it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LimbProfileData {
    /// Law name.
    pub law: String,
    /// Coefficient names.
    pub names: Vec<String>,
    /// Coefficients.
    pub coefficients: Vec<f32>,
    /// RMS of the rings about the law, share of the centre intensity.
    pub rms: f32,
    /// Limb level of the law relative to the centre.
    pub limb_level: f32,
    /// `mu` (cosine of the angle from the disc centre's normal) of each ring.
    pub mu: Vec<f32>,
    /// Measured level of each ring relative to the centre.
    pub measured: Vec<f32>,
    /// The law at each ring.
    pub model: Vec<f32>,
}

/// The limb's edge spread function and the blur fitted to it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LimbPsfData {
    /// Radius from the fitted edge, pixels (negative inside the disc).
    pub offset: Vec<f32>,
    /// Measured level.
    pub measured: Vec<f32>,
    /// Fitted model.
    pub model: Vec<f32>,
    /// Gaussian sigma of the blur, pixels, pixel width taken out.
    pub sigma_px: f32,
    /// Full width at half maximum, pixels.
    pub fwhm_px: f32,
    /// Sigma in arcseconds, when the scale is known.
    pub sigma_arcsec: Option<f32>,
    /// FWHM in arcseconds.
    pub fwhm_arcsec: Option<f32>,
    /// Misfit as a share of the step from disc to sky.
    pub misfit: f32,
}

/// Histogram of one channel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChannelHistogram {
    /// Channel name.
    pub name: String,
    /// Counts in equal bins from `lo` to `hi`.
    pub counts: Vec<u32>,
    /// Lowest value of the first bin.
    pub lo: f32,
    /// Highest value of the last bin.
    pub hi: f32,
    /// Share of the pixels at the lowest value (black clipping), 0 to 1.
    pub low_clip: f32,
    /// Share of the lit pixels on the plateau at the top (white or sensor clipping).
    pub high_clip: f32,
    /// Where the top plateau is (the sensor limit or the white point).
    pub ceiling: f32,
}

/// Histograms of the raw capture, per colour plane.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CaptureHistograms {
    /// One per plane (red, green, blue for a mosaic; one for mono).
    pub planes: Vec<ChannelHistogram>,
    /// Frames sampled.
    pub frames: usize,
}

/// The noise of one image.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NoiseRow {
    /// Image name.
    pub name: String,
    /// Median level of the disc-centre square.
    pub signal: f32,
    /// Noise standard deviation.
    pub sigma: f32,
    /// Signal over noise.
    pub snr: f32,
}

/// Everything the image science plots are drawn from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImageScienceData {
    /// Layout version of this file.
    pub version: u32,
    /// Power spectra and the MTF-like curves.
    pub spectrum: Option<SpectrumData>,
    /// Limb profile and its law.
    pub limb: Option<LimbProfileData>,
    /// Limb blur.
    pub psf: Option<LimbPsfData>,
    /// Histograms of the written image (stack or sharpened), one per channel.
    pub output_histogram: Vec<ChannelHistogram>,
    /// Which image the output histogram is of.
    pub output_name: String,
    /// Histograms of the raw capture.
    pub capture_histogram: Option<CaptureHistograms>,
    /// Noise of each image.
    pub noise: Vec<NoiseRow>,
    /// Frames stacked.
    pub kept: usize,
}

/// A curve to draw: its legend name, colour and points.
type Series = (String, RGBColor, Vec<(f64, f64)>);

/// Bins of a histogram.
const BINS: usize = 128;

fn histogram(name: &str, values: &[f32], lo: f32, hi: f32) -> ChannelHistogram {
    let mut counts = vec![0u32; BINS];
    let span = (hi - lo).max(1e-12);
    let mut at_low = 0usize;
    let mut finite = 0usize;
    let mut sorted: Vec<f32> = Vec::new();
    for &v in values {
        if !v.is_finite() {
            continue;
        }
        finite += 1;
        sorted.push(v);
        let b = (((v - lo) / span) * BINS as f32)
            .floor()
            .clamp(0.0, (BINS - 1) as f32) as usize;
        counts[b] += 1;
        if v <= lo + 1e-6 {
            at_low += 1;
        }
    }
    sorted.sort_by(f32::total_cmp);
    let ceiling = sorted
        .get(((sorted.len().saturating_sub(1)) as f32 * 0.995) as usize)
        .copied()
        .unwrap_or(hi);
    let tol = (0.015 * ceiling).min(1e-3);
    let lit: Vec<f32> = sorted
        .iter()
        .copied()
        .filter(|&v| v >= 0.25 * ceiling)
        .collect();
    let flat = lit.iter().filter(|&&v| v >= ceiling - tol).count();
    ChannelHistogram {
        name: name.to_string(),
        counts,
        lo,
        hi,
        low_clip: if finite > 0 {
            at_low as f32 / finite as f32
        } else {
            0.0
        },
        // A plane whose top is far below the range holds no signal: its plateau
        // is its black level, not clipping.
        high_clip: if lit.is_empty() || ceiling < 0.3 * hi {
            0.0
        } else {
            flat as f32 / lit.len() as f32
        },
        ceiling,
    }
}

impl CaptureHistograms {
    /// Histograms of the planes of raw frames (a Bayer mosaic's red, green and
    /// blue, or the one plane of a mono camera). `None` without frames.
    #[must_use]
    pub fn from_raw(frames: &[Frame]) -> Option<Self> {
        let first = frames.first()?;
        let mosaic = first.bayer.is_some();
        let names: Vec<&str> = if mosaic {
            vec!["red", "green", "blue"]
        } else {
            vec!["mono"]
        };
        let mut planes: Vec<Vec<f32>> = vec![Vec::new(); names.len()];
        for f in frames {
            for y in 0..f.height {
                for (x, &v) in f.row(y).iter().enumerate() {
                    let c = f.bayer.filter(|_| mosaic).map_or(0, |p| p.channel_at(x, y));
                    planes[c.min(names.len() - 1)].push(v);
                }
            }
        }
        let max = planes
            .iter()
            .flatten()
            .copied()
            .filter(|v| v.is_finite())
            .fold(0.0f32, f32::max)
            .max(1e-6);
        Some(Self {
            planes: names
                .iter()
                .zip(&planes)
                .map(|(n, p)| histogram(n, p, 0.0, max))
                .collect(),
            frames: frames.len(),
        })
    }
}

fn spectrum_of(
    name: &str,
    f: &Frame,
    optics: &Optics,
) -> Option<(SpectrumCurve, granulation::Granulation)> {
    let g = granulation::granulation(f, optics)?;
    let (freq, power): (Vec<f32>, Vec<f32>) = g
        .spectrum
        .iter()
        .filter(|b| b.freq > 0.0 && b.power > 0.0 && b.power.is_finite())
        .map(|b| (b.freq, b.power))
        .unzip();
    if freq.len() < 4 {
        return None;
    }
    Some((
        SpectrumCurve {
            name: name.to_string(),
            freq,
            power,
            noise_floor: g.noise_floor,
            resolved_freq: g.resolved_freq,
        },
        g,
    ))
}

/// Linear interpolation of a curve at `x` (clamped at the ends).
fn interp(xs: &[f32], ys: &[f32], x: f32) -> f32 {
    if x <= xs[0] {
        return ys[0];
    }
    let i = xs.partition_point(|&v| v < x);
    if i >= xs.len() {
        return ys[ys.len() - 1];
    }
    let t = (x - xs[i - 1]) / (xs[i] - xs[i - 1]).max(1e-12);
    ys[i - 1] * (1.0 - t) + ys[i] * t
}

/// The diffraction-limited MTF of a circular aperture at `f` cycles per pixel
/// for a cut-off `fc` (no central obstruction).
#[must_use]
pub fn diffraction_mtf(f: f32, fc: f32) -> f32 {
    let x = (f / fc).clamp(0.0, 1.0);
    if x >= 1.0 {
        return 0.0;
    }
    (2.0 / std::f32::consts::PI) * (x.acos() - x * (1.0 - x * x).sqrt())
}

impl ImageScienceData {
    /// File name of the JSON.
    pub const JSON: &'static str = "image-science.json";
    /// File name of the spectra table.
    pub const SPECTRUM_CSV: &'static str = "spectrum.csv";
    /// File name of the limb profile table.
    pub const LIMB_CSV: &'static str = "limb-profile.csv";
    /// File name of the limb edge table.
    pub const PSF_CSV: &'static str = "limb-psf.csv";
    /// File name of the histogram table.
    pub const HIST_CSV: &'static str = "histogram.csv";
    /// File name of the noise table.
    pub const NOISE_CSV: &'static str = "noise.csv";

    /// Gather what the stack and the extra frames hold.
    #[must_use]
    pub fn from_output(out: &StackOutput, extras: &Extras) -> Self {
        let scale = out
            .disc
            .and_then(|d| helia_core::autoap::arcsec_per_px(d.r));
        let optics = Optics {
            aperture_mm: extras.aperture_mm,
            wavelength_nm: extras.wavelength_nm,
            arcsec_per_px: scale,
        };
        let spectrum = Self::spectra(out, extras, &optics);
        let limb = out
            .limb
            .as_ref()
            .and_then(|fit| limb_profile(&out.image, fit));
        let psf = helia_core::edit::limbpsf::edge_fit(&out.image)
            .ok()
            .map(|e| {
                let fwhm = e.psf.sigma * 2.354_82;
                LimbPsfData {
                    offset: e.radius.iter().map(|r| r - e.psf.radius).collect(),
                    measured: e.measured,
                    model: e.model,
                    sigma_px: e.psf.sigma,
                    fwhm_px: fwhm,
                    sigma_arcsec: scale.map(|s| s * e.psf.sigma),
                    fwhm_arcsec: scale.map(|s| s * fwhm),
                    misfit: e.psf.misfit,
                }
            });
        let shown = extras.sharpened.as_ref().unwrap_or(&out.image);
        let (output_name, output_histogram) = match &out.colour {
            Some(c) => {
                let planes: Vec<Vec<f32>> = (0..3)
                    .map(|k| c.rgb.data.iter().skip(k).step_by(3).copied().collect())
                    .collect();
                (
                    "colour stack".to_string(),
                    ["red", "green", "blue"]
                        .iter()
                        .zip(&planes)
                        .map(|(n, p)| histogram(n, p, 0.0, 1.0))
                        .collect(),
                )
            }
            None => (
                if extras.sharpened.is_some() {
                    "sharpened image"
                } else {
                    "stack"
                }
                .to_string(),
                vec![histogram("grey", &shown.data, 0.0, 1.0)],
            ),
        };
        let mut noise = Vec::new();
        let mut add = |name: &str, f: &Frame| {
            let area = helia_core::measure::centre_crop(f, granulation::REGION);
            if let Some(e) = helia_core::noise::estimate(f, area) {
                noise.push(NoiseRow {
                    name: name.to_string(),
                    signal: e.signal,
                    sigma: e.sigma,
                    snr: e.snr,
                });
            }
        };
        if let Some(b) = &extras.best_frame {
            add("best frame", b);
        }
        add("stack", &out.image);
        if let Some(s) = &extras.sharpened {
            add("sharpened", s);
        }
        Self {
            version: 1,
            spectrum,
            limb,
            psf,
            output_histogram,
            output_name,
            capture_histogram: extras.capture.clone(),
            noise,
            kept: out.kept.len(),
        }
    }

    fn spectra(out: &StackOutput, extras: &Extras, optics: &Optics) -> Option<SpectrumData> {
        let (stack, g) = spectrum_of("stack", &out.image, optics)?;
        let mut curves = vec![stack];
        for (name, f) in [
            ("best frame", &extras.best_frame),
            ("sharpened", &extras.sharpened),
        ] {
            if let Some((c, _)) = f.as_ref().and_then(|f| spectrum_of(name, f, optics)) {
                curves.push(c);
            }
        }
        Some(SpectrumData {
            curves,
            granulation_peak: g.peak_freq,
            rolloff: g.rolloff_freq,
            diffraction_cutoff: g.cutoff_freq,
            aperture_mm: optics.aperture_mm,
            wavelength_nm: optics.wavelength_nm,
            arcsec_per_px: optics.arcsec_per_px,
            region: [
                g.region.x0,
                g.region.y0,
                g.region.width(),
                g.region.height(),
            ],
        })
    }

    /// Draw one of the image science plots.
    pub fn plot(&self, kind: PlotKind, size: Size) -> Result<Vec<u8>> {
        match kind {
            PlotKind::PowerSpectrum => self.plot_spectrum(size, false),
            PlotKind::Mtf => self.plot_spectrum(size, true),
            PlotKind::LimbProfile => self.plot_limb(size),
            PlotKind::LimbPsf => self.plot_psf(size),
            PlotKind::Histogram => self.plot_histogram(size),
            PlotKind::NoiseSnr => self.plot_noise(size),
            _ => Err(Error::NoData(format!(
                "{} is not an image science plot",
                kind.name()
            ))),
        }
    }

    fn spectrum_or_err(&self) -> Result<&SpectrumData> {
        self.spectrum.as_ref().ok_or_else(|| {
            Error::NoData("the disc-centre square of the image has no measurable contrast".into())
        })
    }

    fn plot_spectrum(&self, size: Size, mtf: bool) -> Result<Vec<u8>> {
        let s = self.spectrum_or_err()?;
        if mtf && s.curves.len() < 2 {
            return Err(Error::NoData(
                "the transfer curve compares the stack with the best frame or the sharpened image, and neither is available".into(),
            ));
        }
        let stack = &s.curves[0];
        let fmin = stack
            .freq
            .iter()
            .copied()
            .fold(f32::INFINITY, f32::min)
            .max(1e-4);
        let fmax = NYQUIST;
        // Curves to draw: (name, colour, points).
        let mut series: Vec<Series> = Vec::new();
        let colour = |n: &str| match n {
            "stack" => palette::BLUE,
            "best frame" => palette::ORANGE,
            _ => palette::GREEN,
        };
        if mtf {
            let best = s.curves.iter().find(|c| c.name == "best frame");
            let sharp = s.curves.iter().find(|c| c.name == "sharpened");
            let ratio = |num: &SpectrumCurve, den: &SpectrumCurve| -> Vec<(f64, f64)> {
                num.freq
                    .iter()
                    .zip(&num.power)
                    .map(|(&f, &p)| {
                        let d = interp(&den.freq, &den.power, f).max(1e-30);
                        (f64::from(f), f64::from((p / d).sqrt()))
                    })
                    .collect()
            };
            if let Some(b) = best {
                series.push(("stack / best frame".into(), palette::BLUE, ratio(stack, b)));
            }
            if let Some(sh) = sharp {
                series.push(("sharpened / stack".into(), palette::GREEN, ratio(sh, stack)));
            }
        } else {
            for c in &s.curves {
                series.push((
                    c.name.clone(),
                    colour(&c.name),
                    c.freq
                        .iter()
                        .zip(&c.power)
                        .map(|(&f, &p)| (f64::from(f), f64::from(p)))
                        .collect(),
                ));
            }
        }
        let (mut lo, mut hi) = (f64::INFINITY, 0.0f64);
        for (_, _, pts) in &series {
            for &(_, y) in pts {
                if y > 0.0 && y.is_finite() {
                    lo = lo.min(y);
                    hi = hi.max(y);
                }
            }
        }
        if hi.partial_cmp(&lo) != Some(std::cmp::Ordering::Greater) {
            return Err(Error::NoData("the spectra are empty".into()));
        }
        if !mtf {
            let floor = s
                .curves
                .iter()
                .map(|c| f64::from(c.noise_floor))
                .fold(f64::INFINITY, f64::min);
            if floor.is_finite() && floor > 0.0 {
                lo = lo.min(floor);
            }
        }
        let (lo, hi) = (lo / 1.6, hi * 1.8);
        let title = if mtf {
            "MTF-like transfer: amplitude ratios of the spectra".to_string()
        } else {
            format!(
                "Radially averaged power spectrum of the disc centre ({} x {} px)",
                s.region[2], s.region[3]
            )
        };
        canvas::render(size, |root| {
            let mut chart = ChartBuilder::on(root)
                .caption(&title, (FONT, 19))
                .margin(12)
                .x_label_area_size(46)
                .y_label_area_size(76)
                .build_cartesian_2d(
                    (f64::from(fmin)..f64::from(fmax)).log_scale(),
                    (lo..hi).log_scale(),
                )
                .map_err(plot_err)?;
            chart
                .configure_mesh()
                .x_desc("spatial frequency (cycles per pixel)")
                .y_desc(if mtf {
                    "amplitude ratio"
                } else {
                    "power (contrast squared)"
                })
                .label_style((FONT, 14))
                .axis_desc_style((FONT, 15))
                .x_label_formatter(&|v| log_tick(*v))
                .y_label_formatter(&|v| log_tick(*v))
                .light_line_style(RGBColor(238, 238, 238))
                .bold_line_style(RGBColor(210, 210, 210))
                .draw()
                .map_err(plot_err)?;
            for (name, c, pts) in &series {
                let c = *c;
                chart
                    .draw_series(LineSeries::new(
                        pts.iter().copied().filter(|p| p.1 > 0.0),
                        c.stroke_width(2),
                    ))
                    .map_err(plot_err)?
                    .label(name.clone())
                    .legend(move |(x, y)| {
                        PathElement::new(vec![(x, y), (x + 20, y)], c.stroke_width(2))
                    });
            }
            if mtf {
                chart
                    .draw_series(DashedLineSeries::new(
                        vec![(f64::from(fmin), 1.0), (f64::from(fmax), 1.0)],
                        4,
                        4,
                        palette::GREY.stroke_width(1),
                    ))
                    .map_err(plot_err)?;
                if let Some(fc) = s.diffraction_cutoff {
                    chart
                        .draw_series(LineSeries::new(
                            (0..200)
                                .map(|i| {
                                    f64::from(fmin)
                                        * (f64::from(fmax / fmin)).powf(f64::from(i) / 199.0)
                                })
                                .map(|f| {
                                    (f, f64::from(diffraction_mtf(f as f32, fc)).max(lo * 1.0001))
                                })
                                .filter(|p| p.1 > lo),
                            palette::PURPLE.stroke_width(2),
                        ))
                        .map_err(plot_err)?
                        .label(format!(
                            "diffraction-limited MTF ({} mm)",
                            s.aperture_mm.map_or(String::new(), |a| tick(f64::from(a)))
                        ))
                        .legend(|(x, y)| {
                            PathElement::new(
                                vec![(x, y), (x + 20, y)],
                                palette::PURPLE.stroke_width(2),
                            )
                        });
                }
            } else {
                for c in &s.curves {
                    if c.noise_floor > 0.0 {
                        let col = colour(&c.name);
                        chart
                            .draw_series(DashedLineSeries::new(
                                vec![
                                    (f64::from(fmin), f64::from(c.noise_floor)),
                                    (f64::from(fmax), f64::from(c.noise_floor)),
                                ],
                                2,
                                5,
                                col.mix(0.6).stroke_width(1),
                            ))
                            .map_err(plot_err)?;
                    }
                }
            }
            // Vertical marks.
            let mut marks: Vec<(f32, String, RGBColor)> = vec![
                (
                    s.granulation_peak,
                    format!("granulation peak {}", tick(f64::from(s.granulation_peak))),
                    palette::SKY,
                ),
                (
                    s.rolloff,
                    format!("roll-off {}", tick(f64::from(s.rolloff))),
                    palette::GREY,
                ),
                (NYQUIST, "sampling limit 0.5".to_string(), palette::LIGHT),
            ];
            if let Some(fc) = s.diffraction_cutoff {
                marks.push((
                    fc,
                    format!("diffraction cut-off {}", tick(f64::from(fc))),
                    palette::RED,
                ));
            }
            for (f, label, col) in marks {
                if f > fmin && f <= fmax {
                    chart
                        .draw_series(DashedLineSeries::new(
                            vec![(f64::from(f), lo), (f64::from(f), hi)],
                            6,
                            4,
                            col.stroke_width(2),
                        ))
                        .map_err(plot_err)?
                        .label(label)
                        .legend(move |(x, y)| {
                            PathElement::new(vec![(x, y), (x + 20, y)], col.stroke_width(2))
                        });
                }
            }
            chart
                .configure_series_labels()
                .background_style(WHITE.mix(0.9))
                .border_style(RGBColor(160, 160, 160))
                .label_font((FONT, 13))
                .position(SeriesLabelPosition::UpperRight)
                .draw()
                .map_err(plot_err)
        })
    }

    fn plot_limb(&self, size: Size) -> Result<Vec<u8>> {
        let l = self.limb.as_ref().ok_or_else(|| {
            Error::NoData("no limb-darkening fit (no clear solar disc in the stack)".into())
        })?;
        let title = format!(
            "Limb darkening: {} law, {}; rings differ by {:.2}% RMS",
            l.law,
            l.names
                .iter()
                .zip(&l.coefficients)
                .map(|(n, v)| format!("{n} {v:.3}"))
                .collect::<Vec<_>>()
                .join(", "),
            l.rms * 100.0
        );
        let ymax = l
            .measured
            .iter()
            .chain(&l.model)
            .copied()
            .fold(1.0f32, f32::max);
        let ymin = l
            .measured
            .iter()
            .chain(&l.model)
            .copied()
            .fold(1.0f32, f32::min)
            * 0.92;
        let rmax = l
            .residual()
            .iter()
            .map(|r| r.abs())
            .fold(0.005f32, f32::max)
            * 1.2;
        canvas::render(size, |root| {
            let (top, bottom) = root.split_vertically(size.height * 68 / 100);
            let mut chart = ChartBuilder::on(&top)
                .caption(&title, (FONT, 17))
                .margin(12)
                .x_label_area_size(8)
                .y_label_area_size(66)
                .build_cartesian_2d(0.0..1.0f64, f64::from(ymin)..f64::from(ymax) * 1.05)
                .map_err(plot_err)?;
            chart
                .configure_mesh()
                .y_desc("intensity (centre = 1)")
                .label_style((FONT, 14))
                .axis_desc_style((FONT, 15))
                .x_label_formatter(&|_| String::new())
                .light_line_style(RGBColor(238, 238, 238))
                .bold_line_style(RGBColor(215, 215, 215))
                .draw()
                .map_err(plot_err)?;
            chart
                .draw_series(l.mu.iter().zip(&l.measured).map(|(&m, &v)| {
                    Circle::new(
                        (f64::from(m), f64::from(v)),
                        2,
                        palette::BLUE.mix(0.8).filled(),
                    )
                }))
                .map_err(plot_err)?
                .label("measured (median of each ring)")
                .legend(|(x, y)| Circle::new((x + 10, y), 3, palette::BLUE.filled()));
            chart
                .draw_series(LineSeries::new(
                    l.mu.iter()
                        .zip(&l.model)
                        .map(|(&m, &v)| (f64::from(m), f64::from(v))),
                    palette::RED.stroke_width(2),
                ))
                .map_err(plot_err)?
                .label(format!("{} law", l.law))
                .legend(|(x, y)| {
                    PathElement::new(vec![(x, y), (x + 20, y)], palette::RED.stroke_width(2))
                });
            chart
                .configure_series_labels()
                .background_style(WHITE.mix(0.9))
                .border_style(RGBColor(160, 160, 160))
                .label_font((FONT, 13))
                .position(SeriesLabelPosition::LowerRight)
                .draw()
                .map_err(plot_err)?;

            let mut res = ChartBuilder::on(&bottom)
                .margin(12)
                .x_label_area_size(44)
                .y_label_area_size(66)
                .build_cartesian_2d(0.0..1.0f64, -f64::from(rmax)..f64::from(rmax))
                .map_err(plot_err)?;
            res.configure_mesh()
                .x_desc("mu = cos(angle from disc centre); 1 is the centre, 0 the limb")
                .y_desc("residual")
                .label_style((FONT, 13))
                .axis_desc_style((FONT, 14))
                .y_label_formatter(&|v| format!("{:.1}%", v * 100.0))
                .light_line_style(RGBColor(238, 238, 238))
                .draw()
                .map_err(plot_err)?;
            res.draw_series(l.mu.iter().zip(l.residual()).map(|(&m, r)| {
                Circle::new((f64::from(m), f64::from(r)), 2, palette::ORANGE.filled())
            }))
            .map_err(plot_err)?;
            res.draw_series(LineSeries::new(
                vec![(0.0, 0.0), (1.0, 0.0)],
                palette::GREY.stroke_width(1),
            ))
            .map_err(plot_err)?;
            Ok(())
        })
    }

    fn plot_psf(&self, size: Size) -> Result<Vec<u8>> {
        let p = self.psf.as_ref().ok_or_else(|| {
            Error::NoData(
                "the limb is not a clean edge here, or too little of it is in the frame (the blur cannot be measured)".into(),
            )
        })?;
        let (lo, hi) = (
            p.measured
                .iter()
                .chain(&p.model)
                .copied()
                .fold(f32::INFINITY, f32::min),
            p.measured
                .iter()
                .chain(&p.model)
                .copied()
                .fold(f32::NEG_INFINITY, f32::max),
        );
        let (lo, hi) = canvas::padded(f64::from(lo), f64::from(hi), 0.06);
        let (xl, xh) = (
            f64::from(p.offset.first().copied().unwrap_or(-12.0)),
            f64::from(p.offset.last().copied().unwrap_or(12.0)),
        );
        let arc = |v: Option<f32>| v.map_or(String::new(), |a| format!(" ({a:.2} arcsec)"));
        let title = format!(
            "Limb edge and fitted blur: sigma {:.2} px{}, FWHM {:.2} px{}",
            p.sigma_px,
            arc(p.sigma_arcsec),
            p.fwhm_px,
            arc(p.fwhm_arcsec)
        );
        // The line spread function: a Gaussian of the measured sigma, drawn
        // on a second scale at the foot of the plot.
        canvas::render(size, |root| {
            let mut chart = ChartBuilder::on(root)
                .caption(&title, (FONT, 17))
                .margin(12)
                .x_label_area_size(46)
                .y_label_area_size(66)
                .build_cartesian_2d(xl..xh, lo..hi)
                .map_err(plot_err)?;
            chart
                .configure_mesh()
                .x_desc("distance from the fitted edge (pixels; negative is on the disc)")
                .y_desc("level")
                .label_style((FONT, 14))
                .axis_desc_style((FONT, 15))
                .x_label_formatter(&|v| format!("{v:.0}"))
                .y_label_formatter(&|v| tick(*v))
                .light_line_style(RGBColor(238, 238, 238))
                .bold_line_style(RGBColor(215, 215, 215))
                .draw()
                .map_err(plot_err)?;
            chart
                .draw_series(p.offset.iter().zip(&p.measured).map(|(&o, &v)| {
                    Circle::new(
                        (f64::from(o), f64::from(v)),
                        3,
                        palette::BLUE.mix(0.8).filled(),
                    )
                }))
                .map_err(plot_err)?
                .label("measured edge (median over all angles)")
                .legend(|(x, y)| Circle::new((x + 10, y), 3, palette::BLUE.filled()));
            chart
                .draw_series(LineSeries::new(
                    p.offset
                        .iter()
                        .zip(&p.model)
                        .map(|(&o, &v)| (f64::from(o), f64::from(v))),
                    palette::RED.stroke_width(2),
                ))
                .map_err(plot_err)?
                .label("step blurred by the fitted Gaussian")
                .legend(|(x, y)| {
                    PathElement::new(vec![(x, y), (x + 20, y)], palette::RED.stroke_width(2))
                });
            let sigma = f64::from(p.sigma_px);
            let span = hi - lo;
            let gauss = |x: f64| lo + 0.25 * span * (-(x * x) / (2.0 * sigma * sigma)).exp();
            chart
                .draw_series(LineSeries::new(
                    (0..=160)
                        .map(|i| xl + (xh - xl) * f64::from(i) / 160.0)
                        .map(|x| (x, gauss(x))),
                    palette::GREEN.stroke_width(2),
                ))
                .map_err(plot_err)?
                .label("the PSF it implies (Gaussian, arbitrary height)")
                .legend(|(x, y)| {
                    PathElement::new(vec![(x, y), (x + 20, y)], palette::GREEN.stroke_width(2))
                });
            chart
                .configure_series_labels()
                .background_style(WHITE.mix(0.9))
                .border_style(RGBColor(160, 160, 160))
                .label_font((FONT, 13))
                .position(SeriesLabelPosition::UpperLeft)
                .draw()
                .map_err(plot_err)
        })
    }

    fn plot_histogram(&self, size: Size) -> Result<Vec<u8>> {
        let sets: Vec<(&str, &[ChannelHistogram])> = [
            (self.output_name.as_str(), self.output_histogram.as_slice()),
            (
                "raw capture",
                self.capture_histogram
                    .as_ref()
                    .map_or(&[][..], |c| c.planes.as_slice()),
            ),
        ]
        .into_iter()
        .filter(|(_, h)| !h.is_empty())
        .collect();
        if sets.is_empty() {
            return Err(Error::NoData("no histogram data".into()));
        }
        let colour = |n: &str| match n {
            "red" => RGBColor(204, 51, 51),
            "green" => palette::GREEN,
            "blue" => palette::BLUE,
            _ => RGBColor(70, 70, 70),
        };
        canvas::render(size, |root| {
            let areas = root.split_evenly((sets.len(), 1));
            for (area, (name, hists)) in areas.iter().zip(&sets) {
                let top = hists
                    .iter()
                    .flat_map(|h| h.counts.iter().copied())
                    .max()
                    .unwrap_or(1)
                    .max(2);
                let hi_x = hists
                    .iter()
                    .map(|h| f64::from(h.hi))
                    .fold(0.0f64, f64::max)
                    .max(1e-6);
                let title = format!(
                    "Histogram of the {name}: {}",
                    hists
                        .iter()
                        .map(|h| format!(
                            "{} {:.1}% at the top, {:.1}% at zero",
                            h.name,
                            h.high_clip * 100.0,
                            h.low_clip * 100.0
                        ))
                        .collect::<Vec<_>>()
                        .join("; ")
                );
                let mut chart = ChartBuilder::on(area)
                    .caption(&title, (FONT, 15))
                    .margin(10)
                    .x_label_area_size(36)
                    .y_label_area_size(66)
                    .build_cartesian_2d(0.0..hi_x, (1.0..f64::from(top) * 2.0).log_scale())
                    .map_err(plot_err)?;
                chart
                    .configure_mesh()
                    .x_desc("level")
                    .y_desc("pixels (log)")
                    .label_style((FONT, 13))
                    .axis_desc_style((FONT, 14))
                    .x_label_formatter(&|v| tick(*v))
                    .y_label_formatter(&|v| log_tick(*v))
                    .light_line_style(RGBColor(238, 238, 238))
                    .bold_line_style(RGBColor(215, 215, 215))
                    .draw()
                    .map_err(plot_err)?;
                for h in *hists {
                    let c = colour(&h.name);
                    let w = f64::from(h.hi - h.lo) / h.counts.len() as f64;
                    chart
                        .draw_series(LineSeries::new(
                            h.counts.iter().enumerate().map(|(i, &n)| {
                                (f64::from(h.lo) + (i as f64 + 0.5) * w, f64::from(n.max(1)))
                            }),
                            c.stroke_width(2),
                        ))
                        .map_err(plot_err)?
                        .label(h.name.clone())
                        .legend(move |(x, y)| {
                            PathElement::new(vec![(x, y), (x + 20, y)], c.stroke_width(2))
                        });
                    if h.high_clip >= 0.02 {
                        chart
                            .draw_series(DashedLineSeries::new(
                                vec![
                                    (f64::from(h.ceiling), 1.0),
                                    (f64::from(h.ceiling), f64::from(top) * 2.0),
                                ],
                                6,
                                4,
                                c.stroke_width(2),
                            ))
                            .map_err(plot_err)?;
                    }
                }
                chart
                    .configure_series_labels()
                    .background_style(WHITE.mix(0.9))
                    .border_style(RGBColor(160, 160, 160))
                    .label_font((FONT, 12))
                    .position(SeriesLabelPosition::UpperLeft)
                    .draw()
                    .map_err(plot_err)?;
            }
            Ok(())
        })
    }

    fn plot_noise(&self, size: Size) -> Result<Vec<u8>> {
        if self.noise.is_empty() {
            return Err(Error::NoData(
                "the disc-centre square has no measurable noise (it is flat or too small)".into(),
            ));
        }
        let n = self.noise.len();
        let top = self.noise.iter().map(|r| r.snr).fold(1.0f32, f32::max) * 1.25;
        let title = match self.measured_gain() {
            Some((g, want)) => format!(
                "Signal to noise: stacking {} frames gained {:.1}x (sqrt of the frames: {:.1}x)",
                self.kept, g, want
            ),
            None => "Signal to noise of the disc-centre square".to_string(),
        };
        canvas::render(size, |root| {
            let mut chart = ChartBuilder::on(root)
                .caption(&title, (FONT, 18))
                .margin(12)
                .x_label_area_size(46)
                .y_label_area_size(66)
                .build_cartesian_2d(0.0..n as f64, 0.0..f64::from(top))
                .map_err(plot_err)?;
            chart
                .configure_mesh()
                .y_desc("signal / noise")
                .y_label_formatter(&|v| format!("{v:.0}"))
                .label_style((FONT, 14))
                .axis_desc_style((FONT, 15))
                .x_labels(0)
                .disable_x_mesh()
                .light_line_style(RGBColor(238, 238, 238))
                .draw()
                .map_err(plot_err)?;
            for (i, r) in self.noise.iter().enumerate() {
                let c = match r.name.as_str() {
                    "stack" => palette::BLUE,
                    "best frame" => palette::ORANGE,
                    _ => palette::GREEN,
                };
                chart
                    .draw_series(std::iter::once(Rectangle::new(
                        [(i as f64 + 0.15, 0.0), (i as f64 + 0.85, f64::from(r.snr))],
                        c.mix(0.85).filled(),
                    )))
                    .map_err(plot_err)?;
                let label = format!(
                    "{}: SNR {}, noise {}",
                    r.name,
                    tick(f64::from(r.snr)),
                    tick(f64::from(r.sigma))
                );
                chart
                    .draw_series(std::iter::once(Text::new(
                        label,
                        (i as f64 + 0.2, f64::from(r.snr) + f64::from(top) * 0.03),
                        (FONT, 14),
                    )))
                    .map_err(plot_err)?;
            }
            Ok(())
        })
    }

    /// (measured, expected) gain in SNR from the best frame to the stack.
    fn measured_gain(&self) -> Option<(f32, f32)> {
        let b = self.noise.iter().find(|r| r.name == "best frame")?;
        let s = self.noise.iter().find(|r| r.name == "stack")?;
        Some((s.snr / b.snr, (self.kept as f32).sqrt()))
    }

    /// The tables as CSV: (file name, text).
    #[must_use]
    pub fn csvs(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        if let Some(s) = &self.spectrum {
            let mut t = String::from("image,freq_cycles_per_px,power\n");
            for c in &s.curves {
                for (f, p) in c.freq.iter().zip(&c.power) {
                    let _ = writeln!(t, "{},{f},{p}", c.name);
                }
            }
            out.push((Self::SPECTRUM_CSV, t));
        }
        if let Some(l) = &self.limb {
            let mut t = String::from("mu,measured,model,residual\n");
            for ((m, v), r) in l.mu.iter().zip(&l.measured).zip(l.residual()) {
                let model = interp(&l.mu_sorted(), &l.model_sorted(), *m);
                let _ = writeln!(t, "{m},{v},{model},{r}");
            }
            out.push((Self::LIMB_CSV, t));
        }
        if let Some(p) = &self.psf {
            let mut t = String::from("offset_px,measured,model\n");
            for ((o, m), f) in p.offset.iter().zip(&p.measured).zip(&p.model) {
                let _ = writeln!(t, "{o},{m},{f}");
            }
            out.push((Self::PSF_CSV, t));
        }
        let mut t = String::from("image,channel,bin_low,bin_high,count\n");
        let mut sets: Vec<(&str, &ChannelHistogram)> = self
            .output_histogram
            .iter()
            .map(|h| (self.output_name.as_str(), h))
            .collect();
        if let Some(c) = &self.capture_histogram {
            sets.extend(c.planes.iter().map(|h| ("raw capture", h)));
        }
        for (img, h) in sets {
            let w = (h.hi - h.lo) / h.counts.len() as f32;
            for (i, n) in h.counts.iter().enumerate() {
                let _ = writeln!(
                    t,
                    "{img},{},{},{},{n}",
                    h.name,
                    h.lo + i as f32 * w,
                    h.lo + (i + 1) as f32 * w
                );
            }
        }
        out.push((Self::HIST_CSV, t));
        if !self.noise.is_empty() {
            let mut t = String::from("image,signal,noise_sigma,snr\n");
            for r in &self.noise {
                let _ = writeln!(t, "{},{},{},{}", r.name, r.signal, r.sigma, r.snr);
            }
            out.push((Self::NOISE_CSV, t));
        }
        out
    }

    /// Draw every plot that has data and add the files and the section to `bundle`.
    pub fn build(&self, bundle: &mut Bundle, size: Size) -> Result<()> {
        let mut json = serde_json::to_vec_pretty(self)?;
        json.push(b'\n');
        bundle.add(Self::JSON, json);
        for (name, text) in self.csvs() {
            bundle.add(name, text.into_bytes());
        }
        let mut figures = Vec::new();
        let mut notes = Vec::new();
        for (kind, csv) in [
            (PlotKind::PowerSpectrum, Self::SPECTRUM_CSV),
            (PlotKind::Mtf, Self::SPECTRUM_CSV),
            (PlotKind::LimbProfile, Self::LIMB_CSV),
            (PlotKind::LimbPsf, Self::PSF_CSV),
            (PlotKind::Histogram, Self::HIST_CSV),
            (PlotKind::NoiseSnr, Self::NOISE_CSV),
        ] {
            match self.plot(kind, size) {
                Ok(png) => {
                    let (title, caption) = describe(kind);
                    let name = format!("{}.png", kind.name());
                    bundle.add(&name, png);
                    figures.push(Figure {
                        png: name,
                        title: title.into(),
                        caption: caption.into(),
                        data: vec![Self::JSON.into(), csv.into()],
                    });
                }
                Err(Error::NoData(why)) => notes.push(format!("{}: {why}", kind.name())),
                Err(e) => return Err(e),
            }
        }
        let mut tables = Vec::new();
        if !self.noise.is_empty() {
            tables.push(Table {
                title: "Noise and signal to noise (disc-centre square)".into(),
                header: ["image", "signal", "noise sigma", "SNR"]
                    .map(String::from)
                    .to_vec(),
                rows: self
                    .noise
                    .iter()
                    .map(|r| {
                        vec![
                            r.name.clone(),
                            tick(f64::from(r.signal)),
                            tick(f64::from(r.sigma)),
                            tick(f64::from(r.snr)),
                        ]
                    })
                    .collect(),
            });
        }
        if let Some(s) = &self.spectrum {
            tables.push(Table {
                title: "Where the detail ends (cycles per pixel)".into(),
                header: ["image", "resolved up to", "noise floor"]
                    .map(String::from)
                    .to_vec(),
                rows: s
                    .curves
                    .iter()
                    .map(|c| {
                        vec![
                            c.name.clone(),
                            tick(f64::from(c.resolved_freq)),
                            tick(f64::from(c.noise_floor)),
                        ]
                    })
                    .collect(),
            });
        }
        let mut intro = String::from(
            "What the image holds at each scale, how it darkens to the limb, how blurred the limb is, whether anything is clipped, and how noisy it is.",
        );
        if let Some(p) = &self.psf {
            let _ = write!(intro, " The limb blur is sigma {:.2} px.", p.sigma_px);
        }
        if let Some((g, want)) = self.measured_gain() {
            let _ = write!(
                intro,
                " Stacking {} frames raised the signal to noise {:.1} times; the square root of the frame count predicts {:.1}.",
                self.kept, g, want
            );
        }
        bundle.sections.push(Section {
            id: "image-science".into(),
            title: "Image science".into(),
            intro,
            figures,
            tables,
            notes,
        });
        Ok(())
    }
}

impl LimbProfileData {
    /// Measured less model, per ring.
    #[must_use]
    pub fn residual(&self) -> Vec<f32> {
        self.measured
            .iter()
            .zip(&self.model)
            .map(|(m, f)| m - f)
            .collect()
    }

    fn mu_sorted(&self) -> Vec<f32> {
        // `mu` falls from the centre outwards; interpolation wants it rising.
        self.mu.iter().rev().copied().collect()
    }

    fn model_sorted(&self) -> Vec<f32> {
        self.model.iter().rev().copied().collect()
    }
}

fn limb_profile(image: &Frame, fit: &limbdark::LimbFit) -> Option<LimbProfileData> {
    let prof = limbdark::radial_profile(image, fit.disc());
    let count = prof.len();
    let centre = fit.centre.max(1e-12);
    let mut mu = Vec::new();
    let mut measured = Vec::new();
    let mut model = Vec::new();
    for (b, v) in prof.iter().enumerate() {
        let Some(v) = v else { continue };
        let x = (b as f32 + 0.5) / count as f32;
        if x > limbdark::FIT_EDGE {
            continue;
        }
        let m = limbdark::mu_at(x);
        mu.push(m);
        measured.push((v - fit.background) / centre);
        model.push(fit.level(m));
    }
    (mu.len() >= 8).then(|| LimbProfileData {
        law: fit.law.id().to_string(),
        names: fit.law.coefficient_names(),
        coefficients: fit.coefficients.clone(),
        rms: fit.rms,
        limb_level: fit.limb_level(),
        mu,
        measured,
        model,
    })
}

/// A tick on a logarithmic axis: `0.01`, `1`, `100`, `1e-5`.
fn log_tick(v: f64) -> String {
    if v <= 0.0 {
        return "0".into();
    }
    let e = v.log10().round();
    if (-3.0..=4.0).contains(&e) {
        tick(v)
    } else {
        format!("1e{e:.0}")
    }
}

fn describe(kind: PlotKind) -> (&'static str, &'static str) {
    match kind {
        PlotKind::PowerSpectrum => (
            "Power spectrum",
            "The power of the disc-centre square at each spatial frequency (cycles per pixel), for the stack, the best single frame and the sharpened image. Detail lives on the left and noise on the right; each curve flattens into its noise floor (dashed). The blue vertical line is the granulation peak, the grey one where the stack's power has fallen to 10% of it, the light one the sampling limit, and the red one the telescope's diffraction cut-off when you gave the aperture. A stack that sits above the best frame has gained detail; the distance between the curve and its floor is how much signal there is.",
        ),
        PlotKind::Mtf => (
            "MTF-like transfer",
            "The ratio of the amplitudes of two spectra, at each frequency. Stack over best frame is what stacking and alignment did: above 1, it recovered detail the best single frame lacked; falling below 1 at high frequency means the stack is smoother there. Sharpened over stack is what the sharpening did, and should rise towards the frequencies the wavelets and deconvolution boost. The purple curve is the diffraction-limited MTF of the aperture you gave; no image should pass it by much. Beyond the noise floor, all the ratios are noise.",
        ),
        PlotKind::LimbProfile => (
            "Limb darkening",
            "The median intensity of each ring of the disc (blue) against the fitted law (red), as a function of mu, the cosine of the angle from the centre of the disc. The lower panel is the difference. A good fit leaves residuals of a fraction of a per cent without a pattern; a curve in the residual means a different law fits better, and a ripple near mu 0 is the limb's own blur.",
        ),
        PlotKind::LimbPsf => (
            "Limb blur (measured PSF)",
            "The image of the solar limb across the edge, the median over every angle (blue), and a sharp step blurred by the fitted Gaussian (red). The Gaussian is the point spread function the stack would have if the limb were a perfect edge: the same number the Richardson-Lucy deconvolution uses with --rl-psf limb. Misfit of more than a few per cent of the step means the limb is not a clean edge (H-alpha, prominences), and the width is then only a size for the blur.",
        ),
        PlotKind::Histogram => (
            "Histogram and clipping",
            "How many pixels have each level, on a logarithmic count scale, for the written image and for the raw capture where it is available. The legend line above each plot gives the share of pixels at the top level and at zero. A plateau that is not a few stray pixels (dashed line) means that channel is clipped: at the sensor's limit in the raw capture (the planes of a colour camera clip separately), or at white in the output.",
        ),
        PlotKind::NoiseSnr => (
            "Noise and signal to noise",
            "The signal to noise of a disc-centre square for the best frame, the stack and the sharpened image, from the noise of the high-pass response (Immerkaer 1996). Stacking N frames should raise it by about the square root of N; less means the frames are not independent or the alignment smears them. Sharpening lowers it, because it amplifies noise: this is the cost of the detail.",
        ),
        _ => ("", ""),
    }
}

/// The disc-centre square the spectra and the noise are measured on.
#[must_use]
pub fn region_of(frame: &Frame) -> Rect {
    helia_core::measure::centre_crop(frame, granulation::REGION)
}

#[cfg(test)]
mod tests {
    use super::*;
    use helia_core::io::source::MemorySource;
    use helia_core::pipeline::{Pipeline, StackConfig};
    use helia_core::testutil::{CaptureConfig, SyntheticCapture, SyntheticConfig, SyntheticSun};
    use std::sync::atomic::AtomicBool;

    fn stacked() -> (StackOutput, Frame) {
        let sun = SyntheticSun::new(&SyntheticConfig {
            width: 384,
            height: 320,
            ..SyntheticConfig::default()
        });
        let cap = SyntheticCapture::generate(
            &sun,
            &CaptureConfig {
                frames: 24,
                noise: 0.02,
                ..CaptureConfig::default()
            },
        );
        let best = cap.frames[0].clone();
        let out = Pipeline::new(StackConfig::default())
            .run(
                &mut MemorySource(cap.frames),
                &|_, _| {},
                &AtomicBool::new(false),
            )
            .expect("stack");
        (out, best)
    }

    #[test]
    fn the_plots_draw_from_a_real_stack() {
        let (out, best) = stacked();
        let extras = Extras {
            best_frame: Some(best),
            sharpened: Some(out.image.clone()),
            capture: CaptureHistograms::from_raw(std::slice::from_ref(&out.image)),
            aperture_mm: Some(100.0),
            wavelength_nm: Some(550.0),
            ..Extras::default()
        };
        let d = ImageScienceData::from_output(&out, &extras);
        let mut drawn = 0;
        for kind in [
            PlotKind::PowerSpectrum,
            PlotKind::Mtf,
            PlotKind::LimbProfile,
            PlotKind::LimbPsf,
            PlotKind::Histogram,
            PlotKind::NoiseSnr,
        ] {
            match d.plot(kind, Size::REPORT) {
                Ok(png) => {
                    drawn += 1;
                    assert_eq!(&png[1..4], b"PNG", "{}", kind.name());
                    if let Some(dir) = std::env::var_os("HELIA_REPORT_DUMP") {
                        std::fs::write(
                            std::path::Path::new(&dir).join(format!("{}.png", kind.name())),
                            &png,
                        )
                        .expect("dump");
                    }
                }
                Err(e) => eprintln!("{}: {e}", kind.name()),
            }
        }
        assert!(drawn >= 4, "only {drawn} plots drew");
        let json = serde_json::to_string(&d).expect("json");
        let back: ImageScienceData = serde_json::from_str(&json).expect("parse");
        assert_eq!(back.noise.len(), d.noise.len());
    }

    #[test]
    fn the_diffraction_mtf_runs_from_one_to_zero() {
        assert!((diffraction_mtf(0.0, 0.4) - 1.0).abs() < 1e-6);
        assert!(diffraction_mtf(0.4, 0.4).abs() < 1e-6);
        assert!(diffraction_mtf(0.2, 0.4) < diffraction_mtf(0.1, 0.4));
        // The known value at half the cut-off: 0.3910.
        assert!((diffraction_mtf(0.2, 0.4) - 0.391).abs() < 1e-3);
    }

    #[test]
    fn histograms_find_a_clipped_plateau() {
        let mut v: Vec<f32> = (0..1000).map(|i| i as f32 / 1000.0 * 0.5).collect();
        v.extend(std::iter::repeat_n(0.8, 600));
        let h = histogram("red", &v, 0.0, 1.0);
        assert!(h.high_clip > 0.3, "{h:?}");
        assert!((h.ceiling - 0.8).abs() < 1e-3);
        let flat: Vec<f32> = (0..2000).map(|i| (i % 500) as f32 / 500.0).collect();
        assert!(histogram("g", &flat, 0.0, 1.0).high_clip < 0.02);
    }

    #[test]
    fn interpolation_clamps_and_blends() {
        let xs = [1.0f32, 2.0, 4.0];
        let ys = [10.0f32, 20.0, 40.0];
        assert_eq!(interp(&xs, &ys, 0.5), 10.0);
        assert_eq!(interp(&xs, &ys, 3.0), 30.0);
        assert_eq!(interp(&xs, &ys, 9.0), 40.0);
    }
}
