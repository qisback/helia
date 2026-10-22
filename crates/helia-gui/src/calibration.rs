// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The Calibrate step: choose a dark, a flat and a bad-pixel map (master files
//! from `helia calibrate`, or the captures to make them from), see the frame
//! before and after, and save the masters.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use eframe::egui::{Align, Label, Layout, Ui};
use helia_core::calibrate::{Calibration, CalibrationSpec};
use helia_core::io::source::OpenOptions;

use crate::activity::ToastKind;
use crate::app::App;
use crate::model::CalBrief;
use crate::theme::palette;
use crate::widgets::{self, Icon, Kind, mono_text};
use crate::worker::CalReady;

/// A place for a file in the Calibrate step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    /// Master dark, or dark captures.
    Dark,
    /// Master flat, or flat captures.
    Flat,
    /// Darks at the exposure of the flats.
    FlatDark,
    /// Bad-pixel map.
    BadPixels,
}

impl Slot {
    /// Heading in the inspector.
    pub fn label(self) -> &'static str {
        match self {
            Self::Dark => "Dark",
            Self::Flat => "Flat",
            Self::FlatDark => "Dark for the flat",
            Self::BadPixels => "Bad pixels",
        }
    }

    /// What goes here.
    pub fn hint(self) -> &'static str {
        match self {
            Self::Dark => {
                "A master dark (FITS), or dark captures: the lens capped, the exposure and gain of the Sun."
            }
            Self::Flat => {
                "A master flat (FITS), or flat captures: an even field at the focus and set-up of the Sun."
            }
            Self::FlatDark => {
                "Darks at the exposure of the flats, when the flat is captures. Without them the flat keeps its offset."
            }
            Self::BadPixels => {
                "A bad-pixel map (FITS) or a list of x y pairs. Leave empty to find the hot and cold pixels from the dark and flat."
            }
        }
    }

    /// Does the dialog take a file of this kind only (not captures)?
    pub fn masters_only(self) -> bool {
        self == Self::BadPixels
    }
}

/// The user's choices.
#[derive(Clone, Debug, PartialEq)]
pub struct CalSettings {
    /// Master dark or its captures.
    pub dark: Option<PathBuf>,
    /// Master flat or its captures.
    pub flat: Option<PathBuf>,
    /// Darks for the flat.
    pub flat_dark: Option<PathBuf>,
    /// Bad-pixel map.
    pub bad_pixels: Option<PathBuf>,
    /// Find and correct hot and cold pixels when there is no map.
    pub cosmetic: bool,
    /// Scale the dark by the ratio of the exposures.
    pub scale_dark: bool,
}

impl Default for CalSettings {
    fn default() -> Self {
        Self {
            dark: None,
            flat: None,
            flat_dark: None,
            bad_pixels: None,
            cosmetic: true,
            scale_dark: false,
        }
    }
}

impl CalSettings {
    /// Anything chosen.
    pub fn any(&self) -> bool {
        self.dark.is_some() || self.flat.is_some() || self.bad_pixels.is_some()
    }

    /// The path in `slot`.
    pub fn path(&self, slot: Slot) -> Option<&PathBuf> {
        match slot {
            Slot::Dark => self.dark.as_ref(),
            Slot::Flat => self.flat.as_ref(),
            Slot::FlatDark => self.flat_dark.as_ref(),
            Slot::BadPixels => self.bad_pixels.as_ref(),
        }
    }

    /// Put `path` in `slot`.
    pub fn set(&mut self, slot: Slot, path: Option<PathBuf>) {
        match slot {
            Slot::Dark => self.dark = path,
            Slot::Flat => self.flat = path,
            Slot::FlatDark => self.flat_dark = path,
            Slot::BadPixels => self.bad_pixels = path,
        }
    }

    /// What to resolve.
    pub fn spec(&self, open: OpenOptions) -> CalibrationSpec {
        CalibrationSpec {
            dark: self.dark.clone(),
            flat: self.flat.clone(),
            dark_flat: self.flat_dark.clone(),
            bad_pixels: self.bad_pixels.clone(),
            cosmetic: self.cosmetic,
            scale_dark: self.scale_dark,
            open,
            ..CalibrationSpec::default()
        }
    }

    /// "Dark, flat, bad pixels" for the rail.
    pub fn name(&self) -> String {
        let mut parts = Vec::new();
        if self.dark.is_some() {
            parts.push("dark");
        }
        if self.flat.is_some() {
            parts.push("flat");
        }
        if self.bad_pixels.is_some() {
            parts.push("pixels");
        }
        let mut s = parts.join(", ");
        if let Some(first) = s.get(..1) {
            s = first.to_uppercase() + &s[1..];
        }
        s
    }
}

/// Where the masters stand.
#[derive(Clone, Debug, Default)]
pub enum CalState {
    /// None chosen.
    #[default]
    Off,
    /// Being built or read.
    Preparing,
    /// In force.
    Ready(Arc<CalReady>),
    /// They could not be used; the reason.
    Failed(String),
}

fn file_name(p: &std::path::Path) -> String {
    p.file_name().map_or_else(
        || p.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

impl App {
    /// The masters in force, for scoring and stacking.
    pub fn cal_ready(&self) -> Option<Arc<Calibration>> {
        match &self.cal {
            CalState::Ready(r) => Some(Arc::clone(&r.calibration)),
            _ => None,
        }
    }

    /// A new capture opened: check the masters against it, scoring after.
    pub fn cal_changed_for_capture(&mut self) {
        self.cal_changed_inner(false);
    }

    /// The choices changed: build the masters again.
    pub fn cal_changed(&mut self) {
        self.cal_changed_inner(true);
    }

    fn cal_changed_inner(&mut self, apply_when_off: bool) {
        if let Some(c) = self.cal_cancel.take() {
            c.store(true, Ordering::Relaxed);
        }
        self.cal_token += 1;
        if !self.settings.calibration.any() {
            let had = !matches!(self.cal, CalState::Off);
            self.cal = CalState::Off;
            if had && apply_when_off {
                self.cal_applied();
            }
            return;
        }
        self.cal = CalState::Preparing;
        let facts = self
            .capture
            .as_ref()
            .map(|c| (c.path.clone(), c.meta.width, c.meta.height, c.meta.bayer));
        self.cal_cancel = Some(self.workers.resolve_calibration(
            self.cal_token,
            self.settings.calibration.spec(self.settings.open_options()),
            facts,
        ));
    }

    /// The masters changed what every frame looks like: previews and scores
    /// are made again, and a stack made without them is out of date.
    fn cal_applied(&mut self) {
        self.invalidate_previews();
        self.start_scoring();
        if self.result.is_some() {
            self.toast(
                ToastKind::Info,
                "The calibration changed. Press Stack again to apply it to the result.",
            );
        }
    }

    pub(crate) fn on_cal_ready(&mut self, token: u64, res: Result<CalReady, String>) {
        if token != self.cal_token {
            return;
        }
        self.cal_cancel = None;
        match res {
            Ok(r) => {
                self.cal = CalState::Ready(Arc::new(r));
            }
            Err(e) if e.contains("cancel") => return,
            Err(e) => {
                self.toast(ToastKind::Error, format!("Calibration: {e}"));
                self.cal = CalState::Failed(e);
            }
        }
        self.cal_applied();
    }

    /// The Calibrate step's brief for the rail.
    pub fn cal_brief(&self) -> CalBrief {
        match &self.cal {
            CalState::Off => CalBrief::Off,
            CalState::Preparing => CalBrief::Preparing,
            CalState::Ready(_) => CalBrief::Ready(self.settings.calibration.name()),
            CalState::Failed(_) => CalBrief::Failed,
        }
    }

    /// One file slot: heading, the chosen name, and the buttons.
    fn cal_slot(&mut self, ui: &mut Ui, slot: Slot) {
        let p = palette();
        let usable = !self.busy();
        widgets::overline(ui, slot.label());
        let chosen = self.settings.calibration.path(slot).cloned();
        ui.horizontal(|ui| {
            let text = chosen
                .as_deref()
                .map_or_else(|| "None".to_owned(), file_name);
            let colour = if chosen.is_some() { p.text } else { p.faint };
            let hover = chosen
                .as_deref()
                .map_or_else(String::new, |c| c.display().to_string());
            ui.add(Label::new(mono_text(text, 12.5, colour)).truncate())
                .on_hover_text(hover);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if chosen.is_some()
                    && widgets::icon_button(ui, Icon::Minus, "Clear", usable, false).clicked()
                {
                    self.settings.calibration.set(slot, None);
                    self.cal_changed();
                }
                if !slot.masters_only()
                    && widgets::button(ui, "Folder", Kind::Outline, usable, 0.0)
                        .on_hover_text("Use a folder of calibration captures (SER, AVI or FITS)")
                        .clicked()
                {
                    self.workers.pick_cal_path(slot, true);
                }
                if widgets::button(ui, "File", Kind::Outline, usable, 0.0)
                    .on_hover_text(slot.hint())
                    .clicked()
                {
                    self.workers.pick_cal_path(slot, false);
                }
            });
        });
        ui.add(Label::new(mono_text(slot.hint(), 11.5, p.faint)).wrap());
    }

    pub(crate) fn calibrate_body(&mut self, ui: &mut Ui) {
        let p = palette();
        for slot in [Slot::Dark, Slot::Flat] {
            self.cal_slot(ui, slot);
            ui.add_space(4.0);
        }
        if self.settings.calibration.flat.is_some() {
            self.cal_slot(ui, Slot::FlatDark);
            ui.add_space(4.0);
        }
        self.cal_slot(ui, Slot::BadPixels);
        ui.add_space(6.0);

        let usable = !self.busy();
        let mut changed = false;
        changed |= widgets::toggle_row(
            ui,
            "Fix hot and cold pixels",
            "Use the median of the neighbours",
            &mut self.settings.calibration.cosmetic,
            usable,
        );
        changed |= widgets::toggle_row(
            ui,
            "Scale the dark by exposure",
            "Needs the exposure in the SharpCap file",
            &mut self.settings.calibration.scale_dark,
            usable,
        );
        if changed {
            self.cal_changed();
        }
        ui.add_space(6.0);

        match self.cal.clone() {
            CalState::Off => {
                ui.add(
                    Label::new(mono_text(
                        "No calibration. Capture darks with the lens capped, at the exposure and gain of the Sun (50 to 100 frames), and flats of an even field at the same focus and set-up (20 to 50 frames) with darks at the flats' exposure.",
                        12.0,
                        p.muted,
                    ))
                    .wrap(),
                );
            }
            CalState::Preparing => {
                ui.add(Label::new(mono_text(
                    "Preparing the masters\u{2026}",
                    12.5,
                    p.accent,
                )));
            }
            CalState::Ready(r) => {
                widgets::overline(ui, "In force");
                for l in &r.lines {
                    ui.add(Label::new(mono_text(l.clone(), 12.0, p.muted)).wrap());
                }
                for w in &r.warnings {
                    ui.add_space(2.0);
                    ui.add(Label::new(mono_text(format!("! {w}"), 12.0, p.accent)).wrap());
                }
                ui.add_space(6.0);
                if widgets::button(ui, "Save the masters", Kind::Outline, usable, ui.available_width())
                    .on_hover_text(
                        "Write master-dark.fits, master-flat.fits and bad-pixels.fits to a folder, to use again",
                    )
                    .clicked()
                {
                    self.workers.save_masters(Arc::clone(&r.calibration));
                }
                ui.add_space(4.0);
                ui.add(
                    Label::new(mono_text(
                        "The picture shows the frame before (left) and after. Every frame is calibrated before it is scored, aligned and stacked.",
                        12.0,
                        p.faint,
                    ))
                    .wrap(),
                );
            }
            CalState::Failed(e) => {
                ui.add(Label::new(mono_text(e, 12.0, p.error_text)).wrap());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_name_what_is_chosen() {
        let mut s = CalSettings::default();
        assert!(!s.any());
        assert_eq!(s.name(), "");
        s.set(Slot::Dark, Some(PathBuf::from("a/master-dark.fits")));
        assert!(s.any());
        assert_eq!(s.name(), "Dark");
        s.set(Slot::Flat, Some(PathBuf::from("flats")));
        assert_eq!(s.name(), "Dark, flat");
        s.set(Slot::BadPixels, Some(PathBuf::from("bad.txt")));
        assert_eq!(s.name(), "Dark, flat, pixels");
        assert_eq!(
            s.path(Slot::Flat).map(PathBuf::as_path),
            Some(std::path::Path::new("flats"))
        );
        s.set(Slot::Dark, None);
        assert_eq!(s.name(), "Flat, pixels");
    }

    #[test]
    fn the_spec_carries_every_choice() {
        let s = CalSettings {
            dark: Some(PathBuf::from("d")),
            flat: Some(PathBuf::from("f")),
            flat_dark: Some(PathBuf::from("fd")),
            bad_pixels: Some(PathBuf::from("b")),
            cosmetic: false,
            scale_dark: true,
        };
        let spec = s.spec(OpenOptions::default());
        assert_eq!(spec.dark, Some(PathBuf::from("d")));
        assert_eq!(spec.dark_flat, Some(PathBuf::from("fd")));
        assert_eq!(spec.bad_pixels, Some(PathBuf::from("b")));
        assert!(!spec.cosmetic && spec.scale_dark);
    }

    #[test]
    fn only_a_map_is_limited_to_files() {
        assert!(Slot::BadPixels.masters_only());
        assert!(!Slot::Dark.masters_only());
        for slot in [Slot::Dark, Slot::Flat, Slot::FlatDark, Slot::BadPixels] {
            assert!(!slot.label().is_empty() && !slot.hint().is_empty());
        }
    }
}
