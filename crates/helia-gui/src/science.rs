// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The Science panel of the Stack step: the plots a stack's science report
//! holds (frame quality, shifts, seeing, ...), drawn by `helia-report`, shown
//! in the viewer, and saved as a report folder.

use std::sync::Arc;

use eframe::egui::{self, ColorImage, Label, TextureHandle, TextureOptions, Ui};

use crate::app::App;
use crate::theme::palette;
use crate::widgets::{self, mono_text};

/// One figure of the report as a texture.
pub struct ScienceFigure {
    /// Short name for the chip.
    pub label: String,
    /// What it shows and how to read it.
    pub caption: String,
    /// The picture.
    pub tex: TextureHandle,
    /// Its size in pixels.
    pub size: [usize; 2],
}

/// The report of the current stack, made when the panel is first opened.
pub struct ScienceView {
    /// Every file, for saving.
    pub bundle: Arc<helia_report::Bundle>,
    /// The figures, in report order.
    pub figures: Vec<ScienceFigure>,
    /// The figure shown.
    pub pick: usize,
}

/// Decode a PNG of the report into a texture.
fn texture(ctx: &egui::Context, name: &str, png: &[u8]) -> Option<(TextureHandle, [usize; 2])> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(png));
    dec.set_transformations(png::Transformations::EXPAND);
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    if info.color_type != png::ColorType::Rgb || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    let size = [info.width as usize, info.height as usize];
    let image = ColorImage::from_rgb(size, &buf[..info.buffer_size()]);
    Some((ctx.load_texture(name, image, TextureOptions::LINEAR), size))
}

impl App {
    /// Draw the science report of the stack on the screen, once.
    pub(crate) fn build_science(&mut self, ctx: &egui::Context) {
        let Some(out) = self.result.clone() else {
            return;
        };
        let cfg = self.settings.stack_config();
        let metric = format!("{:?}", cfg.metric);
        let mut extras = self.science_extras.clone().unwrap_or_default();
        let info = helia_core::io::solarnet::CaptureInfo::read(
            self.capture
                .as_ref()
                .map_or(std::path::Path::new(""), |c| c.path.as_path()),
            self.settings.preset,
        );
        extras.solar = helia_report::SolarInputs {
            when_unix: helia_core::io::solarnet::FitsMeta::from_stack(&info, &out, None)
                .date_avg
                .or_else(|| info.mid_unix()),
            celestial_north_up: self.settings.celestial_north_up,
            rotate_deg: 0.0,
            mirror: false,
            sunspots: self
                .sunspots
                .as_ref()
                .map(|d| (d.report.clone(), d.overlay.clone())),
        };
        let data = helia_report::ScienceData::from_output(&out, &metric, &extras);
        let name = self
            .capture
            .as_ref()
            .and_then(|c| c.path.file_name())
            .map_or_else(|| "stack".to_owned(), |n| n.to_string_lossy().into_owned());
        let meta = helia_report::html::Meta {
            title: name.clone(),
            inputs: vec![("capture".into(), name)],
            settings: helia_report::settings::from_config(&cfg, &out),
            generated: helia_core::io::solarnet::iso_from_unix(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0.0, |d| d.as_secs_f64()),
            ),
            version: helia_core::about::display_version(crate::about::ui_version()),
        };
        match helia_report::generate(&data, &meta, None, helia_report::canvas::Size::default()) {
            Ok(bundle) => {
                let figures = bundle
                    .figures()
                    .into_iter()
                    .filter_map(|(f, png)| {
                        let (tex, size) =
                            texture(ctx, &format!("science-{}-{}", self.run_counter, f.png), png)?;
                        Some(ScienceFigure {
                            label: f.title.clone(),
                            caption: f.caption.clone(),
                            tex,
                            size,
                        })
                    })
                    .collect();
                self.science = Some(ScienceView {
                    bundle: Arc::new(bundle),
                    figures,
                    pick: 0,
                });
            }
            Err(e) => self.toast(
                crate::activity::ToastKind::Error,
                format!("Could not draw the science plots: {e}"),
            ),
        }
    }

    /// The figure the viewer shows in place of the stack, when asked for.
    pub(crate) fn science_overlay(&self) -> Option<(TextureHandle, [usize; 2])> {
        let s = self.science.as_ref().filter(|_| self.show_science)?;
        s.figures.get(s.pick).map(|f| (f.tex.clone(), f.size))
    }

    /// "Science" in the Stack step: pick a plot to look at, and save the report.
    pub(crate) fn science_panel(&mut self, ui: &mut Ui) {
        let p = palette();
        ui.add_space(6.0);
        widgets::overline(ui, "Science");
        ui.add(
            Label::new(mono_text(
                "The graphs astrophotographers review: frame quality and the keep threshold, shifts and drift, seeing over the capture.",
                12.0,
                p.faint,
            ))
            .wrap(),
        );
        let idle = self.run.is_none();
        if widgets::chip(ui, "Residual pass", self.settings.ap_residual, idle)
            .on_hover_text(
                "Measure the alignment points again against the finished stack (one more pass on the next stack) for the residual plot",
            )
            .clicked()
            && idle
        {
            self.settings.ap_residual = !self.settings.ap_residual;
        }
        ui.horizontal_wrapped(|ui| {
            if widgets::chip(ui, "Show plots", self.show_science, true)
                .on_hover_text("Show the science plots in the viewer in place of the stack")
                .clicked()
            {
                self.show_science = !self.show_science;
                if self.show_science && self.science.is_none() {
                    let ctx = ui.ctx().clone();
                    self.build_science(&ctx);
                }
            }
            if widgets::chip(ui, "Save report", false, true)
                .on_hover_text(
                    "Write the plots (PNG), their data (CSV, JSON) and report.html into a folder",
                )
                .clicked()
            {
                if self.science.is_none() {
                    let ctx = ui.ctx().clone();
                    self.build_science(&ctx);
                }
                if let Some(s) = &self.science {
                    let stem = self
                        .capture
                        .as_ref()
                        .and_then(|c| c.path.file_stem())
                        .map_or_else(|| "stack".to_owned(), |s| s.to_string_lossy().into_owned());
                    self.workers.save_report(
                        Arc::clone(&s.bundle),
                        stem,
                        self.prefs.last_export_dir.clone(),
                    );
                }
            }
        });
        if !self.show_science {
            return;
        }
        let Some(s) = self.science.as_mut() else {
            return;
        };
        if s.figures.is_empty() {
            ui.add(Label::new(mono_text("Nothing to plot for this stack.", 12.5, p.muted)).wrap());
            return;
        }
        ui.horizontal_wrapped(|ui| {
            for (i, f) in s.figures.iter().enumerate() {
                if widgets::chip(ui, &f.label, s.pick == i, true).clicked() {
                    s.pick = i;
                }
            }
        });
        if let Some(f) = s.figures.get(s.pick) {
            ui.add(Label::new(mono_text(&f.caption, 12.0, p.muted)).wrap());
        }
    }

    /// Forget the report (a new stack, a new capture).
    pub(crate) fn clear_science(&mut self) {
        self.science = None;
        self.show_science = false;
    }
}
