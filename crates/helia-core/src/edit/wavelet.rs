// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! A-trous wavelet layers (the RegiStax kind), unsharp masking and wavelet
//! denoising.
//!
//! The a-trous ("with holes") transform smooths the image with a small kernel,
//! spacing the taps further apart for each layer. With `c0` the input and
//! `c_j` the j-th smoothing, layer `j` is the detail `d_j = c_(j-1) - c_j`:
//! layer 1 holds the finest structure and noise, the higher layers ever larger
//! structure.
//!
//! # The controls, after RegiStax 6
//!
//! RegiStax is closed source, so this matches the documented behaviour and
//! ranges, not code (the sources are listed in the user guide, "Coming from
//! RegiStax"):
//!
//! * six layers, each with an **amount** (the layer slider, 0 to 100 %, more
//!   than 100 allowed), a **sharpen** value and a **denoise** value;
//! * a **scheme**: dyadic (the tap spacing doubles with each layer) or linear
//!   (it grows by a fixed step), with an **initial layer** and a **step**;
//! * a **filter**: the default B3 spline or a Gaussian;
//! * **linked** layers, where each layer works on the result of the finer ones.
//!
//! Helia reads them as a gain `g` and a noise threshold `t` per layer (see
//! [`Layer`]). The noise level of a layer is estimated from the layer itself
//! with the median absolute deviation, `sigma_j = 1.4826 * median |d_j|`,
//! which a few bright features do not move. The layer is soft-thresholded at
//! `t * sigma_j` and scaled:
//!
//! ```text
//! d'_j = g * sign(d_j) * max(|d_j| - t * sigma_j, 0)
//! out  = in + sum over j of (d'_j - d_j)
//! ```
//!
//! Written as a correction added to the input, a layer with gain 1 and
//! threshold 0 contributes exactly zero, so the all-default transform returns
//! its input bit for bit.
//!
//! # References
//!
//! - Holschneider, M., Kronland-Martinet, R., Morlet, J. and Tchamitchian, P. (1989), A real-time algorithm
//!   for signal analysis with the help of the wavelet transform, in Wavelets: Time-Frequency Methods and
//!   Phase Space, Springer, 286-297, doi:10.1007/978-3-642-75988-8_28 (the a-trous algorithm).
//! - Starck, J.-L., Murtagh, F. and Bijaoui, A. (1994), Image restoration with noise suppression using a wavelet
//!   transform and a multiresolution support constraint, Proc. SPIE 2302, 132-143, doi:10.1117/12.188035.
//! - Donoho, D. L. (1995), De-noising by soft-thresholding, IEEE Trans. Inf. Theory 41, 613-627,
//!   doi:10.1109/18.382009.
//! - Hampel, F. R. (1974), J. Am. Stat. Assoc. 69, 383-393, doi:10.1080/01621459.1974.10482962 (the median
//!   absolute deviation as a noise scale).
//! - The RegiStax controls are matched from the author's published pages, not from code (see the user guide,
//!   "Coming from RegiStax"). RegiStax is a trademark of its owner; Helia is not affiliated with it.

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use super::conv;
use crate::error::{Error, Result};
use crate::filter::gaussian_taps;

/// The B3 spline.
pub const B3: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];
/// Most layers a transform may have.
pub const MAX_LAYERS: usize = 8;
/// MAD to standard deviation for Gaussian noise.
pub const MAD_TO_SIGMA: f32 = 1.4826;
/// The noise estimate uses at most about this many pixels.
const MAD_SAMPLES: usize = 65_536;
/// Largest layer amount (the slider), in per cent.
pub const AMOUNT_MAX: f32 = 200.0;
/// Largest sharpen value.
pub const SHARPEN_MAX: f32 = 1.0;
/// Smallest sharpen value (a gain of 0 at an amount of 100).
pub const SHARPEN_MIN: f32 = -0.1;
/// Largest denoise value (RegiStax users report 1.45).
pub const DENOISE_MAX: f32 = 1.5;
/// Noise sigmas of threshold per unit of denoise at an amount of 100 %.
pub const DENOISE_SIGMAS: f32 = 10.0;
/// Highest initial layer or step.
pub const INITIAL_MAX: u32 = 8;

/// One wavelet layer in RegiStax terms.
///
/// ```text
/// gain      = 1 + (amount / 100) * 100 * sharpen * |sharpen|
/// threshold = (amount / 100) * 10 * denoise          (noise sigmas)
/// ```
///
/// The sharpen scale is quadratic so that the values RegiStax tutorials use
/// (0.1 to 0.25 on the fine layers) give a firm 2x to 7x and a fine control,
/// while the top of the range reaches the 10x to 100x that a soft stack
/// sometimes needs: at an amount of 100 %, sharpen 0.14 is a gain of 3, 0.25
/// of 7.3, 0.5 of 26 and 1 of 101; an amount above 100 goes further. A denoise
/// of 0.25 removes layer detail below 2.5 noise sigmas.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "LayerRepr")]
pub struct Layer {
    /// The layer slider in per cent, 0 to 200: how much of the sharpen and
    /// denoise is applied.
    pub amount: f32,
    /// Sharpen, -0.1 to 1: the gain of the layer is
    /// `1 + amount * sharpen * |sharpen|`.
    pub sharpen: f32,
    /// Denoise, 0 to 1.5: the noise cut is `10 * denoise * amount / 100`
    /// sigmas.
    pub denoise: f32,
}

/// What a layer reads from: the RegiStax terms, or the older gain and noise
/// threshold.
#[derive(Default, Deserialize)]
#[serde(default)]
struct LayerRepr {
    amount: Option<f32>,
    sharpen: Option<f32>,
    denoise: Option<f32>,
    gain: Option<f32>,
    threshold: Option<f32>,
}

impl From<LayerRepr> for Layer {
    fn from(r: LayerRepr) -> Self {
        let old = Layer::from_gain(r.gain.unwrap_or(1.0), r.threshold.unwrap_or(0.0));
        Self {
            amount: r.amount.unwrap_or(old.amount),
            sharpen: r.sharpen.unwrap_or(old.sharpen),
            denoise: r.denoise.unwrap_or(old.denoise),
        }
    }
}

impl Default for Layer {
    fn default() -> Self {
        Self {
            amount: 100.0,
            sharpen: 0.0,
            denoise: 0.0,
        }
    }
}

impl Layer {
    /// The layer that has `gain` and `threshold` (noise sigmas) at an amount of
    /// 100 %.
    #[must_use]
    pub fn from_gain(gain: f32, threshold: f32) -> Self {
        Self {
            amount: 100.0,
            sharpen: (gain - 1.0).signum() * ((gain - 1.0).abs() / 100.0).sqrt(),
            denoise: threshold / DENOISE_SIGMAS,
        }
    }

    /// Multiplier of the layer: 1 leaves it, above 1 sharpens, below 1 softens.
    #[must_use]
    pub fn gain(&self) -> f32 {
        1.0 + self.amount * self.sharpen * self.sharpen.abs()
    }

    /// Soft threshold in noise standard deviations: detail below it is taken
    /// to be noise and removed. 0 keeps everything.
    #[must_use]
    pub fn threshold(&self) -> f32 {
        self.amount / 100.0 * DENOISE_SIGMAS * self.denoise
    }

    /// True when the layer is left exactly as it is.
    #[must_use]
    #[allow(
        clippy::float_cmp,
        reason = "exactly the neutral setting, not nearly it"
    )]
    pub fn is_identity(&self) -> bool {
        self.gain() == 1.0 && self.threshold() == 0.0
    }
}

/// How the tap spacing grows from layer to layer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scheme {
    /// The spacing doubles with each layer: 1, 2, 4, 8, 16, 32 pixels.
    #[default]
    Dyadic,
    /// The spacing grows by `step` per layer from `initial`.
    Linear,
}

/// The smoothing kernel of the transform.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Filter {
    /// The B3 spline `[1, 4, 6, 4, 1] / 16`.
    #[default]
    Default,
    /// A Gaussian of sigma 1 tap, seven taps wide.
    Gaussian,
}

impl Filter {
    /// The taps (odd count, sum 1).
    #[must_use]
    pub fn taps(self) -> Vec<f32> {
        match self {
            Self::Default => B3.to_vec(),
            Self::Gaussian => {
                let mut k: Vec<f32> = (-3i32..=3)
                    .map(|i| (-((i * i) as f32) / 2.0).exp())
                    .collect();
                let s: f32 = k.iter().sum();
                for v in &mut k {
                    *v /= s;
                }
                k
            }
        }
    }
}

fn default_one() -> u32 {
    1
}

fn default_step() -> f32 {
    1.0
}

/// RegiStax-style wavelet sharpening: the layers, the scheme and the filter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Wavelet {
    /// Layers from the finest (1) upwards.
    pub layers: Vec<Layer>,
    /// How the tap spacing grows.
    pub scheme: Scheme,
    /// The first layer's place in the scheme, 1 to 8. Dyadic: spacing
    /// `2^(initial - 1)`; linear: spacing `initial`.
    #[serde(default = "default_one")]
    pub initial: u32,
    /// Linear scheme: the spacing added per layer, 0 to 8.
    #[serde(default = "default_step")]
    pub step: f32,
    /// The smoothing kernel.
    pub filter: Filter,
    /// Each layer works on the result of the finer ones (RegiStax "linked
    /// wavelet layers"): sharpened fine detail is carried into the coarser
    /// layers.
    pub linked: bool,
    /// Tap spacings fixed by a preview at reduced size (see `Op::at_scale`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spacing: Option<Vec<u32>>,
}

impl Default for Wavelet {
    fn default() -> Self {
        Self {
            layers: vec![Layer::default(); 6],
            scheme: Scheme::Dyadic,
            initial: 1,
            step: 1.0,
            filter: Filter::Default,
            linked: false,
            spacing: None,
        }
    }
}

impl Wavelet {
    /// Layers with the default scheme and filter.
    #[must_use]
    pub fn from_layers(layers: Vec<Layer>) -> Self {
        Self {
            layers,
            ..Self::default()
        }
    }

    /// Check the layer list.
    ///
    /// # Errors
    /// [`Error::Invalid`] for too many layers or a value out of range.
    pub fn validate(&self) -> Result<()> {
        if self.layers.len() > MAX_LAYERS {
            return Err(Error::Invalid(format!(
                "at most {MAX_LAYERS} wavelet layers"
            )));
        }
        let bad = |l: &Layer| {
            !(0.0..=AMOUNT_MAX).contains(&l.amount)
                || !(SHARPEN_MIN..=SHARPEN_MAX).contains(&l.sharpen)
                || !(0.0..=DENOISE_MAX).contains(&l.denoise)
        };
        if self.layers.iter().any(bad) {
            return Err(Error::Invalid(format!(
                "wavelet layers need an amount of 0 to {AMOUNT_MAX}, a sharpen of {SHARPEN_MIN} to {SHARPEN_MAX} and a denoise of 0 to {DENOISE_MAX}"
            )));
        }
        if !(1..=INITIAL_MAX).contains(&self.initial)
            || !self.step.is_finite()
            || !(0.0..=INITIAL_MAX as f32).contains(&self.step)
        {
            return Err(Error::Invalid(format!(
                "the wavelet initial layer is 1 to {INITIAL_MAX} and the step 0 to {INITIAL_MAX}"
            )));
        }
        Ok(())
    }

    /// True when every layer is left alone.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.layers.iter().all(Layer::is_identity)
    }

    /// Tap spacing of each layer, in pixels.
    #[must_use]
    pub fn spacings(&self) -> Vec<usize> {
        if let Some(s) = &self.spacing {
            return s.iter().map(|&v| (v as usize).max(1)).collect();
        }
        (0..self.layers.len())
            .map(|j| match self.scheme {
                Scheme::Dyadic => 1usize << (self.initial as usize - 1 + j).min(12),
                Scheme::Linear => {
                    ((self.initial as f32 + self.step * j as f32).round() as usize).max(1)
                }
            })
            .collect()
    }

    /// The transform for a preview shrunk by `2^shift` on each side: spacings
    /// shrink with the image and layers finer than a pixel are dropped.
    #[must_use]
    pub fn at_scale(&self, shift: u32) -> Self {
        let mut layers = Vec::new();
        let mut spacing = Vec::new();
        for (l, s) in self.layers.iter().zip(self.spacings()) {
            let v = s >> shift;
            if v >= 1 {
                layers.push(*l);
                spacing.push(v as u32);
            }
        }
        Self {
            layers,
            spacing: Some(spacing),
            ..self.clone()
        }
    }

    /// Parse the `registax:` form of `--wavelets`:
    /// `l1=AMOUNT/SHARPEN/DENOISE,l2=...,scheme=dyadic|linear,filter=default|gaussian,initial=N,step=X,linked=on|off`.
    /// A layer needs its amount and sharpen; the denoise defaults to 0. Layers
    /// not named keep amount 100 and sharpen 0 (untouched). The `registax:`
    /// prefix is optional.
    ///
    /// # Errors
    /// [`Error::Invalid`] for an unknown key, a malformed number or a value out
    /// of range.
    pub fn from_registax_spec(spec: &str) -> Result<Self> {
        let body = spec.trim();
        let body = body.strip_prefix("registax:").unwrap_or(body);
        let mut w = Self::default();
        let num = |s: &str, what: &str| -> Result<f32> {
            s.trim()
                .parse::<f32>()
                .map_err(|_| Error::Invalid(format!("wavelets {what}: {s:?} is not a number")))
        };
        for item in body.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let (k, v) = item
                .split_once('=')
                .ok_or_else(|| Error::Invalid(format!("wavelets: {item:?} is not key=value")))?;
            let k = k.trim().to_ascii_lowercase();
            let v = v.trim();
            if let Some(n) = k.strip_prefix('l').and_then(|n| n.parse::<usize>().ok()) {
                if n == 0 || n > MAX_LAYERS {
                    return Err(Error::Invalid(format!(
                        "wavelet layers are l1 to l{MAX_LAYERS}"
                    )));
                }
                if w.layers.len() < n {
                    w.layers.resize(n, Layer::default());
                }
                let parts: Vec<&str> = v.split('/').collect();
                if parts.len() < 2 || parts.len() > 3 {
                    return Err(Error::Invalid(format!(
                        "wavelets: {k} is amount/sharpen[/denoise], not {v:?}"
                    )));
                }
                w.layers[n - 1] = Layer {
                    amount: num(parts[0], &k)?,
                    sharpen: num(parts[1], &k)?,
                    denoise: parts.get(2).map_or(Ok(0.0), |d| num(d, &k))?,
                };
                continue;
            }
            match k.as_str() {
                "scheme" => {
                    w.scheme = match v.to_ascii_lowercase().as_str() {
                        "dyadic" => Scheme::Dyadic,
                        "linear" => Scheme::Linear,
                        _ => return Err(Error::Invalid("scheme is dyadic or linear".into())),
                    };
                }
                "filter" => {
                    w.filter = match v.to_ascii_lowercase().as_str() {
                        "default" | "b3" => Filter::Default,
                        "gaussian" => Filter::Gaussian,
                        _ => return Err(Error::Invalid("filter is default or gaussian".into())),
                    };
                }
                "initial" => w.initial = num(v, &k)?.round().max(0.0) as u32,
                "step" => w.step = num(v, &k)?,
                "linked" => {
                    w.linked =
                        matches!(v.to_ascii_lowercase().as_str(), "on" | "true" | "yes" | "1");
                }
                other => {
                    return Err(Error::Invalid(format!(
                        "wavelets: unknown key {other:?} (l1..l8, scheme, filter, initial, step, linked)"
                    )));
                }
            }
        }
        w.validate()?;
        Ok(w)
    }

    /// One line for the file history.
    #[must_use]
    pub fn describe(&self) -> String {
        let per: Vec<String> = self
            .layers
            .iter()
            .map(|l| format!("{:.0}/{:.3}/{:.2}", l.amount, l.sharpen, l.denoise))
            .collect();
        let scheme = match self.scheme {
            Scheme::Dyadic => format!("dyadic from layer {}", self.initial),
            Scheme::Linear => format!("linear, initial {} step {}", self.initial, self.step),
        };
        format!(
            "wavelet sharpen, {} layers ({}), {} filter{}, amount/sharpen/denoise {}",
            self.layers.len(),
            scheme,
            match self.filter {
                Filter::Default => "default",
                Filter::Gaussian => "Gaussian",
            },
            if self.linked { ", linked" } else { "" },
            per.join(" ")
        )
    }
}

/// Wavelet denoise: soft-threshold the finest layers at a multiple of their
/// noise level and leave their gain at 1.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Denoise {
    /// Threshold of layer 1 in noise standard deviations; coarser layers get
    /// a falling share of it (see [`Denoise::layers_for`]).
    pub strength: f32,
    /// Layers treated, from the finest.
    pub layers: u32,
}

impl Default for Denoise {
    fn default() -> Self {
        Self {
            strength: 1.5,
            layers: 3,
        }
    }
}

impl Denoise {
    /// Check the values.
    ///
    /// # Errors
    /// [`Error::Invalid`] for a negative or non-finite strength or too many layers.
    pub fn validate(&self) -> Result<()> {
        if !self.strength.is_finite() || self.strength < 0.0 || self.layers as usize > MAX_LAYERS {
            return Err(Error::Invalid(format!(
                "denoise needs a strength of 0 or more and at most {MAX_LAYERS} layers"
            )));
        }
        Ok(())
    }

    /// The layer list this denoise stands for: gain 1 everywhere, threshold
    /// `strength * (1, 3/4, 1/2, 1/4, ...)`.
    #[must_use]
    pub fn layers_for(&self) -> Vec<Layer> {
        (0..self.layers as usize)
            .map(|j| Layer::from_gain(1.0, self.strength * (1.0 - 0.25 * j as f32).max(0.1)))
            .collect()
    }

    /// One line for the file history.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "wavelet denoise, strength {:.2} over {} layers",
            self.strength, self.layers
        )
    }
}

/// Unsharp mask: add back the difference between the image and a blur of it.
///
/// ```text
/// out = in + amount * soft(in - blur(in, radius), threshold)
/// ```
///
/// `soft(d, t) = sign(d) * max(|d| - t, 0)`, so detail weaker than the
/// threshold (noise) is not boosted.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Unsharp {
    /// Gaussian sigma of the blur, in pixels.
    pub radius: f32,
    /// How much of the detail is added back.
    pub amount: f32,
    /// Detail below this (in image units, 0 to 1) is left alone.
    pub threshold: f32,
}

impl Default for Unsharp {
    fn default() -> Self {
        Self {
            radius: 1.5,
            amount: 0.6,
            threshold: 0.0,
        }
    }
}

impl Unsharp {
    /// Check the values.
    ///
    /// # Errors
    /// [`Error::Invalid`] for a radius outside 0.3..=20, a negative amount or
    /// threshold, or a non-finite value.
    pub fn validate(&self) -> Result<()> {
        if !(0.3..=20.0).contains(&self.radius)
            || !self.amount.is_finite()
            || self.amount < 0.0
            || !self.threshold.is_finite()
            || self.threshold < 0.0
        {
            return Err(Error::Invalid(
                "unsharp needs a radius of 0.3 to 20 pixels and a non-negative amount and threshold"
                    .into(),
            ));
        }
        Ok(())
    }

    /// The Gaussian taps of the blur.
    #[must_use]
    pub fn taps(&self) -> Vec<f32> {
        gaussian_taps(self.radius)
    }

    /// One line for the file history.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "unsharp mask, radius {:.2} px, amount {:.2}, threshold {:.4}",
            self.radius, self.amount, self.threshold
        )
    }
}

/// `sign(d) * max(|d| - t, 0)`.
#[must_use]
pub fn soft(d: f32, t: f32) -> f32 {
    let m = (d.abs() - t).max(0.0);
    if d < 0.0 { -m } else { m }
}

/// Stride of the pixel grid the noise estimate samples.
#[must_use]
pub fn mad_stride(width: usize, height: usize) -> usize {
    ((width * height).div_ceil(MAD_SAMPLES) as f32)
        .sqrt()
        .ceil() as usize
}

/// Noise standard deviation from the layer `a - b`: `1.4826` times the median
/// of `|a - b|` over a regular grid of pixels (at most about 65 000 of them).
#[must_use]
pub fn mad_sigma_of_difference(a: &[f32], b: &[f32], width: usize, height: usize) -> f32 {
    let s = mad_stride(width, height).max(1);
    let mut v: Vec<f32> = Vec::with_capacity(MAD_SAMPLES + width);
    for y in (0..height).step_by(s) {
        for x in (0..width).step_by(s) {
            let i = y * width + x;
            v.push((a[i] - b[i]).abs());
        }
    }
    median_in_place(&mut v) * MAD_TO_SIGMA
}

/// Median of `v` (the upper middle for an even count); 0 for an empty slice.
#[must_use]
pub fn median_in_place(v: &mut [f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let k = v.len() / 2;
    *v.select_nth_unstable_by(k, f32::total_cmp).1
}

/// One smoothing of the a-trous transform: the B3 spline with taps `step`
/// pixels apart.
#[must_use]
pub fn smooth(src: &[f32], w: usize, h: usize, step: usize) -> Vec<f32> {
    conv::separable(src, w, h, &B3, step)
}

/// Run the dyadic B3 transform on `src` with `layers` and return the corrected
/// image (see the module documentation).
#[must_use]
pub fn transform(src: &[f32], w: usize, h: usize, layers: &[Layer]) -> Vec<f32> {
    transform_with(src, w, h, &Wavelet::from_layers(layers.to_vec()))
}

/// Run the transform described by `wv` on `src`.
#[must_use]
pub fn transform_with(src: &[f32], w: usize, h: usize, wv: &Wavelet) -> Vec<f32> {
    let taps = wv.filter.taps();
    let spacing = wv.spacings();
    let layers = &wv.layers;
    let mut out = src.to_vec();
    // Linked layers smooth the running result, so a layer that changes
    // nothing changes nothing for the next either.
    let mut prev = if wv.linked { Vec::new() } else { src.to_vec() };
    for (j, layer) in layers.iter().enumerate() {
        let step = spacing[j];
        let identity = layer.is_identity();
        if wv.linked {
            if identity {
                continue;
            }
        } else if identity {
            // Still need the smoothing for the layers above.
            if layers[j + 1..].iter().all(Layer::is_identity) {
                break;
            }
            prev = conv::separable(&prev, w, h, &taps, step);
            continue;
        }
        let base: &[f32] = if wv.linked { &out } else { &prev };
        let cur = conv::separable(base, w, h, &taps, step);
        let t = if layer.threshold() > 0.0 {
            layer.threshold() * mad_sigma_of_difference(base, &cur, w, h)
        } else {
            0.0
        };
        let gain = layer.gain();
        if wv.linked {
            out.par_iter_mut().zip(cur.par_iter()).for_each(|(o, &c)| {
                let d = *o - c;
                *o += gain * soft(d, t) - d;
            });
        } else {
            out.par_iter_mut()
                .zip(prev.par_iter().zip(cur.par_iter()))
                .for_each(|(o, (&p, &c))| {
                    let d = p - c;
                    *o += gain * soft(d, t) - d;
                });
            prev = cur;
        }
    }
    out
}

/// Apply [`Unsharp`] to `src`.
#[must_use]
pub fn unsharp(src: &[f32], w: usize, h: usize, u: &Unsharp) -> Vec<f32> {
    let blur = conv::separable(src, w, h, &u.taps(), 1);
    let (amount, thr) = (u.amount, u.threshold);
    src.par_iter()
        .zip(blur.par_iter())
        .map(|(&s, &b)| s + amount * soft(s - b, thr))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Rng;

    fn noise(w: usize, h: usize, sigma: f32, seed: u64) -> Vec<f32> {
        let mut r = Rng::new(seed);
        (0..w * h).map(|_| 0.5 + sigma * r.gauss()).collect()
    }

    #[test]
    fn unit_gains_return_the_input_exactly() {
        let (w, h) = (37, 29);
        let src = noise(w, h, 0.05, 1);
        let out = transform(&src, w, h, &Wavelet::default().layers);
        assert_eq!(out, src);
        // And with explicit identity layers of any count.
        assert_eq!(transform(&src, w, h, &[Layer::default(); 3]), src);
    }

    #[test]
    fn the_layers_add_back_to_the_input() {
        // Telescoping sum: input = residual + all details.
        let (w, h) = (24, 20);
        let src = noise(w, h, 0.1, 2);
        let mut prev = src.clone();
        let mut sum = vec![0.0f32; w * h];
        for j in 0..5 {
            let cur = smooth(&prev, w, h, 1 << j);
            for ((s, p), c) in sum.iter_mut().zip(&prev).zip(&cur) {
                *s += p - c;
            }
            prev = cur;
        }
        for ((s, r), o) in sum.iter().zip(&prev).zip(&src) {
            assert!((s + r - o).abs() < 1e-5);
        }
    }

    #[test]
    fn a_gain_above_one_raises_local_contrast_of_that_scale() {
        let (w, h) = (64, 64);
        let mut src = vec![0.5f32; w * h];
        for y in 0..h {
            for x in 0..w {
                src[y * w + x] += 0.1 * (x as f32 * 1.2).sin();
            }
        }
        let mut layers = vec![Layer::default(); 4];
        layers[1] = Layer::from_gain(2.0, 0.0);
        let out = transform(&src, w, h, &layers);
        let swing = |d: &[f32]| {
            let r = &d[32 * w + 16..32 * w + 48];
            r.iter().copied().fold(f32::MIN, f32::max) - r.iter().copied().fold(f32::MAX, f32::min)
        };
        assert!(swing(&out) > 1.2 * swing(&src));
    }

    #[test]
    fn thresholds_remove_noise_from_a_flat_field() {
        let (w, h) = (96, 96);
        let src = noise(w, h, 0.02, 3);
        let layers = vec![Layer::from_gain(1.0, 3.0); 3];
        let out = transform(&src, w, h, &layers);
        let rms = |d: &[f32]| {
            let m = d.iter().sum::<f32>() / d.len() as f32;
            (d.iter().map(|v| (v - m).powi(2)).sum::<f32>() / d.len() as f32).sqrt()
        };
        assert!(
            rms(&out) < 0.7 * rms(&src),
            "{} vs {}",
            rms(&out),
            rms(&src)
        );
    }

    #[test]
    fn mad_sigma_tracks_gaussian_noise_despite_outliers() {
        let (w, h) = (128, 128);
        let mut a = noise(w, h, 0.03, 4);
        let b = vec![0.5f32; w * h];
        for v in a.iter_mut().step_by(50) {
            *v += 5.0; // bright outliers
        }
        let s = mad_sigma_of_difference(&a, &b, w, h);
        assert!((s - 0.03).abs() < 0.006, "sigma {s}");
    }

    #[test]
    fn unsharp_with_zero_amount_is_a_copy_and_boosts_an_edge() {
        let (w, h) = (32, 8);
        let mut src = vec![0.2f32; w * h];
        for y in 0..h {
            for x in w / 2..w {
                src[y * w + x] = 0.8;
            }
        }
        let u0 = Unsharp {
            amount: 0.0,
            ..Unsharp::default()
        };
        assert_eq!(unsharp(&src, w, h, &u0), src);
        let blurred = conv::separable(&src, w, h, &gaussian_taps(1.5), 1);
        let sharp = unsharp(&blurred, w, h, &Unsharp::default());
        let slope = |d: &[f32]| d[3 * w + w / 2] - d[3 * w + w / 2 - 1];
        assert!(slope(&sharp) > slope(&blurred));
    }

    #[test]
    fn unsharp_threshold_leaves_weak_detail_alone() {
        let (w, h) = (40, 40);
        let src = noise(w, h, 0.002, 5);
        let u = Unsharp {
            threshold: 0.05,
            ..Unsharp::default()
        };
        assert_eq!(unsharp(&src, w, h, &u), src);
    }

    #[test]
    fn denoise_layers_fall_off() {
        let l = Denoise {
            strength: 2.0,
            layers: 4,
        }
        .layers_for();
        assert_eq!(l.len(), 4);
        assert!(l.windows(2).all(|p| p[1].threshold() < p[0].threshold()));
        assert!(l.iter().all(|x| (x.gain() - 1.0).abs() < 1e-9));
    }

    fn edge_image(w: usize, h: usize) -> Vec<f32> {
        let mut src = vec![0.3f32; w * h];
        for y in 0..h {
            for x in 0..w {
                src[y * w + x] += 0.2 * ((x as f32 * 0.9).sin() * (y as f32 * 0.4).cos());
            }
        }
        src
    }

    #[test]
    fn registax_ranges_reach_tens_of_times_gain() {
        let l = Layer {
            amount: 100.0,
            sharpen: 0.14,
            denoise: 0.2,
        };
        assert!((l.gain() - 2.96).abs() < 1e-3);
        assert!((l.threshold() - 2.0).abs() < 1e-5);
        let top = Layer {
            amount: 100.0,
            sharpen: 1.0,
            denoise: 0.0,
        };
        assert!((top.gain() - 101.0).abs() < 1e-3);
        assert!(Layer::default().is_identity());
        // The slider scales both.
        let half = Layer { amount: 50.0, ..l };
        assert!((half.gain() - 1.98).abs() < 1e-3 && (half.threshold() - 1.0).abs() < 1e-5);
    }

    #[test]
    fn old_gain_and_threshold_still_load() {
        let l: Layer = serde_json::from_str(r#"{"gain":2.5,"threshold":1.5}"#).expect("old form");
        assert!((l.gain() - 2.5).abs() < 1e-5 && (l.threshold() - 1.5).abs() < 1e-5);
        let n: Layer = serde_json::from_str(r#"{"amount":80,"sharpen":0.1}"#).expect("new form");
        assert!((n.gain() - 1.8).abs() < 1e-5);
    }

    #[test]
    fn spacings_follow_the_scheme() {
        let mut w = Wavelet::default();
        assert_eq!(w.spacings(), vec![1, 2, 4, 8, 16, 32]);
        w.initial = 2;
        assert_eq!(w.spacings(), vec![2, 4, 8, 16, 32, 64]);
        w.scheme = Scheme::Linear;
        w.initial = 1;
        w.step = 0.0;
        assert_eq!(w.spacings(), vec![1; 6]);
        w.step = 1.0;
        assert_eq!(w.spacings(), vec![1, 2, 3, 4, 5, 6]);
        assert_eq!(Wavelet::default().at_scale(2).spacings(), vec![1, 2, 4, 8]);
    }

    #[test]
    fn the_registax_spec_parses_and_checks() {
        let w = Wavelet::from_registax_spec(
            "registax:l1=100/0.14/0.2,l2=100/0.11,scheme=linear,filter=gaussian,initial=2,step=0,linked=on",
        )
        .expect("spec");
        assert_eq!(w.scheme, Scheme::Linear);
        assert_eq!(w.filter, Filter::Gaussian);
        assert!(w.linked && w.initial == 2);
        assert!((w.layers[0].denoise - 0.2).abs() < 1e-6);
        assert!((w.layers[1].gain() - 2.21).abs() < 1e-3);
        assert!(Wavelet::from_registax_spec("l9=1/1").is_err());
        assert!(Wavelet::from_registax_spec("l1=100").is_err());
        assert!(Wavelet::from_registax_spec("l1=100/2").is_err());
        assert!(Wavelet::from_registax_spec("wobble=1").is_err());
    }

    #[test]
    fn schemes_filters_and_linking_change_the_result() {
        let (w, h) = (48, 40);
        let src = edge_image(w, h);
        let mut base = Wavelet::default();
        base.layers[1] = Layer::from_gain(6.0, 0.0);
        base.layers[2] = Layer::from_gain(3.0, 0.0);
        let a = transform_with(&src, w, h, &base);
        let mut b = base.clone();
        b.filter = Filter::Gaussian;
        let mut c = base.clone();
        c.scheme = Scheme::Linear;
        let mut d = base.clone();
        d.linked = true;
        for (name, other) in [("gaussian", b), ("linear", c), ("linked", d)] {
            let o = transform_with(&src, w, h, &other);
            let diff = a
                .iter()
                .zip(&o)
                .map(|(x, y)| (x - y).abs())
                .fold(0.0, f32::max);
            assert!(diff > 1e-3, "{name} changed nothing");
        }
        // A Gaussian filter and a linked scheme still keep unit gains exact.
        let ident = Wavelet {
            filter: Filter::Gaussian,
            linked: true,
            ..Wavelet::default()
        };
        assert_eq!(transform_with(&src, w, h, &ident), src);
    }

    #[test]
    fn denoise_holds_noise_down_at_a_high_gain() {
        let (w, h) = (96, 96);
        let src = noise(w, h, 0.01, 7);
        let rms = |d: &[f32]| {
            let m = d.iter().sum::<f32>() / d.len() as f32;
            (d.iter().map(|v| (v - m).powi(2)).sum::<f32>() / d.len() as f32).sqrt()
        };
        let mut raw = Wavelet::default();
        raw.layers[0] = Layer {
            amount: 100.0,
            sharpen: 0.3,
            denoise: 0.0,
        };
        let mut cut = raw.clone();
        cut.layers[0].denoise = 0.3;
        let (nr, nc) = (
            rms(&transform_with(&src, w, h, &raw)),
            rms(&transform_with(&src, w, h, &cut)),
        );
        eprintln!(
            "noise rms {:.4} -> raw gain 10 {nr:.4}, with denoise 0.3 {nc:.4}",
            rms(&src)
        );
        assert!(nr > 5.0 * rms(&src) && nc < 0.3 * nr);
    }
}
