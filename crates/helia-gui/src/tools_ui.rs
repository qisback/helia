// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Optional tools in the window: the offer to install ffmpeg, the licence page, progress, and the
//! "Optional tools" block of the About page.
//!
//! The work is the shared `helia-external` flow; this file runs it on worker threads and keeps the
//! result in [`ToolsState`]. Nothing is fetched until the user opens the page and presses
//! **Accept and install**: opening the page fetches only the signed list of tools (to show the
//! licence), never the tool. The licence is shown in full first; declining closes the page and
//! changes nothing.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui::{self, Frame, Margin, ProgressBar, RichText, ScrollArea, Stroke, Ui};
use helia_external::{Acceptances, Client, Hooks, Manifest, Offer, Phase, Progress, Route};
use helia_update::ureq_http::UreqHttp;
use helia_update::{Platform, StdFs, SystemClock};

use crate::activity::ToastKind;
use crate::app::App;
use crate::theme::{RADIUS_LARGE, mono, palette, semibold};
use crate::widgets::{self, Kind};

/// The item this window installs.
const ITEM: &str = "ffmpeg";

/// What the user is shown before accepting.
#[derive(Clone, Debug)]
pub struct OfferView {
    /// Display name.
    pub name: String,
    /// Version offered.
    pub version: String,
    /// What it is for.
    pub description: String,
    /// Who provides this build.
    pub provider: String,
    /// "131.5 MB".
    pub size_text: String,
    /// Where it is downloaded from (host names).
    pub hosts: String,
    /// The licence's name.
    pub licence_name: String,
    /// The whole licence text.
    pub licence_text: String,
    /// Where the provider publishes the licence and source.
    pub source_url: String,
    /// When the user accepted exactly this text before, if they did.
    pub accepted_on: Option<String>,
    /// The package manager command, shown to copy (never run from the window).
    pub package_command: Option<String>,
}

/// What the page shows.
#[derive(Clone, Debug)]
pub enum ToolsState {
    /// Nothing open.
    Idle,
    /// The signed list is being fetched.
    Loading,
    /// The licence is shown and waits for an answer.
    Offer(Box<OfferView>),
    /// The download is running.
    Installing {
        /// What is happening.
        label: String,
        /// Bytes so far.
        done: u64,
        /// Bytes in this part.
        total: u64,
    },
    /// Installed.
    Done {
        /// What to tell the user.
        message: String,
        /// The attribution the licence asks to be shown.
        attribution: Option<String>,
    },
    /// It did not work; the text says what, why and what to do.
    Failed(String),
}

/// One installed tool, for the About page.
#[derive(Clone, Debug)]
pub struct InstalledLine {
    /// Display name.
    pub name: String,
    /// Version.
    pub version: String,
    /// How it was installed: download or package manager.
    pub route: String,
    /// Provider.
    pub provider: String,
    /// Licence name.
    pub licence_name: String,
    /// Attribution to show, if the licence needs it.
    pub attribution: Option<String>,
    /// Anything wrong with the files, in plain words.
    pub problems: Vec<String>,
}

enum Msg {
    Offer(Box<Result<(OfferView, Manifest), String>>),
    Progress(Progress),
    Installed(Result<(String, Option<String>), String>),
    Removed(Result<String, String>),
}

/// State of the tools page and the About block.
pub struct ToolsUi {
    /// The page is open (it replaces the viewer).
    pub open: bool,
    /// What it shows.
    pub state: ToolsState,
    /// Why the page was opened, in plain words (a video needed ffmpeg), if it was not the user's own click.
    pub reason: Option<String>,
    /// What is installed with Helia.
    pub installed: Vec<InstalledLine>,
    /// Where Helia finds ffmpeg now, in plain words.
    pub found: Option<String>,
    loaded: bool,
    active: bool,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    ctx: Option<egui::Context>,
    manifest: Option<Manifest>,
    cancel: Arc<AtomicBool>,
}

/// The programs installed with Helia, for `set_installed_locator`.
#[must_use]
pub fn installed_programs() -> helia_core::io::ffmpeg::InstalledPrograms {
    let (ffmpeg, ffprobe) = helia_external::helia::installed_ffmpeg();
    helia_core::io::ffmpeg::InstalledPrograms { ffmpeg, ffprobe }
}

/// Run `f` with a client for Helia's source.
fn with_client<R>(f: impl FnOnce(&Client<'_>) -> R) -> Result<R, String> {
    let source = helia_external::helia::source();
    let dirs = helia_external::helia::dirs().ok_or_else(|| {
        "I could not work out where to keep optional tools, so they are off for now.".to_string()
    })?;
    let http = UreqHttp::new(source.allow_loopback_http);
    let clock = SystemClock;
    let client = Client {
        http: &http,
        fs: &StdFs,
        clock: &clock,
        source: &source,
        dirs: &dirs,
    };
    Ok(f(&client))
}

fn date_text(secs: u64) -> String {
    helia_update::time::format_utc(secs)
}

fn size_text(bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss, reason = "a display figure")]
    let mb = bytes as f64 / (1024.0 * 1024.0);
    format!("{mb:.1} MB")
}

/// Is `program` on the `PATH`?
fn on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|d| {
            d.join(program).is_file()
                || (cfg!(windows) && d.join(format!("{program}.exe")).is_file())
        })
    })
}

/// The download offer for this system, and a package manager command that could be used instead.
fn pick<'m>(offers: &'m [Offer<'m>]) -> Option<(&'m Offer<'m>, Option<String>)> {
    let download = offers
        .iter()
        .find(|o| matches!(o.route, Route::Download(_)))?;
    let package = offers.iter().find_map(|o| match &o.route {
        Route::Package(c)
            if on_path(&c.manager)
                && (c.argv.first().map(String::as_str) != Some("sudo") || on_path("sudo")) =>
        {
            Some(c.display())
        }
        _ => None,
    });
    Some((download, package))
}

fn view_of(offer: &Offer<'_>, accepted: &Acceptances, package: Option<String>) -> OfferView {
    let hosts = match &offer.route {
        Route::Download(parts) => {
            let mut h: Vec<String> = parts
                .iter()
                .filter_map(|d| helia_update::manifest::url_host(&d.url))
                .collect();
            h.dedup();
            h.join(", ")
        }
        Route::Package(_) => String::new(),
    };
    OfferView {
        name: offer.item.name.clone(),
        version: offer.item.version.clone(),
        description: offer.item.description.clone(),
        provider: offer.provider().to_string(),
        size_text: size_text(offer.download_bytes()),
        hosts,
        licence_name: offer.licence.name.clone(),
        licence_text: offer.licence.text.clone(),
        source_url: offer.licence.source_url.clone(),
        accepted_on: accepted
            .records
            .iter()
            .find(|r| r.item == offer.item.id && r.licence_sha256 == offer.licence.sha256)
            .map(|r| date_text(r.accepted_at)),
        package_command: package,
    }
}

impl ToolsUi {
    /// A live instance (`active`) or an inert one for tests and demos.
    #[must_use]
    pub fn new(ctx: &egui::Context, active: bool) -> Self {
        let (tx, rx) = channel();
        Self {
            open: false,
            state: ToolsState::Idle,
            reason: None,
            installed: Vec::new(),
            found: None,
            loaded: !active,
            active,
            tx,
            rx,
            ctx: active.then(|| ctx.clone()),
            manifest: None,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    /// An inert instance that is drawn, showing `state` (screenshots).
    #[must_use]
    #[allow(dead_code, reason = "for the screenshots")]
    pub fn showing(state: ToolsState) -> Self {
        let mut u = Self::new(&egui::Context::default(), false);
        u.open = true;
        u.state = state;
        u
    }

    fn spawn(&self, f: impl FnOnce(&dyn Fn(Msg)) + Send + 'static) {
        if !self.active {
            return;
        }
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let send = |m: Msg| {
                let _ = tx.send(m);
                if let Some(c) = &ctx {
                    c.request_repaint();
                }
            };
            f(&send);
        });
    }

    /// Open the page and fetch the signed list, so the licence can be shown. Nothing else is fetched.
    pub fn begin(&mut self, reason: Option<String>) {
        self.open = true;
        self.reason = reason;
        if matches!(self.state, ToolsState::Installing { .. }) {
            return;
        }
        self.state = ToolsState::Loading;
        self.spawn(|send| {
            let r = with_client(|c| -> Result<(OfferView, Manifest), String> {
                let manifest = c.fetch_manifest().map_err(|e| e.to_string())?;
                let view = {
                    let offers = manifest
                        .offers(ITEM, Platform::current())
                        .map_err(|e| e.to_string())?;
                    let (offer, package) = pick(&offers).ok_or_else(|| {
                        "A download of ffmpeg is not offered for this system. Install it yourself and set HELIA_FFMPEG and HELIA_FFPROBE to the programs.".to_string()
                    })?;
                    let accepted =
                        Acceptances::load(c.fs, &c.dirs.accepted).map_err(|e| e.to_string())?;
                    view_of(offer, &accepted, package)
                };
                Ok((view, manifest))
            })
            .and_then(|r| r);
            send(Msg::Offer(Box::new(r)));
        });
    }

    /// The user pressed **Accept and install**. Records the acceptance of the licence text that was
    /// shown, then downloads, checks and unpacks.
    pub fn accept_and_install(&mut self) {
        let (ToolsState::Offer(v), Some(manifest)) = (&self.state, self.manifest.clone()) else {
            return;
        };
        let shown = v.licence_text.clone();
        self.cancel.store(false, Ordering::SeqCst);
        let cancel = Arc::clone(&self.cancel);
        self.state = ToolsState::Installing {
            label: "Starting".into(),
            done: 0,
            total: 0,
        };
        self.spawn(move |send| {
            let r = with_client(|c| -> Result<(String, Option<String>), String> {
                let offers = manifest
                    .offers(ITEM, Platform::current())
                    .map_err(|e| e.to_string())?;
                let (offer, _) = pick(&offers).ok_or("No download is offered for this system.")?;
                // The text on offer must be the text on screen.
                if offer.licence.text != shown {
                    return Err("The licence changed while the page was open, so nothing was installed. Open the page again to read it.".into());
                }
                let mut accepted =
                    Acceptances::load(c.fs, &c.dirs.accepted).map_err(|e| e.to_string())?;
                if !accepted.is_accepted(offer) {
                    accepted.accept(offer, c.clock.now_unix(), "gui");
                    accepted.save(c.fs, &c.dirs.accepted).map_err(|e| e.to_string())?;
                }
                let mut progress = |p: &Progress| send(Msg::Progress(p.clone()));
                let cancelled = || cancel.load(Ordering::SeqCst);
                let rec = c
                    .install_with(
                        offer,
                        &accepted,
                        &mut Hooks {
                            progress: &mut progress,
                            cancelled: &cancelled,
                        },
                    )
                    .map_err(|e| e.to_string())?;
                Ok((
                    format!("{} {} is installed. Helia uses it automatically.", rec.name, rec.version),
                    rec.attribution.filter(|_| rec.show_attribution),
                ))
            })
            .and_then(|r| r);
            send(Msg::Installed(r));
        });
    }

    /// Stop a download in progress; the part is kept, so the next try resumes.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// Remove what Helia installed.
    pub fn remove(&mut self) {
        self.spawn(|send| {
            let r = with_client(|c| c.remove(ITEM).map_err(|e| e.to_string()))
                .and_then(|r| r)
                .map(|r| {
                    r.note
                        .unwrap_or_else(|| format!("{} was removed.", r.installed.name))
                });
            send(Msg::Removed(r));
        });
    }

    /// Re-read what is installed and where ffmpeg is found (a few small file reads and one run of
    /// `ffmpeg -version`, so only after something changed).
    pub fn refresh(&mut self) {
        self.loaded = true;
        self.installed = with_client(|c| c.list(false).unwrap_or_default())
            .unwrap_or_default()
            .into_iter()
            .map(|h| InstalledLine {
                name: h.installed.name.clone(),
                version: h.installed.version.clone(),
                route: h.installed.route.clone(),
                provider: h.installed.provider.clone(),
                licence_name: h.installed.licence_name.clone(),
                attribution: h
                    .installed
                    .attribution
                    .clone()
                    .filter(|_| h.installed.show_attribution),
                problems: h.problems,
            })
            .collect();
        self.found = helia_core::io::ffmpeg::Tools::locate().ok().map(|t| {
            let how = match helia_core::io::ffmpeg::Tools::source_of_ffmpeg() {
                Some(helia_core::io::ffmpeg::Source::Environment) => "from HELIA_FFMPEG",
                Some(helia_core::io::ffmpeg::Source::Installed) => "installed with Helia",
                _ => "on the PATH",
            };
            format!("{} ({how})", t.ffmpeg.display())
        });
    }

    /// Take worker results; returns toasts to show.
    pub fn poll(&mut self) -> Vec<(ToastKind, String)> {
        let mut toasts = Vec::new();
        if self.active && !self.loaded {
            self.refresh();
        }
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Offer(r) => match *r {
                    Ok((view, manifest)) => {
                        self.manifest = Some(manifest);
                        self.state = ToolsState::Offer(Box::new(view));
                    }
                    Err(e) => self.state = ToolsState::Failed(e),
                },
                Msg::Progress(p) => {
                    let label = match p.phase {
                        Phase::Downloading => "Downloading".to_string(),
                        Phase::Verifying => "Checking the download".to_string(),
                        Phase::Unpacking => "Unpacking".to_string(),
                    };
                    self.state = ToolsState::Installing {
                        label,
                        done: p.done,
                        total: p.total,
                    };
                }
                Msg::Installed(Ok((message, attribution))) => {
                    toasts.push((ToastKind::Info, message.clone()));
                    self.state = ToolsState::Done {
                        message,
                        attribution,
                    };
                    self.refresh();
                }
                Msg::Installed(Err(e)) => {
                    self.state = if e.contains("cancelled") {
                        self.manifest = None;
                        ToolsState::Idle
                    } else {
                        ToolsState::Failed(e)
                    };
                    if matches!(self.state, ToolsState::Idle) {
                        self.open = false;
                        toasts.push((ToastKind::Info, "Cancelled. The part that was downloaded is kept, so the next try carries on.".into()));
                    }
                }
                Msg::Removed(Ok(note)) => {
                    toasts.push((ToastKind::Info, note));
                    self.refresh();
                }
                Msg::Removed(Err(e)) => toasts.push((ToastKind::Error, e)),
            }
        }
        toasts
    }

    /// Is a download running?
    #[must_use]
    pub fn installing(&self) -> bool {
        matches!(self.state, ToolsState::Installing { .. })
    }

    /// Close the page (not while a download runs: cancel it first).
    pub fn close(&mut self) {
        if !self.installing() {
            self.open = false;
            self.state = ToolsState::Idle;
            self.reason = None;
        }
    }
}

impl App {
    /// Once per frame: take worker results, show their toasts.
    pub(crate) fn tools_poll(&mut self) {
        for (kind, text) in self.tools.poll() {
            self.toast(kind, text);
        }
    }

    /// Open the page because a video needs ffmpeg and it is missing.
    pub(crate) fn offer_ffmpeg_for_video(&mut self) {
        self.tools.begin(Some(
            "The file you opened is a video (MP4, MOV, MKV or WebM), and Helia reads those through the free ffmpeg program, which is not installed on this computer.".into(),
        ));
    }

    /// The page that replaces the viewer: what, why, what to do, the provider, the licence in full.
    pub(crate) fn tools_page(&mut self, ui: &mut Ui) {
        let p = palette();
        let mut close = false;
        let mut accept = false;
        let mut retry = false;
        let mut cancel = false;
        let mut copy: Option<String> = None;
        let tools = &self.tools;
        ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_max_width(720.0);
                ui.spacing_mut().item_spacing.y = 10.0;
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Install ffmpeg").font(semibold(22.0)).color(p.text));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !tools.installing()
                            && widgets::button(ui, "Close", Kind::Outline, true, 0.0)
                                .on_hover_text("Back to the viewer (Esc). Nothing is installed.")
                                .clicked()
                        {
                            close = true;
                        }
                    });
                });
                match &tools.state {
                    ToolsState::Idle | ToolsState::Loading => {
                        ui.label(RichText::new("Fetching the list of optional tools, so the licence can be shown. Nothing is downloaded yet.").color(p.muted));
                        ui.add(egui::Spinner::new());
                    }
                    ToolsState::Offer(v) => {
                        explain(ui, tools.reason.as_deref(), v);
                        details(ui, v);
                        ui.label(RichText::new("The licence").font(semibold(15.0)).color(p.text));
                        Frame::new()
                            .fill(p.panel)
                            .stroke(Stroke::new(1.0, p.border))
                            .corner_radius(RADIUS_LARGE)
                            .inner_margin(Margin::same(12))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ScrollArea::vertical()
                                    .id_salt("tools-licence")
                                    .max_height(190.0)
                                    .show(ui, |ui| {
                                        ui.label(RichText::new(&v.licence_text).font(mono(12.0)).color(p.text));
                                    });
                            });
                        if let Some(cmd) = &v.package_command {
                            ui.label(RichText::new("Prefer your package manager? Run this in a terminal instead; it asks for your password itself:").color(p.muted));
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(cmd).font(mono(12.5)).color(p.text));
                                if widgets::button(ui, "Copy command", Kind::Outline, true, 0.0).clicked() {
                                    copy = Some(cmd.clone());
                                }
                            });
                        }
                        if let Some(when) = &v.accepted_on {
                            ui.label(RichText::new(format!("You accepted this licence on {when}; it has not changed.")).color(p.muted));
                        }
                        ui.horizontal(|ui| {
                            if widgets::button(ui, "Accept and install", Kind::Primary, true, 0.0)
                                .on_hover_text("You have read the licence above. Helia downloads ffmpeg from the provider and keeps it in your own folder.")
                                .clicked()
                            {
                                accept = true;
                            }
                            if widgets::button(ui, "Decline", Kind::Outline, true, 0.0)
                                .on_hover_text("Install nothing. Helia keeps working without ffmpeg.")
                                .clicked()
                            {
                                close = true;
                            }
                        });
                    }
                    ToolsState::Installing { label, done, total } => {
                        ui.label(RichText::new(format!("{label}\u{2026}")).color(p.text));
                        #[allow(clippy::cast_precision_loss, reason = "a progress fraction")]
                        let f = if *total == 0 { 0.0 } else { *done as f32 / *total as f32 };
                        ui.add(ProgressBar::new(f).show_percentage());
                        ui.label(RichText::new(format!("{} of {}", size_text(*done), size_text(*total))).color(p.muted));
                        if widgets::button(ui, "Cancel", Kind::Outline, true, 0.0)
                            .on_hover_text("Stop. What was downloaded is kept, so the next try carries on.")
                            .clicked()
                        {
                            cancel = true;
                        }
                    }
                    ToolsState::Done {
                        message,
                        attribution,
                    } => {
                        ui.label(RichText::new(message).font(semibold(15.0)).color(p.text));
                        if let Some(a) = attribution {
                            ui.label(RichText::new(a).color(p.muted));
                        }
                        ui.label(RichText::new("Open your video again. The About page lists what is installed and lets you remove it.").color(p.muted));
                        if widgets::button(ui, "Done", Kind::Primary, true, 0.0).clicked() {
                            close = true;
                        }
                    }
                    ToolsState::Failed(msg) => {
                        ui.label(RichText::new("Nothing was installed.").font(semibold(15.0)).color(p.text));
                        ui.label(RichText::new(msg).color(p.text));
                        ui.horizontal(|ui| {
                            if widgets::button(ui, "Try again", Kind::Primary, true, 0.0).clicked() {
                                retry = true;
                            }
                            if widgets::button(ui, "Close", Kind::Outline, true, 0.0).clicked() {
                                close = true;
                            }
                        });
                    }
                }
            });
        if let Some(c) = copy {
            ui.ctx().copy_text(c);
        }
        if accept {
            self.tools.accept_and_install();
        }
        if cancel {
            self.tools.cancel();
        }
        if retry {
            let reason = self.tools.reason.clone();
            self.tools.begin(reason);
        }
        if close {
            self.tools.close();
        }
    }

    /// The "Optional tools" block of the About page.
    pub(crate) fn tools_about_section(&mut self, ui: &mut Ui) {
        let p = palette();
        ui.add_space(6.0);
        widgets::overline(ui, "Optional tools");
        let mut install = false;
        let mut remove = false;
        if self.tools.installed.is_empty() {
            ui.label(RichText::new("ffmpeg, which Helia uses to read MP4, MOV, MKV and WebM video, is not installed with Helia.").color(p.text));
        }
        for l in &self.tools.installed {
            let how = if l.route == "package" {
                "by your package manager"
            } else {
                "by download"
            };
            ui.label(
                RichText::new(format!(
                    "{} {} installed {how}, from {}. Licence: {}.",
                    l.name, l.version, l.provider, l.licence_name
                ))
                .color(p.text),
            );
            if let Some(a) = &l.attribution {
                ui.label(RichText::new(a).color(p.muted));
            }
            for pr in &l.problems {
                ui.label(
                    RichText::new(format!("{pr}. Press Install again to repair it.")).color(p.text),
                );
            }
        }
        if let Some(f) = &self.tools.found {
            ui.label(RichText::new(format!("Helia finds ffmpeg at {f}.")).color(p.muted));
        } else {
            ui.label(
                RichText::new("Helia cannot find ffmpeg, so video files will not open.")
                    .color(p.muted),
            );
        }
        ui.horizontal(|ui| {
            let label = if self.tools.installed.is_empty() { "Install ffmpeg" } else { "Install again" };
            if widgets::button(ui, label, Kind::Outline, !self.tools.installing(), 0.0)
                .on_hover_text("Shows the provider and the whole licence first. Nothing is downloaded until you accept.")
                .clicked()
            {
                install = true;
            }
            if !self.tools.installed.is_empty()
                && widgets::button(ui, "Remove", Kind::Outline, !self.tools.installing(), 0.0).clicked()
            {
                remove = true;
            }
        });
        if install {
            self.show_about = false;
            self.tools.begin(None);
        }
        if remove {
            self.tools.remove();
        }
    }
}

/// What happened, why, and what to do.
fn explain(ui: &mut Ui, reason: Option<&str>, v: &OfferView) {
    let p = palette();
    Frame::new()
        .fill(p.panel)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(RADIUS_LARGE)
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let what = reason.map_or_else(
                || format!("You asked to install {}. {}", v.name, v.description),
                str::to_owned,
            );
            for (head, text) in [
                ("What happened", what),
                (
                    "Why",
                    format!("{} is a separate program with its own licence and its own authors. Helia does not contain it and never installs it by itself.", v.name),
                ),
                (
                    "What to do",
                    format!("Read the licence below. If you agree, press Accept and install: Helia downloads {} ({}) from the provider, checks it, and keeps it in your own folder. If you decline, Helia keeps working; you can still open SER and uncompressed AVI files.", v.name, v.size_text),
                ),
            ] {
                ui.label(RichText::new(head).font(semibold(14.0)).color(p.text));
                ui.label(RichText::new(text).color(p.muted));
            }
        });
}

/// Provider, size, source link: shown before the licence.
fn details(ui: &mut Ui, v: &OfferView) {
    let p = palette();
    egui::Grid::new("tools-details")
        .num_columns(2)
        .spacing([16.0, 6.0])
        .show(ui, |ui| {
            let mut row = |k: &str, val: String| {
                ui.label(RichText::new(k).color(p.faint));
                ui.label(RichText::new(val).color(p.text));
                ui.end_row();
            };
            row("Tool", format!("{} {}", v.name, v.version));
            row("Provided by", v.provider.clone());
            if !v.hosts.is_empty() {
                row("Downloaded from", format!("{} ({})", v.hosts, v.size_text));
            }
            row("Licence", v.licence_name.clone());
            ui.label(RichText::new("Licence and source").color(p.faint));
            ui.hyperlink_to(&v.source_url, &v.source_url);
            ui.end_row();
        });
    ui.label(
        RichText::new(
            "Helia is not affiliated with the provider and does not distribute this tool.",
        )
        .color(p.muted),
    );
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn offer() -> OfferView {
        OfferView {
            name: "FFmpeg".into(),
            version: "9.0.2".into(),
            description: "FFmpeg and FFprobe read compressed video (MP4, MOV, MKV, WebM). Helia uses them only to decode your own video files on this computer; nothing is encoded or sent anywhere.".into(),
            provider: "BtbN FFmpeg-Builds".into(),
            size_text: "131.5 MB".into(),
            hosts: "github.com".into(),
            licence_name: "GNU Lesser General Public License, version 3 or later".into(),
            licence_text: include_str!("../../../packaging/external/licences/LGPL-3.0-or-later.txt").into(),
            source_url: "https://github.com/BtbN/FFmpeg-Builds".into(),
            accepted_on: None,
            package_command: Some("sudo apt install ffmpeg".into()),
        }
    }

    #[test]
    fn an_inert_instance_starts_nothing() {
        let mut u = ToolsUi::showing(ToolsState::Offer(Box::new(offer())));
        u.accept_and_install();
        // No manifest and no workers: the state is unchanged, nothing was recorded.
        assert!(matches!(u.state, ToolsState::Offer(_)));
        assert_eq!(u.poll().len(), 0);
    }

    #[test]
    fn closing_is_refused_while_installing() {
        let mut u = ToolsUi::showing(ToolsState::Installing {
            label: "Downloading".into(),
            done: 1,
            total: 2,
        });
        u.close();
        assert!(u.open);
        u.state = ToolsState::Failed("x".into());
        u.close();
        assert!(!u.open);
        assert!(matches!(u.state, ToolsState::Idle));
    }

    #[test]
    fn worker_results_move_the_state_and_toast_once() {
        let mut u = ToolsUi::showing(ToolsState::Loading);
        u.tx.send(Msg::Offer(Box::new(Err("no network".into()))))
            .unwrap();
        assert_eq!(u.poll().len(), 0);
        assert!(matches!(&u.state, ToolsState::Failed(m) if m == "no network"));
        u.tx.send(Msg::Installed(Ok((
            "FFmpeg 9.0.2 is installed.".into(),
            Some("attribution".into()),
        ))))
        .unwrap();
        let toasts = u.poll();
        assert_eq!(toasts.len(), 1);
        assert!(matches!(u.state, ToolsState::Done { .. }));
        u.tx.send(Msg::Installed(Err(
            "cancelled; the partial download is kept".into(),
        )))
        .unwrap();
        let toasts = u.poll();
        assert_eq!(toasts.len(), 1);
        assert!(!u.open);
    }
}
