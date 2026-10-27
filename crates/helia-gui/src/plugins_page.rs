// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The Plugins page: every plugin Helia found, its source and checksums, whether it is on, its
//! settings form and a test. It replaces the viewer while open (the Plugins button in the top bar,
//! or Esc to close).
//!
//! Nothing here starts a plugin except the Test button, and that only for a plugin that is on.
//! Enabling shows what is about to be trusted (where it is, both SHA-256 checksums, what it can
//! do and what it asks for, and the plain statement that a plugin runs with the user's rights)
//! and waits for a click on "Enable plugin".

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;

use eframe::egui::{self, Frame, Margin, RichText, ScrollArea, Stroke, Ui};
use helia_plugin::manifest::Loaded;
use helia_plugin::registry::{Approvals, Registry, Source, Status};
use helia_plugin::schema::{Field, FieldKind, Schema};
use serde_json::{Map, Value};

use crate::app::App;
use crate::theme::{RADIUS_LARGE, mono, palette, semibold};
use crate::widgets::{self, Kind};

static CLI_PATHS: std::sync::OnceLock<Vec<PathBuf>> = std::sync::OnceLock::new();

/// Remember the `--plugin` paths this run was started with (set once, from the command line).
pub fn set_cli_paths(paths: Vec<PathBuf>) {
    let _ = CLI_PATHS.set(paths);
}

/// The `--plugin` paths this run was started with.
#[must_use]
pub fn cli_paths() -> Vec<PathBuf> {
    CLI_PATHS.get().cloned().unwrap_or_default()
}

/// What the Test button found.
#[derive(Clone, Debug)]
pub enum TestState {
    /// The plugin is starting or answering.
    Running,
    /// Done: whether it passed, the plugin's message and its log.
    Done {
        /// The plugin's own check passed (or it offers none and started).
        ok: bool,
        /// What to show.
        message: String,
        /// The plugin's log.
        log: String,
    },
}

/// State of the Plugins page.
pub struct PluginsUi {
    /// The plugins found and what was approved.
    pub registry: Registry,
    /// The `--plugin` paths this run was started with.
    pub paths: Vec<PathBuf>,
    /// The plugin shown on the right.
    pub selected: Option<String>,
    /// The plugin whose enable confirmation is open.
    pub confirming: Option<String>,
    /// Settings being edited, per plugin.
    pub forms: HashMap<String, Value>,
    /// Problems found when saving the settings, per plugin.
    pub errors: HashMap<String, Vec<String>>,
    /// Test results, per plugin.
    pub tests: HashMap<String, TestState>,
    /// A folder to show as `<plugins>` instead of its real path (the snapshots, so they do not
    /// depend on where the tests ran).
    pub mask: Option<PathBuf>,
    /// The last thing that went wrong with the approvals file.
    pub note: Option<String>,
    tx: mpsc::Sender<(String, TestState)>,
    rx: mpsc::Receiver<(String, TestState)>,
}

impl PluginsUi {
    /// Look in the plugins folder and at `paths`; `real` reads and writes the user's approvals
    /// file, otherwise they are kept in memory only (tests).
    #[must_use]
    pub fn new(paths: Vec<PathBuf>, real: bool) -> Self {
        let registry = if real {
            Registry::open(&paths)
        } else {
            Registry::discover(None, &paths, Approvals::in_memory())
        };
        Self::with_registry(registry, paths)
    }

    /// State over a registry that is already built.
    #[must_use]
    pub fn with_registry(registry: Registry, paths: Vec<PathBuf>) -> Self {
        let (tx, rx) = mpsc::channel();
        let selected = registry.names().into_iter().next();
        Self {
            registry,
            paths,
            selected,
            confirming: None,
            forms: HashMap::new(),
            errors: HashMap::new(),
            tests: HashMap::new(),
            mask: None,
            note: None,
            tx,
            rx,
        }
    }

    /// Look again at the folders and the approvals (after something changed on disk).
    pub fn rescan(&mut self) {
        let approvals = self.registry.approvals.clone();
        self.registry = Registry::discover(
            helia_plugin::registry::plugins_dir().as_deref(),
            &self.paths,
            approvals,
        );
        if self.selected.is_none() {
            self.selected = self.registry.names().into_iter().next();
        }
    }

    /// Collect finished tests.
    pub fn pump(&mut self) {
        while let Ok((name, state)) = self.rx.try_recv() {
            self.tests.insert(name, state);
        }
    }

    /// The plugins that are on, with their steps.
    #[must_use]
    pub fn enabled(&self) -> Vec<&Loaded> {
        self.registry
            .found
            .iter()
            .filter_map(|f| f.loaded.as_ref().ok())
            .filter(|l| self.registry.approvals.status(l) == Status::Enabled)
            .collect()
    }

    /// A path as shown: the masked prefix replaced when a mask is set.
    #[must_use]
    pub fn shown(&self, p: &Path) -> String {
        match self.mask.as_deref().and_then(|m| p.strip_prefix(m).ok()) {
            Some(rest) => format!("<plugins>/{}", rest.display()).replace('\\', "/"),
            None => p.display().to_string(),
        }
    }

    fn save(&mut self) {
        if let Err(e) = self.registry.approvals.save() {
            self.note = Some(e);
        } else {
            self.note = None;
        }
    }

    /// Switch a plugin on after the user confirmed.
    pub fn enable(&mut self, name: &str) {
        let Some(l) = self.registry.get(name).cloned() else {
            return;
        };
        match self.registry.approvals.approve(&l) {
            Ok(_) => {
                self.confirming = None;
                self.save();
            }
            Err(e) => self.note = Some(e.to_string()),
        }
    }

    /// Switch a plugin off.
    pub fn disable(&mut self, name: &str) {
        self.registry.approvals.disable(name);
        self.save();
    }

    /// Start the plugin's own check on a thread.
    fn test(&mut self, name: &str, ctx: &egui::Context) {
        self.tests.insert(name.to_owned(), TestState::Running);
        let registry = self.registry.clone();
        let (tx, name, ctx) = (self.tx.clone(), name.to_owned(), ctx.clone());
        std::thread::spawn(move || {
            let mut runner = helia_plugin::Runner::new(registry);
            let cancel = AtomicBool::new(false);
            let result = runner.test(&name, &cancel);
            let log = runner.log(&name);
            runner.close_all();
            let state = match result {
                Ok(r) => TestState::Done {
                    ok: r.ok,
                    message: r.message,
                    log,
                },
                Err(e)
                    if e.kind
                        == helia_plugin::Kind::Plugin(
                            helia_plugin_sdk::ErrorKind::MethodNotFound,
                        ) =>
                {
                    TestState::Done {
                        ok: true,
                        message: "The plugin started and answered. It offers no check of its own."
                            .into(),
                        log,
                    }
                }
                Err(e) => TestState::Done {
                    ok: false,
                    message: e.to_string(),
                    log,
                },
            };
            let _ = tx.send((name, state));
            ctx.request_repaint();
        });
    }
}

/// Draw `schema` as a form over `values` (an object). Returns true when a value changed.
pub fn schema_form(ui: &mut Ui, id: &str, schema: &Schema, values: &mut Value) -> bool {
    if !values.is_object() {
        *values = Value::Object(Map::new());
    }
    let mut changed = false;
    for f in &schema.fields {
        let Some(map) = values.as_object_mut() else {
            break;
        };
        changed |= field_row(ui, id, f, map);
    }
    changed
}

fn field_row(ui: &mut Ui, id: &str, f: &Field, map: &mut Map<String, Value>) -> bool {
    let p = palette();
    let mut changed = false;
    ui.add(egui::Label::new(
        RichText::new(format!("{}{}", f.label, if f.required { " *" } else { "" }))
            .font(semibold(13.0))
            .color(p.text),
    ));
    let current = map.get(&f.key).or(f.default.as_ref()).cloned();
    let salt = format!("{id}-{}", f.key);
    match &f.kind {
        FieldKind::Text | FieldKind::Secret | FieldKind::Path => {
            let mut text = current
                .as_ref()
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let secret = f.kind == FieldKind::Secret;
            let r = ui
                .horizontal(|ui| {
                    let width = if f.kind == FieldKind::Path {
                        ui.available_width() - 90.0
                    } else {
                        ui.available_width()
                    };
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut text)
                            .id_salt(&salt)
                            .password(secret)
                            .desired_width(width.max(80.0)),
                    );
                    if f.kind == FieldKind::Path
                        && widgets::button(ui, "Browse", Kind::Outline, true, 0.0).clicked()
                        && let Some(path) = rfd::FileDialog::new().pick_file()
                    {
                        text = path.display().to_string();
                        return true;
                    }
                    r.changed()
                })
                .inner;
            if r {
                map.insert(f.key.clone(), Value::String(text));
                changed = true;
            }
        }
        FieldKind::Number { min, max, integer } => {
            let mut v = current.as_ref().and_then(Value::as_f64).unwrap_or(0.0);
            let before = v;
            if let (Some(lo), Some(hi)) = (min, max) {
                ui.spacing_mut().slider_width = ui.available_width() - 70.0;
                let mut s = egui::Slider::new(&mut v, *lo..=*hi);
                if *integer {
                    s = s.step_by(1.0);
                }
                ui.add(s);
            } else {
                let mut d = egui::DragValue::new(&mut v);
                if *integer {
                    d = d.speed(1.0).max_decimals(0);
                }
                if let Some(lo) = min {
                    d = d.range(*lo..=f64::MAX);
                }
                if let Some(hi) = max {
                    d = d.range(f64::MIN..=*hi);
                }
                ui.add(d);
            }
            if (v - before).abs() > f64::EPSILON || !map.contains_key(&f.key) && current.is_some() {
                let val = if *integer {
                    serde_json::json!(v.round() as i64)
                } else {
                    serde_json::json!(v)
                };
                if map.get(&f.key) != Some(&val) {
                    map.insert(f.key.clone(), val);
                    changed = true;
                }
            }
        }
        FieldKind::Bool => {
            let mut b = current.as_ref().and_then(Value::as_bool).unwrap_or(false);
            if ui.checkbox(&mut b, "").changed() {
                map.insert(f.key.clone(), Value::Bool(b));
                changed = true;
            }
        }
        FieldKind::Choice(options) => {
            let shown = |v: &Value| v.as_str().map_or_else(|| v.to_string(), str::to_owned);
            let now = current.as_ref().map(shown).unwrap_or_default();
            egui::ComboBox::from_id_salt(&salt)
                .selected_text(now)
                .show_ui(ui, |ui| {
                    for o in options {
                        let on = current.as_ref() == Some(o);
                        if ui.selectable_label(on, shown(o)).clicked() {
                            map.insert(f.key.clone(), o.clone());
                            changed = true;
                        }
                    }
                });
        }
    }
    if !f.help.is_empty() {
        ui.add(egui::Label::new(RichText::new(&f.help).font(mono(11.5)).color(p.faint)).wrap());
    }
    ui.add_space(4.0);
    changed
}

fn status_badge(ui: &mut Ui, s: &Status) {
    let (text, tip) = match s {
        Status::Enabled => ("On", "Approved and unchanged: it can run."),
        Status::NotEnabled => ("Off", "Found, but not approved. It never starts."),
        Status::Changed(_) => (
            "Changed",
            "Its program or manifest differs from what you approved. It is off until you approve it again.",
        ),
        Status::Damaged => (
            "Damaged",
            "Its program does not match the checksum its own manifest states.",
        ),
    };
    widgets::badge(ui, text, tip);
}

impl App {
    /// Draw the Plugins page in `ui` (the centre panel).
    pub(crate) fn plugins_page(&mut self, ui: &mut Ui) {
        let p = palette();
        let mut close = false;
        ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Plugins").font(semibold(22.0)).color(p.text));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if widgets::button(ui, "Close", Kind::Outline, true, 0.0)
                            .on_hover_text("Back to the viewer (Esc)")
                            .clicked()
                        {
                            close = true;
                        }
                        if widgets::button(ui, "Look again", Kind::Outline, true, 0.0)
                            .on_hover_text("Read the plugins folder again")
                            .clicked()
                        {
                            self.plugins.rescan();
                        }
                    });
                });
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            "Plugins add steps to the pipeline or stack somewhere else. A plugin is a \
                             program and runs with your rights: it can read and write your files and use \
                             the network. Helia does not sandbox it, and nothing runs until you enable it. \
                             Enable only plugins you trust.",
                        )
                        .color(p.muted),
                    )
                    .wrap(),
                );
                ui.add_space(4.0);
                if let Some(n) = self.plugins.note.clone() {
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!("Could not save the approvals: {n}"))
                                .color(p.accent),
                        )
                        .wrap(),
                    );
                }
                let ctx = ui.ctx().clone();
                let total = ui.available_width();
                let list_w = 300.0_f32.min(total * 0.4);
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(list_w);
                        self.plugin_list(ui);
                    });
                    ui.add_space(12.0);
                    ui.vertical(|ui| {
                        ui.set_width((total - list_w - 24.0).max(280.0));
                        self.plugin_detail(ui, &ctx);
                    });
                });
            });
        if close {
            self.show_plugins = false;
        }
    }

    fn plugin_list(&mut self, ui: &mut Ui) {
        let p = palette();
        widgets::overline(ui, "Found");
        if self.plugins.registry.found.is_empty() {
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "No plugins found. Put a plugin folder in your plugins folder, or start Helia \
                         with --plugin PATH.",
                    )
                    .color(p.muted),
                )
                .wrap(),
            );
            if let Some(dir) = helia_plugin::registry::plugins_dir() {
                ui.add(
                    egui::Label::new(
                        RichText::new(self.plugins.shown(&dir))
                            .font(mono(11.5))
                            .color(p.faint),
                    )
                    .wrap(),
                );
            }
            return;
        }
        let rows: Vec<(String, String, Option<Status>, Option<String>)> = self
            .plugins
            .registry
            .found
            .iter()
            .map(|f| match &f.loaded {
                Ok(l) => (
                    l.manifest.name.clone(),
                    if l.manifest.title.is_empty() {
                        l.manifest.name.clone()
                    } else {
                        l.manifest.title.clone()
                    },
                    Some(self.plugins.registry.approvals.status(l)),
                    None,
                ),
                Err((path, e)) => (
                    path.display().to_string(),
                    path.file_name()
                        .map_or_else(|| "?".into(), |n| n.to_string_lossy().into_owned()),
                    None,
                    Some(e.0.clone()),
                ),
            })
            .collect();
        for (name, title, status, problem) in rows {
            let selected = self.plugins.selected.as_deref() == Some(name.as_str());
            let fill = if selected { p.selected_fill } else { p.panel };
            let stroke = Stroke::new(
                1.0,
                if selected {
                    p.selected_border
                } else {
                    p.border
                },
            );
            let r = Frame::new()
                .fill(fill)
                .stroke(stroke)
                .corner_radius(RADIUS_LARGE)
                .inner_margin(Margin::same(10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.add(egui::Label::new(
                            RichText::new(&title).font(semibold(14.0)).color(p.text),
                        ));
                        if let Some(s) = &status {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| status_badge(ui, s),
                            );
                        }
                    });
                    if let Some(msg) = &problem {
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("Cannot be used: {msg}"))
                                    .font(mono(11.5))
                                    .color(p.accent),
                            )
                            .wrap(),
                        );
                    } else if let Some(l) = self.plugins.registry.get(&name) {
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!(
                                    "{} {}",
                                    l.manifest.name,
                                    l.manifest.capabilities.join(", ")
                                ))
                                .font(mono(11.5))
                                .color(p.faint),
                            )
                            .truncate(),
                        );
                    }
                });
            if r.response.interact(egui::Sense::click()).clicked() && problem.is_none() {
                self.plugins.selected = Some(name);
                self.plugins.confirming = None;
            }
        }
    }

    fn plugin_detail(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let p = palette();
        let Some(name) = self.plugins.selected.clone() else {
            ui.add(egui::Label::new(
                RichText::new("Choose a plugin on the left.").color(p.muted),
            ));
            return;
        };
        let Some(l) = self.plugins.registry.get(&name).cloned() else {
            return;
        };
        let status = self.plugins.registry.approvals.status(&l);
        let m = &l.manifest;
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(if m.title.is_empty() {
                    &m.name
                } else {
                    &m.title
                })
                .font(semibold(18.0))
                .color(p.text),
            );
            status_badge(ui, &status);
        });
        if !m.description.is_empty() {
            ui.add(egui::Label::new(RichText::new(&m.description).color(p.muted)).wrap());
        }
        let on = matches!(status, Status::Enabled);
        if !on {
            // What is about to be trusted comes first, then the button.
            self.plugin_facts(ui, &name, &l);
        }
        ui.add_space(6.0);
        self.plugin_actions(ui, ctx, &name, &status);
        if on {
            self.plugin_test_result(ui, &name);
            if !l.settings.fields.is_empty() {
                self.plugin_settings(ui, &name, &l.settings);
            }
            ui.add_space(6.0);
            egui::CollapsingHeader::new(RichText::new("Details").font(semibold(13.0)))
                .id_salt(format!("plugin-details-{name}"))
                .show(ui, |ui| self.plugin_facts(ui, &name, &l));
        }
    }

    /// Where a plugin is from, its checksums, what it can do and what it asks for.
    fn plugin_facts(&self, ui: &mut Ui, name: &str, l: &Loaded) {
        let p = palette();
        let m = &l.manifest;
        let digests = l.digests().ok();
        let source = match self
            .plugins
            .registry
            .found
            .iter()
            .find(|f| f.name() == Some(name))
            .map(|f| f.source)
        {
            Some(Source::Argument) => "named with --plugin",
            _ => "your plugins folder",
        };
        let rows: Vec<(&str, String)> = vec![
            ("Name and version", format!("{} {}", m.name, m.version)),
            ("Author and licence", format!("{}  {}", m.author, m.licence)),
            (
                "Source",
                format!("{source}: {}", self.plugins.shown(&l.path)),
            ),
            ("Program", self.plugins.shown(l.exe_path())),
            (
                "Interpreter",
                m.interpreter
                    .as_ref()
                    .map(|i| format!("{i} (looked up on the PATH; not part of the checksum)"))
                    .unwrap_or_default(),
            ),
            (
                "SHA-256 of the program",
                digests.as_ref().map(|d| d.exe.clone()).unwrap_or_default(),
            ),
            (
                "SHA-256 of the manifest and its files",
                digests
                    .as_ref()
                    .map(|d| d.manifest.clone())
                    .unwrap_or_default(),
            ),
            (
                "Signature",
                "unsigned (not checked in API version 1)".into(),
            ),
            ("Can do", m.capabilities.join(", ")),
            (
                "Asks for",
                format!(
                    "GPU {}, about {} MB of memory (hints only)",
                    if m.resources.gpu {
                        "wanted"
                    } else {
                        "not needed"
                    },
                    m.resources.memory_mb
                ),
            ),
            (
                "Time limits",
                format!(
                    "{} s without a message, {} s in all",
                    m.limits.timeout_s, m.limits.max_total_s
                ),
            ),
            (
                "Updates",
                m.update
                    .as_ref()
                    .map(|u| u.manifest_url.clone())
                    .unwrap_or_default(),
            ),
        ];
        ui.add_space(4.0);
        for (k, v) in rows.into_iter().filter(|(_, v)| !v.trim().is_empty()) {
            // The name and the value in one wrapping label, so rows stay close together.
            let mut job = egui::text::LayoutJob::default();
            job.append(
                &format!("{k}:  "),
                0.0,
                egui::TextFormat::simple(semibold(12.0), p.faint),
            );
            job.append(&v, 0.0, egui::TextFormat::simple(mono(12.0), p.text));
            ui.add(egui::Label::new(job).selectable(true).wrap());
        }
        for n in &l.notes {
            ui.add(egui::Label::new(RichText::new(n).font(mono(11.5)).color(p.faint)).wrap());
        }
        if !l.steps.is_empty() {
            ui.add_space(4.0);
            widgets::overline(ui, "Steps");
            for s in &l.steps {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!(
                            "{}/{}  ({})  {}",
                            m.name, s.step.id, s.step.capability, s.step.description
                        ))
                        .font(mono(12.0))
                        .color(p.muted),
                    )
                    .wrap(),
                );
            }
        }
    }

    fn plugin_test_result(&self, ui: &mut Ui, name: &str) {
        let p = palette();
        let Some(t) = self.plugins.tests.get(name).cloned() else {
            return;
        };
        widgets::overline(ui, "Test");
        match t {
            TestState::Running => {
                ui.add(egui::Label::new(
                    RichText::new("Starting the plugin and running its check\u{2026}")
                        .color(p.muted),
                ));
            }
            TestState::Done { ok, message, log } => {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!(
                            "{}: {message}",
                            if ok { "Passed" } else { "Failed" }
                        ))
                        .color(if ok { p.success } else { p.accent }),
                    )
                    .wrap(),
                );
                if !log.trim().is_empty() {
                    egui::CollapsingHeader::new("Plugin log")
                        .id_salt(format!("plugin-log-{name}"))
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(log.trim_end())
                                        .font(mono(11.0))
                                        .color(p.faint),
                                )
                                .selectable(true),
                            );
                        });
                }
            }
        }
    }

    fn plugin_actions(&mut self, ui: &mut Ui, ctx: &egui::Context, name: &str, status: &Status) {
        let p = palette();
        let confirming = self.plugins.confirming.as_deref() == Some(name);
        if confirming {
            Frame::new()
                .fill(p.selected_fill)
                .stroke(Stroke::new(1.0, p.selected_border))
                .corner_radius(RADIUS_LARGE)
                .inner_margin(Margin::same(12))
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(
                                "Check the source and the checksums above. This plugin will run with \
                                 your rights: it can read and write your files, use the network and run \
                                 other programs. Helia does not sandbox it. If the program or its \
                                 manifest changes, Helia switches it off until you enable it again.",
                            )
                            .color(p.text),
                        )
                        .wrap(),
                    );
                    ui.horizontal(|ui| {
                        if widgets::button(ui, "Enable plugin", Kind::Primary, true, 0.0).clicked() {
                            self.plugins.enable(name);
                        }
                        if widgets::button(ui, "Cancel", Kind::Outline, true, 0.0).clicked() {
                            self.plugins.confirming = None;
                        }
                    });
                });
            return;
        }
        ui.horizontal_wrapped(|ui| {
            match status {
                Status::Enabled => {
                    if widgets::button(ui, "Disable", Kind::Outline, true, 0.0).clicked() {
                        self.plugins.disable(name);
                    }
                    let running = matches!(self.plugins.tests.get(name), Some(TestState::Running));
                    if widgets::button(ui, "Test", Kind::Outline, !running, 0.0)
                        .on_hover_text("Start the plugin and run its own check")
                        .clicked()
                    {
                        self.plugins.test(name, ctx);
                    }
                }
                Status::NotEnabled | Status::Changed(_) => {
                    let label = if matches!(status, Status::Changed(_)) {
                        "Review and enable again\u{2026}"
                    } else {
                        "Enable\u{2026}"
                    };
                    if widgets::button(ui, label, Kind::Primary, true, 0.0).clicked() {
                        self.plugins.confirming = Some(name.to_owned());
                    }
                }
                Status::Damaged => {
                    ui.add(egui::Label::new(
                        RichText::new("Download the plugin again; it cannot be enabled as it is.")
                            .color(p.accent),
                    ));
                }
            }
            if self.plugins.registry.approvals.get(name).is_some()
                && widgets::button(ui, "Forget", Kind::Outline, true, 0.0)
                    .on_hover_text("Remove its approval and its settings")
                    .clicked()
            {
                self.plugins.registry.approvals.remove(name);
                self.plugins.save();
            }
        });
        if let Status::Changed(part) = status {
            ui.add(
                egui::Label::new(
                    RichText::new(format!(
                        "Its {part} has changed since you approved it, so it is switched off. If you \
                         updated it on purpose, review it and enable it again."
                    ))
                    .color(p.accent),
                )
                .wrap(),
            );
        }
    }

    fn plugin_settings(&mut self, ui: &mut Ui, name: &str, schema: &Schema) {
        let p = palette();
        widgets::overline(ui, "Settings");
        let stored = self.plugins.registry.approvals.settings(name);
        let mut form = self
            .plugins
            .forms
            .remove(name)
            .unwrap_or_else(|| schema.validate(&stored).unwrap_or(stored));
        // A form is easier to read narrow than across a wide window.
        let width = ui.available_width().min(560.0);
        ui.allocate_ui(egui::vec2(width, 0.0), |ui| {
            ui.set_width(width);
            schema_form(ui, &format!("settings-{name}"), schema, &mut form);
            if let Some(errs) = self.plugins.errors.get(name) {
                for e in errs {
                    ui.add(egui::Label::new(RichText::new(e).color(p.accent)).wrap());
                }
            }
            if widgets::button(ui, "Save settings", Kind::Primary, true, 0.0).clicked() {
                match schema.validate(&form) {
                    Ok(clean) => {
                        self.plugins.registry.approvals.set_settings(name, &clean);
                        self.plugins.save();
                        self.plugins.errors.remove(name);
                        form = clean;
                    }
                    Err(problems) => {
                        self.plugins.errors.insert(name.to_owned(), problems);
                    }
                }
            }
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "Settings, including any token, are kept in plugins.toml in Helia's settings folder, readable by you only on Linux and macOS.",
                    )
                    .font(mono(11.5))
                    .color(p.faint),
                )
                .wrap(),
            );
        });
        self.plugins.forms.insert(name.to_owned(), form);
    }
}
