// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Named options to edit list: what `--sharpen wavelet --wavelet-gains ...`
//! on the command line stands for.
//!
//! Every value is optional; the ones left out come from the recommended
//! settings of the filter ([`super::defaults`]). The list comes out in a fixed
//! order, which is the usual one for a solar image:
//!
//! 0. flatten (it measures the stack, so it goes before everything);
//! 1. the operations of `base` (a saved preset or edit file), as they are;
//! 2. denoise;
//! 3. the sharpening method;
//! 4. the prominence composite;
//! 5. levels, then the curve;
//! 6. crop, rotate, flip, resize.
//!
//! A different order needs an edit file or preset, where the list is written
//! out by hand.

use super::deconv::{Deconvolve, Psf};
use super::defaults::{SharpenMethod, sharpen_edits, tuning_for};
use super::flatten::Flatten;
use super::geometry::{Axis, Crop, Flip, Resize, Rotate};
use super::prominence::Prominence;
use super::tone::{Curve, Levels};
use super::wavelet::{Denoise, Layer, Unsharp, Wavelet};
use super::{EditList, Op};
use crate::colour::Preset;
use crate::error::{Error, Result};
use crate::limbdark::{Law, Source};

/// The edit options as named values.
#[derive(Clone, Debug, Default, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is an independent switch"
)]
pub struct EditOptions {
    /// Operations that come first, from a saved preset or edit file.
    pub base: EditList,
    /// Flatten the limb darkening by the fitted law or the measured profile.
    pub flatten: Option<Source>,
    /// The limb-darkening law the flatten fits (default quadratic). Without
    /// `flatten` it changes nothing here.
    pub limb_law: Option<Law>,
    /// Composite a stretched prominence layer with the disc, after the
    /// sharpening.
    pub prominence: Option<Prominence>,
    /// The sharpening method.
    pub sharpen: Option<SharpenMethod>,
    /// Unsharp: Gaussian sigma in pixels.
    pub unsharp_radius: Option<f32>,
    /// Unsharp: amount.
    pub unsharp_amount: Option<f32>,
    /// Unsharp: threshold.
    pub unsharp_threshold: Option<f32>,
    /// Wavelets in RegiStax terms (the `registax:` form, see
    /// [`Wavelet::from_registax_spec`]); replaces the recommended layers,
    /// scheme and filter. Implies `--sharpen wavelet`.
    pub wavelets: Option<Wavelet>,
    /// Wavelet: gains of the first layers.
    pub wavelet_gains: Option<Vec<f32>>,
    /// Wavelet: noise thresholds of the first layers.
    pub wavelet_noise: Option<Vec<f32>>,
    /// Richardson-Lucy: iterations.
    pub rl_iterations: Option<u32>,
    /// Richardson-Lucy: total-variation weight.
    pub rl_tv: Option<f32>,
    /// Richardson-Lucy: the PSF.
    pub rl_psf: Option<Psf>,
    /// Richardson-Lucy: strength of the ringing guard (0 turns it off).
    pub rl_guard: Option<f32>,
    /// Denoise: `Some(None)` for the recommended strength.
    pub denoise: Option<Option<f32>>,
    /// Denoise: layers.
    pub denoise_layers: Option<u32>,
    /// Levels: black point.
    pub black: Option<f32>,
    /// Levels: white point.
    pub white: Option<f32>,
    /// Levels: gamma.
    pub gamma: Option<f32>,
    /// A tone curve.
    pub curve: Option<Curve>,
    /// A crop.
    pub crop: Option<Crop>,
    /// A rotation in degrees clockwise.
    pub rotate: Option<f32>,
    /// Mirror left to right.
    pub flip_h: bool,
    /// Mirror top to bottom.
    pub flip_v: bool,
    /// A resize.
    pub resize: Option<Resize>,
}

fn need(method: Option<SharpenMethod>, wanted: SharpenMethod, flag: &str) -> Result<()> {
    if method == Some(wanted) {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "{flag} belongs to --sharpen {}; add it",
            wanted.id()
        )))
    }
}

impl EditOptions {
    /// True when no option is set (and the base is empty).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    /// Build the list for an image of `width` x `height` shot through
    /// `preset`'s filter (or an unknown one).
    ///
    /// # Errors
    /// [`Error::Invalid`] for a method option without its method, or a value
    /// that fails validation.
    #[allow(
        clippy::too_many_lines,
        reason = "one flat list of options turned into operations"
    )]
    pub fn build(&self, preset: Option<Preset>, width: usize, height: usize) -> Result<EditList> {
        let t = tuning_for(preset);
        let mut ops = self.base.ops.clone();
        if let Some(source) = self.flatten {
            let law = self.limb_law.unwrap_or_default();
            ops.insert(0, Op::Flatten(Flatten::new(source, law)));
        }
        if let Some(strength) = self.denoise {
            ops.push(Op::Denoise(Denoise {
                strength: strength.unwrap_or(t.denoise),
                layers: self.denoise_layers.unwrap_or(Denoise::default().layers),
            }));
        } else if self.denoise_layers.is_some() {
            return Err(Error::Invalid("--denoise-layers needs --denoise".into()));
        }
        let method = self
            .sharpen
            .or_else(|| self.wavelets.as_ref().map(|_| SharpenMethod::Wavelet));
        // With deconvolution the wavelets come after it: deconvolve first,
        // then lift what is left and cut the noise it raised.
        let wavelets_after_rl =
            method == Some(SharpenMethod::RichardsonLucy) && self.wavelets.is_some();
        if self.wavelets.is_some() && !wavelets_after_rl && method != Some(SharpenMethod::Wavelet) {
            return Err(Error::Invalid(
                "--wavelets goes with --sharpen wavelet or --sharpen rl".into(),
            ));
        }
        let unsharp_flags = self.unsharp_radius.is_some()
            || self.unsharp_amount.is_some()
            || self.unsharp_threshold.is_some();
        let wavelet_flags = self.wavelet_gains.is_some() || self.wavelet_noise.is_some();
        let rl_flags = self.rl_iterations.is_some()
            || self.rl_tv.is_some()
            || self.rl_psf.is_some()
            || self.rl_guard.is_some();
        if unsharp_flags {
            need(method, SharpenMethod::Unsharp, "--unsharp-*")?;
        }
        if wavelet_flags {
            need(method, SharpenMethod::Wavelet, "--wavelet-*")?;
        }
        if rl_flags {
            need(method, SharpenMethod::RichardsonLucy, "--rl-*")?;
        }
        if let Some(m) = method {
            let mut sharpen = sharpen_edits(m, preset, width, height);
            for op in &mut sharpen.ops {
                match op {
                    Op::Unsharp(u) => {
                        *u = Unsharp {
                            radius: self.unsharp_radius.unwrap_or(u.radius),
                            amount: self.unsharp_amount.unwrap_or(u.amount),
                            threshold: self.unsharp_threshold.unwrap_or(u.threshold),
                        };
                    }
                    Op::Wavelet(w) => {
                        if let Some(given) = &self.wavelets {
                            *w = given.clone();
                        }
                        let mut layers = w.layers.clone();
                        for (field, list) in [
                            (0, self.wavelet_gains.as_deref()),
                            (1, self.wavelet_noise.as_deref()),
                        ] {
                            let Some(list) = list else { continue };
                            if list.len() > super::wavelet::MAX_LAYERS {
                                return Err(Error::Invalid(format!(
                                    "at most {} wavelet layers",
                                    super::wavelet::MAX_LAYERS
                                )));
                            }
                            if list.len() > layers.len() {
                                layers.resize(list.len(), Layer::default());
                            }
                            for (l, &v) in layers.iter_mut().zip(list) {
                                let (g, t) = (l.gain(), l.threshold());
                                *l = if field == 0 {
                                    Layer::from_gain(v, t)
                                } else {
                                    Layer::from_gain(g, v)
                                };
                            }
                        }
                        w.layers = layers;
                    }
                    Op::Deconvolve(d) => {
                        *d = Deconvolve {
                            psf: self.rl_psf.unwrap_or(d.psf),
                            iterations: self.rl_iterations.unwrap_or(d.iterations),
                            tv: self.rl_tv.unwrap_or(d.tv),
                            protect: self.rl_guard.unwrap_or(d.protect),
                        };
                    }
                    _ => {}
                }
            }
            ops.extend(sharpen.ops);
            if wavelets_after_rl && let Some(w) = &self.wavelets {
                ops.push(Op::Wavelet(w.clone()));
            }
        }
        if let Some(p) = self.prominence {
            ops.push(Op::Prominence(p));
        }
        if self.black.is_some() || self.white.is_some() || self.gamma.is_some() {
            let d = Levels::default();
            ops.push(Op::Levels(Levels {
                black: self.black.unwrap_or(d.black),
                white: self.white.unwrap_or(d.white),
                gamma: self.gamma.unwrap_or(d.gamma),
                ..d
            }));
        }
        if let Some(c) = &self.curve {
            ops.push(Op::Curve(c.clone()));
        }
        if let Some(c) = self.crop {
            ops.push(Op::Crop(c));
        }
        if let Some(degrees) = self.rotate {
            ops.push(Op::Rotate(Rotate { degrees }));
        }
        if self.flip_h {
            ops.push(Op::Flip(Flip {
                axis: Axis::Horizontal,
            }));
        }
        if self.flip_v {
            ops.push(Op::Flip(Flip {
                axis: Axis::Vertical,
            }));
        }
        if let Some(r) = self.resize {
            ops.push(Op::Resize(r));
        }
        let list = EditList::new(ops);
        list.validate(width, height)?;
        Ok(list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_asked_gives_an_empty_list() {
        let o = EditOptions::default();
        assert!(o.is_empty());
        assert!(
            o.build(None, 100, 100)
                .unwrap_or_else(|e| panic!("{e}"))
                .is_empty()
        );
    }

    #[test]
    fn a_method_alone_uses_the_recommended_values() {
        let o = EditOptions {
            sharpen: Some(SharpenMethod::Wavelet),
            ..EditOptions::default()
        };
        let l = o
            .build(Some(Preset::CaK), 800, 600)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            l,
            sharpen_edits(SharpenMethod::Wavelet, Some(Preset::CaK), 800, 600)
        );
    }

    #[test]
    fn flags_override_only_what_they_name() {
        let o = EditOptions {
            sharpen: Some(SharpenMethod::Wavelet),
            wavelet_gains: Some(vec![2.0, 1.1]),
            ..EditOptions::default()
        };
        let l = o
            .build(Some(Preset::White), 800, 600)
            .unwrap_or_else(|e| panic!("{e}"));
        let Op::Wavelet(w) = &l.ops[0] else {
            panic!("wavelet")
        };
        let t = Preset::White.sharpen_tuning();
        assert!((w.layers[0].gain() - 2.0).abs() < 1e-5 && (w.layers[1].gain() - 1.1).abs() < 1e-5);
        assert!((w.layers[2].gain() - t.gains[2]).abs() < 1e-5);
        assert!((w.layers[0].threshold() - t.thresholds[0]).abs() < 1e-5);
    }

    #[test]
    fn a_method_flag_without_its_method_is_an_error() {
        let o = EditOptions {
            unsharp_amount: Some(1.0),
            ..EditOptions::default()
        };
        let e = o.build(None, 100, 100).err().map(|e| e.to_string());
        assert!(e.is_some_and(|m| m.contains("--sharpen unsharp")));
        let o = EditOptions {
            sharpen: Some(SharpenMethod::Wavelet),
            rl_tv: Some(0.001),
            ..EditOptions::default()
        };
        assert!(o.build(None, 100, 100).is_err());
    }

    #[test]
    fn a_registax_spec_implies_the_wavelet_method() {
        let o = EditOptions {
            wavelets: Some(
                Wavelet::from_registax_spec("registax:l1=100/0.2/0.1,filter=gaussian")
                    .expect("spec"),
            ),
            ..EditOptions::default()
        };
        let l = o.build(Some(Preset::Ha), 800, 600).expect("build");
        let Op::Wavelet(w) = &l.ops[0] else {
            panic!("wavelet")
        };
        assert_eq!(w.filter, super::super::wavelet::Filter::Gaussian);
        assert!((w.layers[0].gain() - 5.0).abs() < 1e-4);
        assert!(w.layers[1].is_identity(), "unnamed layers are left alone");
        let clash = EditOptions {
            sharpen: Some(SharpenMethod::Unsharp),
            ..o
        };
        assert!(clash.build(None, 800, 600).is_err());
    }

    #[test]
    fn deconvolution_comes_before_the_wavelets_that_follow_it() {
        let o = EditOptions {
            sharpen: Some(SharpenMethod::RichardsonLucy),
            rl_psf: Some(Psf::Limb),
            wavelets: Some(Wavelet::from_registax_spec("l1=100/0.1/0.3").expect("spec")),
            ..EditOptions::default()
        };
        let l = o.build(Some(Preset::Ha), 800, 600).expect("build");
        let names: Vec<&str> = l.ops.iter().map(Op::name).collect();
        assert_eq!(names, ["deconvolve", "wavelet"]);
        let Op::Deconvolve(d) = &l.ops[0] else {
            panic!("deconvolve")
        };
        assert_eq!(d.psf, Psf::Limb);
    }

    #[test]
    fn the_order_is_denoise_sharpen_tone_then_geometry() {
        let o = EditOptions {
            sharpen: Some(SharpenMethod::Unsharp),
            denoise: Some(None),
            gamma: Some(1.2),
            crop: Some(Crop {
                x: 0,
                y: 0,
                width: 50,
                height: 50,
            }),
            rotate: Some(90.0),
            flip_h: true,
            resize: Some(Resize::by(0.5)),
            ..EditOptions::default()
        };
        let names: Vec<&str> = o
            .build(None, 100, 100)
            .unwrap_or_else(|e| panic!("{e}"))
            .ops
            .iter()
            .map(Op::name)
            .collect();
        assert_eq!(
            names,
            [
                "denoise", "unsharp", "levels", "crop", "rotate", "flip", "resize"
            ]
        );
    }

    #[test]
    fn the_base_comes_first_and_bad_values_are_refused() {
        let base = EditList::new(vec![Op::Flip(Flip::default())]);
        let o = EditOptions {
            base,
            black: Some(0.2),
            ..EditOptions::default()
        };
        let l = o.build(None, 10, 10).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(l.ops[0].name(), "flip");
        assert_eq!(l.ops[1].name(), "levels");
        let o = EditOptions {
            black: Some(2.0),
            ..EditOptions::default()
        };
        assert!(o.build(None, 10, 10).is_err());
    }
}
