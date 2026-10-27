// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Updates in the GUI: the daily background check, the state the screens read,
//! and the three places that show it (a top-bar button, the About page section
//! and toasts).
//!
//! The work is the shared `helia_update::client` flow; this file only runs it on
//! worker threads and keeps the result in [`UpdateStatus`]. Nothing is fetched
//! unless the mode allows the daily check or the user clicks. The background
//! thread never blocks a frame.
//!
//! A started app counts as healthy a few seconds after its first frame
//! ([`HEALTHY_AFTER`]); an updated version that never gets there twice in a row is
//! rolled back at the next start (see `helia_update::state::BootHealth`).

use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText, TextEdit, Ui};
use helia_update::apply::ApplyOutcome;
use helia_update::backend::Panel;
use helia_update::client::{Available, CheckReport, Client, System};
use helia_update::download::Staged;
use helia_update::install::InstallKind;
use helia_update::settings::{Channel, UpdateMode, UpdateSettings};

use crate::activity::ToastKind;
use crate::app::App;
use crate::theme::{mono, palette};
use crate::widgets::{self, Kind};

/// The version of this build.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// How long after the first frame the app counts as having started properly.
pub const HEALTHY_AFTER: Duration = Duration::from_secs(5);

/// The automatic check waits this long after start-up, so it never competes with
/// opening a capture.
const CHECK_DELAY: Duration = Duration::from_secs(3);

/// What the screens show.
#[derive(Clone, Debug)]
pub enum UpdateStatus {
    /// Nothing known or nothing to say.
    Idle,
    /// A check is running.
    Checking,
    /// The newest version is installed.
    UpToDate {
        /// The newest version.
        version: String,
    },
    /// A newer version exists.
    Available(Box<Available>),
    /// A download is running.
    Downloading {
        /// The version being fetched.
        version: String,
        /// Bytes so far.
        done: u64,
        /// Bytes in all.
        total: u64,
    },
    /// Downloaded and checked; installs when Helia closes, or now on request.
    Ready {
        /// The version.
        version: String,
    },
    /// Installed; takes effect the next time Helia starts.
    Installed {
        /// What happened, in words.
        message: String,
    },
    /// The user finishes it (a package command, or a file to open).
    Manual {
        /// What to do, in words.
        message: String,
        /// A command to copy.
        command: Option<String>,
    },
    /// A check or download failed.
    Failed {
        /// What happened and what to do, in words.
        message: String,
    },
    /// The update details need a newer Helia to read.
    UpdateByHand,
}

enum Msg {
    Checked {
        explicit: bool,
        result: Result<CheckReport, String>,
    },
    Progress {
        done: u64,
        total: u64,
    },
    Downloaded(Result<Staged, String>),
    Applied(Result<ApplyOutcome, String>),
}

/// What start-up found, for the first frame to show once.
static STARTUP_NOTICE: OnceLock<Option<String>> = OnceLock::new();

/// Call first thing in `main`, before the window: count this start and roll back
/// a version that keeps failing. A rollback starts the restored program and exits.
pub fn startup() {
    let notice = System::new(true).and_then(|sys| {
        sys.with_client(VERSION, |c| match c.startup() {
            Ok(s) => {
                if let Some(e) = &s.rollback_error {
                    helia_core::applog::error(e);
                }
                if let Some(r) = &s.rollback {
                    helia_core::applog::info(&r.message);
                    if let Some(p) = &r.relaunch {
                        let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
                        let _ = helia_update::apply::Process::spawn_detached(
                            &helia_update::apply::StdProcess,
                            p,
                            &args,
                        );
                    }
                    if r.relaunch.is_some() || r.exit_now {
                        std::process::exit(0);
                    }
                }
                s.notice
            }
            Err(e) => {
                helia_core::applog::error(&e.to_string());
                None
            }
        })
    });
    let _ = STARTUP_NOTICE.set(notice);
}

/// The update state and its worker plumbing.
pub struct UpdateUi {
    /// What the screens show.
    pub status: UpdateStatus,
    /// The saved choices.
    pub settings: UpdateSettings,
    /// Draw the About section and the top-bar button (false in tests and demos).
    visible: bool,
    /// Run workers (false in tests and demos).
    active: bool,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    ctx: Option<egui::Context>,
    started: Instant,
    healthy_marked: bool,
    startup_check_done: bool,
    busy: bool,
    available: Option<Available>,
    ready: Option<(Staged, Option<String>)>,
    queued_toasts: Vec<(ToastKind, String)>,
    /// The notice about a settings file from a newer Helia was shown.
    pub prefs_notice_sent: bool,
    /// The settings block of the build's update backend (`None`: a source build has none).
    pub panel: Option<Panel>,
    /// What is typed in the panel's input. Cleared as soon as a button takes it.
    pub panel_input: String,
}

impl UpdateUi {
    /// A live instance (`active`) or an inert one for tests and demos.
    #[must_use]
    pub fn new(ctx: &egui::Context, active: bool) -> Self {
        let (tx, rx) = channel();
        let settings = if active {
            #[allow(
                clippy::redundant_closure_for_method_calls,
                reason = "the borrow's lifetime needs the closure"
            )]
            System::new(true).map_or_else(UpdateSettings::default, |s| {
                s.with_client(VERSION, |c| c.settings())
            })
        } else {
            UpdateSettings::default()
        };
        #[allow(
            clippy::redundant_closure_for_method_calls,
            reason = "the borrow's lifetime needs the closure"
        )]
        let panel = if active {
            System::new(true).and_then(|s| s.with_client(VERSION, |c| c.backend_panel()))
        } else {
            None
        };
        let mut u = Self {
            panel,
            panel_input: String::new(),
            status: UpdateStatus::Idle,
            settings,
            visible: active,
            active,
            tx,
            rx,
            ctx: active.then(|| ctx.clone()),
            started: Instant::now(),
            healthy_marked: !active,
            startup_check_done: !active,
            busy: false,
            available: None,
            ready: None,
            queued_toasts: Vec::new(),
            prefs_notice_sent: false,
        };
        if active && let Some(Some(n)) = STARTUP_NOTICE.get() {
            u.queued_toasts.push((ToastKind::Info, n.clone()));
        }
        u
    }

    /// An inert instance that is drawn, showing `status` (screenshots).
    #[must_use]
    #[allow(dead_code, reason = "for the update screenshots")]
    pub fn showing(status: UpdateStatus) -> Self {
        Self::showing_with_panel(status, None)
    }

    /// As [`Self::showing`], with a backend panel (screenshots).
    #[must_use]
    #[allow(dead_code, reason = "for the update screenshots")]
    pub fn showing_with_panel(status: UpdateStatus, panel: Option<Panel>) -> Self {
        let (tx, rx) = channel();
        Self {
            panel,
            panel_input: String::new(),
            status,
            settings: UpdateSettings::default(),
            visible: true,
            active: false,
            tx,
            rx,
            ctx: None,
            started: Instant::now(),
            healthy_marked: true,
            startup_check_done: true,
            busy: false,
            available: None,
            ready: None,
            queued_toasts: Vec::new(),
            prefs_notice_sent: false,
        }
    }

    fn spawn(&mut self, f: impl FnOnce(&Client<'_>, &dyn Fn(Msg)) + Send + 'static) {
        if !self.active {
            return;
        }
        self.busy = true;
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let send = |m: Msg| {
                let _ = tx.send(m);
                if let Some(c) = &ctx {
                    c.request_repaint();
                }
            };
            match System::new(true) {
                Some(sys) => sys.with_client(VERSION, |c| f(c, &send)),
                None => send(Msg::Checked {
                    explicit: true,
                    result: Err("I could not work out where to keep update files, so updates are off for now.".into()),
                }),
            }
        });
    }

    /// Check now, because the user asked.
    pub fn check_now(&mut self) {
        if self.busy {
            return;
        }
        self.status = UpdateStatus::Checking;
        self.spawn(|c, send| {
            send(Msg::Checked {
                explicit: true,
                result: c.check(true).map_err(|e| e.to_string()),
            });
        });
    }

    fn check_in_background(&mut self) {
        self.startup_check_done = true;
        if self.settings.mode == UpdateMode::Off || self.busy {
            return;
        }
        self.spawn(|c, send| {
            send(Msg::Checked {
                explicit: false,
                result: c.check(false).map_err(|e| e.to_string()),
            });
        });
    }

    /// Download the offered update (and install it when Helia can do that itself).
    pub fn download(&mut self) {
        let Some(a) = self.available.clone() else {
            return;
        };
        if self.busy {
            return;
        }
        self.status = UpdateStatus::Downloading {
            version: a.version.clone(),
            done: 0,
            total: a.asset.as_ref().map_or(0, |f| f.size),
        };
        self.spawn(move |c, send| {
            let mut last = Instant::now();
            let staged = c.download(&a, &mut |done, total| {
                if last.elapsed() > Duration::from_millis(200) || done == total {
                    last = Instant::now();
                    send(Msg::Progress { done, total });
                }
            });
            send(Msg::Downloaded(staged.map_err(|e| e.to_string())));
        });
    }

    /// Install a downloaded update now. For the Windows installer this schedules
    /// it for when Helia closes; the caller then closes the window.
    pub fn install_now(&mut self) -> bool {
        let Some((staged, notes)) = self.ready.take() else {
            return false;
        };
        if !self.active {
            return false;
        }
        let version = staged.version.clone();
        self.status = UpdateStatus::Downloading {
            version,
            done: 1,
            total: 1,
        };
        self.spawn(move |c, send| {
            send(Msg::Applied(
                c.apply(&staged, notes.as_deref())
                    .map_err(|e| e.to_string()),
            ));
        });
        true
    }

    #[allow(
        clippy::redundant_closure_for_method_calls,
        reason = "the borrow's lifetime needs the closure"
    )]
    fn refresh_panel(&mut self) {
        if self.active {
            self.panel =
                System::new(true).and_then(|s| s.with_client(VERSION, |c| c.backend_panel()));
        }
    }

    /// A button of the backend's panel was pressed. What was typed goes to the backend
    /// and is cleared at once; it is never logged or shown again. Errors name the
    /// problem, not the text.
    pub fn panel_action(&mut self, id: &str) {
        let typed = std::mem::take(&mut self.panel_input);
        if !self.active {
            return;
        }
        let result =
            System::new(true).map(|sys| sys.with_client(VERSION, |c| c.backend_action(id, &typed)));
        match result {
            Some(Ok(done)) => {
                self.queued_toasts.push((ToastKind::Success, done.message));
                self.refresh_panel();
                if done.recheck {
                    self.check_now();
                }
            }
            Some(Err(e)) => {
                self.queued_toasts.push((ToastKind::Error, e.to_string()));
                self.refresh_panel();
            }
            None => {}
        }
    }

    /// Change the mode and save it.
    pub fn set_mode(&mut self, mode: UpdateMode) {
        self.settings.mode = mode;
        self.save_settings();
    }

    /// Change the channel and save it.
    pub fn set_channel(&mut self, channel: Channel) {
        self.settings.channel = channel;
        self.available = None;
        self.status = UpdateStatus::Idle;
        self.save_settings();
    }

    fn save_settings(&mut self) {
        if !self.active {
            return;
        }
        let s = self.settings;
        let result = System::new(true).map(|sys| sys.with_client(VERSION, |c| c.save_settings(&s)));
        if let Some(Err(e)) = result {
            self.queued_toasts.push((ToastKind::Error, e.to_string()));
        }
    }

    /// Drain worker messages, start the daily check, mark a healthy start. Returns
    /// the toasts to show.
    pub fn poll(&mut self) -> Vec<(ToastKind, String)> {
        if !self.healthy_marked && self.started.elapsed() >= HEALTHY_AFTER {
            self.healthy_marked = true;
            self.spawn_quiet(|c| {
                let _ = c.mark_healthy();
            });
        }
        if !self.startup_check_done && self.started.elapsed() >= CHECK_DELAY {
            self.check_in_background();
        }
        while let Ok(m) = self.rx.try_recv() {
            self.handle(m);
        }
        std::mem::take(&mut self.queued_toasts)
    }

    /// A worker whose result nobody waits for.
    fn spawn_quiet(&self, f: impl FnOnce(&Client<'_>) + Send + 'static) {
        if self.active {
            std::thread::spawn(move || {
                if let Some(sys) = System::new(true) {
                    sys.with_client(VERSION, f);
                }
            });
        }
    }

    fn handle(&mut self, m: Msg) {
        match m {
            Msg::Checked { explicit, result } => {
                self.busy = false;
                self.refresh_panel();
                match result {
                    Ok(CheckReport::Available(a)) => {
                        let auto =
                            self.settings.mode == UpdateMode::Automatic && a.may_auto_download();
                        if !explicit {
                            let text = a.note.as_ref().map_or_else(
                                || {
                                    format!(
                                        "Helia {} is available. Open About to update.",
                                        a.version
                                    )
                                },
                                |n| format!("{n} Open About for the link."),
                            );
                            self.queued_toasts.push((ToastKind::Info, text));
                        }
                        self.available = Some((*a).clone());
                        self.status = UpdateStatus::Available(a);
                        if auto {
                            self.download();
                        }
                    }
                    Ok(
                        CheckReport::UpToDate { version } | CheckReport::OlderOnServer { version },
                    ) => {
                        self.status = UpdateStatus::UpToDate { version };
                    }
                    Ok(CheckReport::SchemaTooNew { .. }) => {
                        self.status = UpdateStatus::UpdateByHand;
                    }
                    Ok(CheckReport::Disabled | CheckReport::NotDue) => {
                        if matches!(self.status, UpdateStatus::Checking) {
                            self.status = UpdateStatus::Idle;
                        }
                    }
                    Err(message) => {
                        helia_core::applog::error(&message);
                        // A failed daily check stays quiet; a failed click says so.
                        if explicit {
                            self.status = UpdateStatus::Failed { message };
                        } else if matches!(self.status, UpdateStatus::Checking) {
                            self.status = UpdateStatus::Idle;
                        }
                    }
                }
            }
            Msg::Progress { done, total } => {
                if let UpdateStatus::Downloading {
                    done: d, total: t, ..
                } = &mut self.status
                {
                    *d = done;
                    *t = total;
                }
            }
            Msg::Downloaded(result) => match result {
                Ok(staged) => {
                    let notes = self.available.as_ref().and_then(|a| a.notes_url.clone());
                    let windows = self
                        .available
                        .as_ref()
                        .is_some_and(|a| matches!(a.install, InstallKind::Nsis { .. }));
                    self.busy = false;
                    self.ready = Some((staged.clone(), notes));
                    if windows && self.available.as_ref().is_some_and(Available::self_applies) {
                        // The installer replaces the running program, so it waits until
                        // Helia closes: on exit, or when the user presses the button.
                        self.status = UpdateStatus::Ready {
                            version: staged.version,
                        };
                    } else {
                        // An AppImage is replaced at once; a package or archive only
                        // gets its instructions.
                        self.install_now();
                    }
                }
                Err(message) => {
                    self.busy = false;
                    helia_core::applog::error(&message);
                    self.status = UpdateStatus::Failed { message };
                }
            },
            Msg::Applied(result) => {
                self.busy = false;
                match result {
                    Ok(ApplyOutcome::Installed { message }) => {
                        self.queued_toasts
                            .push((ToastKind::Success, message.clone()));
                        self.status = UpdateStatus::Installed { message };
                    }
                    Ok(ApplyOutcome::Scheduled { message }) => {
                        self.queued_toasts
                            .push((ToastKind::Success, message.clone()));
                        self.status = UpdateStatus::Installed { message };
                    }
                    Ok(ApplyOutcome::Manual {
                        message, command, ..
                    }) => self.status = UpdateStatus::Manual { message, command },
                    Err(message) => {
                        helia_core::applog::error(&message);
                        self.status = UpdateStatus::Failed { message };
                    }
                }
            }
        }
    }

    /// The version offered, when there is an update the top bar should point at.
    #[must_use]
    pub fn offered(&self) -> Option<&str> {
        match &self.status {
            UpdateStatus::Available(a) => Some(&a.version),
            UpdateStatus::Ready { version } => Some(version),
            _ => None,
        }
    }

    /// One line for the About page.
    #[must_use]
    pub fn status_line(&self) -> String {
        match &self.status {
            UpdateStatus::Idle => format!("Helia {VERSION}. Not checked yet in this session."),
            UpdateStatus::Checking => "Checking for updates...".into(),
            UpdateStatus::UpToDate { version } => {
                format!("Helia {VERSION} is up to date (newest: {version}).")
            }
            UpdateStatus::Available(a) => format!("Helia {} is available.", a.version),
            UpdateStatus::Downloading {
                version,
                done,
                total,
            } => {
                if *total == 0 {
                    format!("Getting Helia {version}...")
                } else {
                    format!(
                        "Getting Helia {version}: {} of {} MB",
                        done / (1024 * 1024),
                        total / (1024 * 1024)
                    )
                }
            }
            UpdateStatus::Ready { version } => {
                format!("Helia {version} is downloaded and checked.")
            }
            UpdateStatus::Installed { message } | UpdateStatus::Manual { message, .. } => {
                message.clone()
            }
            UpdateStatus::Failed { message } => message.clone(),
            UpdateStatus::UpdateByHand => {
                "A newer Helia exists, but this version cannot read its update details. Download the new version from the Helia website.".into()
            }
        }
    }

    /// As Helia exits: a normal close counts as a healthy start however short it was
    /// (a crash never gets here), and a downloaded Windows installer is started.
    pub fn on_exit(&mut self) {
        if !self.active {
            return;
        }
        let Some(sys) = System::new(true) else {
            return;
        };
        if !self.healthy_marked {
            self.healthy_marked = true;
            sys.with_client(VERSION, |c| {
                let _ = c.mark_healthy();
            });
        }
        if let Some((staged, notes)) = self.ready.take() {
            let _ = sys.with_client(VERSION, |c| c.apply(&staged, notes.as_deref()));
        }
    }
}

impl App {
    /// Once per frame: take worker results, show their toasts.
    pub(crate) fn update_poll(&mut self) {
        if !self.update.prefs_notice_sent {
            self.update.prefs_notice_sent = true;
            if let Some(n) = self.prefs.newer_schema {
                self.toast(
                    ToastKind::Error,
                    format!(
                        "Your Helia settings were saved by a newer Helia (format {n}). I am reading them as far as I understand them and will not change the file, so nothing is lost when you go back to the newer one."
                    ),
                );
            }
        }
        for (kind, text) in self.update.poll() {
            self.toast(kind, text);
        }
    }

    /// A button in the top bar while an update is waiting; it opens About.
    pub(crate) fn update_topbar_button(&mut self, ui: &mut Ui) {
        if !self.update.visible {
            return;
        }
        let Some(v) = self.update.offered().map(str::to_owned) else {
            return;
        };
        if widgets::button(ui, &format!("Update {v}"), Kind::Primary, true, 0.0)
            .on_hover_text("A newer Helia is available. Opens About, where you can install it.")
            .clicked()
        {
            self.show_about = true;
            self.feedback.open = false;
        }
    }

    /// The "Updates" block of the About page.
    pub(crate) fn update_about_section(&mut self, ui: &mut Ui) {
        if !self.update.visible {
            return;
        }
        let p = palette();
        ui.add_space(6.0);
        widgets::overline(ui, "Updates");
        let line = self.update.status_line();
        ui.label(RichText::new(line).color(p.text));
        if let UpdateStatus::Manual {
            command: Some(c), ..
        } = &self.update.status
        {
            ui.label(RichText::new(c).font(mono(12.5)).color(p.muted));
            if widgets::button(ui, "Copy command", Kind::Outline, true, 0.0).clicked() {
                ui.ctx().copy_text(c.clone());
            }
        }
        if let UpdateStatus::Available(a) = &self.update.status
            && let Some(n) = &a.note
        {
            ui.label(RichText::new(n).color(p.text));
            if let Some(l) = &a.link {
                ui.hyperlink_to("Open the store", l);
            }
        }
        let busy = matches!(
            self.update.status,
            UpdateStatus::Checking | UpdateStatus::Downloading { .. }
        );
        let mut check = false;
        let mut download = false;
        let mut restart = false;
        ui.horizontal_wrapped(|ui| {
            if widgets::button(ui, "Check for updates", Kind::Outline, !busy, 0.0)
                .on_hover_text("Ask the update server whether a newer Helia exists")
                .clicked()
            {
                check = true;
            }
            if let UpdateStatus::Available(a) = &self.update.status
                && a.asset.is_some()
                && a.note.is_none()
            {
                let label = if a.self_applies() {
                    "Download and install"
                } else {
                    "Download"
                };
                if widgets::button(ui, label, Kind::Primary, !busy, 0.0).clicked() {
                    download = true;
                }
            }
            if matches!(self.update.status, UpdateStatus::Ready { .. })
                && widgets::button(ui, "Close Helia and install", Kind::Primary, true, 0.0)
                    .on_hover_text("Helia closes, the new version installs, then Helia opens again")
                    .clicked()
            {
                restart = true;
            }
        });
        if let UpdateStatus::Available(a) = &self.update.status
            && let Some(u) = &a.notes_url
        {
            ui.hyperlink_to("What changed", u);
        }
        ui.add_space(4.0);
        ui.label(RichText::new("When Helia may check").color(p.muted));
        let mut mode = self.update.settings.mode;
        for m in UpdateMode::ALL {
            ui.radio_value(&mut mode, m, m.describe());
        }
        let mut pre = self.update.settings.channel == Channel::Prerelease;
        ui.checkbox(&mut pre, "Include pre-release versions");
        if mode != self.update.settings.mode {
            self.update.set_mode(mode);
        }
        if pre != (self.update.settings.channel == Channel::Prerelease) {
            self.update.set_channel(if pre {
                Channel::Prerelease
            } else {
                Channel::Stable
            });
        }
        let mut pressed: Option<String> = None;
        if let Some(panel) = self.update.panel.clone() {
            ui.add_space(4.0);
            ui.label(RichText::new(&panel.heading).color(p.muted));
            for line in &panel.lines {
                let colour = if line.muted { p.muted } else { p.text };
                ui.label(RichText::new(&line.text).color(colour));
            }
            if let Some((label, url)) = &panel.link {
                ui.hyperlink_to(label, url);
            }
            ui.horizontal_wrapped(|ui| {
                if let Some(input) = &panel.input {
                    ui.add(
                        TextEdit::singleline(&mut self.update.panel_input)
                            .password(true)
                            .hint_text(&input.hint)
                            .desired_width(320.0),
                    );
                }
                let typed = !self.update.panel_input.trim().is_empty();
                for b in &panel.buttons {
                    let enabled = typed || !b.needs_input;
                    if widgets::button(ui, &b.label, Kind::Outline, enabled, 0.0)
                        .on_hover_text(&b.hover)
                        .clicked()
                    {
                        pressed = Some(b.id.clone());
                    }
                }
            });
            if let Some(f) = &panel.footer {
                ui.label(RichText::new(f).color(p.muted));
            }
        }
        ui.label(
            RichText::new(format!(
                "Privacy: Helia contacts only {} for updates. It fetches a small signed file that names the newest version, and the new version itself when you ask. It sends no identifiers, only the program name and version ({}), and nothing else leaves your computer.",
                helia_update::helia::base_url(),
                helia_update::USER_AGENT
            ))
            .color(p.muted),
        );
        if let Some(text) = helia_update::backend::installed()
            .and_then(helia_update::backend::UpdateBackend::privacy)
        {
            ui.label(RichText::new(text).color(p.muted));
        }
        if let Some(id) = pressed {
            self.update.panel_action(&id);
        }
        if check {
            self.update.check_now();
        }
        if download {
            self.update.download();
        }
        if restart {
            self.update.on_exit();
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::unwrap_used, clippy::assert_is_empty, reason = "tests")]
    use super::*;
    use helia_update::check::Update;
    use helia_update::manifest::{Asset, Kind as AssetKind, Manifest, Platform};

    pub(crate) fn available(self_applies: bool) -> Available {
        let manifest = Manifest {
            schema: 1,
            app: "helia".into(),
            channel: "stable".into(),
            version: semver::Version::parse("9.9.9").unwrap(),
            date: 0,
            sequence: 1,
            expires: u64::MAX,
            min_supported: None,
            notes_url: Some("https://example.org/notes".into()),
            assets: Vec::new(),
            skipped_assets: 0,
        };
        Available {
            update: Update {
                manifest,
                downgrade: false,
                required: false,
            },
            version: "9.9.9".into(),
            notes_url: Some("https://example.org/notes".into()),
            required: false,
            asset: Some(Asset {
                platform: Platform::LinuxX86_64,
                kind: AssetKind::Appimage,
                url: "https://example.org/a".into(),
                sha256: "0".repeat(64),
                size: 10 << 20,
            }),
            install: if self_applies {
                InstallKind::AppImage {
                    path: "/a.AppImage".into(),
                }
            } else {
                InstallKind::Deb
            },
            staged: None,
            auto_download: true,
            note: None,
            link: None,
        }
    }

    fn ui() -> UpdateUi {
        UpdateUi::new(&egui::Context::default(), false)
    }

    #[test]
    fn inert_instances_draw_nothing_and_start_nothing() {
        let mut u = ui();
        assert!(!u.visible);
        u.check_now();
        assert!(matches!(u.status, UpdateStatus::Checking));
        assert!(!u.busy, "no worker was started");
    }

    #[test]
    fn an_offered_update_is_pointed_at_by_the_top_bar_and_toasted_once() {
        let mut u = ui();
        u.handle(Msg::Checked {
            explicit: false,
            result: Ok(CheckReport::Available(Box::new(available(true)))),
        });
        assert_eq!(u.offered(), Some("9.9.9"));
        let toasts = u.poll();
        assert_eq!(toasts.len(), 1);
        assert!(toasts[0].1.contains("9.9.9"));
        assert!(u.poll().is_empty());
    }

    #[test]
    fn automatic_mode_downloads_a_self_applying_update_by_itself() {
        let mut u = ui();
        u.settings.mode = UpdateMode::Automatic;
        u.handle(Msg::Checked {
            explicit: false,
            result: Ok(CheckReport::Available(Box::new(available(true)))),
        });
        assert!(matches!(u.status, UpdateStatus::Downloading { .. }));
        // Notify mode and a package install wait for the user.
        let mut n = ui();
        n.handle(Msg::Checked {
            explicit: false,
            result: Ok(CheckReport::Available(Box::new(available(true)))),
        });
        assert!(matches!(n.status, UpdateStatus::Available(_)));
        let mut d = ui();
        d.settings.mode = UpdateMode::Automatic;
        d.handle(Msg::Checked {
            explicit: false,
            result: Ok(CheckReport::Available(Box::new(available(false)))),
        });
        assert!(matches!(d.status, UpdateStatus::Available(_)));
    }

    #[test]
    fn a_plan_without_automatic_updates_stops_automatic_mode_and_explains() {
        let mut u = ui();
        u.settings.mode = UpdateMode::Automatic;
        let mut a = available(true);
        a.auto_download = false;
        a.note = Some("Helia 9.9.9 is out. Your purchase covers 0.8 only.".into());
        a.link = Some("https://12hobbies.studio/store/helia/".into());
        u.handle(Msg::Checked {
            explicit: false,
            result: Ok(CheckReport::Available(Box::new(a))),
        });
        assert!(
            matches!(u.status, UpdateStatus::Available(_)),
            "no download"
        );
        let toasts = u.poll();
        assert_eq!(toasts.len(), 1);
        assert!(toasts[0].1.contains("covers 0.8 only"), "{}", toasts[0].1);
    }

    #[test]
    fn failures_are_loud_only_when_the_user_asked() {
        let mut u = ui();
        u.status = UpdateStatus::Checking;
        u.handle(Msg::Checked {
            explicit: false,
            result: Err("could not reach the update server".into()),
        });
        assert!(matches!(u.status, UpdateStatus::Idle));
        u.status = UpdateStatus::Checking;
        u.handle(Msg::Checked {
            explicit: true,
            result: Err("could not reach the update server".into()),
        });
        assert!(u.status_line().contains("could not reach"));
    }

    #[test]
    fn progress_and_results_update_the_status_line() {
        let mut u = ui();
        u.status = UpdateStatus::Downloading {
            version: "9.9.9".into(),
            done: 0,
            total: 10 << 20,
        };
        u.handle(Msg::Progress {
            done: 5 << 20,
            total: 10 << 20,
        });
        assert!(
            u.status_line().contains("5 of 10 MB"),
            "{}",
            u.status_line()
        );
        u.handle(Msg::Applied(Ok(ApplyOutcome::Manual {
            message: "Run this".into(),
            command: Some("sudo apt install x.deb".into()),
            file: None,
        })));
        assert!(matches!(
            u.status,
            UpdateStatus::Manual {
                command: Some(_),
                ..
            }
        ));
    }
}
