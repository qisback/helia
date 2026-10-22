// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `--shots <dir>`: scripted run that opens a capture, stacks it and saves
//! window screenshots. A debugging aid, not part of the normal UI.

use std::path::PathBuf;

use eframe::egui::{self, Event, UserData, ViewportCommand};

use crate::app::App;
use crate::model::{Scoring, Step};

/// Scripted screenshot run.
pub struct Demo {
    dir: PathBuf,
    phase: Phase,
    frames: u32,
    waiting: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    WaitReady,
    ShotOpen,
    StartStack,
    ShotStacking,
    WaitResult,
    ShotResult,
    ShotExport,
    Quit,
}

impl Demo {
    /// Write screenshots into `dir`.
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        Self {
            dir,
            phase: Phase::WaitReady,
            frames: 0,
            waiting: None,
        }
    }

    fn shoot(&mut self, ctx: &egui::Context, name: &str) {
        ctx.send_viewport_cmd(ViewportCommand::Screenshot(UserData::new(name.to_string())));
        self.waiting = Some(name.to_string());
    }

    fn collect(&mut self, ctx: &egui::Context) {
        let shots: Vec<_> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    Event::Screenshot {
                        user_data, image, ..
                    } => user_data
                        .data
                        .as_ref()
                        .and_then(|d| d.downcast_ref::<String>())
                        .map(|n| (n.clone(), image.clone())),
                    _ => None,
                })
                .collect()
        });
        for (name, img) in shots {
            let path = self.dir.join(format!("{name}.png"));
            match write_png(&path, &img) {
                Ok(()) => eprintln!("screenshot {}", path.display()),
                Err(e) => eprintln!("screenshot {} failed: {e}", path.display()),
            }
            if self.waiting.as_deref() == Some(name.as_str()) {
                self.waiting = None;
            }
        }
    }

    /// Advance the script by one frame.
    pub fn drive(&mut self, app: &mut App, ctx: &egui::Context) {
        ctx.request_repaint();
        self.collect(ctx);
        if self.waiting.is_some() {
            return;
        }
        self.frames += 1;
        match self.phase {
            Phase::WaitReady => {
                let ready = app.capture.is_some()
                    && app.scoring == Scoring::Done
                    && app.display_grid().is_some();
                if ready {
                    self.frames = 0;
                    self.phase = Phase::ShotOpen;
                }
            }
            Phase::ShotOpen if self.frames > 30 => {
                self.shoot(ctx, "01-open-align");
                self.phase = Phase::StartStack;
            }
            Phase::StartStack => {
                app.select(Step::Stack);
                app.start_stack();
                self.frames = 0;
                self.phase = Phase::ShotStacking;
            }
            Phase::ShotStacking => {
                let mid = app.run.as_ref().is_some_and(|r| {
                    r.fraction > 0.3 && r.stage != helia_core::pipeline::Stage::Quality
                });
                if mid || app.result.is_some() || self.frames > 600 {
                    self.shoot(ctx, "02-stacking");
                    self.phase = Phase::WaitResult;
                }
            }
            Phase::WaitResult if app.result.is_some() => {
                self.frames = 0;
                self.phase = Phase::ShotResult;
            }
            Phase::ShotResult if self.frames > 30 => {
                self.shoot(ctx, "03-result");
                self.phase = Phase::ShotExport;
            }
            Phase::ShotExport => {
                app.select(Step::Export);
                if self.frames > 40 {
                    self.shoot(ctx, "04-export");
                    self.phase = Phase::Quit;
                }
            }
            Phase::Quit => ctx.send_viewport_cmd(ViewportCommand::Close),
            _ => {}
        }
    }
}

fn write_png(path: &std::path::Path, img: &egui::ColorImage) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(
        std::io::BufWriter::new(file),
        img.size[0] as u32,
        img.size[1] as u32,
    );
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().map_err(|e| e.to_string())?;
    let bytes: Vec<u8> = img
        .pixels
        .iter()
        .flat_map(egui::Color32::to_array)
        .collect();
    w.write_image_data(&bytes).map_err(|e| e.to_string())
}
