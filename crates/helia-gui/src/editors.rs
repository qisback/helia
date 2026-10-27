// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The inspector bodies of the Sharpen and Adjust steps.
//!
//! Every control changes [`crate::edits::EditSettings`] and then asks for a
//! new preview; the preview itself is made off the UI thread and coalesced, so
//! a slider drag costs one render at a time.

use std::ops::RangeInclusive;

use eframe::egui::{
    self, Align, DragValue, Label, Layout, Sense, Slider, Stroke, StrokeKind, Ui, pos2, vec2,
};
use helia_core::edit::tone::{Curve, Levels};
use helia_core::edit::{Psf, SharpenMethod};
use helia_core::limbdark::{Law, Source};

use crate::app::App;
use crate::edits::PsfKind;
use crate::theme::{RADIUS_LARGE, palette};
use crate::widgets::{self, Icon, Kind, mono_text};

/// A caption and a number above a full-width slider. True when it moved.
#[allow(
    clippy::too_many_arguments,
    reason = "a caption, a value and how to show it"
)]
fn slider(
    ui: &mut Ui,
    label: &str,
    value: &mut f32,
    range: RangeInclusive<f32>,
    step: f64,
    decimals: usize,
    unit: &str,
    tip: &str,
) -> bool {
    let p = palette();
    ui.horizontal(|ui| {
        widgets::overline(ui, label);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add(Label::new(mono_text(
                format!("{value:.decimals$}{unit}"),
                13.0,
                p.text,
            )));
        });
    });
    ui.spacing_mut().slider_width = ui.available_width();
    ui.add(
        Slider::new(value, range)
            .step_by(step)
            .show_value(false)
            .trailing_fill(true),
    )
    .on_hover_text(tip)
    .changed()
}

/// As [`slider`] for a whole number.
fn int_slider(
    ui: &mut Ui,
    label: &str,
    value: &mut u32,
    range: RangeInclusive<u32>,
    tip: &str,
) -> bool {
    let p = palette();
    ui.horizontal(|ui| {
        widgets::overline(ui, label);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add(Label::new(mono_text(value.to_string(), 13.0, p.text)));
        });
    });
    ui.spacing_mut().slider_width = ui.available_width();
    ui.add(
        Slider::new(value, range)
            .show_value(false)
            .trailing_fill(true),
    )
    .on_hover_text(tip)
    .changed()
}

fn note(ui: &mut Ui, text: &str) {
    ui.add(Label::new(mono_text(text, 11.5, palette().faint)).wrap());
}

impl App {
    /// "Stack a capture first." when there is nothing to edit.
    fn need_result(&self, ui: &mut Ui) -> bool {
        if self.result.is_some() {
            return false;
        }
        ui.add(Label::new(mono_text(
            "Stack a capture first.",
            12.5,
            palette().faint,
        )));
        true
    }

    /// What the last preview took, or why there is none.
    fn preview_line(&self, ui: &mut Ui) {
        let p = palette();
        if let Some(e) = &self.edit_error {
            ui.add(Label::new(mono_text(e, 12.0, p.error_text)).wrap());
        } else if let Some(t) = self.preview_took {
            ui.add(Label::new(mono_text(
                format!(
                    "Preview {} ms{}",
                    t.as_millis(),
                    if self.settings.full_preview {
                        ", full size"
                    } else {
                        ""
                    }
                ),
                11.5,
                p.faint,
            )));
        }
    }

    /// "Limb darkening" at the top of the Sharpen step: divide the disc by its
    /// brightness profile, so the sharpening sees flat contrast. True when
    /// something changed.
    fn flatten_controls(&mut self, ui: &mut Ui) -> bool {
        let p = palette();
        let mut changed = false;
        widgets::overline(ui, "Limb darkening");
        ui.horizontal_wrapped(|ui| {
            for (label, source, tip) in [
                ("Off", None, "Leave the disc as it is"),
                (
                    "Model",
                    Some(Source::Model),
                    "Divide the disc by the fitted limb-darkening law",
                ),
                (
                    "Profile",
                    Some(Source::Profile),
                    "Divide the disc by its measured azimuthal median profile; also removes any other radial gradient",
                ),
            ] {
                if widgets::chip(ui, label, self.settings.edits.flatten == source, true)
                    .on_hover_text(tip)
                    .clicked()
                    && self.settings.edits.flatten != source
                {
                    self.settings.edits.flatten = source;
                    changed = true;
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            for law in Law::ALL {
                let tip = match law {
                    Law::Linear => "1 - u (1 - mu)",
                    Law::Quadratic => "1 - u1 (1 - mu) - u2 (1 - mu)^2, the usual law for the Sun",
                    Law::Neckel => "Neckel and Labs: five terms of 1 - mu^k",
                };
                let label = match law {
                    Law::Linear => "Linear",
                    Law::Quadratic => "Quadratic",
                    Law::Neckel => "Neckel",
                };
                if widgets::chip(ui, label, self.settings.edits.limb_law == law, true)
                    .on_hover_text(tip)
                    .clicked()
                    && self.settings.edits.limb_law != law
                {
                    self.settings.edits.limb_law = law;
                    changed = true;
                }
            }
        });
        let law = self.settings.edits.limb_law;
        let line = match self.result.clone() {
            Some(out) => self.limb_fits.get(&out, law).map_or_else(
                || "No clear disc to fit, so flattening would change nothing.".to_string(),
                |f| format!("Fitted to this stack: {}.", f.summary()),
            ),
            None => String::new(),
        };
        ui.add(Label::new(mono_text(line, 11.5, p.muted)).wrap());
        note(
            ui,
            "Model divides by the fitted law, Profile by the measured radial profile (a vignette or a filter's gradient included). Only the disc changes, the gain eases to 1 at the limb, and it runs before the sharpening.",
        );
        changed
    }

    /// The Sharpen step.
    pub(crate) fn sharpen_body(&mut self, ui: &mut Ui) {
        let p = palette();
        if self.need_result(ui) {
            return;
        }
        let (w, h) = self
            .result
            .as_ref()
            .map_or((1, 1), |r| (r.image.width, r.image.height));
        let preset = self.settings.preset;
        let mut changed = false;
        ui.spacing_mut().item_spacing.y = 6.0;

        changed |= self.flatten_controls(ui);
        ui.add_space(4.0);

        widgets::overline(ui, "Method");
        ui.horizontal_wrapped(|ui| {
            for m in SharpenMethod::ALL {
                let label = match m {
                    SharpenMethod::None => "Off",
                    SharpenMethod::Wavelet => "Wavelets",
                    SharpenMethod::Unsharp => "Unsharp",
                    SharpenMethod::RichardsonLucy => "Deconvolution",
                };
                if widgets::chip(ui, label, self.settings.edits.method == m, true)
                    .on_hover_text(m.summary())
                    .clicked()
                    && self.settings.edits.method != m
                {
                    self.settings.edits.method = m;
                    changed = true;
                }
            }
        });
        ui.add(
            Label::new(mono_text(
                self.settings.edits.method.summary(),
                12.0,
                p.muted,
            ))
            .wrap(),
        );
        if let Some(pr) = preset {
            ui.add(
                Label::new(mono_text(
                    format!("{} values: {}", pr.name(), pr.info().sharpen_reason),
                    11.5,
                    p.faint,
                ))
                .wrap(),
            );
        }
        ui.add_space(2.0);

        match self.settings.edits.method {
            SharpenMethod::None => {}
            SharpenMethod::Wavelet => {
                changed |= self.wavelet_controls(ui);
                self.sharpen_check_line(ui);
            }
            SharpenMethod::Unsharp => {
                changed |= self.unsharp_controls(ui);
                self.sharpen_check_line(ui);
            }
            SharpenMethod::RichardsonLucy => changed |= self.deconvolution_controls(ui),
        }

        ui.add_space(4.0);
        let recommended = self.settings.edits.sharpen_is_recommended(preset, w, h);
        ui.horizontal(|ui| {
            if widgets::button(ui, "Reset to recommended", Kind::Outline, !recommended, 0.0)
                .on_hover_text(match preset {
                    Some(pr) => format!("Back to the {} values", pr.name()),
                    None => "Back to the generic values".to_string(),
                })
                .clicked()
            {
                self.settings.edits.reset_sharpen(preset, w, h);
                changed = true;
            }
        });
        self.preview_line(ui);
        ui.add_space(2.0);
        self.presets_row(ui, &mut changed);
        ui.add_space(2.0);
        note(
            ui,
            "Sharpening can create artefacts: halos at the limb and round sunspots, ringing, and noise turned into texture. Use Before / after in the toolbar, and ease off if the picture starts to look processed.",
        );
        note(
            ui,
            "Sharpening runs on the mono stack, before colour. A FITS file is the unsharpened stack unless you ask for it in Export.",
        );
        if changed {
            self.edits_changed();
        }
    }

    fn wavelet_controls(&mut self, ui: &mut Ui) -> bool {
        use helia_core::edit::wavelet::{
            AMOUNT_MAX, DENOISE_MAX, Filter, SHARPEN_MAX, SHARPEN_MIN, Scheme,
        };
        let p = palette();
        let mut changed = false;
        ui.add_space(2.0);
        {
            let w = &mut self.settings.edits.wavelet;
            widgets::overline(ui, "Scheme and filter");
            ui.horizontal_wrapped(|ui| {
                for (s, label, tip) in [
                    (Scheme::Dyadic, "Dyadic", "Each layer is twice the size of the one before: 1, 2, 4, 8, 16, 32 pixels"),
                    (Scheme::Linear, "Linear", "The layer size grows by the step from the initial layer: finer control of the middle sizes"),
                ] {
                    if widgets::chip(ui, label, w.scheme == s, true).on_hover_text(tip).clicked()
                        && w.scheme != s
                    {
                        w.scheme = s;
                        changed = true;
                    }
                }
                ui.add_space(8.0);
                for (f, label, tip) in [
                    (Filter::Default, "Default", "The B3 spline, as in a standard a-trous transform"),
                    (Filter::Gaussian, "Gaussian", "A Gaussian kernel: a little smoother layers, usually the choice for sharpening with denoise"),
                ] {
                    if widgets::chip(ui, label, w.filter == f, true).on_hover_text(tip).clicked()
                        && w.filter != f
                    {
                        w.filter = f;
                        changed = true;
                    }
                }
                if widgets::chip(ui, "Linked", w.linked, true)
                    .on_hover_text("Each layer works on the result of the finer ones, so sharpened fine detail carries into the larger layers. Stronger, and it needs more denoise")
                    .clicked()
                {
                    w.linked = !w.linked;
                    changed = true;
                }
            });
            ui.horizontal(|ui| {
                ui.add(Label::new(mono_text("Initial layer", 12.0, p.muted)));
                let mut initial = w.initial as f32;
                if layer_cell_inline(ui, &mut initial, 1.0..=8.0, (1.0, 0), "The first slider's place in the scheme. 2 starts at the second layer size, skipping the pixel-scale noise") {
                    w.initial = initial.round() as u32;
                    changed = true;
                }
            });
            if w.scheme == Scheme::Linear {
                // Its own row: both boxes on one line are wider than the panel.
                ui.horizontal(|ui| {
                    ui.add(Label::new(mono_text("Step", 12.0, p.muted)));
                    changed |= layer_cell_inline(ui, &mut w.step, 0.0..=8.0, (0.5, 1), "Pixels added to the layer size for each layer; 0 keeps every layer at the initial size");
                });
            }
            w.spacing = None;
        }
        ui.add_space(2.0);
        let sizes = self.settings.edits.wavelet.spacings();
        egui::Grid::new("wavelet_layers")
            .num_columns(4)
            .spacing([6.0, 6.0])
            .show(ui, |ui| {
                widgets::overline(ui, "Layer");
                widgets::overline(ui, "Amount %");
                widgets::overline(ui, "Sharpen");
                widgets::overline(ui, "Denoise");
                ui.end_row();
                for (i, l) in self.settings.edits.wavelet.layers.iter_mut().enumerate() {
                    let px = sizes.get(i).copied().unwrap_or(1);
                    let tip = format!(
                        "Layer {}: detail about {} to {} pixels across{}",
                        i + 1,
                        px,
                        2 * px,
                        match i {
                            0 => ", and most of the noise",
                            1 => ", such as granulation and fibrils",
                            _ => "",
                        }
                    );
                    ui.add(Label::new(mono_text(format!("{}", i + 1), 12.5, p.muted)))
                        .on_hover_text(tip);
                    changed |= layer_cell(
                        ui,
                        &mut l.amount,
                        0.0..=AMOUNT_MAX,
                        (1.0, 0),
                        "The layer slider, as in RegiStax: how much of this layer's sharpen and denoise is applied. 100 is full; more than 100 pushes further",
                    );
                    changed |= drag_cell(
                        ui,
                        &mut l.sharpen,
                        SHARPEN_MIN..=SHARPEN_MAX,
                        (0.005, 3),
                        "Sharpen. The gain is 1 + amount x sharpen squared: 0.14 is 3x, 0.25 is 7x, 0.5 is 26x, 1 is 101x at 100%",
                    );
                    changed |= drag_cell(
                        ui,
                        &mut l.denoise,
                        0.0..=DENOISE_MAX,
                        (0.01, 2),
                        "Denoise: detail weaker than 10 x this many noise sigmas is removed from the layer. 0.2 is a 2 sigma cut",
                    );
                    ui.end_row();
                }
            });
        let g: Vec<String> = self
            .settings
            .edits
            .wavelet
            .layers
            .iter()
            .map(|l| format!("{:.1}", l.gain()))
            .collect();
        ui.add(
            Label::new(mono_text(
                format!("Gain by layer: {}", g.join("  ")),
                11.5,
                p.faint,
            ))
            .wrap(),
        );
        note(
            ui,
            "Sharpen 0 and denoise 0 leave a layer exactly as it is. The noise level of each layer is measured from the image itself. Coming from RegiStax? The user guide maps its controls to these.",
        );
        changed
    }

    fn unsharp_controls(&mut self, ui: &mut Ui) -> bool {
        let mut changed = false;
        let u = &mut self.settings.edits.unsharp;
        changed |= slider(
            ui,
            "Radius",
            &mut u.radius,
            0.3..=10.0,
            0.1,
            1,
            " px",
            "Size of the blur that is subtracted; match it to the detail you want",
        );
        changed |= slider(
            ui,
            "Amount",
            &mut u.amount,
            0.0..=3.0,
            0.05,
            2,
            "",
            "How much of the detail is added back",
        );
        changed |= slider(
            ui,
            "Threshold",
            &mut u.threshold,
            0.0..=0.05,
            0.001,
            3,
            "",
            "Detail weaker than this is not boosted, so noise stays quiet",
        );
        changed
    }

    fn deconvolution_controls(&mut self, ui: &mut Ui) -> bool {
        let p = palette();
        let mut changed = false;
        widgets::overline(ui, "Blur to undo (PSF)");
        let kind = PsfKind::of(self.settings.edits.rl.psf);
        ui.horizontal_wrapped(|ui| {
            for (k, label, tip) in [
                (
                    PsfKind::Auto,
                    "Auto",
                    "Sized from the image scale: typical seeing for this many pixels across the Sun",
                ),
                (
                    PsfKind::Limb,
                    "Limb",
                    "Measured from the edge of the Sun in this stack: the width of the limb is the width of the blur. Best in white light and calcium; the H-alpha limb is not a sharp edge",
                ),
                (
                    PsfKind::Gaussian,
                    "Gaussian",
                    "Seeing and a slightly soft focus",
                ),
                (
                    PsfKind::Airy,
                    "Airy",
                    "The diffraction pattern of the aperture; for a sharp, steady image",
                ),
            ] {
                if widgets::chip(ui, label, kind == k, true)
                    .on_hover_text(tip)
                    .clicked()
                    && kind != k
                {
                    self.settings.edits.set_psf(k);
                    changed = true;
                }
            }
        });
        match &mut self.settings.edits.rl.psf {
            Psf::Auto => {
                let s = self.auto_sigma.unwrap_or(1.2);
                ui.add(Label::new(mono_text(
                    format!("Gaussian, sigma {s:.2} px, from the size of the disc"),
                    12.0,
                    p.muted,
                )));
            }
            Psf::Limb => {
                let text = match (self.limb_sigma, self.auto_sigma) {
                    (Some(s), _) => format!("Gaussian, sigma {s:.2} px, measured from the limb"),
                    (None, Some(s)) => format!(
                        "No clear limb to measure: using sigma {s:.2} px from the size of the disc"
                    ),
                    (None, None) => "Measured from the limb once the stack is made".to_string(),
                };
                ui.add(Label::new(mono_text(text, 12.0, p.muted)).wrap());
            }
            Psf::Gaussian { sigma } => {
                changed |= slider(
                    ui,
                    "Sigma",
                    sigma,
                    0.3..=8.0,
                    0.05,
                    2,
                    " px",
                    "Width of the blur to undo (standard deviation)",
                );
            }
            Psf::Airy { radius } => {
                changed |= slider(
                    ui,
                    "First dark ring",
                    radius,
                    0.5..=helia_core::edit::deconv::MAX_AIRY_RADIUS,
                    0.05,
                    2,
                    " px",
                    "Radius of the first dark ring: 1.22 x wavelength / aperture, over the pixel scale",
                );
            }
        }
        let rl = &mut self.settings.edits.rl;
        changed |= int_slider(
            ui,
            "Iterations",
            &mut rl.iterations,
            1..=100,
            "More is sharper and noisier; stop when noise starts to look like texture",
        );
        changed |= slider(
            ui,
            "Regularisation (TV weight)",
            &mut rl.tv,
            0.0..=0.02,
            0.0005,
            4,
            "",
            "Total-variation weight: keeps noise from being amplified into speckle. 0 is plain Richardson-Lucy; higher is smoother",
        );
        changed |= slider(
            ui,
            "Limb and sunspot guard",
            &mut rl.protect,
            0.0..=1.0,
            0.05,
            2,
            "",
            "Holds the result inside the range of the original near the limb and sunspot edges, where deconvolution rings",
        );
        // The scrub: look at the result after fewer iterations, without
        // changing how many are made. The export always runs them all.
        let total = self.settings.edits.rl.iterations;
        let mut shown = self.rl_scrub.map_or(total, |k| k.min(total));
        if int_slider(
            ui,
            "Show iteration",
            &mut shown,
            0..=total,
            "Preview the result after this many iterations (0 is the original). Scrub to see when the detail stops coming and the noise starts. The export always runs all of them",
        ) {
            self.rl_scrub = (shown < total).then_some(shown);
            changed = true;
        }
        if self.rl_scrub.is_some_and(|k| k >= total) {
            self.rl_scrub = None;
        }
        ui.horizontal_wrapped(|ui| {
            if widgets::chip(ui, "Then wavelets", self.settings.edits.then_wavelets, true)
                .on_hover_text("Follow the deconvolution with the Wavelets settings (choose them under Wavelets): deconvolve first, then lift the detail and denoise the noise it raised")
                .clicked()
            {
                self.settings.edits.then_wavelets = !self.settings.edits.then_wavelets;
                changed = true;
            }
        });
        self.sharpen_check_line(ui);
        note(
            ui,
            "Deconvolution is the strongest option and the one most likely to leave artefacts. Keep the guard on, and compare with the original. It works best first: deconvolve, then use Wavelets with some denoise on what is left (see the user guide).",
        );
        changed
    }

    /// What the last preview did to the noise, and a warning when it mostly
    /// amplified it.
    fn sharpen_check_line(&self, ui: &mut Ui) {
        let p = palette();
        let Some(c) = &self.sharpen_check else {
            return;
        };
        ui.add(
            Label::new(mono_text(
                format!("Detail x{:.2}, noise x{:.2}", c.detail_gain, c.noise_gain),
                11.5,
                p.faint,
            ))
            .wrap(),
        );
        if let Some(w) = c.warning() {
            ui.add(Label::new(mono_text(w, 12.0, p.error_text)).wrap());
        }
    }

    /// Saved presets: use one, save the current settings, delete one.
    fn presets_row(&mut self, ui: &mut Ui, changed: &mut bool) {
        let p = palette();
        widgets::overline(ui, "My presets");
        let mut load = None;
        let mut delete = None;
        if self.user_presets.is_empty() {
            ui.add(Label::new(mono_text(
                "None yet. Name the current sharpening and adjustments to keep them.",
                11.5,
                p.faint,
            )));
        } else {
            ui.horizontal_wrapped(|ui| {
                for pr in &self.user_presets {
                    if widgets::chip(ui, &pr.name, false, true)
                        .on_hover_text(format!(
                            "Use this preset: {} edit{}",
                            pr.edits.ops.len(),
                            if pr.edits.ops.len() == 1 { "" } else { "s" }
                        ))
                        .clicked()
                    {
                        load = Some(pr.clone());
                    }
                    if widgets::icon_button(
                        ui,
                        Icon::Minus,
                        &format!("Delete {}", pr.name),
                        true,
                        false,
                    )
                    .clicked()
                    {
                        delete = Some(pr.name.clone());
                    }
                }
            });
        }
        let ok = !self.preset_name.trim().is_empty() && self.presets_dir.is_some();
        if widgets::text_and_button(
            ui,
            &mut self.preset_name,
            "Preset name",
            "Save",
            ok,
            "Keep the current sharpening and adjustments as a preset",
        ) {
            let name = self.preset_name.clone();
            self.save_edit_preset(&name);
        }
        if let Some(pr) = load {
            self.load_edit_preset(&pr);
            *changed = false;
        }
        if let Some(name) = delete {
            self.delete_edit_preset(&name);
        }
    }

    /// The Adjust step.
    /// "Prominences" in the Adjust step: a stretched off-limb layer joined to
    /// the disc. True when something changed.
    fn prominence_controls(&mut self, ui: &mut Ui) -> bool {
        let mut changed = false;
        widgets::overline(ui, "Prominences");
        changed |= widgets::toggle_row(
            ui,
            "Prominence layer",
            "Take the scattered light off outside the limb, stretch what is left and join it to the disc",
            &mut self.settings.edits.prominence_on,
            true,
        );
        if self.settings.edits.prominence_on {
            let pr = &mut self.settings.edits.prominence;
            changed |= slider(
                ui,
                "Strength",
                &mut pr.strength,
                0.1..=4.0,
                0.05,
                2,
                "",
                "How hard the layer is lifted; 1 refers it to the brightest prominence found",
            );
            changed |= slider(
                ui,
                "Stretch",
                &mut pr.stretch,
                0.0..=30.0,
                0.5,
                1,
                "",
                "The k of the asinh stretch: 0 is linear, larger lifts the faint parts more",
            );
            changed |= slider(
                ui,
                "Level",
                &mut pr.level,
                0.1..=1.2,
                0.01,
                2,
                "",
                "Brightness of the brightest prominence as a share of the disc's",
            );
            changed |= slider(
                ui,
                "Feather",
                &mut pr.feather,
                0.0..=0.06,
                0.002,
                3,
                " R",
                "Half-width of the soft join at the limb, in disc radii",
            );
            changed |= slider(
                ui,
                "Reach",
                &mut pr.reach,
                1.1..=2.0,
                0.01,
                2,
                " R",
                "How far out the layer reaches, in disc radii",
            );
            changed |= slider(
                ui,
                "Gate",
                &mut pr.gate,
                0.0..=6.0,
                0.1,
                1,
                " sigma",
                "Noise taken off the layer before the stretch",
            );
            changed |= slider(
                ui,
                "Join offset",
                &mut pr.offset,
                -0.05..=0.1,
                0.002,
                3,
                " R",
                "Move the join of layer and disc in or out from the fitted limb, in disc radii",
            );
            changed |= slider(
                ui,
                "Disc gamma",
                &mut pr.disc_gamma,
                0.4..=3.0,
                0.02,
                2,
                "",
                "Tone of the disc alone: above 1 darkens its mid tones, 1 leaves it as it is",
            );
            changed |= slider(
                ui,
                "Layer gamma",
                &mut pr.layer_gamma,
                0.4..=3.0,
                0.02,
                2,
                "",
                "Tone of the prominence layer alone: above 1 darkens its faint parts",
            );
        }
        changed
    }

    pub(crate) fn adjust_body(&mut self, ui: &mut Ui) {
        let p = palette();
        if self.need_result(ui) {
            return;
        }
        let (w, h) = self
            .result
            .as_ref()
            .map_or((1, 1), |r| (r.image.width, r.image.height));
        let mut changed = false;
        ui.spacing_mut().item_spacing.y = 6.0;

        changed |= self.prominence_controls(ui);
        ui.add_space(8.0);

        widgets::overline(ui, "Noise");
        changed |= widgets::toggle_row(
            ui,
            "Denoise",
            "Wavelet soft threshold on the finest layers",
            &mut self.settings.edits.denoise_on,
            true,
        );
        if self.settings.edits.denoise_on {
            let d = &mut self.settings.edits.denoise;
            changed |= slider(
                ui,
                "Strength",
                &mut d.strength,
                0.0..=6.0,
                0.1,
                1,
                " sigma",
                "Threshold of the finest layer in noise sigmas; coarser layers get less",
            );
            changed |= int_slider(
                ui,
                "Layers",
                &mut d.layers,
                1..=6,
                "How many layers are denoised",
            );
        }
        ui.add_space(8.0);

        changed |= self.levels_controls(ui);
        ui.add_space(8.0);
        changed |= self.curve_controls(ui);
        ui.add_space(8.0);
        changed |= self.geometry_controls(ui, w, h);

        ui.add_space(8.0);
        let any = self.settings.edits.any_adjustment();
        if widgets::button(
            ui,
            "Reset all adjustments",
            Kind::Outline,
            any,
            ui.available_width(),
        )
        .clicked()
        {
            self.settings.edits.denoise_on = false;
            self.settings.edits.prominence_on = false;
            self.settings.edits.reset_adjust(w, h);
            changed = true;
        }
        let out = self.edit_list().validate(w, h).ok();
        if let Some((ow, oh)) = out {
            ui.add(Label::new(mono_text(
                format!("Output {ow} x {oh} px"),
                12.5,
                p.muted,
            )));
        }
        self.preview_line(ui);
        note(
            ui,
            "Each adjustment is recorded in the file. A FITS file is the unedited stack unless you ask for the edits in Export.",
        );
        if changed {
            self.edits_changed();
        }
    }

    fn levels_controls(&mut self, ui: &mut Ui) -> bool {
        let mut changed = false;
        widgets::overline(ui, "Levels");
        let l = &mut self.settings.edits.levels;
        let before = *l;
        slider(
            ui,
            "Black point",
            &mut l.black,
            0.0..=1.0,
            0.005,
            3,
            "",
            "The input value that becomes black",
        );
        slider(
            ui,
            "White point",
            &mut l.white,
            0.0..=1.0,
            0.005,
            3,
            "",
            "The input value that becomes white",
        );
        slider(
            ui,
            "Gamma",
            &mut l.gamma,
            0.2..=4.0,
            0.05,
            2,
            "",
            "Above 1 brightens the mid-tones, below 1 darkens them",
        );
        // The white point stays above the black point.
        l.white = l.white.max(l.black + 0.01).min(1.0);
        l.black = l.black.min(l.white - 0.01).max(0.0);
        changed |= *l != before;
        let has = self.result.is_some();
        ui.horizontal(|ui| {
            if widgets::button(ui, "Auto levels", Kind::Outline, has, 0.0)
                .on_hover_text("Black and white at the 0.1% and 99.9% points of the stack")
                .clicked()
                && let Some(out) = &self.result
            {
                let auto = Levels::auto(&out.image.data, 0.001, 0.999);
                let l = &mut self.settings.edits.levels;
                l.black = auto.black;
                l.white = auto.white;
                changed = true;
            }
            let l = &mut self.settings.edits.levels;
            if widgets::button(ui, "Reset", Kind::Outline, !l.is_identity(), 0.0).clicked() {
                *l = Levels::default();
                changed = true;
            }
        });
        changed
    }

    fn curve_controls(&mut self, ui: &mut Ui) -> bool {
        let p = palette();
        let mut changed = false;
        widgets::overline(ui, "Curve");
        let plot_changed = curve_plot(ui, &mut self.settings.edits.curve);
        let mut remove = None;
        {
            let c = &mut self.settings.edits.curve;
            for (i, pt) in c.points.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.add(Label::new(mono_text("in", 12.0, p.muted)));
                    changed |= ui
                        .add(
                            DragValue::new(&mut pt[0])
                                .range(0.0..=1.0)
                                .speed(0.005)
                                .fixed_decimals(3),
                        )
                        .changed();
                    ui.add(Label::new(mono_text("out", 12.0, p.muted)));
                    changed |= ui
                        .add(
                            DragValue::new(&mut pt[1])
                                .range(0.0..=1.0)
                                .speed(0.005)
                                .fixed_decimals(3),
                        )
                        .changed();
                    if widgets::icon_button(ui, Icon::Minus, "Remove point", true, false).clicked()
                    {
                        remove = Some(i);
                    }
                });
            }
        }
        if let Some(i) = remove {
            self.settings.edits.curve.points.remove(i);
            changed = true;
        }
        let c = &mut self.settings.edits.curve;
        let ok = c.validate().is_ok();
        if !ok {
            ui.add(
                Label::new(mono_text(
                    "Points must have different inputs, and the curve must not fall.",
                    11.5,
                    p.error_text,
                ))
                .wrap(),
            );
        }
        ui.horizontal(|ui| {
            if widgets::button(
                ui,
                "Add point",
                Kind::Outline,
                c.points.len() < Curve::MAX_POINTS,
                0.0,
            )
            .on_hover_text("A point on the middle of the curve")
            .clicked()
            {
                let x = (0.2 + 0.1 * c.points.len() as f32).min(0.9);
                c.points.push([x, x]);
                changed = true;
            }
            if widgets::button(ui, "Reset", Kind::Outline, !c.is_identity(), 0.0).clicked() {
                *c = Curve::default();
                changed = true;
            }
        });
        // An invalid curve is held back from the preview, not applied.
        (changed || plot_changed) && ok
    }

    fn geometry_controls(&mut self, ui: &mut Ui, w: usize, h: usize) -> bool {
        let p = palette();
        let mut changed = false;
        widgets::overline(ui, "Crop");
        changed |= widgets::toggle_row(
            ui,
            "Crop",
            "Keep a rectangle of the image",
            &mut self.settings.edits.crop_on,
            true,
        );
        if self.settings.edits.crop_on {
            let c = &mut self.settings.edits.crop;
            ui.horizontal(|ui| {
                for (axis, v, max) in [("x", &mut c.x, w), ("y", &mut c.y, h)] {
                    ui.add(Label::new(mono_text(axis, 12.0, p.muted)));
                    changed |= ui
                        .add(
                            DragValue::new(v)
                                .range(0..=max.saturating_sub(1))
                                .speed(1.0),
                        )
                        .changed();
                }
                for (axis, v, max) in [("w", &mut c.width, w), ("h", &mut c.height, h)] {
                    ui.add(Label::new(mono_text(axis, 12.0, p.muted)));
                    changed |= ui
                        .add(DragValue::new(v).range(1..=max).speed(1.0))
                        .changed();
                }
            });
            // Keep the rectangle inside the picture.
            c.x = c.x.min(w.saturating_sub(1));
            c.y = c.y.min(h.saturating_sub(1));
            c.width = c.width.min(w - c.x).max(1);
            c.height = c.height.min(h - c.y).max(1);
            if widgets::button(ui, "Whole image", Kind::Outline, true, 0.0).clicked() {
                self.settings.edits.crop = helia_core::edit::Crop {
                    x: 0,
                    y: 0,
                    width: w,
                    height: h,
                };
                changed = true;
            }
        }
        ui.add_space(8.0);
        widgets::overline(ui, "Rotate and flip");
        ui.horizontal_wrapped(|ui| {
            let e = &mut self.settings.edits;
            for (label, turn, tip) in [
                ("90\u{b0} left", -90.0, "Quarter turn anticlockwise"),
                ("90\u{b0} right", 90.0, "Quarter turn clockwise"),
                ("180\u{b0}", 180.0, "Half turn"),
            ] {
                if widgets::chip(ui, label, false, true)
                    .on_hover_text(tip)
                    .clicked()
                {
                    e.rotate = wrap_degrees(e.rotate + turn);
                    changed = true;
                }
            }
            if widgets::chip(ui, "Flip left-right", e.flip_h, true)
                .on_hover_text("Mirror the image")
                .clicked()
            {
                e.flip_h = !e.flip_h;
                changed = true;
            }
            if widgets::chip(ui, "Flip top-bottom", e.flip_v, true)
                .on_hover_text("Mirror the image")
                .clicked()
            {
                e.flip_v = !e.flip_v;
                changed = true;
            }
        });
        changed |= slider(
            ui,
            "Angle",
            &mut self.settings.edits.rotate,
            -180.0..=180.0,
            0.1,
            1,
            "\u{b0}",
            "Degrees clockwise. Quarter turns are exact; other angles are interpolated and enlarge the canvas",
        );
        ui.add_space(8.0);
        widgets::overline(ui, "Resize");
        changed |= slider(
            ui,
            "Scale",
            &mut self.settings.edits.resize_percent,
            10.0..=400.0,
            1.0,
            0,
            "%",
            "Averages down exactly; interpolates up with Lanczos-3",
        );
        ui.horizontal_wrapped(|ui| {
            for pc in [25.0, 50.0, 100.0, 200.0] {
                if widgets::chip(
                    ui,
                    &format!("{pc:.0}%"),
                    (self.settings.edits.resize_percent - pc).abs() < 0.5,
                    true,
                )
                .clicked()
                {
                    self.settings.edits.resize_percent = pc;
                    changed = true;
                }
            }
        });
        changed
    }
}

/// A slider with its number beside it, for a grid cell. True when it moved.
fn layer_cell(
    ui: &mut Ui,
    value: &mut f32,
    range: RangeInclusive<f32>,
    step_and_decimals: (f64, usize),
    tip: &str,
) -> bool {
    ui.horizontal(|ui| layer_cell_inline(ui, value, range, step_and_decimals, tip))
        .inner
}

/// The slider and number box of [`layer_cell`], added to the current row, so a
/// wrapping row can move them to the next line as a pair.
fn layer_cell_inline(
    ui: &mut Ui,
    value: &mut f32,
    range: RangeInclusive<f32>,
    (step, decimals): (f64, usize),
    tip: &str,
) -> bool {
    ui.spacing_mut().slider_width = 58.0;
    ui.spacing_mut().item_spacing.x = 6.0;
    let a = ui
        .add(
            Slider::new(value, range.clone())
                .step_by(step)
                .show_value(false)
                .trailing_fill(true),
        )
        .on_hover_text(tip)
        .changed();
    let b = ui
        .add(
            DragValue::new(value)
                .range(range)
                .speed(step * 0.2)
                .fixed_decimals(decimals),
        )
        .on_hover_text(tip)
        .changed();
    a || b
}

/// A numeric box that is dragged or typed into, without a slider.
fn drag_cell(
    ui: &mut Ui,
    value: &mut f32,
    range: RangeInclusive<f32>,
    (step, decimals): (f64, usize),
    tip: &str,
) -> bool {
    ui.add(
        DragValue::new(value)
            .range(range)
            .speed(step)
            .fixed_decimals(decimals),
    )
    .on_hover_text(tip)
    .changed()
}

/// Degrees brought into -180..=180.
fn wrap_degrees(d: f32) -> f32 {
    let mut v = d % 360.0;
    if v > 180.0 {
        v -= 360.0;
    } else if v < -180.0 {
        v += 360.0;
    }
    v
}

/// How near (in points on screen) the pointer must be to grab a curve point.
const GRAB_RADIUS: f32 = 10.0;

/// Nudge sizes for the arrow keys: fine, and coarse with Shift.
const NUDGE: f32 = 0.005;
const NUDGE_COARSE: f32 = 0.05;

/// The tone curve with its points, which can be dragged. Click empty space to
/// add a point, drag a point to move it, double-click or press Delete to remove
/// it. With the plot focused, `[` and `]` choose a point and the arrow keys
/// nudge it (Shift for bigger steps). Points never pass their neighbours, see
/// [`crate::curve_edit`]. True when the curve changed.
fn curve_plot(ui: &mut Ui, curve: &mut Curve) -> bool {
    use crate::curve_edit as edit;
    let p = palette();
    let side = ui.available_width().min(200.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(side, side * 0.7), Sense::click_and_drag());
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Other,
            true,
            "Tone curve. Click to add a point, drag a point to move it, double-click or press Delete to remove it. Press [ or ] to choose a point and the arrow keys to nudge it.",
        )
    });
    let at = |x: f32, y: f32| {
        pos2(
            rect.left() + x * rect.width(),
            rect.bottom() - y * rect.height(),
        )
    };
    let unit = |q: egui::Pos2| {
        [
            ((q.x - rect.left()) / rect.width()).clamp(0.0, 1.0),
            ((rect.bottom() - q.y) / rect.height()).clamp(0.0, 1.0),
        ]
    };
    let reach = [GRAB_RADIUS / rect.width(), GRAB_RADIUS / rect.height()];
    let sel_id = resp.id.with("selected");
    let mut sel: Option<usize> = ui
        .data(|d| d.get_temp::<Option<usize>>(sel_id))
        .flatten()
        .filter(|&i| i < curve.points.len());
    let mut changed = false;

    // Arrow keys must nudge, not move focus to the next widget.
    if resp.has_focus() {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                resp.id,
                egui::EventFilter {
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    ..Default::default()
                },
            );
        });
    }

    let hover_hit = resp
        .hover_pos()
        .and_then(|q| edit::nearest(&curve.points, unit(q), reach));
    if hover_hit.is_some() {
        ui.output_mut(|o| {
            o.cursor_icon = if resp.dragged() {
                egui::CursorIcon::Grabbing
            } else {
                egui::CursorIcon::Grab
            }
        });
    }

    // Pointer: a press picks (or adds) a point, dragging moves it.
    if resp.drag_started() || resp.clicked() {
        resp.request_focus();
        if let Some(origin) = ui
            .input(|i| i.pointer.press_origin())
            .or_else(|| resp.interact_pointer_pos())
        {
            let want = unit(origin);
            let picked = edit::nearest(&curve.points, want, reach).or_else(|| {
                let added = edit::insert(&mut curve.points, want);
                changed |= added.is_some();
                added
            });
            if picked.is_some() {
                sel = picked;
            }
        }
    }
    if resp.dragged()
        && let (Some(i), Some(q)) = (sel, resp.interact_pointer_pos())
    {
        changed |= edit::place(&mut curve.points, i, unit(q));
    }
    if (resp.double_clicked() || resp.secondary_clicked())
        && let Some(i) = resp
            .interact_pointer_pos()
            .and_then(|q| edit::nearest(&curve.points, unit(q), reach))
        && edit::remove(&mut curve.points, i)
    {
        changed = true;
        sel = None;
    }

    // Keyboard.
    if resp.has_focus() && !curve.points.is_empty() {
        use egui::Key;
        let mut pick = 0isize;
        let (mut dx, mut dy, mut del) = (0.0f32, 0.0f32, false);
        ui.input_mut(|i| {
            let k = if i.modifiers.shift {
                NUDGE_COARSE
            } else {
                NUDGE
            };
            for m in [egui::Modifiers::NONE, egui::Modifiers::SHIFT] {
                if i.consume_key(m, Key::ArrowLeft) {
                    dx -= k;
                }
                if i.consume_key(m, Key::ArrowRight) {
                    dx += k;
                }
                if i.consume_key(m, Key::ArrowUp) {
                    dy += k;
                }
                if i.consume_key(m, Key::ArrowDown) {
                    dy -= k;
                }
            }
            let none = egui::Modifiers::NONE;
            if i.consume_key(none, Key::CloseBracket) {
                pick += 1;
            }
            if i.consume_key(none, Key::OpenBracket) {
                pick -= 1;
            }
            del = i.consume_key(none, Key::Delete) || i.consume_key(none, Key::Backspace);
        });
        if pick != 0 {
            sel = Some(match sel {
                Some(i) => edit::step_selection(&curve.points, i, pick > 0),
                None => edit::order(&curve.points)[0],
            });
        }
        if let Some(i) = sel {
            if dx != 0.0 || dy != 0.0 {
                let c = curve.points[i];
                changed |= edit::place(&mut curve.points, i, [c[0] + dx, c[1] + dy]);
            }
            if del && edit::remove(&mut curve.points, i) {
                changed = true;
                sel = None;
            }
        }
    }
    ui.data_mut(|d| d.insert_temp(sel_id, sel));

    let painter = ui.painter_at(rect.expand(6.0));
    painter.rect(
        rect,
        egui::CornerRadius::same(RADIUS_LARGE),
        p.ground,
        Stroke::new(1.0, if resp.has_focus() { p.accent } else { p.border }),
        StrokeKind::Inside,
    );
    painter.line_segment([at(0.0, 0.0), at(1.0, 1.0)], Stroke::new(1.0, p.border));
    if let Ok(k) = curve.knots() {
        let pts: Vec<egui::Pos2> = (0..=64)
            .map(|i| {
                let x = i as f32 / 64.0;
                at(x, Curve::apply_knots(&k, x))
            })
            .collect();
        painter.add(egui::Shape::line(pts, Stroke::new(1.5, p.accent)));
    }
    for (i, pt) in curve.points.iter().enumerate() {
        let c = at(pt[0], pt[1]);
        if Some(i) == sel {
            painter.circle(c, 6.0, p.text, Stroke::new(2.0, p.accent));
        } else if hover_hit == Some(i) {
            painter.circle_filled(c, 5.0, p.text);
        } else {
            painter.circle_filled(c, 3.5, p.text);
        }
    }
    if let Some(i) = sel {
        let q = curve.points[i];
        resp.on_hover_text(format!("in {:.3}, out {:.3}", q[0], q[1]));
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plot_harness(points: Vec<[f32; 2]>) -> egui_kittest::Harness<'static, Curve> {
        let mut h = egui_kittest::Harness::new_ui_state(
            |ui, c: &mut Curve| {
                curve_plot(ui, c);
            },
            Curve { points },
        );
        h.run();
        h
    }

    /// Screen position of curve coordinates `(x, y)`.
    fn at(h: &egui_kittest::Harness<'_, Curve>, x: f32, y: f32) -> egui::Pos2 {
        use egui_kittest::kittest::Queryable as _;
        let r = h.get_by_label_contains("Tone curve").rect();
        pos2(r.left() + x * r.width(), r.bottom() - y * r.height())
    }

    #[test]
    fn clicking_adds_and_dragging_moves_a_point_without_breaking_the_curve() {
        let mut h = plot_harness(vec![[0.0, 0.0], [1.0, 1.0]]);
        let (p, q) = (at(&h, 0.5, 0.3), at(&h, 0.9, 0.9));
        h.hover_at(p);
        h.run();
        h.drag_at(p);
        h.run();
        h.drop_at(p);
        h.run();
        assert_eq!(h.state().points.len(), 3, "{:?}", h.state().points);
        assert!((h.state().points[1][0] - 0.5).abs() < 0.02);

        // Drag it far to the right and up: it stops short of the end point.
        let p = at(&h, h.state().points[1][0], h.state().points[1][1]);
        h.hover_at(p);
        h.run();
        h.drag_at(p);
        h.run();
        h.hover_at(q);
        h.run();
        h.hover_at(at(&h, 0.99, 0.99));
        h.run();
        h.drop_at(q);
        h.run();
        let pts = &h.state().points;
        assert_eq!(pts.len(), 3);
        assert!(pts[1][0] > 0.9 && pts[1][0] < 1.0, "{pts:?}");
        assert!(h.state().validate().is_ok(), "{pts:?}");
    }

    #[test]
    fn keys_nudge_and_delete_the_chosen_point() {
        let mut h = plot_harness(vec![[0.0, 0.0], [0.5, 0.5], [1.0, 1.0]]);
        let p = at(&h, 0.5, 0.5);
        h.hover_at(p);
        h.run();
        // Focus the plot and choose the middle point.
        h.drag_at(p);
        h.run();
        h.drop_at(p);
        h.run();
        let before = h.state().points[1];
        h.key_press(egui::Key::ArrowRight);
        h.run();
        h.key_press(egui::Key::ArrowUp);
        h.run();
        let after = h.state().points[1];
        assert!((after[0] - before[0] - 0.005).abs() < 1e-5, "{after:?}");
        assert!((after[1] - before[1] - 0.005).abs() < 1e-5, "{after:?}");
        h.key_press(egui::Key::Delete);
        h.run();
        assert_eq!(h.state().points.len(), 2, "{:?}", h.state().points);
        assert!(h.state().is_identity());
    }

    #[test]
    fn angles_wrap_into_half_turns() {
        assert!((wrap_degrees(270.0) - (-90.0)).abs() < 1e-6);
        assert!((wrap_degrees(-270.0) - 90.0).abs() < 1e-6);
        assert!((wrap_degrees(180.0) - 180.0).abs() < 1e-6);
        assert!(wrap_degrees(360.0).abs() < 1e-6);
    }
}
