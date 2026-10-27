// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `helia-gui`: egui front end over `helia-core` and `helia-gpu`.
//!
//! All image maths lives in the core crate. The GUI drives the pipeline on
//! worker threads ([`worker`]), keeps its decisions in a testable view model
//! ([`model`]) and draws with the tokens in [`theme`].
//!
//! `helia-gui [capture]` opens `capture` on start, so a file manager's "Open
//! with" works. The window size and the recent captures are remembered between
//! runs (see [`prefs`]).
//!
//! Debug flags: `--open <capture>` opens a file on start; `--shots <dir>` also
//! runs a scripted stack and writes window screenshots (see [`demo`]).
//! `-V` prints the name and version, `--version` adds the copyright and licence
//! notices; both exit without opening a window, which is how the release
//! workflow checks a packaged binary starts.

// A double-clicked helia-gui.exe must not open a console window. The test harness
// is a console program, so it keeps the default subsystem.
#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]
#![allow(
    clippy::struct_excessive_bools,
    reason = "UI state is a flat set of independent toggles"
)]
#![allow(
    clippy::too_many_lines,
    reason = "immediate-mode drawing reads best top to bottom"
)]
#![allow(
    clippy::match_same_arms,
    reason = "stale-event arms are kept explicit beside their guarded siblings"
)]

mod about;
mod activity;
mod app;
mod branding;
mod calibration;
mod capture_view;
mod centre;
mod console;
mod curve;
mod curve_edit;
mod demo;
mod editors;
mod edits;
mod feedback;
mod inspector;
mod model;
mod mosaic;
mod notices;
#[cfg(test)]
mod plugin_shots;
mod plugin_steps;
mod plugins_page;
mod prefs;
#[cfg(test)]
mod real_shots;
mod science;
#[cfg(test)]
mod snapshots;
// Screenshots of the official store build's settings block: the file is added by that build.
#[cfg(all(test, feature = "store"))]
mod store_shots;
mod theme;
mod timelapse;
#[cfg(test)]
mod tools_shots;
mod tools_ui;
mod topbar;
mod update_ui;
mod widgets;
mod worker;

use std::path::PathBuf;

use eframe::egui;

fn main() -> eframe::Result {
    // An official store build plugs in a backend at start-up; a source build has none.
    #[cfg(feature = "store")]
    store_backend::install();
    let mut open: Option<PathBuf> = None;
    let mut shots: Option<PathBuf> = None;
    let mut size = prefs::Prefs::load_default()
        .window
        .unwrap_or([1360.0, 860.0]);
    let mut plugin_paths: Vec<PathBuf> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-V" => {
                console::attach();
                println!("helia-gui {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--version" => {
                console::attach();
                let version = env!("CARGO_PKG_VERSION");
                println!(
                    "{}",
                    helia_core::about::long_version("helia-gui", version, helia_gpu::WGPU_VERSION)
                );
                if let Some(line) = helia_update::backend::installed()
                    .and_then(helia_update::backend::UpdateBackend::build_line)
                {
                    println!("{line}");
                }
                return Ok(());
            }
            "--open" => open = args.next().map(PathBuf::from),
            "--plugin" => plugin_paths.extend(args.next().map(PathBuf::from)),
            other if other.starts_with("--plugin=") => {
                plugin_paths.push(PathBuf::from(&other["--plugin=".len()..]));
            }
            "--shots" => shots = args.next().map(PathBuf::from),
            "--size" => {
                if let Some((w, h)) = args.next().as_deref().and_then(|s| s.split_once('x'))
                    && let (Ok(w), Ok(h)) = (w.parse(), h.parse())
                {
                    size = [w, h];
                }
            }
            other if !other.starts_with('-') => open = Some(PathBuf::from(other)),
            _ => {}
        }
    }
    plugins_page::set_cli_paths(plugin_paths);
    helia_core::io::ffmpeg::set_installed_locator(tools_ui::installed_programs);
    helia_core::applog::init("helia-gui");
    helia_core::applog::info(&format!(
        "started {}",
        helia_core::about::display_version(env!("CARGO_PKG_VERSION"))
    ));
    if shots.is_none() {
        // Counts this start for the rollback check; may restart the previous version and exit.
        update_ui::startup();
    }
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size(size)
        .with_min_inner_size([640.0, 480.0])
        .with_title(helia_core::about::window_title(env!("CARGO_PKG_VERSION")))
        .with_app_id("helia");
    if let Some(icon) = branding::window_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..eframe::NativeOptions::default()
    };
    eframe::run_native(
        "Helia",
        options,
        Box::new(move |cc| {
            let demo = shots.map(demo::Demo::new);
            Ok(Box::new(app::App::new(cc, open, demo)))
        }),
    )
}
