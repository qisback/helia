// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The sharpening and adjustment options of `helia stack` and `helia edit`.

use std::path::PathBuf;

use helia_core::edit::deconv::Psf;
use helia_core::edit::geometry::{Crop, Resize};
use helia_core::edit::tone::Curve;
use helia_core::edit::{EditOptions, SharpenMethod, parse, store};
use helia_core::limbdark::{Law, Source};
use helia_core::{EditList, Error, Preset};
use helia_gpu::BackendChoice;

fn parse_psf(s: &str) -> Result<Psf, Error> {
    parse::psf(s)
}

fn parse_wavelets(s: &str) -> Result<helia_core::edit::Wavelet, Error> {
    helia_core::edit::Wavelet::from_registax_spec(s)
}

fn parse_resize(s: &str) -> Result<Resize, Error> {
    parse::resize(s)
}

fn parse_crop(s: &str) -> Result<Crop, Error> {
    parse::crop(s)
}

fn parse_curve(s: &str) -> Result<Curve, Error> {
    parse::curve(s)
}

/// A comma-separated list of numbers (a newtype so clap reads it as one value).
#[derive(Clone, Debug, PartialEq)]
pub struct FloatList(pub Vec<f32>);

impl std::str::FromStr for FloatList {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        parse::number_list("list", s).map(Self)
    }
}

/// Sharpening and everyday edits, shared by `helia stack` and `helia edit`.
///
/// The edits run in this order: those of `--edits`, denoise, the sharpening
/// method, levels, the curve, crop, rotate, flip, resize. Anything left out
/// takes the recommended value for the filter. Sharpening always runs on the
/// mono image, before any colour.
#[derive(clap::Args, Clone, Debug, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is an independent command-line switch"
)]
pub struct EditArgs {
    /// Sharpening: wavelet (Registax-style layers, the usual choice), unsharp,
    /// rl (Richardson-Lucy deconvolution) or none. The default is none.
    #[arg(long, value_name = "METHOD", value_parser = clap::value_parser!(SharpenMethod), help_heading = "Sharpening")]
    pub sharpen: Option<SharpenMethod>,
    /// Unsharp mask: Gaussian radius (sigma) in pixels.
    #[arg(long, value_name = "PX", help_heading = "Sharpening")]
    pub unsharp_radius: Option<f32>,
    /// Unsharp mask: how much detail is added back (0.6 is gentle, 2 is strong).
    #[arg(long, value_name = "A", help_heading = "Sharpening")]
    pub unsharp_amount: Option<f32>,
    /// Unsharp mask: detail weaker than this (0 to 1) is left alone, so noise
    /// is not boosted.
    #[arg(long, value_name = "T", help_heading = "Sharpening")]
    pub unsharp_threshold: Option<f32>,
    /// Wavelets as in RegiStax 6: `registax:l1=AMOUNT/SHARPEN/DENOISE,l2=...,
    /// scheme=dyadic|linear,filter=default|gaussian,initial=N,step=X,linked=on`.
    /// AMOUNT is the layer slider (0 to 200 %), SHARPEN 0 to 1 (gain is
    /// 1 + AMOUNT x SHARPEN, so 100 and 0.14 is 15x), DENOISE 0 to 1.5 (a
    /// noise cut of 10 x DENOISE sigmas). Layers not named are left alone.
    /// Implies --sharpen wavelet; with --sharpen rl the wavelets run after
    /// the deconvolution (deconvolve first, then lift and denoise).
    #[arg(long, value_name = "SPEC", value_parser = parse_wavelets, help_heading = "Sharpening")]
    pub wavelets: Option<helia_core::edit::Wavelet>,
    /// Wavelets: gain of layers 1, 2, 3 ... (comma separated; 1 leaves a layer
    /// alone, above 1 sharpens it). Layers not listed keep the recommended gain.
    #[arg(long, value_name = "G1,G2,...", value_parser = clap::value_parser!(FloatList), help_heading = "Sharpening")]
    pub wavelet_gains: Option<FloatList>,
    /// Wavelets: noise threshold of layers 1, 2, 3 ... in noise sigmas (0 keeps
    /// all detail; 2 removes what is not clearly above the noise).
    #[arg(long, value_name = "T1,T2,...", value_parser = clap::value_parser!(FloatList), help_heading = "Sharpening")]
    pub wavelet_noise: Option<FloatList>,
    /// Richardson-Lucy: iterations (more is sharper and noisier).
    #[arg(long, value_name = "N", help_heading = "Sharpening")]
    pub rl_iterations: Option<u32>,
    /// Richardson-Lucy: total-variation weight, 0 to 0.05 (keeps noise from
    /// becoming speckle).
    #[arg(long, value_name = "L", help_heading = "Sharpening")]
    pub rl_tv: Option<f32>,
    /// Richardson-Lucy: the blur to undo. auto (from the image scale), limb
    /// (measured from the solar limb: best for white light and calcium),
    /// gaussian:SIGMA, airy:RADIUS (first dark ring, pixels) or
    /// airy-scope:NM,MM,ARCSEC_PER_PX.
    #[arg(long, value_name = "PSF", value_parser = parse_psf, help_heading = "Sharpening")]
    pub rl_psf: Option<Psf>,
    /// Richardson-Lucy: strength of the limb and sunspot ringing guard, 0 (off)
    /// to 1.
    #[arg(long, value_name = "S", help_heading = "Sharpening")]
    pub rl_guard: Option<f32>,
    /// Divide the disc by its limb-darkening profile before anything else, so
    /// sharpening sees flat contrast: `model` (the fitted law) or `profile` (the
    /// measured azimuthal median). Outside the limb nothing changes.
    #[arg(long, value_name = "SOURCE", value_parser = clap::value_parser!(Source), help_heading = "Limb darkening")]
    pub flatten: Option<Source>,
    /// The limb-darkening law to fit: linear, quadratic (the default) or
    /// neckel (Neckel and Labs, five terms). `helia stack` prints the fit.
    #[arg(long, value_name = "LAW", value_parser = clap::value_parser!(Law), help_heading = "Limb darkening")]
    pub limb_law: Option<Law>,
    /// Composite a stretched prominence layer with the disc: outside the fitted
    /// limb the scattered light is taken off and what is left is stretched to
    /// read like the disc, joined to it by a feathered edge. For H-alpha and
    /// the other lines with prominences. Runs after the sharpening.
    #[arg(long, help_heading = "Prominences")]
    pub prominence: bool,
    /// How hard the prominence layer is lifted: 1 refers it to the brightest
    /// prominence found.
    #[arg(
        long,
        value_name = "S",
        requires = "prominence",
        help_heading = "Prominences"
    )]
    pub prominence_strength: Option<f32>,
    /// The k of the asinh stretch of the layer; 0 is linear, larger lifts the
    /// faint parts more (default 6).
    #[arg(
        long,
        value_name = "K",
        requires = "prominence",
        help_heading = "Prominences"
    )]
    pub prominence_stretch: Option<f32>,
    /// Brightness of the brightest prominence as a share of the disc's
    /// (default 0.75).
    #[arg(
        long,
        value_name = "SHARE",
        requires = "prominence",
        help_heading = "Prominences"
    )]
    pub prominence_level: Option<f32>,
    /// Half-width of the feathered join at the limb, as a share of the disc
    /// radius (default 0.012).
    #[arg(
        long,
        value_name = "SHARE",
        requires = "prominence",
        help_heading = "Prominences"
    )]
    pub prominence_feather: Option<f32>,
    /// How far out the layer reaches, in disc radii (default 1.4).
    #[arg(
        long,
        value_name = "RADII",
        requires = "prominence",
        help_heading = "Prominences"
    )]
    pub prominence_reach: Option<f32>,
    /// Noise sigmas taken off the layer before the stretch (default 3).
    #[arg(
        long,
        value_name = "SIGMA",
        requires = "prominence",
        help_heading = "Prominences"
    )]
    pub prominence_gate: Option<f32>,
    /// Move the join of layer and disc, as a share of the disc radius added to
    /// the fitted limb (negative inside the limb, default 0).
    #[arg(
        long,
        value_name = "SHARE",
        allow_negative_numbers = true,
        requires = "prominence",
        help_heading = "Prominences"
    )]
    pub prominence_offset: Option<f32>,
    /// Gamma of the disc's tone above the sky while the layer is on (default 1:
    /// the disc is left alone).
    #[arg(
        long,
        value_name = "G",
        requires = "prominence",
        help_heading = "Prominences"
    )]
    pub prominence_disc_gamma: Option<f32>,
    /// Gamma of the stretched layer's tone (default 1; above 1 darkens its
    /// faint parts).
    #[arg(
        long,
        value_name = "G",
        requires = "prominence",
        help_heading = "Prominences"
    )]
    pub prominence_layer_gamma: Option<f32>,
    /// Wavelet denoise; the strength is in noise sigmas for the finest layer
    /// (default: the filter's recommended value).
    #[arg(long, value_name = "STRENGTH", num_args = 0..=1, help_heading = "Adjustments")]
    pub denoise: Option<Option<f32>>,
    /// Layers the denoise treats, from the finest.
    #[arg(long, value_name = "N", help_heading = "Adjustments")]
    pub denoise_layers: Option<u32>,
    /// Levels: input value (0 to 1) that becomes black.
    #[arg(long, value_name = "V", help_heading = "Adjustments")]
    pub black: Option<f32>,
    /// Levels: input value (0 to 1) that becomes white.
    #[arg(long, value_name = "V", help_heading = "Adjustments")]
    pub white: Option<f32>,
    /// Levels: mid-tone gamma (above 1 brightens).
    #[arg(long, value_name = "G", help_heading = "Adjustments")]
    pub gamma: Option<f32>,
    /// A tone curve through IN:OUT points, for example 0.25:0.15,0.75:0.85.
    #[arg(long, value_name = "IN:OUT,...", value_parser = parse_curve, help_heading = "Adjustments")]
    pub curve: Option<Curve>,
    /// Crop to X,Y,W,H in pixels (top-left origin).
    #[arg(long, value_name = "X,Y,W,H", value_parser = parse_crop, help_heading = "Adjustments")]
    pub crop: Option<Crop>,
    /// Rotate clockwise by this many degrees (multiples of 90 are exact).
    #[arg(
        long,
        value_name = "DEG",
        allow_negative_numbers = true,
        help_heading = "Adjustments"
    )]
    pub rotate: Option<f32>,
    /// Mirror left to right.
    #[arg(long, help_heading = "Adjustments")]
    pub flip_h: bool,
    /// Mirror top to bottom. An edit on the finished picture, applied after
    /// the capture is read; to correct how FITS frames are read use
    /// --row-order.
    #[arg(long, help_heading = "Adjustments")]
    pub flip_v: bool,
    /// Resize: 50% or 0.5, 1200x800, 1200x (width) or x800 (height).
    #[arg(long, value_name = "SIZE", value_parser = parse_resize, help_heading = "Adjustments")]
    pub resize: Option<Resize>,
    /// Start from a saved edit preset (a name from the presets folder, or a
    /// .json file); the options above are added after it.
    #[arg(long, value_name = "NAME|FILE", help_heading = "Presets")]
    pub edits: Option<String>,
    /// Save the resulting edit list as a preset under this name, for --edits.
    #[arg(long, value_name = "NAME", help_heading = "Presets")]
    pub save_edits: Option<String>,
}

impl EditArgs {
    /// The options as core values, reading `--edits` if given.
    ///
    /// # Errors
    /// An unknown or unreadable preset.
    pub fn options(&self) -> Result<EditOptions, Error> {
        let base = match &self.edits {
            Some(r) => {
                store::find(r, store::presets_dir().as_deref().filter(|d| d.is_dir()))?.edits
            }
            None => EditList::default(),
        };
        Ok(EditOptions {
            base,
            flatten: self.flatten,
            prominence: self.prominence.then(|| {
                let d = helia_core::edit::Prominence::default();
                helia_core::edit::Prominence {
                    strength: self.prominence_strength.unwrap_or(d.strength),
                    stretch: self.prominence_stretch.unwrap_or(d.stretch),
                    level: self.prominence_level.unwrap_or(d.level),
                    feather: self.prominence_feather.unwrap_or(d.feather),
                    reach: self.prominence_reach.unwrap_or(d.reach),
                    gate: self.prominence_gate.unwrap_or(d.gate),
                    offset: self.prominence_offset.unwrap_or(d.offset),
                    disc_gamma: self.prominence_disc_gamma.unwrap_or(d.disc_gamma),
                    layer_gamma: self.prominence_layer_gamma.unwrap_or(d.layer_gamma),
                }
            }),
            limb_law: self.limb_law,
            sharpen: self.sharpen,
            unsharp_radius: self.unsharp_radius,
            unsharp_amount: self.unsharp_amount,
            unsharp_threshold: self.unsharp_threshold,
            wavelets: self.wavelets.clone(),
            wavelet_gains: self.wavelet_gains.clone().map(|l| l.0),
            wavelet_noise: self.wavelet_noise.clone().map(|l| l.0),
            rl_iterations: self.rl_iterations,
            rl_tv: self.rl_tv,
            rl_psf: self.rl_psf,
            rl_guard: self.rl_guard,
            denoise: self.denoise,
            denoise_layers: self.denoise_layers,
            black: self.black,
            white: self.white,
            gamma: self.gamma,
            curve: self.curve.clone(),
            crop: self.crop,
            rotate: self.rotate,
            flip_h: self.flip_h,
            flip_v: self.flip_v,
            resize: self.resize,
        })
    }
}

/// Options of `helia edit`.
#[derive(clap::Args)]
pub struct EditCommandArgs {
    /// Image to edit: .tif, .tiff, .png or .fits (mono or RGB).
    pub input: PathBuf,
    /// Output image; the extension picks the format. A colour image stays
    /// colour (TIFF or PNG).
    #[arg(short, long)]
    pub output: PathBuf,
    /// The filter the image was taken with, for the recommended sharpening
    /// values (ha, cak, ...). Without it the filter is read from the file name
    /// or folder when possible.
    #[arg(long, value_name = "FILTER", value_parser = clap::value_parser!(Preset))]
    pub preset: Option<Preset>,
    /// Sharpening and adjustments.
    #[command(flatten)]
    pub edits: EditArgs,
    /// Compute backend.
    #[arg(long, default_value = "auto", value_parser = clap::value_parser!(BackendChoice))]
    pub backend: BackendChoice,
    /// Plugin steps.
    #[command(flatten)]
    pub plugins: crate::PluginRunArgs,
    /// Hide the summary.
    #[arg(short, long)]
    pub quiet: bool,
}
