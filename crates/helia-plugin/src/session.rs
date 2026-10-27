// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! One running plugin: the process, the JSON-RPC conversation and the typed calls.
//!
//! The host is the only one that starts requests, one at a time. While a call is waiting it
//! enforces the timeouts, forwards progress, turns the user's cancel into a `cancel` notification
//! and, if the plugin does not obey within five seconds, kills it. A plugin that exits, hangs or
//! talks nonsense becomes a [`HostError`], never a panic or a hang.

use std::collections::BTreeMap;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use helia_core::frame::{BayerPattern, Frame};
use helia_core::io::fits::HeaderCard;
use helia_plugin_sdk::hlpx::{self, DEFAULT_MAX_BYTES, Header, Image};
use helia_plugin_sdk::wire::{self, MAX_LINE, Message, WireError};
use helia_plugin_sdk::{AnalyseResult, ErrorKind, Info, PluginError, TestResult, code};
use serde_json::{Value, json};
use std::io::Write;

use crate::error::{HostError, Kind};
use crate::manifest::Loaded;
use crate::registry::Registry;

const GRACE: Duration = Duration::from_secs(5);
const LOG_KEEP: usize = 64 * 1024;
const MAX_METRICS: usize = 1000;
const MAX_OVERLAYS: usize = 10_000;
const MAX_NOTES: usize = 100;
const OVERLAY_KINDS: &[&str] = &["circle", "rect", "line", "points", "text"];

/// A progress report from a plugin.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Progress {
    /// 0 to 1.
    pub fraction: Option<f64>,
    /// A line of text for the status area.
    pub status: Option<String>,
    /// One of Helia's stage names.
    pub stage: Option<String>,
}

#[derive(Default)]
struct Log {
    text: String,
}

impl Log {
    fn push(&mut self, s: &str) {
        self.text.push_str(s);
        if !s.ends_with('\n') {
            self.text.push('\n');
        }
        if self.text.len() > LOG_KEEP {
            let mut cut = self.text.len() - LOG_KEEP;
            while !self.text.is_char_boundary(cut) {
                cut += 1;
            }
            self.text.drain(..cut);
        }
    }
}

enum Incoming {
    Msg(Message),
    Bad(WireError),
    Eof,
}

/// The result of `analyse.run`, bounded and checked.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Analysis {
    /// Named numbers.
    pub metrics: BTreeMap<String, f64>,
    /// Shapes to draw.
    pub overlays: Vec<Value>,
    /// Lines of text.
    pub notes: Vec<String>,
}

/// The stack a `stack-backend` plugin made.
#[derive(Clone, Debug)]
pub struct Stacked {
    /// The stacked mono image.
    pub frame: Frame,
    /// The plugin's note for the summary.
    pub note: Option<String>,
}

/// A started, initialised plugin.
pub struct Session {
    loaded: Loaded,
    settings: Value,
    child: Child,
    stdin: Option<ChildStdin>,
    rx: Receiver<Incoming>,
    log: Arc<Mutex<Log>>,
    /// Set when the reader of the plugin's standard error has seen the end of it.
    err_done: Arc<AtomicBool>,
    workdir: tempfile::TempDir,
    next_id: i64,
    counter: u64,
    max_buffer: u64,
    dead: bool,
    /// The steps that ran in this session, in the order they first ran.
    used: Vec<String>,
    /// The latest `meta` each step returned.
    metas: BTreeMap<String, Value>,
    /// What the plugin said in answer to `initialize`.
    pub info: Info,
}

fn name_of(l: &Loaded) -> &str {
    &l.manifest.name
}

/// Start the process, trying again for a moment when the system says the program is busy.
///
/// `ETXTBSY` ("Text file busy", os error 26) happens when the program was written a moment ago
/// (a plugin just installed or copied) and another thread of this process forked while the file
/// was still open for writing: the child inherits that descriptor until it execs. It clears
/// within milliseconds, so a short retry is the right answer, not an error to the user.
fn spawn_retrying(cmd: &mut Command) -> std::io::Result<Child> {
    let mut tries = 0;
    loop {
        match cmd.spawn() {
            Err(e) if e.raw_os_error() == Some(26) && tries < 40 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(25));
            }
            other => return other,
        }
    }
}

impl Session {
    /// Check that the plugin is approved and unchanged, start it and initialise it with the stored
    /// settings.
    ///
    /// # Errors
    /// [`HostError`]: not enabled, changed, damaged, bad settings, could not start, or a failed
    /// handshake.
    pub fn start(reg: &Registry, name: &str) -> Result<Self, HostError> {
        let loaded = reg.runnable(name)?.clone();
        let stored = reg.approvals.settings(name);
        Self::start_with(&loaded, &stored)
    }

    /// As [`Session::start`] but with settings given by the caller. The approval was checked by
    /// whoever got `loaded` from [`Registry::runnable`].
    ///
    /// # Errors
    /// See [`Session::start`].
    #[allow(clippy::too_many_lines)]
    pub fn start_with(loaded: &Loaded, settings: &Value) -> Result<Self, HostError> {
        let name = name_of(loaded).to_owned();
        let settings = loaded.settings.validate(settings).map_err(|problems| {
            HostError::refused(
                &name,
                format!("The settings of the plugin {name} are not complete."),
                problems.join(" "),
                "Open the plugin's settings on the Plugins page and correct them.",
            )
        })?;
        let workdir = tempfile::Builder::new()
            .prefix("helia-plugin-")
            .tempdir()
            .map_err(|e| HostError::invalid(&name, format!("cannot make a working folder: {e}")))?;
        let m = &loaded.manifest;
        let mut cmd = match &m.interpreter {
            Some(i) => {
                let mut c = Command::new(i);
                c.arg(loaded.exe_path());
                c
            }
            None => Command::new(loaded.exe_path()),
        };
        cmd.args(&m.args)
            .current_dir(&loaded.dir)
            .env("HELIA_PLUGIN_API", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        let mut child = spawn_retrying(&mut cmd).map_err(|e| {
            HostError::new(
                Kind::Spawn,
                &name,
                format!("The plugin {name} could not be started."),
                format!("The system said: {e}."),
                "Check that the program is there and that you may run it (on Linux and macOS it needs the executable permission).",
            )
        })?;
        let stdin = child.stdin.take();
        let (tx, rx) = mpsc::channel();
        let log = Arc::new(Mutex::new(Log::default()));
        if let Some(out) = child.stdout.take() {
            std::thread::spawn(move || {
                let mut r = BufReader::new(out);
                loop {
                    match wire::read_line_limited(&mut r, MAX_LINE) {
                        Ok(Some(line)) => match wire::parse_message(&line) {
                            Ok(m) => {
                                if tx.send(Incoming::Msg(m)).is_err() {
                                    return;
                                }
                            }
                            Err(WireError::Empty) => {}
                            Err(e) => {
                                let _ = tx.send(Incoming::Bad(e));
                                return;
                            }
                        },
                        Ok(None) => {
                            let _ = tx.send(Incoming::Eof);
                            return;
                        }
                        Err(e) => {
                            let _ = tx.send(Incoming::Bad(e));
                            return;
                        }
                    }
                }
            });
        }
        let err_done = Arc::new(AtomicBool::new(false));
        if let Some(mut err) = child.stderr.take() {
            let log = Arc::clone(&log);
            let done = Arc::clone(&err_done);
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(n) = err.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    if let Ok(mut l) = log.lock() {
                        l.push(&String::from_utf8_lossy(&buf[..n]));
                    }
                }
                done.store(true, Ordering::SeqCst);
            });
        } else {
            err_done.store(true, Ordering::SeqCst);
        }
        let mut s = Self {
            loaded: loaded.clone(),
            settings,
            child,
            stdin,
            rx,
            log,
            err_done,
            workdir,
            next_id: 0,
            counter: 0,
            max_buffer: DEFAULT_MAX_BYTES,
            dead: false,
            used: Vec::new(),
            metas: BTreeMap::new(),
            info: Info::new("", "", &[]),
        };
        s.handshake()?;
        Ok(s)
    }

    fn handshake(&mut self) -> Result<(), HostError> {
        let name = name_of(&self.loaded).to_owned();
        let params = json!({
            "api_version": helia_plugin_sdk::API_VERSION,
            "host": {"name": "helia", "version": env!("CARGO_PKG_VERSION")},
            "workdir": self.workdir.path().display().to_string(),
            "settings": self.settings,
            "limits": {"max_buffer_bytes": self.max_buffer},
            "locale": "en-GB",
        });
        let never = AtomicBool::new(false);
        let v = self.call(
            "initialize",
            params,
            Duration::from_secs(30),
            &never,
            &mut |_| {},
        )?;
        let info: Info = serde_json::from_value(v).map_err(|e| {
            HostError::protocol(
                &name,
                format!("its answer to initialize is not valid ({e})"),
            )
        })?;
        if info.api_version != helia_plugin_sdk::API_VERSION {
            self.kill();
            return Err(HostError::new(
                Kind::Protocol,
                &name,
                format!("The plugin {name} does not speak the same plugin version as Helia."),
                format!(
                    "It speaks version {}; this Helia speaks version {}.",
                    info.api_version,
                    helia_plugin_sdk::API_VERSION
                ),
                "Update the plugin, or use a Helia that supports it.",
            ));
        }
        if info.name != name {
            self.kill();
            return Err(HostError::protocol(
                &name,
                format!(
                    "it calls itself {:?}, not {name}",
                    helia_plugin_sdk::clip(&info.name, 40)
                ),
            ));
        }
        let confirmed: Vec<String> = self
            .loaded
            .manifest
            .capabilities
            .iter()
            .filter(|c| info.capabilities.contains(c))
            .cloned()
            .collect();
        self.info = Info {
            capabilities: confirmed,
            ..info
        };
        Ok(())
    }

    /// The plugin's manifest.
    #[must_use]
    pub fn loaded(&self) -> &Loaded {
        &self.loaded
    }

    /// The plugin's log: its standard error and `log` notifications (last 64 KiB).
    #[must_use]
    pub fn log(&self) -> String {
        self.log.lock().map(|l| l.text.clone()).unwrap_or_default()
    }

    /// True if the process has stopped or was stopped.
    #[must_use]
    pub fn is_dead(&mut self) -> bool {
        self.dead || self.child.try_ready()
    }

    fn log_tail(&self) -> String {
        // The end of standard output and the end of standard error reach us on different
        // threads; a crash error built from the first must still quote the last words that
        // came on the second. Wait for the reader to finish, but not for long: a plugin that
        // left a child holding its standard error open must not stall the error.
        let t0 = Instant::now();
        while !self.err_done.load(Ordering::SeqCst) && t0.elapsed() < Duration::from_secs(1) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let log = self.log();
        let last = log
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        helia_plugin_sdk::clip(last.trim(), 300)
    }

    fn kill(&mut self) {
        self.dead = true;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn send(&mut self, text: &str) -> bool {
        match self.stdin.as_mut() {
            Some(w) => w
                .write_all(text.as_bytes())
                .and_then(|()| w.flush())
                .is_ok(),
            None => false,
        }
    }

    fn exit_text(&mut self) -> String {
        let status = self.child.try_wait().ok().flatten().or_else(|| {
            std::thread::sleep(Duration::from_millis(100));
            self.child.try_wait().ok().flatten()
        });
        match status {
            Some(s) => match s.code() {
                Some(c) => format!("exited with code {c}"),
                None => "was stopped by the system".to_owned(),
            },
            None => "stopped sending messages".to_owned(),
        }
    }

    /// One request and its answer. `idle` is the time without any message before the call is
    /// cancelled; the manifest's absolute limit also applies.
    ///
    /// # Errors
    /// [`HostError`] for every way the call can fail.
    #[allow(clippy::too_many_lines, clippy::needless_pass_by_value)]
    pub fn call(
        &mut self,
        method: &str,
        params: Value,
        idle: Duration,
        cancel: &AtomicBool,
        on_progress: &mut dyn FnMut(Progress),
    ) -> Result<Value, HostError> {
        let name = name_of(&self.loaded).to_owned();
        if self.dead {
            return Err(HostError::crashed(
                &name,
                "is no longer running",
                &self.log_tail(),
            ));
        }
        self.next_id += 1;
        let id = self.next_id;
        if !self.send(&wire::request(id, method, &params)) {
            let exit = self.exit_text();
            self.kill();
            return Err(HostError::crashed(&name, &exit, &self.log_tail()));
        }
        let total = Duration::from_secs(self.loaded.manifest.limits.max_total_s);
        let start = Instant::now();
        let mut last = Instant::now();
        let mut reason: Option<Why> = None;
        let mut cancel_sent: Option<Instant> = None;
        loop {
            match self.rx.recv_timeout(Duration::from_millis(40)) {
                Ok(Incoming::Msg(Message::Response { id: rid, result })) => {
                    if rid.as_i64() != Some(id) {
                        self.kill();
                        return Err(HostError::protocol(
                            &name,
                            "it answered a request Helia did not send",
                        ));
                    }
                    return match (result, reason) {
                        (_, Some(Why::User)) => Err(HostError::cancelled(&name)),
                        (Ok(v), _) => Ok(v),
                        (Err(e), Some(Why::Idle)) if e.kind() == ErrorKind::Cancelled => {
                            Err(HostError::timeout(&name, idle.as_secs()))
                        }
                        (Err(e), Some(Why::Total)) if e.kind() == ErrorKind::Cancelled => {
                            Err(HostError::timeout(&name, total.as_secs()))
                        }
                        (Err(e), _) => Err(HostError::from_plugin(&name, &e)),
                    };
                }
                Ok(Incoming::Msg(Message::Notification {
                    method: m,
                    params: p,
                })) => match m.as_str() {
                    "progress" => {
                        last = Instant::now();
                        if p.get("id").and_then(Value::as_i64).is_none_or(|x| x == id) {
                            on_progress(Progress {
                                fraction: p
                                    .get("fraction")
                                    .and_then(Value::as_f64)
                                    .filter(|f| f.is_finite())
                                    .map(|f| f.clamp(0.0, 1.0)),
                                status: p
                                    .get("status")
                                    .and_then(Value::as_str)
                                    .map(|s| helia_plugin_sdk::clip(s, 200)),
                                stage: p
                                    .get("stage")
                                    .and_then(Value::as_str)
                                    .map(|s| helia_plugin_sdk::clip(s, 40)),
                            });
                        }
                    }
                    "log" => {
                        last = Instant::now();
                        let lvl = p.get("level").and_then(Value::as_str).unwrap_or("info");
                        let msg = p.get("message").and_then(Value::as_str).unwrap_or("");
                        if let Ok(mut l) = self.log.lock() {
                            l.push(&format!(
                                "[{}] {}",
                                helia_plugin_sdk::clip(lvl, 8),
                                helia_plugin_sdk::clip(msg, 2000)
                            ));
                        }
                    }
                    _ => {}
                },
                Ok(Incoming::Msg(Message::Request { id: rid, .. })) => {
                    let e = PluginError::new(
                        code::METHOD_NOT_FOUND,
                        "Helia accepts no requests from plugins.",
                    );
                    self.send(&wire::response_err(&rid, &e));
                }
                Ok(Incoming::Bad(e)) => {
                    self.kill();
                    return Err(HostError::protocol(&name, e));
                }
                Ok(Incoming::Eof) | Err(RecvTimeoutError::Disconnected) => {
                    let exit = self.exit_text();
                    self.dead = true;
                    let _ = self.child.wait();
                    return Err(if reason == Some(Why::User) {
                        HostError::cancelled(&name)
                    } else {
                        HostError::crashed(&name, &exit, &self.log_tail())
                    });
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
            if reason.is_none() {
                reason = if cancel.load(Ordering::Relaxed) {
                    Some(Why::User)
                } else if last.elapsed() > idle {
                    Some(Why::Idle)
                } else if start.elapsed() > total {
                    Some(Why::Total)
                } else {
                    None
                };
                if reason.is_some() {
                    self.send(&wire::notification("cancel", &json!({"id": id})));
                    cancel_sent = Some(Instant::now());
                }
            }
            if cancel_sent.is_some_and(|t| t.elapsed() > GRACE) {
                self.kill();
                return Err(match reason {
                    Some(Why::User) => HostError::cancelled(&name),
                    Some(Why::Total) => HostError::timeout(&name, total.as_secs()),
                    _ => HostError::timeout(&name, idle.as_secs()),
                });
            }
        }
    }

    fn idle(&self) -> Duration {
        Duration::from_secs(self.loaded.manifest.limits.timeout_s)
    }

    fn need(&self, cap: &str, step: Option<&str>) -> Result<(), HostError> {
        let name = name_of(&self.loaded);
        if !self.info.capabilities.iter().any(|c| c == cap) {
            return Err(HostError::refused(
                name,
                format!("The plugin {name} cannot do that."),
                format!("It does not offer {cap}."),
                "Choose another plugin, or check the plugin's page.",
            ));
        }
        if let Some(id) = step {
            match self.loaded.step(id) {
                Some(s) if s.step.capability == cap => {}
                _ => {
                    return Err(HostError::refused(
                        name,
                        format!("The plugin {name} has no step called {id} of that kind."),
                        format!(
                            "Its {cap} steps are: {}.",
                            self.loaded
                                .steps
                                .iter()
                                .filter(|s| s.step.capability == cap)
                                .map(|s| s.step.id.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                        "Check the step name; `helia plugins info` lists them.",
                    ));
                }
            }
        }
        Ok(())
    }

    /// The Test button.
    ///
    /// # Errors
    /// [`HostError`]; a plugin without a test answers "method not found", reported as such.
    pub fn test(&mut self, cancel: &AtomicBool) -> Result<TestResult, HostError> {
        let name = name_of(&self.loaded).to_owned();
        let v = self.call(
            "test",
            json!({"settings": self.settings}),
            Duration::from_secs(60),
            cancel,
            &mut |_| {},
        )?;
        serde_json::from_value(v).map_err(|e| {
            HostError::protocol(&name, format!("its answer to test is not valid ({e})"))
        })
    }

    /// Remember that `step` ran, and the record the plugin gave about it.
    fn note_use(&mut self, step: &str, meta: Option<&Value>) {
        if !self.used.iter().any(|s| s == step) {
            self.used.push(step.to_owned());
        }
        if let Some(m) = meta.filter(|m| !m.is_null()) {
            self.metas.insert(step.to_owned(), m.clone());
        }
    }

    /// The FITS cards for what this session made: the host's own plugin and version cards, then
    /// the allow-listed `HELIA ML` cards from the latest `meta.provenance` of any step.
    #[must_use]
    pub fn fits_cards(&self) -> Vec<HeaderCard> {
        if self.used.is_empty() {
            return Vec::new();
        }
        let m = &self.loaded.manifest;
        let mut cards = crate::cards::plugin_cards(&[(m.name.clone(), m.version.clone())]);
        for meta in self.metas.values() {
            cards.extend(crate::cards::ml_cards(meta));
        }
        cards
    }

    /// Lines for the history of a saved file: which plugin took part, which version of it (and the
    /// checksum of the program that ran), and what it said about how it made the result.
    #[must_use]
    pub fn provenance(&self) -> Vec<String> {
        if self.used.is_empty() {
            return Vec::new();
        }
        let m = &self.loaded.manifest;
        let sum = self.loaded.digests().map_or_else(
            |_| String::new(),
            |d| format!(", program SHA-256 {}", d.exe),
        );
        let mut out = vec![format!(
            "Derived by plugin {} {}{sum}: {}",
            m.name,
            m.version,
            self.used.join(", ")
        )];
        for (step, meta) in &self.metas {
            out.push(format!(
                "Plugin {} {step}: {}",
                m.name,
                helia_plugin_sdk::clip(&meta.to_string(), 900)
            ));
        }
        out
    }

    fn fresh(&mut self, stem: &str, ext: &str) -> String {
        self.counter += 1;
        format!("{stem}-{:06}.{ext}", self.counter)
    }

    fn read_result(
        &self,
        r: &Value,
        expect_size: Option<(usize, usize)>,
        accept_fits: bool,
    ) -> Result<Frame, HostError> {
        let name = name_of(&self.loaded);
        let bad = |d: String| HostError::bad_result(name, d);
        let rel = r
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("the answer names no file".into()))?;
        let format = r.get("format").and_then(Value::as_str).unwrap_or("hlpx");
        let path = hlpx::resolve_in(self.workdir.path(), rel, true).map_err(|e| {
            HostError::protocol(name, format!("it named a file Helia will not open ({e})"))
        })?;
        let frame = match format {
            "hlpx" => {
                let img =
                    Image::read_file(&path, self.max_buffer).map_err(|e| bad(e.to_string()))?;
                if img.channels != 1 {
                    return Err(bad(format!(
                        "it has {} channels; Helia takes one",
                        img.channels
                    )));
                }
                for (k, claimed) in [("width", r.get("width")), ("height", r.get("height"))] {
                    if let Some(c) = claimed.and_then(Value::as_u64) {
                        let actual = if k == "width" { img.width } else { img.height } as u64;
                        if c != actual {
                            return Err(bad(format!(
                                "the answer says {k} {c} but the file says {actual}"
                            )));
                        }
                    }
                }
                Frame::new(img.width, img.height, img.data).map_err(|e| bad(e.to_string()))?
            }
            "fits" if accept_fits => {
                let meta = std::fs::symlink_metadata(&path).map_err(|e| bad(e.to_string()))?;
                if !meta.is_file() || meta.len() > self.max_buffer {
                    return Err(bad(format!(
                        "the FITS file is not a regular file under {} bytes",
                        self.max_buffer
                    )));
                }
                let f = std::fs::File::open(&path).map_err(|e| bad(e.to_string()))?;
                let frame = helia_core::io::fits::read_fits(f)
                    .map_err(|e| bad(format!("the FITS file cannot be read ({e})")))?;
                for (k, claimed) in [("width", r.get("width")), ("height", r.get("height"))] {
                    if let Some(c) = claimed.and_then(Value::as_u64) {
                        let actual = if k == "width" {
                            frame.width
                        } else {
                            frame.height
                        } as u64;
                        if c != actual {
                            return Err(bad(format!(
                                "the answer says {k} {c} but the FITS header says {actual}"
                            )));
                        }
                    }
                }
                frame
            }
            other => {
                return Err(bad(format!(
                    "it uses the file format {:?}, which is not allowed here",
                    helia_plugin_sdk::clip(other, 20)
                )));
            }
        };
        if let Some((w, h)) = expect_size
            && (frame.width, frame.height) != (w, h)
        {
            return Err(bad(format!(
                "it is {}x{} but a frame step must keep {w}x{h}",
                frame.width, frame.height
            )));
        }
        let mut frame = frame;
        for v in &mut frame.data {
            if !v.is_finite() {
                *v = 0.0;
            }
        }
        Ok(frame)
    }

    /// `stack.run`: have the plugin stack the capture at `input`.
    ///
    /// # Errors
    /// [`HostError`].
    pub fn stack(
        &mut self,
        input: &Path,
        job: &Value,
        cancel: &AtomicBool,
        on_progress: &mut dyn FnMut(Progress),
    ) -> Result<Stacked, HostError> {
        self.need("stack-backend", None)?;
        let name = name_of(&self.loaded).to_owned();
        let out = self.fresh("stack", "hlpx");
        let params = json!({
            "settings": self.settings,
            "input": input.display().to_string(),
            "job": job,
            "output": {"path": out, "format": "hlpx", "accept": ["hlpx", "fits"]},
        });
        let idle = self.idle();
        let v = self.call("stack.run", params, idle, cancel, on_progress)?;
        let r = v
            .get("output")
            .ok_or_else(|| HostError::protocol(&name, "its answer to stack.run has no output"))?;
        let frame = self.read_result(r, None, true)?;
        if let Some(p) = r.get("path").and_then(Value::as_str)
            && let Ok(f) = hlpx::resolve_in(self.workdir.path(), p, true)
        {
            let _ = std::fs::remove_file(f);
        }
        let note = v
            .get("note")
            .and_then(Value::as_str)
            .map(|s| helia_plugin_sdk::clip(s, 400));
        self.note_use("stack", v.get("meta"));
        Ok(Stacked { frame, note })
    }

    #[allow(clippy::too_many_arguments, clippy::needless_pass_by_value)]
    fn process(
        &mut self,
        method: &str,
        cap: &str,
        step: &str,
        params: &Value,
        frame: &Frame,
        meta: Value,
        cancel: &AtomicBool,
        on_progress: &mut dyn FnMut(Progress),
    ) -> Result<Frame, HostError> {
        self.need(cap, Some(step))?;
        let name = name_of(&self.loaded).to_owned();
        let params = self.loaded.step(step).map_or(Ok(params.clone()), |s| {
            s.params.validate(params).map_err(|p| {
                HostError::refused(
                    &name,
                    format!("The settings of the step {step} are not right."),
                    p.join(" "),
                    "Correct the step's values and try again.",
                )
            })
        })?;
        let input = self.fresh("in", "hlpx");
        let output = self.fresh("out", "hlpx");
        let img = Image::new(frame.width, frame.height, 1, frame.data.clone()).map_err(|e| {
            HostError::refused(
                &name,
                "That picture cannot be sent to a plugin.",
                e.to_string(),
                "Use a smaller picture.",
            )
        })?;
        let in_path = self.workdir.path().join(&input);
        img.write_file(&in_path).map_err(|e| {
            HostError::invalid(&name, format!("cannot write the working file: {e}"))
        })?;
        drop(img);
        let req = json!({
            "step": step,
            "settings": self.settings,
            "params": params,
            "input": {"path": input, "format": "hlpx", "width": frame.width, "height": frame.height, "channels": 1},
            "output": {"path": output, "format": "hlpx"},
            "meta": meta,
        });
        let idle = self.idle();
        let res = self.call(method, req, idle, cancel, on_progress);
        let _ = std::fs::remove_file(&in_path);
        let v = res?;
        let r = v.get("output").ok_or_else(|| {
            HostError::protocol(&name, format!("its answer to {method} has no output"))
        })?;
        let expect = (method == "frame.process").then_some((frame.width, frame.height));
        let mut out = self.read_result(r, expect, false)?;
        if let Some(p) = r.get("path").and_then(Value::as_str)
            && let Ok(f) = hlpx::resolve_in(self.workdir.path(), p, true)
        {
            let _ = std::fs::remove_file(f);
        }
        out.bayer = frame.bayer;
        if out.width != frame.width || out.height != frame.height {
            out.bayer = None;
        }
        self.note_use(step, v.get("meta"));
        Ok(out)
    }

    /// `image.process`: run an `image-op` step on `frame`.
    ///
    /// # Errors
    /// [`HostError`].
    pub fn image_op(
        &mut self,
        step: &str,
        params: &Value,
        frame: &Frame,
        stage: &str,
        cancel: &AtomicBool,
        on_progress: &mut dyn FnMut(Progress),
    ) -> Result<Frame, HostError> {
        self.process(
            "image.process",
            "image-op",
            step,
            params,
            frame,
            json!({"stage": stage}),
            cancel,
            on_progress,
        )
    }

    /// `frame.process`: run a `frame-op` step on one frame.
    ///
    /// # Errors
    /// [`HostError`].
    pub fn frame_op(
        &mut self,
        step: &str,
        params: &Value,
        frame: &Frame,
        index: usize,
        count: usize,
        cancel: &AtomicBool,
    ) -> Result<Frame, HostError> {
        let bayer = frame.bayer.map(bayer_name);
        let meta = json!({"frame": index, "frames": count, "bayer": bayer});
        self.process(
            "frame.process",
            "frame-op",
            step,
            params,
            frame,
            meta,
            cancel,
            &mut |_| {},
        )
    }

    /// `analyse.run`: measure `frame`.
    ///
    /// # Errors
    /// [`HostError`].
    pub fn analyse(
        &mut self,
        step: &str,
        params: &Value,
        frame: &Frame,
        cancel: &AtomicBool,
        on_progress: &mut dyn FnMut(Progress),
    ) -> Result<Analysis, HostError> {
        self.need("analyse", Some(step))?;
        let name = name_of(&self.loaded).to_owned();
        let params = self.loaded.step(step).map_or(Ok(params.clone()), |s| {
            s.params.validate(params).map_err(|p| {
                HostError::refused(
                    &name,
                    format!("The settings of the step {step} are not right."),
                    p.join(" "),
                    "Correct the step's values and try again.",
                )
            })
        })?;
        let input = self.fresh("in", "hlpx");
        let img = Image::new(frame.width, frame.height, 1, frame.data.clone()).map_err(|e| {
            HostError::refused(
                &name,
                "That picture cannot be sent to a plugin.",
                e.to_string(),
                "Use a smaller picture.",
            )
        })?;
        let in_path = self.workdir.path().join(&input);
        img.write_file(&in_path).map_err(|e| {
            HostError::invalid(&name, format!("cannot write the working file: {e}"))
        })?;
        drop(img);
        let req = json!({
            "step": step,
            "settings": self.settings,
            "params": params,
            "input": {"path": input, "format": "hlpx", "width": frame.width, "height": frame.height, "channels": 1},
            "meta": {"stage": "final"},
        });
        let idle = self.idle();
        let res = self.call("analyse.run", req, idle, cancel, on_progress);
        let _ = std::fs::remove_file(&in_path);
        let v = res?;
        let r: AnalyseResult = serde_json::from_value(v).map_err(|e| {
            HostError::protocol(
                &name,
                format!("its answer to analyse.run is not valid ({e})"),
            )
        })?;
        Ok(clean_analysis(r))
    }

    /// Ask the plugin to finish, then stop it. Safe to call twice.
    pub fn close(&mut self) {
        if self.dead {
            return;
        }
        let never = AtomicBool::new(false);
        let _ = self.call_quick("shutdown", &never);
        self.send(&wire::notification("exit", &Value::Null));
        self.stdin = None;
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(2) {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                self.dead = true;
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        self.kill();
    }

    fn call_quick(&mut self, method: &str, cancel: &AtomicBool) -> Result<Value, HostError> {
        let saved = self.loaded.manifest.limits.max_total_s;
        self.loaded.manifest.limits.max_total_s = 5;
        let r = self.call(
            method,
            Value::Null,
            Duration::from_secs(5),
            cancel,
            &mut |_| {},
        );
        self.loaded.manifest.limits.max_total_s = saved;
        r
    }

    /// The working folder (tests and tools).
    #[must_use]
    pub fn workdir(&self) -> PathBuf {
        self.workdir.path().to_owned()
    }

    /// Dimension check helper for tests.
    #[must_use]
    pub fn header_of(path: &Path) -> Option<Header> {
        let mut b = [0u8; hlpx::HEADER_LEN];
        std::fs::File::open(path).ok()?.read_exact(&mut b).ok()?;
        Header::parse(&b).ok()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if !self.dead {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Why {
    User,
    Idle,
    Total,
}

trait TryReady {
    fn try_ready(&mut self) -> bool;
}

impl TryReady for Child {
    fn try_ready(&mut self) -> bool {
        matches!(self.try_wait(), Ok(Some(_)))
    }
}

fn bayer_name(p: BayerPattern) -> &'static str {
    match p {
        BayerPattern::Rggb => "rggb",
        BayerPattern::Grbg => "grbg",
        BayerPattern::Gbrg => "gbrg",
        BayerPattern::Bggr => "bggr",
    }
}

fn clean_analysis(r: AnalyseResult) -> Analysis {
    let mut a = Analysis::default();
    let mut dropped = false;
    for (k, v) in r.metrics {
        if a.metrics.len() >= MAX_METRICS {
            dropped = true;
            break;
        }
        if v.is_finite() {
            a.metrics.insert(helia_plugin_sdk::clip(&k, 80), v);
        }
    }
    for o in r.overlays {
        if a.overlays.len() >= MAX_OVERLAYS {
            dropped = true;
            break;
        }
        let known = o
            .get("kind")
            .and_then(Value::as_str)
            .is_some_and(|k| OVERLAY_KINDS.contains(&k));
        if known {
            a.overlays.push(o);
        }
    }
    for n in r.notes {
        if a.notes.len() >= MAX_NOTES {
            dropped = true;
            break;
        }
        a.notes.push(helia_plugin_sdk::clip(&n, 400));
    }
    if dropped {
        a.notes
            .push("Some results were dropped because there were too many.".into());
    }
    a
}

/// Convenience: start `name` from `reg`, run `f`, close it.
///
/// # Errors
/// Whatever starting or `f` returns.
pub fn with_session<T>(
    reg: &Registry,
    name: &str,
    f: impl FnOnce(&mut Session) -> Result<T, HostError>,
) -> Result<T, HostError> {
    let mut s = Session::start(reg, name)?;
    let r = f(&mut s);
    s.close();
    r
}

/// Settings of a plugin for display, with secrets hidden.
#[must_use]
pub fn redacted(l: &Loaded, settings: &Value) -> Value {
    let mut out = settings.clone();
    if let Value::Object(m) = &mut out {
        for f in &l.settings.fields {
            if f.kind == crate::schema::FieldKind::Secret && m.contains_key(&f.key) {
                m.insert(f.key.clone(), Value::String("(hidden)".into()));
            }
        }
    }
    out
}

#[allow(dead_code)]
fn _assert_send(s: Session) -> impl Send {
    s
}
