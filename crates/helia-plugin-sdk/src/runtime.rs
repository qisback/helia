// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The plugin side run loop: reads requests, calls the [`Plugin`], writes answers.

use std::io::{BufRead, Write};
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::hlpx::{self, BufferRef, DEFAULT_MAX_BYTES, HlpxError, Image, OutputSpec};
use crate::types::{
    API_VERSION, AnalyseRequest, AnalyseResult, Info, InitializeParams, PluginError,
    ProcessRequest, ProcessResult, StackRequest, StackResult, TestResult, code,
};
use crate::wire::{self, MAX_LINE, Message};

type Out = Arc<Mutex<Box<dyn Write + Send>>>;

fn send(out: &Out, text: &str) {
    if let Ok(mut w) = out.lock() {
        let _ = w.write_all(text.as_bytes());
        let _ = w.flush();
    }
}

/// What a plugin implements. Every method but [`Plugin::info`] has a default that answers
/// "method not found", so implement the ones for the capabilities you declare.
pub trait Plugin {
    /// Name, version, capabilities and steps. Called to answer `initialize`.
    fn info(&self) -> Info;

    /// Called once after the host's `initialize` has been checked, with the working folder and
    /// settings. Return an error to refuse to start (for example, bad settings).
    ///
    /// # Errors
    /// Any [`PluginError`]; it is sent as the answer to `initialize`.
    fn initialize(&mut self, params: &InitializeParams) -> Result<(), PluginError> {
        let _ = params;
        Ok(())
    }

    /// The Test button: check the settings and the connection.
    ///
    /// # Errors
    /// Any [`PluginError`].
    fn test(&mut self, ctx: &Ctx, settings: &Value) -> Result<TestResult, PluginError> {
        let _ = (ctx, settings);
        Err(PluginError::method_not_found("test"))
    }

    /// `stack-backend`: stack a capture.
    ///
    /// # Errors
    /// Any [`PluginError`].
    fn stack(&mut self, ctx: &Ctx, req: StackRequest) -> Result<StackResult, PluginError> {
        let _ = (ctx, req);
        Err(PluginError::method_not_found("stack.run"))
    }

    /// `frame-op`: process one frame (same size out as in).
    ///
    /// # Errors
    /// Any [`PluginError`].
    fn frame_process(
        &mut self,
        ctx: &Ctx,
        req: ProcessRequest,
    ) -> Result<ProcessResult, PluginError> {
        let _ = (ctx, req);
        Err(PluginError::method_not_found("frame.process"))
    }

    /// `image-op`: process an image (the size may change).
    ///
    /// # Errors
    /// Any [`PluginError`].
    fn image_process(
        &mut self,
        ctx: &Ctx,
        req: ProcessRequest,
    ) -> Result<ProcessResult, PluginError> {
        let _ = (ctx, req);
        Err(PluginError::method_not_found("image.process"))
    }

    /// `analyse`: measure an image.
    ///
    /// # Errors
    /// Any [`PluginError`].
    fn analyse(&mut self, ctx: &Ctx, req: AnalyseRequest) -> Result<AnalyseResult, PluginError> {
        let _ = (ctx, req);
        Err(PluginError::method_not_found("analyse.run"))
    }
}

/// The call in progress: progress reports, cancellation and pixel files.
pub struct Ctx {
    id: i64,
    workdir: PathBuf,
    max_bytes: u64,
    cancelled: Arc<AtomicBool>,
    out: Out,
}

impl Ctx {
    /// The id of the request being served.
    #[must_use]
    pub fn request_id(&self) -> i64 {
        self.id
    }

    /// The working folder.
    #[must_use]
    pub fn workdir(&self) -> &std::path::Path {
        &self.workdir
    }

    /// Report progress: `fraction` 0 to 1 and a short line for the status area. Also restarts the
    /// host's inactivity timeout.
    pub fn progress(&self, fraction: f64, status: &str) {
        self.progress_stage(None, fraction, status);
    }

    /// As [`Ctx::progress`], naming one of Helia's stages (`quality`, `reference`, `global_align`,
    /// `local_align`, `stack`, `finish`).
    pub fn progress_stage(&self, stage: Option<&str>, fraction: f64, status: &str) {
        let mut p = json!({"id": self.id, "fraction": fraction.clamp(0.0, 1.0), "status": status});
        if let Some(s) = stage {
            p["stage"] = json!(s);
        }
        send(&self.out, &wire::notification("progress", &p));
    }

    /// Add a line to the plugin's log (`level` is `debug`, `info`, `warn` or `error`).
    pub fn log(&self, level: &str, message: &str) {
        let p = json!({"level": level, "message": message});
        send(&self.out, &wire::notification("log", &p));
    }

    /// True once the host has asked to cancel this call.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// `Err(cancelled)` once the host has asked to cancel; use `?` in loops.
    ///
    /// # Errors
    /// [`PluginError::cancelled`].
    pub fn check_cancelled(&self) -> Result<(), PluginError> {
        if self.is_cancelled() {
            Err(PluginError::cancelled())
        } else {
            Ok(())
        }
    }

    /// Read an input image named in the request.
    ///
    /// # Errors
    /// A [`PluginError`] if the path breaks the rules, the file is not `hlpx`, or its size does not
    /// match the reference.
    pub fn read_image(&self, r: &BufferRef) -> Result<Image, PluginError> {
        if r.format != "hlpx" {
            return Err(PluginError::new(
                code::UNSUPPORTED_INPUT,
                format!(
                    "Unsupported buffer format {}.",
                    crate::types::clip(&r.format, 20)
                ),
            ));
        }
        let p = hlpx::resolve_in(&self.workdir, &r.path, true).map_err(buffer_error)?;
        let img = Image::read_file(&p, self.max_bytes).map_err(buffer_error)?;
        if (r.width, r.height, r.channels)
            != (img.width as u32, img.height as u32, img.channels as u32)
            && (r.width != 0 || r.height != 0)
        {
            return Err(buffer_error(HlpxError::SizeMismatch(
                "the message and the file header give different dimensions".into(),
            )));
        }
        Ok(img)
    }

    /// Write a result image where the host asked for it and return the reference to send back.
    ///
    /// # Errors
    /// A [`PluginError`] on a bad path, an unsupported format or a file error.
    pub fn write_image(&self, out: &OutputSpec, img: &Image) -> Result<BufferRef, PluginError> {
        if out.format != "hlpx" {
            return Err(PluginError::new(
                code::UNSUPPORTED_INPUT,
                "This SDK writes hlpx only.",
            ));
        }
        let p = hlpx::resolve_in(&self.workdir, &out.path, false).map_err(buffer_error)?;
        img.write_file(&p).map_err(buffer_error)?;
        Ok(BufferRef {
            path: out.path.clone(),
            format: "hlpx".into(),
            width: img.width as u32,
            height: img.height as u32,
            channels: img.channels as u32,
        })
    }
}

// Used as a `map_err` function, so it takes the error by value.
#[allow(clippy::needless_pass_by_value)]
fn buffer_error(e: HlpxError) -> PluginError {
    PluginError::new(code::INVALID_PARAMS, e.to_string())
}

enum Event {
    Msg(Message),
    Bad(String),
    Eof,
}

/// Run the plugin on the process's standard input and output. Returns the exit code.
pub fn run<P: Plugin>(plugin: P) -> i32 {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    run_with(
        plugin,
        Box::new(std::io::BufReader::new(stdin)),
        Box::new(stdout),
    )
}

/// Run the plugin on any reader and writer (used by tests).
#[allow(clippy::too_many_lines)]
pub fn run_with<P: Plugin>(
    mut plugin: P,
    reader: Box<dyn BufRead + Send>,
    writer: Box<dyn Write + Send>,
) -> i32 {
    let out: Out = Arc::new(Mutex::new(writer));
    let cancelled = Arc::new(AtomicBool::new(false));
    let current = Arc::new(AtomicI64::new(-1));
    let (tx, rx) = mpsc::channel::<Event>();
    {
        let cancelled = Arc::clone(&cancelled);
        let current = Arc::clone(&current);
        std::thread::spawn(move || {
            let mut reader = reader;
            loop {
                if let Ok(Some(line)) = wire::read_line_limited(&mut reader, MAX_LINE) {
                    match wire::parse_message(&line) {
                        Ok(Message::Notification { method, params }) if method == "cancel" => {
                            if params.get("id").and_then(Value::as_i64)
                                == Some(current.load(Ordering::SeqCst))
                            {
                                cancelled.store(true, Ordering::SeqCst);
                            }
                        }
                        Ok(m) => {
                            if let Message::Request { id, .. } = &m {
                                current.store(id.as_i64().unwrap_or(-1), Ordering::SeqCst);
                                cancelled.store(false, Ordering::SeqCst);
                            }
                            if tx.send(Event::Msg(m)).is_err() {
                                return;
                            }
                        }
                        Err(wire::WireError::Empty) => {}
                        Err(e) => {
                            if tx.send(Event::Bad(e.to_string())).is_err() {
                                return;
                            }
                        }
                    }
                } else {
                    let _ = tx.send(Event::Eof);
                    return;
                }
            }
        });
    }

    let mut state: Option<(PathBuf, u64)> = None;
    let mut shut = false;
    loop {
        let Ok(ev) = rx.recv() else { return 1 };
        let (id, method, params) = match ev {
            Event::Eof => return i32::from(!shut),
            Event::Bad(why) => {
                let e = PluginError::new(code::PARSE_ERROR, why);
                send(&out, &wire::response_err(&Value::Null, &e));
                continue;
            }
            Event::Msg(Message::Request { id, method, params }) => (id, method, params),
            Event::Msg(Message::Notification { method, .. }) => {
                if method == "exit" {
                    return 0;
                }
                continue;
            }
            Event::Msg(Message::Response { .. }) => continue,
        };
        let result =
            if method == "initialize" {
                handle_initialize(&mut plugin, &params, &mut state)
            } else if method == "shutdown" {
                shut = true;
                Ok(Value::Null)
            } else if let Some((workdir, max_bytes)) = &state {
                let ctx = Ctx {
                    id: id.as_i64().unwrap_or(-1),
                    workdir: workdir.clone(),
                    max_bytes: *max_bytes,
                    cancelled: Arc::clone(&cancelled),
                    out: Arc::clone(&out),
                };
                let r = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    dispatch(&mut plugin, &ctx, &method, &params)
                }));
                r.unwrap_or_else(|_| {
                Err(PluginError::new(code::INTERNAL, "The plugin hit an internal error.").explain(
                    "The plugin failed while working.",
                    "It stopped on an unexpected condition (a bug in the plugin).",
                    "Try again; if it keeps happening, report it to the plugin's author.",
                ))
            })
            } else {
                Err(PluginError::new(
                    code::INVALID_REQUEST,
                    "initialize must be the first request.",
                ))
            };
        let line = match result {
            Ok(v) => wire::response_ok(&id, &v),
            Err(e) => wire::response_err(&id, &e),
        };
        send(&out, &line);
    }
}

fn handle_initialize<P: Plugin>(
    plugin: &mut P,
    params: &Value,
    state: &mut Option<(PathBuf, u64)>,
) -> Result<Value, PluginError> {
    let p: InitializeParams = parse(params)?;
    if p.api_version != API_VERSION {
        return Err(PluginError::new(
            code::INVALID_PARAMS,
            format!(
                "This plugin speaks API version {API_VERSION}, the host asked for {}.",
                p.api_version
            ),
        )
        .explain(
            "The plugin and Helia do not speak the same plugin version.",
            format!(
                "The plugin speaks version {API_VERSION}; this Helia asked for version {}.",
                p.api_version
            ),
            "Update the plugin, or use a Helia that supports it.",
        ));
    }
    if p.workdir.is_empty() {
        return Err(PluginError::invalid_params("initialize needs a workdir."));
    }
    let max_bytes = p
        .limits
        .get("max_buffer_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_MAX_BYTES);
    plugin.initialize(&p)?;
    *state = Some((PathBuf::from(&p.workdir), max_bytes));
    serde_json::to_value(plugin.info()).map_err(|e| PluginError::new(code::INTERNAL, e.to_string()))
}

fn parse<T: DeserializeOwned>(params: &Value) -> Result<T, PluginError> {
    serde_json::from_value(params.clone())
        .map_err(|e| PluginError::invalid_params(format!("Bad parameters: {e}")))
}

fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, PluginError> {
    serde_json::to_value(v).map_err(|e| PluginError::new(code::INTERNAL, e.to_string()))
}

fn dispatch<P: Plugin>(
    plugin: &mut P,
    ctx: &Ctx,
    method: &str,
    params: &Value,
) -> Result<Value, PluginError> {
    match method {
        "test" => {
            let settings = params.get("settings").cloned().unwrap_or(Value::Null);
            to_value(&plugin.test(ctx, &settings)?)
        }
        "stack.run" => to_value(&plugin.stack(ctx, parse(params)?)?),
        "frame.process" => to_value(&plugin.frame_process(ctx, parse(params)?)?),
        "image.process" => to_value(&plugin.image_process(ctx, parse(params)?)?),
        "analyse.run" => to_value(&plugin.analyse(ctx, parse(params)?)?),
        other => Err(PluginError::method_not_found(other)),
    }
}
