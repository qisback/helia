// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `helia plugins` and the plugin options of `helia stack` and `helia edit`.
//!
//! The protocol and the host are in the `helia-plugin` crate; this module is the command line on
//! top of it: listing, approving and testing plugins, and running plugin steps in a pipeline.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use helia_core::frame::Frame;
use helia_core::io::source::FrameSource;
use helia_plugin::manifest::Loaded;
use helia_plugin::registry::{Source, Status};
use helia_plugin::schema::FieldKind;
use helia_plugin::{Analysis, PluginFrames, Progress, Registry, Runner, StepSpec, StepStage};
use serde_json::{Value, json};

/// Plugin options shared by `stack` and `edit`.
#[derive(Args, Clone, Debug, Default)]
pub struct PluginRunArgs {
    /// Run a step of a plugin: `PLUGIN/STEP`, optionally followed by `:KEY=VALUE,KEY=VALUE` or
    /// `:{"json":"object"}` for its parameters. Repeat for several steps, which run in the order
    /// given. A `frame-op` step runs on every frame before stacking (`stack` only); an `image-op`
    /// step runs on the stack after Helia's own sharpening and edits, or before them when written
    /// `pre:PLUGIN/STEP`; an `analyse` step reads the result and writes `OUTPUT.analysis.json`. The
    /// plugin must be enabled first (`helia plugins enable`).
    #[arg(long = "plugin-step", value_name = "SPEC")]
    pub plugin_steps: Vec<String>,
    /// Stack with this `stack-backend` plugin instead of on this computer (`stack` only). The
    /// plugin gets the capture and the stacking settings; sharpening, colour and export stay in
    /// Helia. True colour, calibration and field rotation are not available with a backend.
    #[arg(long, value_name = "PLUGIN")]
    pub stack_backend: Option<String>,
}

impl PluginRunArgs {
    /// Nothing about plugins was asked for.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.plugin_steps.is_empty() && self.stack_backend.is_none()
    }
}

/// `helia plugins`: list, inspect, approve and test plugins.
#[derive(Args, Debug)]
pub struct PluginsArgs {
    /// What to do.
    #[command(subcommand)]
    pub action: PluginsAction,
}

/// Actions of `helia plugins`.
#[derive(Subcommand, Debug)]
pub enum PluginsAction {
    /// List the plugins Helia found (in the plugins folder and named with `--plugin`) and
    /// whether each is on. Finding a plugin only lists it; nothing runs until you enable it.
    List {
        /// Print JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Show everything about one plugin: where it is, its checksums, what it can do, its
    /// settings and the steps it offers.
    Info {
        /// Plugin name, as `helia plugins list` shows it.
        name: String,
    },
    /// Approve a plugin and switch it on. Shows where it comes from, its SHA-256 and what it
    /// asks for, and asks you to confirm. A plugin runs with your rights, not in a sandbox.
    /// If its program or manifest changes later, it is switched off until you enable it again.
    Enable {
        /// Plugin name.
        name: String,
        /// Do not ask. The checksum is still recorded.
        #[arg(short, long)]
        yes: bool,
    },
    /// Switch a plugin off. Its settings are kept.
    Disable {
        /// Plugin name.
        name: String,
    },
    /// Forget a plugin: its approval and its settings.
    Forget {
        /// Plugin name.
        name: String,
    },
    /// Set the plugin's settings, `KEY=VALUE` pairs checked against its settings form. A value
    /// of `-` for a secret reads it from standard input, so it stays out of your shell history.
    Set {
        /// Plugin name.
        name: String,
        /// The settings to change.
        #[arg(value_name = "KEY=VALUE", required = true)]
        pairs: Vec<String>,
    },
    /// Start a plugin and run its own check (for example, can it reach its server), then show
    /// its log. The plugin must be enabled.
    Test {
        /// Plugin name.
        name: String,
    },
}

static PATHS: std::sync::OnceLock<Vec<PathBuf>> = std::sync::OnceLock::new();

/// Remember the `--plugin` paths of this run (set once, from the command line).
pub fn set_paths(paths: Vec<PathBuf>) {
    let _ = PATHS.set(paths);
}

fn paths() -> &'static [PathBuf] {
    PATHS.get().map_or(&[], Vec::as_slice)
}

fn status_text(s: &Status) -> String {
    match s {
        Status::NotEnabled => "off".into(),
        Status::Enabled => "on".into(),
        Status::Changed(part) => format!("changed ({part}); enable again"),
        Status::Damaged => "damaged".into(),
    }
}

fn source_text(s: Source) -> &'static str {
    match s {
        Source::UserFolder => "plugins folder",
        Source::Argument => "--plugin",
    }
}

fn load(reg: &Registry, name: &str) -> Result<Loaded> {
    reg.get(name).cloned().with_context(|| {
        format!(
            "no plugin called {name}. Look at `helia plugins list`, or name its folder with --plugin."
        )
    })
}

/// Run `helia plugins ...`.
///
/// # Errors
/// Unknown plugin, an answer of no, or a failed plugin call.
pub fn run(args: &PluginsArgs) -> Result<()> {
    let mut reg = Registry::open(paths());
    match &args.action {
        PluginsAction::List { json } => list(&reg, *json),
        PluginsAction::Info { name } => info(&reg, name),
        PluginsAction::Enable { name, yes } => {
            let l = load(&reg, name)?;
            enable(&mut reg, &l, *yes)
        }
        PluginsAction::Disable { name } => {
            if !reg.approvals.disable(name) {
                bail!("{name} is not enabled, so there is nothing to switch off");
            }
            reg.approvals.save().map_err(anyhow::Error::msg)?;
            println!("{name} is off.");
            Ok(())
        }
        PluginsAction::Forget { name } => {
            if !reg.approvals.remove(name) {
                bail!("{name} has no approval or settings to forget");
            }
            reg.approvals.save().map_err(anyhow::Error::msg)?;
            println!("{name} forgotten.");
            Ok(())
        }
        PluginsAction::Set { name, pairs } => {
            let l = load(&reg, name)?;
            set(&mut reg, &l, pairs)
        }
        PluginsAction::Test { name } => test(reg, name),
    }
}

fn list(reg: &Registry, json: bool) -> Result<()> {
    if json {
        let rows: Vec<Value> = reg
            .found
            .iter()
            .map(|f| match &f.loaded {
                Ok(l) => json!({
                    "name": l.manifest.name,
                    "version": l.manifest.version,
                    "title": l.manifest.title,
                    "status": reg.approvals.status(l).label(),
                    "capabilities": l.manifest.capabilities,
                    "source": source_text(f.source),
                    "manifest": l.path.display().to_string(),
                }),
                Err((p, e)) => json!({"error": e.0, "path": p.display().to_string()}),
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if reg.found.is_empty() {
        println!(
            "No plugins found. Put a plugin folder in {} or name one with --plugin.",
            helia_plugin::registry::plugins_dir()
                .map_or_else(|| "the plugins folder".into(), |p| p.display().to_string())
        );
        return Ok(());
    }
    println!(
        "{:<16} {:<8} {:<10} {:<26} SOURCE",
        "NAME", "VERSION", "STATUS", "CAPABILITIES"
    );
    for f in &reg.found {
        match &f.loaded {
            Ok(l) => println!(
                "{:<16} {:<8} {:<10} {:<26} {} ({})",
                l.manifest.name,
                l.manifest.version,
                reg.approvals.status(l).label(),
                l.manifest.capabilities.join(","),
                l.path.display(),
                source_text(f.source)
            ),
            Err((p, e)) => println!("{:<16} cannot be used: {} ({})", "?", e.0, p.display()),
        }
    }
    println!("\nOnly plugins marked `on` can run. Enable one with `helia plugins enable NAME`.");
    Ok(())
}

fn describe(l: &Loaded, out: &mut impl Write) -> Result<()> {
    let m = &l.manifest;
    let d = l.digests().map_err(|e| anyhow::anyhow!(e.0))?;
    writeln!(out, "name:         {}", m.name)?;
    if !m.title.is_empty() {
        writeln!(out, "title:        {}", m.title)?;
    }
    writeln!(out, "version:      {}", m.version)?;
    if !m.description.is_empty() {
        writeln!(out, "about:        {}", m.description)?;
    }
    if !m.author.is_empty() {
        writeln!(out, "author:       {}", m.author)?;
    }
    if !m.licence.is_empty() {
        writeln!(out, "licence:      {}", m.licence)?;
    }
    if !m.homepage.is_empty() {
        writeln!(out, "homepage:     {}", m.homepage)?;
    }
    writeln!(out, "manifest:     {}", l.path.display())?;
    writeln!(out, "program:      {}", l.exe_path().display())?;
    if let Some(i) = &m.interpreter {
        writeln!(
            out,
            "interpreter:  {i} (looked up on the PATH; not part of the checksum)"
        )?;
    }
    writeln!(out, "sha256:       {}  (program)", d.exe)?;
    writeln!(
        out,
        "              {}  (manifest and its files)",
        d.manifest
    )?;
    writeln!(
        out,
        "signature:    unsigned (checking signatures is not in API version 1)"
    )?;
    writeln!(out, "capabilities: {}", m.capabilities.join(", "))?;
    writeln!(
        out,
        "resources:    GPU {}, about {} MB of memory (hints only)",
        if m.resources.gpu {
            "wanted"
        } else {
            "not needed"
        },
        m.resources.memory_mb
    )?;
    writeln!(
        out,
        "limits:       {} s without a message, {} s in all",
        m.limits.timeout_s, m.limits.max_total_s
    )?;
    if let Some(u) = &m.update {
        writeln!(
            out,
            "updates:      {} (channel {}, signed with key {}...)",
            u.manifest_url,
            if u.channel.is_empty() {
                "default"
            } else {
                &u.channel
            },
            u.signing_key.get(..16).unwrap_or("")
        )?;
    }
    for s in &l.steps {
        writeln!(
            out,
            "step:         {}/{} [{}] {}",
            m.name, s.step.id, s.step.capability, s.step.description
        )?;
        for f in &s.params.fields {
            writeln!(out, "                {}", field_line(f))?;
        }
    }
    for f in &l.settings.fields {
        writeln!(out, "setting:      {}", field_line(f))?;
    }
    for n in &l.notes {
        writeln!(out, "note:         {n}")?;
    }
    Ok(())
}

fn field_line(f: &helia_plugin::schema::Field) -> String {
    let kind = match &f.kind {
        FieldKind::Text => "text".to_owned(),
        FieldKind::Secret => "secret text".to_owned(),
        FieldKind::Path => "path".to_owned(),
        FieldKind::Number { min, max, integer } => {
            let range = match (min, max) {
                (Some(a), Some(b)) => format!(" {a} to {b}"),
                (Some(a), None) => format!(" from {a}"),
                (None, Some(b)) => format!(" up to {b}"),
                (None, None) => String::new(),
            };
            format!(
                "{}{range}",
                if *integer { "whole number" } else { "number" }
            )
        }
        FieldKind::Bool => "yes/no".to_owned(),
        FieldKind::Choice(v) => format!(
            "one of {}",
            v.iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    let def = f
        .default
        .as_ref()
        .map_or(String::new(), |d| format!(", default {d}"));
    let req = if f.required { ", required" } else { "" };
    format!("{} ({kind}{def}{req}) {}", f.key, f.help)
}

fn info(reg: &Registry, name: &str) -> Result<()> {
    let l = load(reg, name)?;
    let mut out = std::io::stdout().lock();
    describe(&l, &mut out)?;
    writeln!(
        out,
        "status:       {}",
        status_text(&reg.approvals.status(&l))
    )?;
    let shown = helia_plugin::session::redacted(&l, &reg.approvals.settings(name));
    if shown.as_object().is_some_and(|m| !m.is_empty()) {
        writeln!(out, "settings now: {shown}")?;
    }
    Ok(())
}

fn ask(question: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        bail!(
            "cannot ask for confirmation here; run `helia plugins enable NAME --yes` to approve it"
        );
    }
    eprint!("{question} [y/N] ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn enable(reg: &mut Registry, l: &Loaded, yes: bool) -> Result<()> {
    let mut err = std::io::stderr().lock();
    writeln!(err, "You are about to enable this plugin:\n")?;
    describe(l, &mut err)?;
    writeln!(
        err,
        "\nA plugin is a program. It runs with your rights: it can read and write your files, use the\n\
         network and run other programs. Helia does not sandbox it. Enable only plugins you trust, and\n\
         compare the checksum above with the one its author publishes. If the program or its manifest\n\
         changes later, Helia switches it off until you enable it again.\n"
    )?;
    drop(err);
    if !yes && !ask("Enable this plugin?")? {
        bail!("not enabled");
    }
    let d = reg
        .approvals
        .approve(l)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    reg.approvals.save().map_err(anyhow::Error::msg)?;
    println!(
        "{} is on. Approved program checksum {}.",
        l.manifest.name, d.exe
    );
    Ok(())
}

fn set(reg: &mut Registry, l: &Loaded, pairs: &[String]) -> Result<()> {
    let name = l.manifest.name.clone();
    let mut now = reg.approvals.settings(&name);
    if !now.is_object() {
        now = json!({});
    }
    let Some(obj) = now.as_object_mut() else {
        bail!("settings are not an object");
    };
    for p in pairs {
        let (k, raw) = p
            .split_once('=')
            .with_context(|| format!("{p:?} should look like KEY=VALUE"))?;
        let field = l.settings.fields.iter().find(|f| f.key == k);
        let raw = if raw == "-" && field.is_some_and(|f| f.kind == FieldKind::Secret) {
            let mut s = String::new();
            std::io::stdin().lock().read_line(&mut s)?;
            s.trim_end_matches(['\r', '\n']).to_owned()
        } else {
            raw.to_owned()
        };
        let v = match field.map(|f| &f.kind) {
            Some(FieldKind::Bool) => Value::Bool(
                raw.parse::<bool>()
                    .with_context(|| format!("{k} is yes/no: write true or false"))?,
            ),
            Some(FieldKind::Number { integer: true, .. }) => json!(
                raw.parse::<i64>()
                    .with_context(|| format!("{k} must be a whole number"))?
            ),
            Some(FieldKind::Number { .. }) => json!(
                raw.parse::<f64>()
                    .ok()
                    .filter(|n| n.is_finite())
                    .with_context(|| format!("{k} must be a number"))?
            ),
            Some(FieldKind::Choice(options)) => options
                .iter()
                .find(|o| o.as_str() == Some(raw.as_str()) || o.to_string() == raw)
                .cloned()
                .with_context(|| format!("{k} is not one of the allowed values"))?,
            _ => Value::String(raw),
        };
        obj.insert(k.to_owned(), v);
    }
    let checked = l.settings.validate(&now).map_err(|p| {
        anyhow::anyhow!(
            "The settings are not right. {} What to do: correct them and try again.",
            p.join(" ")
        )
    })?;
    reg.approvals.set_settings(&name, &checked);
    reg.approvals.save().map_err(anyhow::Error::msg)?;
    println!("Settings of {name} saved.");
    Ok(())
}

fn test(reg: Registry, name: &str) -> Result<()> {
    let mut runner = Runner::new(reg);
    let cancel = AtomicBool::new(false);
    let r = runner.test(name, &cancel);
    let log = runner.log(name);
    runner.close_all();
    let r = match r {
        Ok(r) => r,
        Err(e)
            if e.kind
                == helia_plugin::Kind::Plugin(helia_plugin_sdk::ErrorKind::MethodNotFound) =>
        {
            println!("ok: {name} started and answered. It offers no check of its own.");
            if !log.trim().is_empty() {
                println!("\nlog of {name}:\n{}", log.trim_end());
            }
            return Ok(());
        }
        Err(e) => return Err(anyhow::anyhow!("{e}")),
    };
    println!("{}: {}", if r.ok { "ok" } else { "failed" }, r.message);
    if !log.trim().is_empty() {
        println!("\nlog of {name}:\n{}", log.trim_end());
    }
    if r.ok {
        Ok(())
    } else {
        bail!("the plugin's test failed")
    }
}

/// The plugins one `stack` or `edit` run uses.
pub struct PluginRun {
    runner: Arc<Mutex<Runner>>,
    specs: Vec<StepSpec>,
    backend: Option<String>,
    cancel: Arc<AtomicBool>,
    quiet: bool,
}

impl PluginRun {
    /// Check the plugin options before any work starts. `None` when no plugin is asked for.
    ///
    /// `frames_ok` is false for `edit`, which has no frames to run a `frame-op` step on.
    ///
    /// # Errors
    /// A step that cannot be parsed or used, or a plugin that is not enabled.
    pub fn new(args: &PluginRunArgs, frames_ok: bool, quiet: bool) -> Result<Option<Self>> {
        if args.is_empty() {
            return Ok(None);
        }
        let specs = args
            .plugin_steps
            .iter()
            .map(|t| StepSpec::parse(t).map_err(|e| anyhow::anyhow!("--plugin-step: {e}")))
            .collect::<Result<Vec<_>>>()?;
        let runner = Runner::new(Registry::open(paths()));
        for s in &specs {
            let cap = runner.check(s).map_err(|e| anyhow::anyhow!("{e}"))?;
            if cap == "frame-op" && !frames_ok {
                bail!(
                    "{}/{} works on frames before stacking, which `helia edit` does not have. \
                     Use it with `helia stack`, or choose an image step.",
                    s.plugin,
                    s.step
                );
            }
            if !quiet {
                eprintln!(
                    "plugin:   {}/{} ({}{})",
                    s.plugin,
                    s.step,
                    cap,
                    if cap == "image-op" {
                        format!(", {}", stage_words(s.stage))
                    } else {
                        String::new()
                    }
                );
            }
        }
        if let Some(b) = &args.stack_backend {
            let l = runner
                .registry
                .runnable(b)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            if !l.has("stack-backend") {
                bail!(
                    "The plugin {b} cannot stack: it does not offer the stack-backend capability."
                );
            }
            if !frames_ok {
                bail!("--stack-backend belongs to `helia stack`.");
            }
            if !quiet {
                eprintln!("plugin:   {b} (stack-backend: stacks on its own, not on this computer)");
            }
        }
        Ok(Some(Self {
            runner: Arc::new(Mutex::new(runner)),
            specs,
            backend: args.stack_backend.clone(),
            cancel: Arc::new(AtomicBool::new(false)),
            quiet,
        }))
    }

    /// The `stack-backend` plugin, if one was asked for.
    #[must_use]
    pub fn backend(&self) -> Option<&str> {
        self.backend.as_deref()
    }

    /// Does any `frame-op` step run? (They need the frames re-read through [`Self::wrap`].)
    #[must_use]
    pub fn has_frame_steps(&self) -> bool {
        self.lock()
            .frame_steps(&self.specs)
            .is_ok_and(|v| !v.is_empty())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Runner> {
        self.runner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Pass every frame of `src` through the `frame-op` steps. The second value is a handle to
    /// the plugin's own error, to read when the pipeline reports a failure.
    #[must_use]
    pub fn wrap(
        &self,
        src: Box<dyn FrameSource>,
    ) -> (
        Box<dyn FrameSource>,
        Arc<Mutex<Option<helia_plugin::HostError>>>,
    ) {
        let steps = self.lock().frame_steps(&self.specs).unwrap_or_default();
        if steps.is_empty() {
            return (src, Arc::new(Mutex::new(None)));
        }
        let frames = PluginFrames::new(
            src,
            Arc::clone(&self.runner),
            steps,
            Arc::clone(&self.cancel),
        );
        let failure = frames.failure();
        (Box::new(frames), failure)
    }

    fn progress(&self) -> impl FnMut(&StepSpec, Progress) + '_ {
        let mut last = String::new();
        move |spec, p| {
            if let Some(s) = p.status.filter(|s| !s.is_empty() && *s != last)
                && !self.quiet
            {
                eprintln!("plugin:   {}/{}: {s}", spec.plugin, spec.step);
                last = s;
            }
        }
    }

    /// Run the `image-op` steps for `stage` on `frame`.
    ///
    /// # Errors
    /// The plugin's error, in Helia's What / Why / What to do form.
    pub fn image(&self, stage: StepStage, frame: Frame) -> Result<Frame> {
        let mut p = self.progress();
        self.lock()
            .image_steps(&self.specs, stage, frame, &self.cancel, &mut p)
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// Are there `image-op` steps for `stage`?
    #[must_use]
    pub fn has_image(&self, stage: StepStage) -> bool {
        let r = self.lock();
        self.specs
            .iter()
            .any(|s| s.stage == stage && r.check(s).is_ok_and(|c| c == "image-op"))
    }

    /// Are there `analyse` steps?
    #[must_use]
    pub fn has_analyse(&self) -> bool {
        let r = self.lock();
        self.specs
            .iter()
            .any(|s| r.check(s).is_ok_and(|c| c == "analyse"))
    }

    /// A line for the file's history naming the `image-op` steps that ran at `stage`.
    #[must_use]
    pub fn describe_image_steps(&self, stage: StepStage) -> String {
        let r = self.lock();
        let names: Vec<String> = self
            .specs
            .iter()
            .filter(|s| s.stage == stage && r.check(s).is_ok_and(|c| c == "image-op"))
            .map(|s| format!("{}/{}", s.plugin, s.step))
            .collect();
        format!(
            "Plugin steps ({}): {}",
            stage_words(stage),
            names.join(", ")
        )
    }

    /// Run the `analyse` steps on `frame` and write `path` (JSON) if any ran.
    ///
    /// # Errors
    /// The plugin's error, or a file that cannot be written.
    pub fn analyse(&self, frame: &Frame, path: &Path) -> Result<Vec<(StepSpec, Analysis)>> {
        let mut p = self.progress();
        let done = self
            .lock()
            .analyse_steps(&self.specs, frame, &self.cancel, &mut p)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        if done.is_empty() {
            return Ok(done);
        }
        let doc: Vec<Value> = done
            .iter()
            .map(|(s, a)| {
                json!({
                    "plugin": s.plugin, "step": s.step, "params": s.params,
                    "metrics": a.metrics, "overlays": a.overlays, "notes": a.notes,
                })
            })
            .collect();
        std::fs::write(path, serde_json::to_string_pretty(&doc)?)
            .with_context(|| format!("cannot write {}", path.display()))?;
        if !self.quiet {
            for (s, a) in &done {
                for (k, v) in &a.metrics {
                    eprintln!("plugin:   {}/{}: {k} = {v}", s.plugin, s.step);
                }
                for n in &a.notes {
                    eprintln!("plugin:   {}/{}: {n}", s.plugin, s.step);
                }
            }
            eprintln!("wrote:    {}", path.display());
        }
        Ok(done)
    }

    /// Have the `stack-backend` plugin stack `input` with `job`.
    ///
    /// # Errors
    /// The plugin's error.
    pub fn stack(&self, input: &Path, job: &Value) -> Result<helia_plugin::Stacked> {
        let name = self
            .backend
            .clone()
            .context("no stack-backend plugin was asked for")?;
        let mut last = String::new();
        let quiet = self.quiet;
        let who = name.clone();
        let mut cb = move |p: Progress| {
            if let Some(s) = p.status.filter(|s| !s.is_empty() && *s != last)
                && !quiet
            {
                eprintln!("plugin:   {who}: {s}");
                last = s;
            }
        };
        self.lock()
            .stack(&name, input, job, &self.cancel, &mut cb)
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// Lines for the history of the saved file about the plugins that took part.
    #[must_use]
    pub fn provenance(&self) -> Vec<String> {
        self.lock().provenance()
    }

    /// FITS cards for the plugins that took part.
    #[must_use]
    pub fn fits_cards(&self) -> Vec<helia_core::io::fits::HeaderCard> {
        self.lock().fits_cards()
    }

    /// Stop the plugins.
    pub fn close(&self) {
        self.lock().close_all();
    }
}

fn stage_words(s: StepStage) -> &'static str {
    match s {
        StepStage::PreEdit => "before Helia's edits",
        StepStage::PostEdit => "after Helia's edits",
    }
}
