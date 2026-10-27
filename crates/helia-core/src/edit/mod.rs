// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Sharpening and everyday edits as an ordered, recorded list of operations.
//!
//! An [`EditList`] is applied, in order, to the stacked image. The stack
//! itself is never changed: the list is the recipe, so it can be edited,
//! reordered, saved as a preset and written into the output file's metadata
//! ([`EditList::notes`]).
//!
//! | Operation | What it does |
//! |---|---|
//! | [`Op::Unsharp`] | unsharp mask: add back a share of the detail a blur removes |
//! | [`Op::Wavelet`] | Registax-style a-trous layers with a gain and a noise threshold each |
//! | [`Op::Deconvolve`] | Richardson-Lucy with total-variation regularisation and a limb guard |
//! | [`Op::Denoise`] | wavelet denoise (soft threshold on the finest layers) |
//! | [`Op::Flatten`] | divide out the disc's limb darkening (first in a list, see [`crate::limbdark`]) |
//! | [`Op::Prominence`] | composite a stretched off-limb prominence layer with the disc |
//! | [`Op::Levels`], [`Op::Curve`] | black point, white point, gamma; a monotone curve |
//! | [`Op::Crop`], [`Op::Rotate`], [`Op::Flip`], [`Op::Resize`] | geometry |
//!
//! The CPU code here is the reference. `helia-gpu` runs the pointwise and
//! filter operations on the device and must agree with it (the parity tests).
//! Geometry always runs on the CPU: it moves or averages pixels and costs
//! next to nothing beside a deconvolution.
//!
//! Sharpening belongs before colourisation: it works on the mono stack, where
//! detail and noise are measurements, and colour is applied afterwards as
//! presentation.

#![allow(
    clippy::many_single_char_names,
    reason = "image maths uses w, h, x, y and the like"
)]

pub mod conv;
pub mod deconv;
pub mod defaults;
pub mod diagnose;
pub mod flatten;
pub mod geometry;
pub mod limbpsf;
pub mod options;
pub mod parse;
pub mod prominence;
pub mod store;
pub mod tone;
pub mod wavelet;

use serde::{Deserialize, Serialize};

pub use deconv::{Deconvolve, Psf};
pub use defaults::{SharpenMethod, SharpenTuning, sharpen_edits, tuning_for};
pub use flatten::Flatten;
pub use geometry::{Axis, Crop, Flip, Resize, Rotate};
pub use options::EditOptions;
pub use prominence::Prominence;
pub use tone::{Curve, Levels};
pub use wavelet::{Denoise, Filter, Layer, Scheme, Unsharp, Wavelet};

use crate::error::{Error, Result};
use crate::frame::Frame;

/// Most operations a list may hold.
pub const MAX_OPS: usize = 64;

/// One edit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Unsharp mask.
    Unsharp(Unsharp),
    /// A-trous wavelet sharpening.
    Wavelet(Wavelet),
    /// Richardson-Lucy deconvolution.
    Deconvolve(Deconvolve),
    /// Wavelet denoise.
    Denoise(Denoise),
    /// Flatten the limb darkening.
    Flatten(Flatten),
    /// Composite a stretched prominence layer with the disc.
    Prominence(Prominence),
    /// Levels.
    Levels(Levels),
    /// Tone curve.
    Curve(Curve),
    /// Crop.
    Crop(Crop),
    /// Rotate.
    Rotate(Rotate),
    /// Flip.
    Flip(Flip),
    /// Resize.
    Resize(Resize),
}

impl Op {
    /// Short name (`unsharp`, `wavelet`, ...), the `op` tag of the JSON form.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Unsharp(_) => "unsharp",
            Self::Wavelet(_) => "wavelet",
            Self::Deconvolve(_) => "deconvolve",
            Self::Denoise(_) => "denoise",
            Self::Flatten(_) => "flatten",
            Self::Prominence(_) => "prominence",
            Self::Levels(_) => "levels",
            Self::Curve(_) => "curve",
            Self::Crop(_) => "crop",
            Self::Rotate(_) => "rotate",
            Self::Flip(_) => "flip",
            Self::Resize(_) => "resize",
        }
    }

    /// True for the three sharpening operations.
    #[must_use]
    pub fn is_sharpening(&self) -> bool {
        matches!(
            self,
            Self::Unsharp(_) | Self::Wavelet(_) | Self::Deconvolve(_)
        )
    }

    /// True for operations that change the pixel grid.
    #[must_use]
    pub fn is_geometry(&self) -> bool {
        matches!(
            self,
            Self::Crop(_) | Self::Rotate(_) | Self::Flip(_) | Self::Resize(_)
        )
    }

    /// True when the operation would leave every sample unchanged.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        match self {
            Self::Unsharp(u) => u.amount == 0.0,
            Self::Wavelet(w) => w.is_identity(),
            Self::Denoise(d) => d.strength == 0.0 || d.layers == 0,
            Self::Levels(l) => l.is_identity(),
            Self::Curve(c) => c.is_identity(),
            Self::Deconvolve(_)
            | Self::Prominence(_)
            | Self::Flatten(_)
            | Self::Crop(_)
            | Self::Resize(_)
            | Self::Flip(_) => false,
            Self::Rotate(r) => r.quarter_turns() == Some(0),
        }
    }

    /// Check the parameters for an input of `w` x `h`.
    ///
    /// # Errors
    /// [`Error::Invalid`] naming the operation.
    pub fn validate(&self, w: usize, h: usize) -> Result<()> {
        let named = |r: Result<()>| {
            r.map_err(|e| match e {
                Error::Invalid(m) => Error::Invalid(format!("{}: {m}", self.name())),
                other => other,
            })
        };
        named(match self {
            Self::Unsharp(u) => u.validate(),
            Self::Wavelet(v) => v.validate(),
            Self::Deconvolve(d) => d.validate(),
            Self::Denoise(d) => d.validate(),
            Self::Prominence(p) => p.validate(),
            Self::Levels(l) => l.validate(),
            Self::Curve(c) => c.validate(),
            Self::Crop(c) => c.validate(w, h),
            Self::Rotate(r) if !r.degrees.is_finite() => {
                Err(Error::Invalid("angle must be finite".into()))
            }
            Self::Rotate(_) | Self::Flip(_) | Self::Flatten(_) => Ok(()),
            Self::Resize(r) => r.target(w, h).map(|_| ()),
        })
    }

    /// Size of the result for an input of `w` x `h`.
    ///
    /// # Errors
    /// As [`Op::validate`].
    pub fn output_size(&self, w: usize, h: usize) -> Result<(usize, usize)> {
        self.validate(w, h)?;
        Ok(match self {
            Self::Crop(c) => (c.width, c.height),
            Self::Rotate(r) => geometry::rotated_size(w, h, r),
            Self::Resize(r) => r.target(w, h)?,
            _ => (w, h),
        })
    }

    /// One line saying what the operation does, for the file history.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Unsharp(u) => u.describe(),
            Self::Wavelet(w) => w.describe(),
            Self::Deconvolve(d) => d.describe(),
            Self::Denoise(d) => d.describe(),
            Self::Flatten(f) => f.describe(),
            Self::Prominence(p) => p.describe(),
            Self::Levels(l) => l.describe(),
            Self::Curve(c) => c.describe(),
            Self::Crop(c) => c.describe(),
            Self::Rotate(r) => r.describe(),
            Self::Flip(f) => f.describe(),
            Self::Resize(r) => r.describe(),
        }
    }

    /// The same operation for an image shrunk by `2^shift` on each side (a
    /// preview): lengths in pixels shrink with it, and wavelet layers finer
    /// than the preview can show are dropped. An approximation for looking at;
    /// the export always runs on the full image.
    #[must_use]
    pub fn at_scale(&self, shift: u32) -> Op {
        if shift == 0 {
            return self.clone();
        }
        let s = 0.5f32.powf(shift as f32);
        let down = |v: usize| v >> shift;
        match self {
            Self::Unsharp(u) => Self::Unsharp(Unsharp {
                radius: (u.radius * s).max(0.3),
                ..*u
            }),
            Self::Wavelet(w) => Self::Wavelet(w.at_scale(shift)),
            Self::Denoise(d) => Self::Denoise(Denoise {
                layers: d.layers.saturating_sub(shift),
                ..*d
            }),
            Self::Deconvolve(d) => Self::Deconvolve(Deconvolve {
                psf: match d.psf {
                    Psf::Auto => Psf::Auto,
                    Psf::Limb => Psf::Limb,
                    Psf::Gaussian { sigma } => Psf::Gaussian {
                        sigma: (sigma * s).max(0.3),
                    },
                    Psf::Airy { radius } => Psf::Airy {
                        radius: (radius * s).max(0.5),
                    },
                },
                ..*d
            }),
            Self::Crop(c) => Self::Crop(Crop {
                x: down(c.x),
                y: down(c.y),
                width: down(c.width).max(1),
                height: down(c.height).max(1),
            }),
            Self::Resize(r) => Self::Resize(Resize {
                scale: r.scale,
                width: r.width.map(|v| down(v).max(1)),
                height: r.height.map(|v| down(v).max(1)),
            }),
            other => other.clone(),
        }
    }
}

/// An ordered list of edits.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EditList {
    /// The operations, first to last.
    pub ops: Vec<Op>,
}

impl From<Vec<Op>> for EditList {
    fn from(ops: Vec<Op>) -> Self {
        Self { ops }
    }
}

impl EditList {
    /// A list of `ops`.
    #[must_use]
    pub fn new(ops: Vec<Op>) -> Self {
        Self { ops }
    }

    /// True when there is nothing to do.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// True when the list sharpens (so that it must run before colour).
    #[must_use]
    pub fn has_sharpening(&self) -> bool {
        self.ops.iter().any(Op::is_sharpening)
    }

    /// Check every operation in turn, following the image size through the
    /// geometry edits. Returns the size of the result.
    ///
    /// # Errors
    /// [`Error::Invalid`] for a bad operation or too many of them.
    pub fn validate(&self, w: usize, h: usize) -> Result<(usize, usize)> {
        if self.ops.len() > MAX_OPS {
            return Err(Error::Invalid(format!("at most {MAX_OPS} edits in a list")));
        }
        if let Some(i) = self
            .ops
            .iter()
            .position(|o| matches!(o, Op::Flatten(_)))
            .filter(|&i| i > 0)
        {
            return Err(Error::Invalid(format!(
                "edit {}: flatten measures the stack itself, so it has to be the first edit",
                i + 1
            )));
        }
        if self
            .ops
            .iter()
            .filter(|o| matches!(o, Op::Flatten(_)))
            .count()
            > 1
        {
            return Err(Error::Invalid("flatten can be used once".into()));
        }
        if self
            .ops
            .iter()
            .filter(|o| matches!(o, Op::Prominence(_)))
            .count()
            > 1
        {
            return Err(Error::Invalid("prominence can be used once".into()));
        }
        let mut dims = (w, h);
        for (i, op) in self.ops.iter().enumerate() {
            dims = op.output_size(dims.0, dims.1).map_err(|e| match e {
                Error::Invalid(m) => Error::Invalid(format!("edit {}: {m}", i + 1)),
                other => other,
            })?;
        }
        Ok(dims)
    }

    /// Replace the choices left to the image: a deconvolution PSF of
    /// [`Psf::Auto`] becomes a Gaussian sized from the image scale (see
    /// [`deconv::auto_sigma`]) and [`Psf::Limb`] one measured from the solar
    /// limb (the scale's when there is no clear limb), both scaled by any
    /// resize that came before them.
    #[must_use]
    pub fn resolve(&self, frame: &Frame) -> EditList {
        let mut base: Option<f32> = None;
        let mut limb: Option<Option<f32>> = None;
        let mut cum = 1.0f32;
        let mut dims = (frame.width, frame.height);
        let ops = self
            .ops
            .iter()
            .map(|op| {
                let out = match op {
                    Op::Deconvolve(d) if matches!(d.psf, Psf::Auto | Psf::Limb) => {
                        let measured = if d.psf == Psf::Limb {
                            *limb.get_or_insert_with(|| limbpsf::measure(frame).map(|m| m.sigma))
                        } else {
                            None
                        };
                        let s = measured.unwrap_or_else(|| {
                            *base.get_or_insert_with(|| deconv::auto_sigma(frame))
                        });
                        Op::Deconvolve(Deconvolve {
                            psf: Psf::Gaussian {
                                sigma: (s * cum).clamp(0.3, 8.0),
                            },
                            ..*d
                        })
                    }
                    Op::Flatten(f) if f.state == flatten::State::Unresolved => {
                        Op::Flatten(f.resolved(frame))
                    }
                    other => other.clone(),
                };
                if let Ok(next) = out.output_size(dims.0, dims.1) {
                    if matches!(out, Op::Resize(_)) {
                        cum *= next.0 as f32 / dims.0 as f32;
                    }
                    dims = next;
                }
                out
            })
            .collect();
        EditList { ops }
    }

    /// The list for a preview shrunk by `2^shift` (see [`Op::at_scale`]).
    #[must_use]
    pub fn at_scale(&self, shift: u32) -> EditList {
        EditList {
            ops: self.ops.iter().map(|o| o.at_scale(shift)).collect(),
        }
    }

    /// One line per operation.
    #[must_use]
    pub fn describe(&self) -> Vec<String> {
        self.ops
            .iter()
            .enumerate()
            .map(|(i, op)| format!("{}. {}", i + 1, op.describe()))
            .collect()
    }

    /// The lines recorded in an output file: a heading, one line per
    /// operation and the list as JSON, from which the edit can be rerun
    /// (`helia edit --edits`). ASCII only, so FITS `HISTORY` cards take it.
    #[must_use]
    pub fn notes(&self) -> Vec<String> {
        if self.ops.is_empty() {
            return Vec::new();
        }
        let mut lines = vec![format!(
            "Helia edits, applied in order to the stack ({} operations){}",
            self.ops.len(),
            if self.has_sharpening() {
                "; sharpening runs on the mono stack before any colour"
            } else {
                ""
            }
        )];
        lines.extend(self.describe());
        lines.push(format!("Helia edit list (JSON): {}", self.to_json()));
        lines
    }

    /// Compact JSON: an array of `{"op": ..., ...}` objects.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "[]".into())
    }

    /// Read the JSON form.
    ///
    /// # Errors
    /// [`Error::Invalid`] when the text is not a list of known operations.
    pub fn from_json(text: &str) -> Result<Self> {
        let list: EditList =
            serde_json::from_str(text).map_err(|e| Error::Invalid(format!("edit list: {e}")))?;
        if list.ops.len() > MAX_OPS {
            return Err(Error::Invalid(format!("at most {MAX_OPS} edits in a list")));
        }
        Ok(list)
    }
}

/// Apply one operation on the CPU. `Psf::Auto` must have been resolved.
///
/// # Errors
/// [`Error::Invalid`] for bad parameters.
pub fn apply_op(frame: &Frame, op: &Op) -> Result<Frame> {
    use rayon::prelude::*;
    op.validate(frame.width, frame.height)?;
    let (w, h) = (frame.width, frame.height);
    let plain = |data: Vec<f32>| Frame {
        width: w,
        height: h,
        data,
        bayer: None,
    };
    Ok(match op {
        Op::Unsharp(u) => plain(wavelet::unsharp(&frame.data, w, h, u)),
        Op::Wavelet(v) => plain(wavelet::transform_with(&frame.data, w, h, v)),
        Op::Denoise(d) => plain(wavelet::transform(&frame.data, w, h, &d.layers_for())),
        Op::Deconvolve(d) => deconv::deconvolve(frame, d)?,
        Op::Flatten(f) => f.apply(frame),
        Op::Prominence(p) => p.apply(frame),
        Op::Levels(l) => {
            let l = *l;
            plain(frame.data.par_iter().map(|&v| l.apply(v)).collect())
        }
        Op::Curve(c) => {
            let k = c.knots()?;
            plain(
                frame
                    .data
                    .par_iter()
                    .map(|&v| Curve::apply_knots(&k, v))
                    .collect(),
            )
        }
        Op::Crop(c) => geometry::crop(frame, c)?,
        Op::Rotate(r) => geometry::rotate(frame, r),
        Op::Flip(f) => geometry::flip(frame, f.axis),
        Op::Resize(r) => geometry::resize(frame, r)?,
    })
}

/// Apply a whole list on the CPU: validate, resolve, run in order.
///
/// # Errors
/// [`Error::Invalid`] for a bad list.
pub fn apply_cpu(frame: &Frame, list: &EditList) -> Result<Frame> {
    list.validate(frame.width, frame.height)?;
    let list = list.resolve(frame);
    let mut cur = frame.clone();
    cur.bayer = None;
    for op in &list.ops {
        if op.is_identity() {
            continue;
        }
        cur = apply_op(&cur, op)?;
    }
    Ok(cur)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Rng;

    fn image() -> Frame {
        let mut r = Rng::new(3);
        Frame::new(
            48,
            40,
            (0..48 * 40).map(|_| 0.5 + 0.1 * r.gauss()).collect(),
        )
        .unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn json_round_trips_every_operation() {
        let list = EditList::new(vec![
            Op::Unsharp(Unsharp::default()),
            Op::Wavelet(Wavelet::default()),
            Op::Deconvolve(Deconvolve::default()),
            Op::Denoise(Denoise::default()),
            Op::Levels(Levels {
                gamma: 1.4,
                ..Levels::default()
            }),
            Op::Curve(Curve {
                points: vec![[0.5, 0.4]],
            }),
            Op::Crop(Crop {
                x: 1,
                y: 2,
                width: 3,
                height: 4,
            }),
            Op::Rotate(Rotate { degrees: 12.5 }),
            Op::Flip(Flip {
                axis: Axis::Vertical,
            }),
            Op::Resize(Resize::by(0.5)),
        ]);
        let text = list.to_json();
        assert!(text.contains(r#""op":"unsharp""#));
        assert_eq!(
            EditList::from_json(&text).unwrap_or_else(|e| panic!("{e}")),
            list
        );
    }

    #[test]
    fn json_fills_missing_fields_with_defaults_and_rejects_unknown_ops() {
        let l = EditList::from_json(r#"[{"op":"unsharp","amount":1.2}]"#)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            l.ops,
            vec![Op::Unsharp(Unsharp {
                amount: 1.2,
                ..Unsharp::default()
            })]
        );
        assert!(EditList::from_json(r#"[{"op":"sparkle"}]"#).is_err());
        assert!(EditList::from_json("{").is_err());
    }

    #[test]
    fn a_list_validates_through_geometry() {
        let list = EditList::new(vec![
            Op::Crop(Crop {
                x: 0,
                y: 0,
                width: 20,
                height: 10,
            }),
            Op::Rotate(Rotate { degrees: 90.0 }),
            Op::Resize(Resize::by(2.0)),
        ]);
        assert_eq!(list.validate(48, 40).ok(), Some((20, 40)));
        let bad = EditList::new(vec![Op::Crop(Crop {
            x: 40,
            y: 0,
            width: 20,
            height: 10,
        })]);
        let err = bad.validate(48, 40).err().map(|e| e.to_string());
        assert!(err.is_some_and(|m| m.contains("edit 1") && m.contains("crop")));
    }

    #[test]
    fn flatten_must_come_first_and_only_once() {
        let f = Op::Flatten(Flatten::default());
        let ok = EditList::new(vec![f.clone(), Op::Levels(Levels::default())]);
        assert!(ok.validate(48, 40).is_ok());
        let late = EditList::new(vec![Op::Levels(Levels::default()), f.clone()]);
        let e = late.validate(48, 40).err().map(|e| e.to_string());
        assert!(e.is_some_and(|m| m.contains("first edit")), "{late:?}");
        assert!(EditList::new(vec![f.clone(), f]).validate(48, 40).is_err());
    }

    #[test]
    fn flatten_json_round_trips_without_the_fit() {
        let list = EditList::new(vec![Op::Flatten(Flatten::new(
            crate::limbdark::Source::Profile,
            crate::limbdark::Law::Neckel,
        ))]);
        let json = list.to_json();
        assert!(json.contains("\"op\":\"flatten\"") && json.contains("profile"));
        assert_eq!(
            EditList::from_json(&json).unwrap_or_else(|e| panic!("{e}")),
            list
        );
        // Resolving fits the image but does not change what the list says.
        let resolved = list.resolve(&image());
        assert!(matches!(
            &resolved.ops[0],
            Op::Flatten(f) if f.state == flatten::State::NoDisc
        ));
        assert_eq!(list.to_json(), resolved.to_json());
    }

    #[test]
    fn identity_edits_return_the_input() {
        let f = image();
        let list = EditList::new(vec![
            Op::Wavelet(Wavelet::default()),
            Op::Levels(Levels::default()),
            Op::Curve(Curve::default()),
            Op::Unsharp(Unsharp {
                amount: 0.0,
                ..Unsharp::default()
            }),
            Op::Rotate(Rotate { degrees: 360.0 }),
        ]);
        assert_eq!(apply_cpu(&f, &list).unwrap_or_else(|e| panic!("{e}")), f);
    }

    #[test]
    fn an_empty_list_is_a_copy() {
        let f = image();
        assert_eq!(
            apply_cpu(&f, &EditList::default()).unwrap_or_else(|e| panic!("{e}")),
            f
        );
    }

    #[test]
    fn order_matters_and_is_kept() {
        let f = image();
        let a = EditList::new(vec![
            Op::Levels(Levels {
                gamma: 2.0,
                ..Levels::default()
            }),
            Op::Flip(Flip::default()),
            Op::Unsharp(Unsharp::default()),
        ]);
        let b = EditList::new(vec![a.ops[2].clone(), a.ops[1].clone(), a.ops[0].clone()]);
        let (ra, rb) = (
            apply_cpu(&f, &a).unwrap_or_else(|e| panic!("{e}")),
            apply_cpu(&f, &b).unwrap_or_else(|e| panic!("{e}")),
        );
        assert_ne!(ra, rb);
    }

    #[test]
    fn auto_psf_is_resolved_and_recorded() {
        let f = image();
        let list = EditList::new(vec![Op::Deconvolve(Deconvolve::default())]);
        let r = list.resolve(&f);
        match &r.ops[0] {
            Op::Deconvolve(d) => assert!(matches!(d.psf, Psf::Gaussian { .. })),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn notes_list_every_step_and_carry_the_json() {
        let list = EditList::new(vec![
            Op::Wavelet(Wavelet::default()),
            Op::Levels(Levels::default()),
        ]);
        let n = list.notes();
        assert!(n[0].contains("2 operations") && n[0].contains("before any colour"));
        assert!(n[1].starts_with("1. wavelet") && n[2].starts_with("2. levels"));
        let tail = n.last().cloned().unwrap_or_default();
        let json = tail.strip_prefix("Helia edit list (JSON): ").unwrap_or("");
        assert_eq!(
            EditList::from_json(json).unwrap_or_else(|e| panic!("{e}")),
            list
        );
        assert!(n.iter().all(|l| l.is_ascii()));
        assert_eq!(EditList::default().notes().len(), 0);
    }

    #[test]
    fn preview_lists_shrink_lengths_and_drop_fine_layers() {
        let w = Op::Wavelet(Wavelet::default()).at_scale(1);
        match w {
            Op::Wavelet(v) => assert_eq!(v.layers.len(), 5),
            other => panic!("{other:?}"),
        }
        match Op::Unsharp(Unsharp {
            radius: 2.0,
            ..Unsharp::default()
        })
        .at_scale(2)
        {
            Op::Unsharp(u) => assert!((u.radius - 0.5).abs() < 1e-6),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn noise_is_reduced_on_flat_regions_without_flattening_an_edge() {
        // Left half dark, right half bright, Gaussian noise throughout.
        let (w, h) = (96usize, 64usize);
        let mut r = Rng::new(21);
        let sigma = 0.02;
        let mut f = Frame::zeros(w, h);
        let mut clean = Frame::zeros(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = if x < w / 2 { 0.3 } else { 0.7 };
                clean.data[y * w + x] = v;
                f.data[y * w + x] = v + sigma * r.gauss();
            }
        }
        let out = apply_cpu(&f, &EditList::new(vec![Op::Denoise(Denoise::default())]))
            .unwrap_or_else(|e| panic!("{e}"));
        let flat_rms = |img: &Frame| {
            let mut s = 0.0f64;
            let mut n = 0.0f64;
            for y in 8..h - 8 {
                for x in (8..w / 2 - 12).chain(w / 2 + 12..w - 8) {
                    let d = f64::from(img.get(x, y) - clean.get(x, y));
                    s += d * d;
                    n += 1.0;
                }
            }
            (s / n).sqrt() as f32
        };
        let (before, after) = (flat_rms(&f), flat_rms(&out));
        eprintln!("denoise: flat-region noise RMS {before:.5} -> {after:.5}");
        assert!(after < 0.6 * before, "noise {before} -> {after}");
        let step = |img: &Frame| {
            let m = |x0: usize, x1: usize| {
                let mut s = 0.0f32;
                let mut n = 0.0f32;
                for y in 8..h - 8 {
                    for x in x0..x1 {
                        s += img.get(x, y);
                        n += 1.0;
                    }
                }
                s / n
            };
            m(w / 2 + 2, w / 2 + 6) - m(w / 2 - 6, w / 2 - 2)
        };
        let (e0, e1) = (step(&f), step(&out));
        eprintln!("denoise: edge contrast {e0:.5} -> {e1:.5}");
        assert!(e1 > 0.95 * e0, "edge contrast {e0} -> {e1}");
    }
}
