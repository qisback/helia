// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The Open step's account of how a capture was read: what the file's header
//! says, what Helia uses instead and why, what to read about it, and the
//! overrides. The words come from `helia_core::io::capture::CaptureReport`,
//! the same report `helia info` and the API print.

use eframe::egui::{Align, CornerRadius, Frame, Label, Layout, Margin, RichText, Stroke, Ui};
use helia_core::BayerPattern;
use helia_core::io::capture::{Level, Origin};
use helia_core::io::source::{BayerSetting, Endian};

use crate::app::App;
use crate::theme::{RADIUS_LARGE, palette, semibold};
use crate::widgets::{self, mono_text};

/// One label and value on a line, the value right-aligned.
fn row(ui: &mut Ui, label: &str, value: &str, tail: Option<(&str, bool)>) {
    let p = palette();
    ui.horizontal(|ui| {
        ui.add(Label::new(mono_text(label, 11.5, p.faint)));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Some((text, standout)) = tail {
                ui.add(Label::new(mono_text(
                    text,
                    11.5,
                    if standout { p.accent } else { p.faint },
                )));
            }
            ui.add(Label::new(mono_text(value, 11.5, p.text)).truncate());
        });
    });
}

impl App {
    /// Re-read the open capture with the settings as they now are.
    fn reopen(&mut self) {
        if let Some(path) = self.capture.as_ref().map(|c| c.path.clone()) {
            self.begin_open(path);
        }
    }

    /// What to read about how the file was understood, above everything else.
    pub(crate) fn capture_notices(&self, ui: &mut Ui) {
        let p = palette();
        let Some(cap) = &self.capture else { return };
        for n in &cap.report.notices {
            let warning = n.level == Level::Warning;
            let (fill, border) = if warning {
                (p.selected_fill, p.selected_border)
            } else {
                (p.raised, p.border)
            };
            Frame::new()
                .fill(fill)
                .stroke(Stroke::new(1.0, border))
                .corner_radius(CornerRadius::same(RADIUS_LARGE))
                .inner_margin(Margin::same(14))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    if warning {
                        ui.add(Label::new(
                            RichText::new("Check this")
                                .font(semibold(14.0))
                                .color(p.accent),
                        ));
                    }
                    ui.add(
                        Label::new(RichText::new(&n.text).color(if warning {
                            p.text
                        } else {
                            p.muted
                        }))
                        .wrap(),
                    );
                });
            ui.add_space(8.0);
        }
    }

    /// The values in force and where each came from, then every header field as written.
    pub(crate) fn header_cards(&self, ui: &mut Ui) {
        let p = palette();
        let Some(cap) = &self.capture else { return };
        let report = &cap.report;
        let card = |ui: &mut Ui, title: &str, body: &mut dyn FnMut(&mut Ui)| {
            Frame::new()
                .fill(p.raised)
                .stroke(Stroke::new(1.0, p.border))
                .corner_radius(CornerRadius::same(RADIUS_LARGE))
                .inner_margin(Margin::same(14))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    widgets::overline(ui, title);
                    body(ui);
                });
            ui.add_space(8.0);
        };
        card(ui, "Helia will use", &mut |ui| {
            for (label, value, origin) in report.effective_rows() {
                row(
                    ui,
                    label,
                    &value,
                    Some((origin.label(), origin != Origin::Header)),
                );
            }
        });
        let title = format!("{} header, as written in the file", report.container);
        card(ui, &title, &mut |ui| {
            for (label, value) in report.header_rows() {
                row(ui, &label, &value, None);
            }
        });
    }

    /// Say what the file gets wrong. A change reopens the capture.
    pub(crate) fn override_controls(&mut self, ui: &mut Ui) {
        let is_ser = self
            .capture
            .as_ref()
            .is_some_and(|c| c.meta.container == "SER");
        let idle = !self.busy();
        let before = (
            self.settings.bayer,
            self.settings.bit_depth,
            self.settings.endian,
            self.settings.flip_rows,
        );

        widgets::overline(ui, "Bayer pattern");
        ui.horizontal_wrapped(|ui| {
            let choices = [
                ("Auto", BayerSetting::Auto),
                ("Mono", BayerSetting::Mono),
                ("RGGB", BayerSetting::Pattern(BayerPattern::Rggb)),
                ("GRBG", BayerSetting::Pattern(BayerPattern::Grbg)),
                ("GBRG", BayerSetting::Pattern(BayerPattern::Gbrg)),
                ("BGGR", BayerSetting::Pattern(BayerPattern::Bggr)),
            ];
            for (label, value) in choices {
                if widgets::chip(ui, label, self.settings.bayer == value, idle)
                    .on_hover_text(match value {
                        BayerSetting::Auto => {
                            "Trust the header, but check a mono-labelled SER file for a hidden mosaic"
                        }
                        BayerSetting::Mono => "Treat the frames as mono, whatever the header says",
                        BayerSetting::Pattern(_) => {
                            "Debayer as this pattern, whatever the header says"
                        }
                    })
                    .clicked()
                {
                    self.settings.bayer = value;
                }
            }
        });

        widgets::overline(ui, "Bit depth");
        ui.horizontal_wrapped(|ui| {
            for (label, value) in [("Auto", None), ("8-bit", Some(8)), ("16-bit", Some(16))] {
                if widgets::chip(ui, label, self.settings.bit_depth == value, idle && is_ser)
                    .on_hover_text("Use this instead of the SER header's bit depth")
                    .clicked()
                {
                    self.settings.bit_depth = value;
                }
            }
        });

        widgets::overline(ui, "Byte order of 16-bit pixels");
        ui.horizontal_wrapped(|ui| {
            for (label, value) in [
                ("Auto", None),
                ("Little-endian", Some(Endian::Little)),
                ("Big-endian", Some(Endian::Big)),
            ] {
                if widgets::chip(ui, label, self.settings.endian == value, idle && is_ser)
                    .on_hover_text("Use this instead of the SER header flag")
                    .clicked()
                {
                    self.settings.endian = value;
                }
            }
        });

        widgets::overline(ui, "Row order");
        ui.horizontal_wrapped(|ui| {
            for (label, value) in [("As stored", false), ("Bottom to top", true)] {
                if widgets::chip(ui, label, self.settings.flip_rows == value, idle && is_ser)
                    .on_hover_text("Read the rows upside down, for a file stored flipped")
                    .clicked()
                {
                    self.settings.flip_rows = value;
                }
            }
        });
        if !is_ser && self.capture.is_some() {
            ui.add(
                Label::new(mono_text(
                    "Bit depth, byte order and row order apply to SER files.",
                    11.5,
                    palette().faint,
                ))
                .wrap(),
            );
        }

        let after = (
            self.settings.bayer,
            self.settings.bit_depth,
            self.settings.endian,
            self.settings.flip_rows,
        );
        if after != before {
            self.reopen();
        }
    }
}
