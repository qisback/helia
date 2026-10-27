// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The Sharpen and Adjust steps' settings, and how they become an edit list.
//!
//! The list is what the preview, the export and the file's history all use,
//! so what is on screen is what is written. The order is fixed: flatten, denoise,
//! the sharpening method, levels, the curve, crop, rotate, flip, resize.
//! Sharpening therefore always runs on the mono stack, before colour (the
//! colour look is applied after the list, in the worker).

use helia_core::edit::deconv::Deconvolve;
use helia_core::edit::geometry::{Axis, Crop, Flip, Resize, Rotate};
use helia_core::edit::tone::{Curve, Levels};
use helia_core::edit::wavelet::{Denoise, Unsharp, Wavelet};
use helia_core::edit::{
    EditList, Flatten, Op, Prominence, Psf, SharpenMethod, sharpen_edits, tuning_for,
};
use helia_core::limbdark::{Law, Source};
use helia_core::{Frame, Preset};

/// Largest preview side, in pixels. The preview is the stack shrunk by a
/// power of two until it fits, so a slider drag stays within about 150 ms.
pub const PREVIEW_SIDE: usize = 1280;

/// The power of two (as an exponent) that brings a `width` x `height` image
/// within [`PREVIEW_SIDE`].
#[must_use]
pub fn preview_shift(width: usize, height: usize) -> u32 {
    let mut shift = 0;
    while width.max(height) >> shift > PREVIEW_SIDE {
        shift += 1;
    }
    shift
}

/// The stack shrunk by `2^shift` on each side, by averaging (exact area
/// weights), for previews.
#[must_use]
pub fn shrink(frame: &Frame, shift: u32) -> Frame {
    if shift == 0 {
        return frame.clone();
    }
    helia_core::edit::geometry::resize(frame, &Resize::by(0.5f32.powf(shift as f32)))
        .unwrap_or_else(|_| frame.clone())
}

/// A colour image shrunk by `2^shift` on each side, channel by channel.
#[must_use]
pub fn shrink_rgb(rgb: &helia_core::RgbImage, shift: u32) -> helia_core::RgbImage {
    if shift == 0 {
        return rgb.clone();
    }
    let planes: Vec<Frame> = (0..3)
        .filter_map(|c| {
            Frame::new(
                rgb.width,
                rgb.height,
                rgb.data.as_chunks::<3>().0.iter().map(|p| p[c]).collect(),
            )
            .ok()
        })
        .map(|f| shrink(&f, shift))
        .collect();
    let [r, g, b] = planes.as_slice() else {
        return rgb.clone();
    };
    helia_core::RgbImage {
        width: r.width,
        height: r.height,
        data: (0..r.data.len())
            .flat_map(|i| [r.data[i], g.data[i], b.data[i]])
            .collect(),
    }
}

/// All settings of the Sharpen and Adjust steps.
#[derive(Clone, Debug, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is an independent switch of the interface"
)]
pub struct EditSettings {
    /// Flatten the limb darkening, by the fitted law or the measured profile;
    /// `None` is off.
    pub flatten: Option<Source>,
    /// The law the flatten fits.
    pub limb_law: Law,
    /// The sharpening method.
    pub method: SharpenMethod,
    /// Unsharp mask values (kept while another method is chosen).
    pub unsharp: Unsharp,
    /// Wavelet layers.
    pub wavelet: Wavelet,
    /// Richardson-Lucy values.
    pub rl: Deconvolve,
    /// Follow the deconvolution with the wavelet layers (deconvolve first,
    /// then lift and denoise what is left).
    pub then_wavelets: bool,
    /// Gaussian sigma remembered while another PSF is chosen.
    pub gauss_sigma: f32,
    /// Airy first-null radius remembered while another PSF is chosen.
    pub airy_radius: f32,
    /// Denoise before sharpening.
    pub denoise_on: bool,
    /// Denoise values.
    pub denoise: Denoise,
    /// Composite a stretched prominence layer with the disc.
    pub prominence_on: bool,
    /// Prominence values.
    pub prominence: Prominence,
    /// Levels (the defaults do nothing).
    pub levels: Levels,
    /// Tone curve (the straight line does nothing).
    pub curve: Curve,
    /// Crop on.
    pub crop_on: bool,
    /// The crop rectangle.
    pub crop: Crop,
    /// Rotation, degrees clockwise.
    pub rotate: f32,
    /// Mirror left to right.
    pub flip_h: bool,
    /// Mirror top to bottom.
    pub flip_v: bool,
    /// Resize in percent (100 does nothing).
    pub resize_percent: f32,
    /// Apply the edits to FITS output too.
    pub edit_fits: bool,
}

impl Default for EditSettings {
    fn default() -> Self {
        Self::recommended(None, 1280, 960)
    }
}

impl EditSettings {
    /// The recommended sharpening for `preset` on a `width` x `height` image,
    /// with the adjustments off.
    #[must_use]
    pub fn recommended(preset: Option<Preset>, width: usize, height: usize) -> Self {
        let pick = |m: SharpenMethod| sharpen_edits(m, preset, width, height).ops;
        let unsharp = match pick(SharpenMethod::Unsharp).first() {
            Some(Op::Unsharp(u)) => *u,
            _ => Unsharp::default(),
        };
        let wavelet = match pick(SharpenMethod::Wavelet).first() {
            Some(Op::Wavelet(w)) => w.clone(),
            _ => Wavelet::default(),
        };
        let rl = match pick(SharpenMethod::RichardsonLucy).first() {
            Some(Op::Deconvolve(d)) => *d,
            _ => Deconvolve::default(),
        };
        let t = tuning_for(preset);
        Self {
            flatten: None,
            limb_law: Law::default(),
            method: SharpenMethod::Wavelet,
            unsharp,
            wavelet,
            rl,
            then_wavelets: false,
            gauss_sigma: helia_core::edit::deconv::heuristic_sigma(width, height),
            airy_radius: 2.0,
            denoise_on: false,
            prominence_on: false,
            prominence: Prominence::default(),
            denoise: Denoise {
                strength: t.denoise,
                ..Denoise::default()
            },
            levels: Levels::default(),
            curve: Curve::default(),
            crop_on: false,
            crop: Crop {
                x: 0,
                y: 0,
                width,
                height,
            },
            rotate: 0.0,
            flip_h: false,
            flip_v: false,
            resize_percent: 100.0,
            edit_fits: false,
        }
    }

    /// Put the sharpening values back to the recommended ones for `preset`
    /// (the adjustments are left alone).
    pub fn reset_sharpen(&mut self, preset: Option<Preset>, width: usize, height: usize) {
        let r = Self::recommended(preset, width, height);
        self.method = r.method;
        self.unsharp = r.unsharp;
        self.wavelet = r.wavelet;
        self.rl = r.rl;
        self.then_wavelets = false;
        self.gauss_sigma = r.gauss_sigma;
        self.airy_radius = r.airy_radius;
        self.denoise_on = false;
        self.denoise = r.denoise;
    }

    /// Switch every adjustment off.
    pub fn reset_adjust(&mut self, width: usize, height: usize) {
        self.prominence_on = false;
        self.prominence = Prominence::default();
        self.levels = Levels::default();
        self.curve = Curve::default();
        self.crop_on = false;
        self.crop = Crop {
            x: 0,
            y: 0,
            width,
            height,
        };
        self.rotate = 0.0;
        self.flip_h = false;
        self.flip_v = false;
        self.resize_percent = 100.0;
    }

    /// True when the sharpening values are the recommended ones.
    #[must_use]
    pub fn sharpen_is_recommended(
        &self,
        preset: Option<Preset>,
        width: usize,
        height: usize,
    ) -> bool {
        let r = Self::recommended(preset, width, height);
        self.method == r.method
            && self.unsharp == r.unsharp
            && self.wavelet == r.wavelet
            && self.rl == r.rl
            && !self.then_wavelets
            && !self.denoise_on
    }

    /// True when any adjustment is on.
    #[must_use]
    pub fn any_adjustment(&self) -> bool {
        self.denoise_on
            || self.prominence_on
            || !self.levels.is_identity()
            || !self.curve.is_identity()
            || self.crop_on
            || self.rotate != 0.0
            || self.flip_h
            || self.flip_v
            || (self.resize_percent - 100.0).abs() > f32::EPSILON
    }

    /// Choose the PSF kind, keeping the numbers of the others.
    pub fn set_psf(&mut self, kind: PsfKind) {
        if let Psf::Gaussian { sigma } = self.rl.psf {
            self.gauss_sigma = sigma;
        }
        if let Psf::Airy { radius } = self.rl.psf {
            self.airy_radius = radius;
        }
        self.rl.psf = match kind {
            PsfKind::Auto => Psf::Auto,
            PsfKind::Limb => Psf::Limb,
            PsfKind::Gaussian => Psf::Gaussian {
                sigma: self.gauss_sigma,
            },
            PsfKind::Airy => Psf::Airy {
                radius: self.airy_radius,
            },
        };
    }

    /// The edit list. A deconvolution PSF of [`Psf::Auto`] becomes a Gaussian of
    /// `auto_sigma` when that is known (the stack's disc was measured).
    #[must_use]
    pub fn list(&self, auto_sigma: Option<f32>) -> EditList {
        let mut ops = Vec::new();
        if let Some(source) = self.flatten {
            ops.push(Op::Flatten(Flatten::new(source, self.limb_law)));
        }
        if self.denoise_on {
            ops.push(Op::Denoise(self.denoise));
        }
        match self.method {
            SharpenMethod::None => {}
            SharpenMethod::Unsharp => ops.push(Op::Unsharp(self.unsharp)),
            SharpenMethod::Wavelet => ops.push(Op::Wavelet(self.wavelet.clone())),
            SharpenMethod::RichardsonLucy => {
                let mut d = self.rl;
                if let (Psf::Auto, Some(s)) = (d.psf, auto_sigma) {
                    d.psf = Psf::Gaussian {
                        sigma: s.clamp(0.3, 8.0),
                    };
                }
                ops.push(Op::Deconvolve(d));
                if self.then_wavelets {
                    ops.push(Op::Wavelet(self.wavelet.clone()));
                }
            }
        }
        if self.prominence_on {
            ops.push(Op::Prominence(self.prominence));
        }
        if !self.levels.is_identity() {
            ops.push(Op::Levels(self.levels));
        }
        if !self.curve.is_identity() {
            ops.push(Op::Curve(self.curve.clone()));
        }
        if self.crop_on {
            ops.push(Op::Crop(self.crop));
        }
        if self.rotate != 0.0 {
            ops.push(Op::Rotate(Rotate {
                degrees: self.rotate,
            }));
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
        if (self.resize_percent - 100.0).abs() > f32::EPSILON {
            ops.push(Op::Resize(Resize::by(self.resize_percent / 100.0)));
        }
        EditList::new(ops)
    }

    /// Settings that stand for `list` (a saved preset). Returns them and how
    /// many operations could not be shown: the interface holds one of each
    /// kind in a fixed order, so a repeat is dropped.
    #[must_use]
    pub fn from_list(list: &EditList, base: &EditSettings) -> (Self, usize) {
        let mut s = base.clone();
        s.flatten = None;
        s.method = SharpenMethod::None;
        s.then_wavelets = false;
        s.denoise_on = false;
        s.prominence_on = false;
        s.levels = Levels::default();
        s.curve = Curve::default();
        s.crop_on = false;
        s.rotate = 0.0;
        s.flip_h = false;
        s.flip_v = false;
        s.resize_percent = 100.0;
        let mut dropped = 0;
        let mut sharpened = false;
        for op in &list.ops {
            match op {
                Op::Flatten(f) if s.flatten.is_none() => {
                    s.flatten = Some(f.source);
                    s.limb_law = f.law;
                }
                Op::Denoise(d) if !s.denoise_on => {
                    s.denoise_on = true;
                    s.denoise = *d;
                }
                Op::Unsharp(u) if !sharpened => {
                    sharpened = true;
                    s.method = SharpenMethod::Unsharp;
                    s.unsharp = *u;
                }
                Op::Wavelet(w) if !sharpened => {
                    sharpened = true;
                    s.method = SharpenMethod::Wavelet;
                    s.wavelet = w.clone();
                }
                Op::Deconvolve(d) if !sharpened => {
                    sharpened = true;
                    s.method = SharpenMethod::RichardsonLucy;
                    s.rl = *d;
                }
                Op::Wavelet(w) if s.method == SharpenMethod::RichardsonLucy && !s.then_wavelets => {
                    s.then_wavelets = true;
                    s.wavelet = w.clone();
                }
                Op::Prominence(p) if !s.prominence_on => {
                    s.prominence_on = true;
                    s.prominence = *p;
                }
                Op::Levels(l) if s.levels.is_identity() => s.levels = *l,
                Op::Curve(c) if s.curve.is_identity() => s.curve = c.clone(),
                Op::Crop(c) if !s.crop_on => {
                    s.crop_on = true;
                    s.crop = *c;
                }
                Op::Rotate(r) if s.rotate == 0.0 => s.rotate = r.degrees,
                Op::Flip(f) if f.axis == Axis::Horizontal && !s.flip_h => s.flip_h = true,
                Op::Flip(f) if f.axis == Axis::Vertical && !s.flip_v => s.flip_v = true,
                Op::Resize(r) if (s.resize_percent - 100.0).abs() < f32::EPSILON => {
                    if let Some(f) = r.scale {
                        s.resize_percent = f * 100.0;
                    } else {
                        dropped += 1;
                    }
                }
                _ => dropped += 1,
            }
        }
        (s, dropped)
    }

    /// The Sharpen rail status.
    #[must_use]
    pub fn sharpen_status(&self) -> String {
        match self.method {
            SharpenMethod::None => "Off".into(),
            m => m.name().into(),
        }
    }

    /// The Adjust rail status.
    #[must_use]
    pub fn adjust_status(&self) -> String {
        let n = [
            self.denoise_on,
            self.prominence_on,
            !self.levels.is_identity(),
            !self.curve.is_identity(),
            self.crop_on,
            self.rotate != 0.0,
            self.flip_h || self.flip_v,
            (self.resize_percent - 100.0).abs() > f32::EPSILON,
        ]
        .into_iter()
        .filter(|&b| b)
        .count();
        match n {
            0 => "None".into(),
            1 => "1 adjustment".into(),
            n => format!("{n} adjustments"),
        }
    }
}

/// The PSF choices the interface offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PsfKind {
    /// From the image scale.
    Auto,
    /// Measured from the solar limb.
    Limb,
    /// A Gaussian of a given sigma.
    Gaussian,
    /// An Airy disc of a given first-null radius.
    Airy,
}

impl PsfKind {
    /// The kind of `psf`.
    #[must_use]
    pub fn of(psf: Psf) -> Self {
        match psf {
            Psf::Auto => Self::Auto,
            Psf::Limb => Self::Limb,
            Psf::Gaussian { .. } => Self::Gaussian,
            Psf::Airy { .. } => Self::Airy,
        }
    }
}

/// The part of `list` that moves pixels, for the "before" half of a split
/// view: it has the same size as the "after" half, so the two line up.
#[must_use]
pub fn geometry_only(list: &EditList) -> EditList {
    EditList::new(
        list.ops
            .iter()
            .filter(|o| o.is_geometry())
            .cloned()
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use helia_core::edit::wavelet::Layer;

    use super::*;

    #[test]
    fn the_preview_is_a_power_of_two_within_the_limit() {
        assert_eq!(preview_shift(1000, 800), 0);
        assert_eq!(preview_shift(1280, 1280), 0);
        assert_eq!(preview_shift(2000, 1500), 1);
        assert_eq!(preview_shift(4096, 3000), 2);
        assert_eq!(preview_shift(9000, 6000), 3);
        let small = shrink(&Frame::zeros(2048, 1536), 1);
        assert_eq!((small.width, small.height), (1024, 768));
    }

    #[test]
    fn defaults_are_the_recommended_wavelets_and_no_adjustments() {
        let s = EditSettings::recommended(Some(Preset::Ha), 1280, 960);
        assert_eq!(s.method, SharpenMethod::Wavelet);
        assert!(!s.any_adjustment());
        let l = s.list(None);
        assert_eq!(
            l,
            sharpen_edits(SharpenMethod::Wavelet, Some(Preset::Ha), 1280, 960)
        );
        assert!(s.sharpen_is_recommended(Some(Preset::Ha), 1280, 960));
        assert!(!s.sharpen_is_recommended(Some(Preset::White), 1280, 960));
    }

    #[test]
    fn the_list_follows_the_fixed_order() {
        let mut s = EditSettings::recommended(None, 1000, 800);
        s.denoise_on = true;
        s.levels.gamma = 1.2;
        s.curve.points = vec![[0.5, 0.4]];
        s.crop_on = true;
        s.crop = Crop {
            x: 1,
            y: 1,
            width: 500,
            height: 400,
        };
        s.rotate = 90.0;
        s.flip_h = true;
        s.flip_v = true;
        s.resize_percent = 50.0;
        let names: Vec<&str> = s.list(None).ops.iter().map(Op::name).collect();
        assert_eq!(
            names,
            [
                "denoise", "wavelet", "levels", "curve", "crop", "rotate", "flip", "flip", "resize"
            ]
        );
        assert_eq!(s.adjust_status(), "7 adjustments");
        assert!(s.list(None).validate(1000, 800).is_ok());
    }

    #[test]
    fn method_none_leaves_only_the_adjustments() {
        let mut s = EditSettings::recommended(None, 1000, 800);
        s.method = SharpenMethod::None;
        assert!(s.list(None).is_empty());
        assert_eq!(s.sharpen_status(), "Off");
        s.levels.gamma = 1.5;
        assert_eq!(s.list(None).ops.len(), 1);
    }

    #[test]
    fn the_auto_psf_is_filled_in_from_the_measured_disc() {
        let mut s = EditSettings::recommended(None, 1000, 800);
        s.method = SharpenMethod::RichardsonLucy;
        // The recommended PSF is measured from the limb when the edit runs.
        assert!(matches!(&s.list(None).ops[0], Op::Deconvolve(d) if d.psf == Psf::Limb));
        s.rl.psf = Psf::Auto;
        let open = s.list(None);
        assert!(matches!(&open.ops[0], Op::Deconvolve(d) if d.psf == Psf::Auto));
        let set = s.list(Some(1.7));
        assert!(matches!(&set.ops[0], Op::Deconvolve(d) if d.psf == Psf::Gaussian { sigma: 1.7 }));
    }

    #[test]
    fn switching_the_psf_kind_remembers_the_numbers() {
        let mut s = EditSettings::recommended(None, 1000, 800);
        s.set_psf(PsfKind::Gaussian);
        s.rl.psf = Psf::Gaussian { sigma: 2.4 };
        s.set_psf(PsfKind::Airy);
        s.rl.psf = Psf::Airy { radius: 3.1 };
        s.set_psf(PsfKind::Gaussian);
        assert_eq!(s.rl.psf, Psf::Gaussian { sigma: 2.4 });
        s.set_psf(PsfKind::Airy);
        assert_eq!(s.rl.psf, Psf::Airy { radius: 3.1 });
        s.set_psf(PsfKind::Auto);
        assert_eq!(PsfKind::of(s.rl.psf), PsfKind::Auto);
    }

    #[test]
    fn a_saved_list_comes_back_as_the_same_settings() {
        let mut s = EditSettings::recommended(Some(Preset::CaK), 1000, 800);
        s.method = SharpenMethod::Unsharp;
        s.unsharp.amount = 1.4;
        s.denoise_on = true;
        s.levels.black = 0.05;
        s.rotate = 12.5;
        s.flip_v = true;
        s.resize_percent = 75.0;
        let list = s.list(None);
        let (back, dropped) = EditSettings::from_list(&list, &EditSettings::default());
        assert_eq!(dropped, 0);
        assert_eq!(back.list(None), list);
    }

    #[test]
    fn a_list_the_interface_cannot_show_reports_what_it_dropped() {
        let list = EditList::new(vec![
            Op::Unsharp(Unsharp::default()),
            Op::Wavelet(Wavelet::from_layers(vec![Layer::default()])),
            Op::Resize(Resize {
                width: Some(100),
                ..Resize::default()
            }),
        ]);
        let (s, dropped) = EditSettings::from_list(&list, &EditSettings::default());
        assert_eq!(dropped, 2);
        assert_eq!(s.method, SharpenMethod::Unsharp);
    }

    #[test]
    fn geometry_only_keeps_the_size_changes() {
        let mut s = EditSettings::recommended(None, 1000, 800);
        s.crop_on = true;
        s.crop = Crop {
            x: 0,
            y: 0,
            width: 400,
            height: 300,
        };
        s.levels.gamma = 1.5;
        let g = geometry_only(&s.list(None));
        assert_eq!(g.ops.len(), 1);
        assert_eq!(
            g.validate(1000, 800).ok(),
            s.list(None).validate(1000, 800).ok()
        );
    }

    #[test]
    fn reset_puts_things_back() {
        let mut s = EditSettings::recommended(Some(Preset::White), 1000, 800);
        s.wavelet.layers[0].sharpen = 0.03;
        s.denoise_on = true;
        s.rotate = 5.0;
        s.reset_sharpen(Some(Preset::White), 1000, 800);
        assert!(s.sharpen_is_recommended(Some(Preset::White), 1000, 800));
        assert!(s.any_adjustment(), "rotation is an adjustment");
        s.reset_adjust(1000, 800);
        assert!(!s.any_adjustment());
    }
}
