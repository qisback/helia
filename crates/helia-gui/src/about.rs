// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The About page: dedication, version and build record, copyright, licence,
//! links and third-party licences. It replaces the viewer while open (F1, the
//! sun in the top bar, or Esc to close). Nothing here touches the network; a
//! link is only opened, in the system browser, when it is clicked.

use std::path::PathBuf;

use eframe::egui::{self, Frame, Margin, RichText, ScrollArea, Stroke, Ui, vec2};
use helia_core::about;

use crate::app::App;
use crate::branding;
use crate::theme::{RADIUS_LARGE, mono, palette, semibold};
use crate::widgets::{self, Kind};
use crate::worker::gpu_chip;

/// The version the GUI shows. Under `cargo test` it is a fixed string, so a
/// release bump never changes a snapshot (the footer and About heading would
/// otherwise move about 40 pixels and break the Windows tolerance).
#[must_use]
pub(crate) fn ui_version() -> &'static str {
    if cfg!(test) {
        "0.7.0"
    } else {
        env!("CARGO_PKG_VERSION")
    }
}

/// Where the third-party licence list ships: next to the executable.
const THIRD_PARTY_FILE: &str = "THIRD-PARTY-LICENSES.txt";

fn third_party_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let p = exe.parent()?.join(THIRD_PARTY_FILE);
    p.is_file().then_some(p)
}

impl App {
    /// The adapter or backend in use, for the version block.
    pub(crate) fn gpu_description(&self) -> String {
        let (api, name, gpu) = gpu_chip(self.settings.backend, self.gpu.as_ref());
        if gpu { format!("{api}: {name}") } else { api }
    }

    /// The text the "Copy version info" button puts on the clipboard.
    pub(crate) fn version_report(&self) -> String {
        if let Some(text) = &self.version_override {
            return text.clone();
        }
        about::version_block(
            "helia-gui",
            ui_version(),
            helia_gpu::WGPU_VERSION,
            Some(&self.gpu_description()),
        )
    }

    /// Draw the page in `ui` (the centre panel).
    pub(crate) fn about_page(&mut self, ui: &mut Ui) {
        let p = palette();
        let report = self.version_report();
        let mut close = false;
        let mut feedback = false;
        let mut bug_report = false;
        ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_max_width(680.0);
                ui.spacing_mut().item_spacing.y = 10.0;
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("About Helia")
                            .font(semibold(22.0))
                            .color(p.text),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if widgets::button(ui, "Close", Kind::Outline, true, 0.0)
                            .on_hover_text("Back to the viewer (Esc)")
                            .clicked()
                        {
                            close = true;
                        }
                    });
                });
                ui.add_space(6.0);

                // The dedication comes first.
                Frame::new()
                    .fill(p.panel)
                    .stroke(Stroke::new(1.0, p.border))
                    .corner_radius(RADIUS_LARGE)
                    .inner_margin(Margin::same(24))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        if let Some(tex) = branding::texture(ui.ctx(), branding::Image::Logo) {
                            ui.add(
                                egui::Image::new(&tex)
                                    .fit_to_exact_size(vec2(280.0, 280.0))
                                    .corner_radius(RADIUS_LARGE),
                            )
                            .widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::WidgetType::Other,
                                    true,
                                    "Helia logo",
                                )
                            });
                            ui.add_space(12.0);
                        }
                        widgets::overline(ui, "Dedication");
                        ui.add(
                            egui::Label::new(
                                RichText::new(about::DEDICATION_FULL)
                                    .font(semibold(17.0))
                                    .color(p.text),
                            )
                            .wrap(),
                        );
                    });

                ui.add_space(6.0);
                widgets::overline(ui, "Version");
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("Helia {}", ui_version()))
                            .font(semibold(18.0))
                            .color(p.text),
                    );
                    if about::PRE_RELEASE {
                        widgets::badge(
                            ui,
                            "Pre-release",
                            "Helia is below version 1.0.0: it works, but expect rough edges.",
                        );
                    }
                });
                Frame::new()
                    .fill(p.panel)
                    .stroke(Stroke::new(1.0, p.border))
                    .corner_radius(RADIUS_LARGE)
                    .inner_margin(Margin::same(14))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.add(
                            egui::Label::new(
                                RichText::new(&report).font(mono(12.5)).color(p.muted),
                            )
                            .selectable(true),
                        );
                    });
                if widgets::button(ui, "Copy version info", Kind::Outline, true, 0.0)
                    .on_hover_text("Copy the lines above for a bug report")
                    .clicked()
                {
                    ui.ctx().copy_text(report.clone());
                }
                if widgets::button(ui, "Save bug report", Kind::Outline, true, 0.0)
                    .on_hover_text(
                        "A .zip of text to attach to an issue: version, GPU, capture header, \
                         settings and the end of the log. No images; nothing is sent.",
                    )
                    .clicked()
                {
                    bug_report = true;
                }
                if widgets::button(ui, "Send feedback", Kind::Outline, true, 0.0)
                    .on_hover_text("Report a bug or ask for a feature (Ctrl+Shift+F)")
                    .clicked()
                {
                    feedback = true;
                }

                ui.add_space(6.0);
                widgets::overline(ui, "Copyright and licence");
                ui.label(
                    RichText::new(about::COPYRIGHT)
                        .font(mono(13.0))
                        .color(p.text),
                );
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(format!("More from {}:", about::AUTHOR)).color(p.muted));
                    ui.hyperlink_to("12hobbies.studio", about::HOBBIES_URL);
                });
                ui.label(
                    RichText::new(format!("Licence: {}", about::LICENCE_ID))
                        .font(mono(13.0))
                        .color(p.muted),
                );
                ui.label(RichText::new(about::LICENCE_NOTICE).color(p.text));
                ui.label(RichText::new(about::WARRANTY_NOTICE).color(p.muted));
                ui.label(RichText::new(about::ADDITIONAL_TERMS_NOTICE).color(p.muted));
                ui.hyperlink_to("Additional terms", about::ADDITIONAL_TERMS_URL);

                ui.add_space(6.0);
                widgets::overline(ui, "Links");
                ui.horizontal_wrapped(|ui| {
                    ui.hyperlink_to("Source code", about::SOURCE_URL);
                    ui.hyperlink_to("Licence text", about::LICENCE_URL);
                    ui.hyperlink_to("Releases", format!("{}/releases", about::SOURCE_URL));
                });

                ui.add_space(6.0);
                widgets::overline(ui, "Support Helia");
                ui.label(
                    RichText::new(
                        "Helia is free. If it saves you time, you can buy the author a coffee.",
                    )
                    .color(p.muted),
                );
                if widgets::button(ui, "Support on Ko-fi", Kind::Outline, true, 0.0)
                    .on_hover_text(format!("Opens {} in your browser", about::KOFI_URL))
                    .clicked()
                {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(about::KOFI_URL));
                }

                ui.add_space(6.0);
                widgets::overline(ui, "Third-party licences");
                self.third_party(ui);
            });
        if close {
            self.show_about = false;
        }
        if bug_report {
            self.save_bug_report();
        }
        if feedback {
            let ctx = ui.ctx().clone();
            self.begin_feedback(&ctx);
        }
    }

    fn third_party(&mut self, ui: &mut Ui) {
        let p = palette();
        if let Some(path) = third_party_path() {
            let label = if self.show_third_party {
                "Hide third-party licences"
            } else {
                "View third-party licences"
            };
            if widgets::button(ui, label, Kind::Outline, true, 0.0).clicked() {
                self.show_third_party = !self.show_third_party;
                self.third_party_text = None;
            }
            if self.show_third_party {
                let text = self.third_party_text.get_or_insert_with(|| {
                    std::fs::read_to_string(&path)
                        .unwrap_or_else(|e| format!("Cannot read {}: {e}", path.display()))
                });
                ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(text.as_str()).font(mono(11.5)).color(p.muted),
                        )
                        .selectable(true),
                    );
                });
            }
        } else {
            ui.label(
                RichText::new(format!(
                    "The list of third-party licences ships as {THIRD_PARTY_FILE} next to the \
                     program in release archives. This build has none beside it."
                ))
                .color(p.muted),
            );
            ui.hyperlink_to("Releases", format!("{}/releases", about::SOURCE_URL));
        }
    }
}
