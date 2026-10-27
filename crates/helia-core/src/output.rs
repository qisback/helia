// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The last step: edits, colour, and the file, in one place so the command
//! line, the GUI and the API write the same thing.
//!
//! The order is fixed and is the point of this module:
//!
//! 1. **Edits** (sharpening first, then whatever else the list holds) run on
//!    the mono stack. Sharpening is a measurement-side operation: it needs
//!    the real signal and noise, not a colour mapping of them.
//! 2. **Colour** is applied afterwards, to the edited mono image.
//! 3. The file records the edits and the colour in its metadata.
//!
//! FITS is the exception to step 1. A FITS file is for further processing, so
//! by default it holds the unedited stack; the edits are applied to it only
//! when [`OutputPlan::edit_fits`] is set, and the file says which it is.

use std::path::Path;

use crate::backend::Backend;
use crate::colour::{ColourParams, RgbImage};
use crate::edit::EditList;
use crate::error::Result;
use crate::frame::Frame;
use crate::io::any::Loaded;
use crate::io::solarnet::FitsMeta;
use crate::io::{ImageFormat, ImageNotes, save_image, save_image_with, save_rgb};
use crate::truecolour::rgb_luminance;

/// What to do to the stack on its way to a file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OutputPlan {
    /// The edits, in order.
    pub edits: EditList,
    /// The colour look.
    pub colour: ColourParams,
    /// Apply the edits to FITS output too (default: FITS is the unedited stack).
    pub edit_fits: bool,
    /// Lines recorded ahead of the others (where the image came from).
    pub lead_notes: Vec<String>,
    /// Observation metadata for a FITS file (SOLARNET keywords).
    pub fits: Option<FitsMeta>,
    /// A true-colour stack: the picture that is edited (channel by channel) and
    /// written in place of the mono stack, to TIFF, PNG or a three-plane FITS.
    /// The colour look and invert do not apply to it.
    pub rgb: Option<RgbImage>,
}

/// What was written.
#[derive(Clone, Debug, PartialEq)]
pub struct Written {
    /// Width of the file's image.
    pub width: usize,
    /// Height of the file's image.
    pub height: usize,
    /// The edits were applied to the pixels.
    pub edited: bool,
    /// The pixels are colour.
    pub coloured: bool,
    /// The lines recorded in the file.
    pub notes: Vec<String>,
    /// The part of `notes` about the edits (not the colour).
    pub edit_notes: Vec<String>,
}

/// Said in a FITS file that was left unedited although edits were asked for.
pub const FITS_UNEDITED: &str = "This FITS file is the unedited stack: sharpening and other edits were not applied to it (they are in the TIFF or PNG; ask for them with --sharpen-fits)";

/// The invert of a mono image, as the colour step does it.
fn inverted(frame: &Frame) -> Frame {
    let mut inv = frame.clone();
    for v in &mut inv.data {
        *v = 1.0 - v.clamp(0.0, 1.0);
    }
    inv
}

/// Apply `edits` to each channel of a colour image.
///
/// # Errors
/// Invalid edits.
pub fn edit_rgb(backend: &dyn Backend, img: &RgbImage, edits: &EditList) -> Result<RgbImage> {
    if edits.is_empty() {
        return Ok(img.clone());
    }
    let (w, h) = (img.width, img.height);
    let mut planes = Vec::with_capacity(3);
    for c in 0..3 {
        let data: Vec<f32> = img.data.as_chunks::<3>().0.iter().map(|p| p[c]).collect();
        planes.push(backend.apply_edits(&Frame::new(w, h, data)?, edits)?);
    }
    let (ow, oh) = (planes[0].width, planes[0].height);
    let mut data = Vec::with_capacity(ow * oh * 3);
    for i in 0..ow * oh {
        data.extend([planes[0].data[i], planes[1].data[i], planes[2].data[i]]);
    }
    Ok(RgbImage {
        width: ow,
        height: oh,
        data,
    })
}

/// The stack after the edits, ready to be coloured and written.
#[derive(Clone, Debug)]
pub struct Prepared {
    /// The mono image to colour or write; for a true-colour stack its
    /// luminance, for looking at.
    pub image: Frame,
    /// The edited true-colour picture, when the plan has one.
    pub rgb: Option<RgbImage>,
    /// The edits were applied to it.
    pub edited: bool,
    /// The lines the file will record (without the colour line).
    pub lines: Vec<String>,
}

/// Run the edits of `plan` on `stack` for output to `path`: nothing for FITS
/// unless [`OutputPlan::edit_fits`] is set.
///
/// # Errors
/// An unknown extension or invalid edits.
pub fn prepare(
    backend: &dyn Backend,
    stack: &Frame,
    path: &Path,
    plan: &OutputPlan,
) -> Result<Prepared> {
    let fits = ImageFormat::from_path(path)? == ImageFormat::Fits;
    let apply = !plan.edits.is_empty() && (!fits || plan.edit_fits);
    // Check the list against the stack before any work is done.
    plan.edits.validate(stack.width, stack.height)?;
    let (image, rgb) = if let Some(colour) = &plan.rgb {
        let out = if apply {
            edit_rgb(backend, colour, &plan.edits)?
        } else {
            colour.clone()
        };
        (rgb_luminance(&out)?, Some(out))
    } else if apply {
        (backend.apply_edits(stack, &plan.edits)?, None)
    } else {
        (stack.clone(), None)
    };
    let mut lines: Vec<String> = plan.lead_notes.clone();
    if apply {
        lines.extend(plan.edits.notes());
    } else if fits && !plan.edits.is_empty() {
        lines.push(FITS_UNEDITED.to_string());
    }
    Ok(Prepared {
        image,
        rgb,
        edited: apply,
        lines,
    })
}

/// Colour (when the plan says so and the format allows) and write a
/// [`Prepared`] image to `path`.
///
/// # Errors
/// A failure colouring or writing.
pub fn finish(
    backend: &dyn Backend,
    prepared: &Prepared,
    path: &Path,
    plan: &OutputPlan,
) -> Result<Written> {
    let fits = ImageFormat::from_path(path)? == ImageFormat::Fits;
    let mut lines = prepared.lines.clone();
    if plan.colour.preset.is_some() && prepared.rgb.is_none() {
        lines.push(plan.colour.describe());
    }
    let notes = ImageNotes {
        lines: lines.clone(),
        fits: if fits { plan.fits.clone() } else { None },
    };
    let edited = &prepared.image;
    if let Some(rgb) = &prepared.rgb {
        save_rgb(path, rgb, &notes)?;
        return Ok(Written {
            width: rgb.width,
            height: rgb.height,
            edited: prepared.edited,
            coloured: true,
            notes: lines,
            edit_notes: prepared.lines.clone(),
        });
    }
    let coloured = plan.colour.preset.is_some() && !fits;
    if coloured {
        let rgb = backend.colourise(edited, &plan.colour.lut())?;
        save_rgb(path, &rgb, &notes)?;
    } else {
        let mono = if plan.colour.invert && plan.colour.preset.is_none() && !fits {
            inverted(edited)
        } else {
            edited.clone()
        };
        if lines.is_empty() && notes.fits.is_none() {
            save_image(path, &mono)?;
        } else {
            save_image_with(path, &mono, &notes)?;
        }
    }
    Ok(Written {
        width: edited.width,
        height: edited.height,
        edited: prepared.edited,
        coloured,
        notes: lines,
        edit_notes: prepared.lines.clone(),
    })
}

/// The picture [`write_stack`] would write to a TIFF, kept in memory: the stack
/// after the edits and the colour look.
///
/// # Errors
/// Invalid edits or a failure colouring.
pub fn render(backend: &dyn Backend, stack: &Frame, plan: &OutputPlan) -> Result<Loaded> {
    let prepared = prepare(backend, stack, Path::new("render.tif"), plan)?;
    if let Some(rgb) = prepared.rgb {
        return Ok(Loaded::Rgb(rgb));
    }
    if plan.colour.preset.is_some() {
        return Ok(Loaded::Rgb(
            backend.colourise(&prepared.image, &plan.colour.lut())?,
        ));
    }
    Ok(Loaded::Mono(if plan.colour.invert {
        inverted(&prepared.image)
    } else {
        prepared.image
    }))
}

/// Write the stack to `path` as the extension says, following `plan`
/// ([`prepare`], then [`finish`]).
///
/// # Errors
/// Invalid edits, an unknown extension, or a failure writing the file.
pub fn write_stack(
    backend: &dyn Backend,
    stack: &Frame,
    path: &Path,
    plan: &OutputPlan,
) -> Result<Written> {
    finish(backend, &prepare(backend, stack, path, plan)?, path, plan)
}

/// Edit a loaded image and write it to `path`: the path of `helia edit`. A
/// colour image is edited channel by channel and stays colour; a mono image
/// follows [`write_stack`] with no colour look.
///
/// # Errors
/// Invalid edits, FITS output for a colour image, or a failure writing.
pub fn write_loaded(
    backend: &dyn Backend,
    image: &Loaded,
    path: &Path,
    edits: &EditList,
    extra_notes: &[String],
) -> Result<Written> {
    match image {
        Loaded::Mono(f) => {
            let plan = OutputPlan {
                edits: edits.clone(),
                colour: ColourParams::default(),
                edit_fits: true,
                lead_notes: extra_notes.to_vec(),
                fits: None,
                rgb: None,
            };
            write_stack(backend, f, path, &plan)
        }
        Loaded::Rgb(img) => {
            edits.validate(img.width, img.height)?;
            let out = edit_rgb(backend, img, edits)?;
            let mut lines = extra_notes.to_vec();
            lines.extend(edits.notes());
            save_rgb(
                path,
                &out,
                &ImageNotes {
                    lines: lines.clone(),
                    ..ImageNotes::default()
                },
            )?;
            Ok(Written {
                width: out.width,
                height: out.height,
                edited: !edits.is_empty(),
                coloured: true,
                edit_notes: lines.clone(),
                notes: lines,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::CpuBackend;
    use crate::colour::Preset;
    use crate::edit::{Flip, Levels, Op, SharpenMethod, sharpen_edits};
    use crate::io::any::read_any;

    fn dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("helia-out-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap_or_else(|e| panic!("{e}"));
        d
    }

    fn stack() -> Frame {
        let mut f = Frame::zeros(40, 30);
        for y in 0..30 {
            for x in 0..40 {
                f.data[y * 40 + x] = if (x as f32 - 20.0).hypot(y as f32 - 15.0) < 10.0 {
                    0.8
                } else {
                    0.1
                };
            }
        }
        crate::filter::gaussian_blur(&f, 1.5)
    }

    #[test]
    fn fits_is_the_unedited_stack_unless_asked() {
        let d = dir("fits");
        let s = stack();
        let plan = OutputPlan {
            edits: sharpen_edits(SharpenMethod::Wavelet, Some(Preset::White), 40, 30),
            ..OutputPlan::default()
        };
        let a = write_stack(&CpuBackend, &s, &d.join("a.fits"), &plan)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(!a.edited);
        assert!(a.notes.iter().any(|l| l.contains("unedited stack")));
        let (Loaded::Mono(back), _) = read_any(&d.join("a.fits")).unwrap_or_else(|e| panic!("{e}"))
        else {
            panic!("mono")
        };
        assert_eq!(back.data, s.data);
        let plan = OutputPlan {
            edit_fits: true,
            ..plan
        };
        let b = write_stack(&CpuBackend, &s, &d.join("b.fits"), &plan)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(b.edited);
        let (Loaded::Mono(back), _) = read_any(&d.join("b.fits")).unwrap_or_else(|e| panic!("{e}"))
        else {
            panic!("mono")
        };
        assert_ne!(back.data, s.data);
        assert!(
            String::from_utf8_lossy(&std::fs::read(d.join("b.fits")).unwrap_or_default())
                .contains("HISTORY 1. wavelet")
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn sharpening_comes_before_colour_and_is_recorded() {
        let d = dir("tif");
        let s = stack();
        let edits = sharpen_edits(SharpenMethod::Unsharp, Some(Preset::Ha), 40, 30);
        let plan = OutputPlan {
            edits: edits.clone(),
            colour: ColourParams::preset(Preset::Ha),
            edit_fits: false,
            lead_notes: Vec::new(),
            fits: None,
            rgb: None,
        };
        let w =
            write_stack(&CpuBackend, &s, &d.join("c.tif"), &plan).unwrap_or_else(|e| panic!("{e}"));
        assert!(w.coloured && w.edited);
        let (Loaded::Rgb(got), desc) = read_any(&d.join("c.tif")).unwrap_or_else(|e| panic!("{e}"))
        else {
            panic!("rgb")
        };
        let desc = desc.unwrap_or_default();
        assert!(desc.contains("unsharp mask") && desc.contains("preset ha"));
        // Colouring the edited mono image gives the file; colouring then
        // sharpening would not.
        let mono = CpuBackend
            .apply_edits(&s, &edits)
            .unwrap_or_else(|e| panic!("{e}"));
        let want = crate::colour::colourise(&mono, &plan.colour);
        let worst = got
            .data
            .iter()
            .zip(&want.data)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(worst <= 1.0 / 65535.0 + 1e-6, "{worst}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_bad_list_writes_nothing() {
        let d = dir("bad");
        let plan = OutputPlan {
            edits: EditList::new(vec![Op::Levels(Levels {
                black: 0.9,
                white: 0.1,
                ..Levels::default()
            })]),
            ..OutputPlan::default()
        };
        assert!(write_stack(&CpuBackend, &stack(), &d.join("x.tif"), &plan).is_err());
        assert!(!d.join("x.tif").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn edit_rgb_edits_every_channel_and_keeps_colour() {
        let img = RgbImage {
            width: 4,
            height: 2,
            data: (0..24).map(|i| i as f32 / 23.0).collect(),
        };
        let out = edit_rgb(
            &CpuBackend,
            &img,
            &EditList::new(vec![Op::Flip(Flip::default())]),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(out.pixel(0, 0), img.pixel(3, 0));
        assert_eq!(out.pixel(3, 1), img.pixel(0, 1));
    }

    fn colour_stack() -> RgbImage {
        let (w, h) = (40usize, 30usize);
        let mut data = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                let d = (x as f32 - 20.0).hypot(y as f32 - 15.0);
                let on = if d < 10.0 { 1.0 } else { 0.05 };
                data.extend([0.9 * on, 0.7 * on, 0.4 * on]);
            }
        }
        RgbImage {
            width: w,
            height: h,
            data,
        }
    }

    #[test]
    fn a_true_colour_stack_is_written_as_colour_in_every_format() {
        let d = dir("truecolour");
        let rgb = colour_stack();
        let lum = rgb_luminance(&rgb).unwrap_or_else(|e| panic!("{e}"));
        for name in ["c.tif", "c.png", "c.fits"] {
            let plan = OutputPlan {
                rgb: Some(rgb.clone()),
                lead_notes: vec!["True colour: test".to_owned()],
                ..OutputPlan::default()
            };
            let w = write_stack(&CpuBackend, &lum, &d.join(name), &plan)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(w.coloured && !w.edited, "{name}");
            assert_eq!((w.width, w.height), (40, 30));
            let (back, _) = read_any(&d.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
            let Loaded::Rgb(got) = back else {
                panic!("{name} did not read back as colour");
            };
            assert_eq!((got.width, got.height), (40, 30), "{name}");
            // 8-bit PNG and 16-bit TIFF round the samples; FITS keeps them.
            let tol = if name == "c.png" { 0.004 } else { 0.0005 };
            let worst = got
                .data
                .iter()
                .zip(&rgb.data)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max);
            assert!(worst < tol, "{name}: {worst}");
        }
    }

    #[test]
    fn a_true_colour_stack_is_edited_channel_by_channel() {
        let d = dir("truecolour-edit");
        let rgb = colour_stack();
        let lum = rgb_luminance(&rgb).unwrap_or_else(|e| panic!("{e}"));
        let plan = OutputPlan {
            rgb: Some(rgb.clone()),
            edits: EditList::new(vec![Op::Flip(Flip::default())]),
            ..OutputPlan::default()
        };
        let prepared =
            prepare(&CpuBackend, &lum, &d.join("e.tif"), &plan).unwrap_or_else(|e| panic!("{e}"));
        let out = prepared.rgb.expect("colour");
        assert!(prepared.edited);
        // The flip moved every channel the same way.
        assert_eq!(out.pixel(0, 0), rgb.pixel(0, 29));
        assert_eq!(
            prepared.image,
            rgb_luminance(&out).unwrap_or_else(|e| panic!("{e}"))
        );
        // A FITS file is unedited unless the plan says so, and says so.
        let fits =
            prepare(&CpuBackend, &lum, &d.join("e.fits"), &plan).unwrap_or_else(|e| panic!("{e}"));
        assert!(!fits.edited);
        assert_eq!(fits.rgb.expect("colour"), rgb);
    }
}
