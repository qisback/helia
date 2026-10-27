// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Protocol types shared by the plugin and the host.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::hlpx::{BufferRef, OutputSpec};

/// The protocol major version this SDK speaks.
pub const API_VERSION: u32 = 1;

/// JSON-RPC and Helia error codes.
pub mod code {
    /// The line was not valid JSON.
    pub const PARSE_ERROR: i64 = -32700;
    /// Not a valid JSON-RPC message.
    pub const INVALID_REQUEST: i64 = -32600;
    /// The method is not implemented.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// The parameters are wrong.
    pub const INVALID_PARAMS: i64 = -32602;
    /// A bug in the plugin.
    pub const INTERNAL: i64 = -32603;
    /// The call was cancelled.
    pub const CANCELLED: i64 = -32800;
    /// Something the plugin needs is not reachable.
    pub const UNAVAILABLE: i64 = -32010;
    /// Credentials are wrong or missing.
    pub const UNAUTHORISED: i64 = -32011;
    /// The data or settings cannot be handled.
    pub const UNSUPPORTED_INPUT: i64 = -32012;
    /// Out of memory, GPU memory or disk.
    pub const RESOURCE: i64 = -32013;
    /// A setting is wrong.
    pub const INVALID_SETTINGS: i64 = -32014;
    /// Any other failure.
    pub const FAILED: i64 = -32000;
}

/// The kind of a [`PluginError`], from its code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// Invalid JSON.
    Parse,
    /// Invalid request or parameters.
    Invalid,
    /// Method not implemented.
    MethodNotFound,
    /// A bug in the plugin.
    Internal,
    /// Cancelled.
    Cancelled,
    /// A needed service is not reachable.
    Unavailable,
    /// Wrong credentials.
    Unauthorised,
    /// Input the plugin cannot handle.
    UnsupportedInput,
    /// Out of a resource.
    Resource,
    /// Wrong settings.
    InvalidSettings,
    /// Anything else.
    Failed,
}

/// An error in Helia's standard form: what happened, why, what to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginError {
    /// JSON-RPC error code, see [`code`].
    pub code: i64,
    /// One-line fallback text.
    pub message: String,
    /// What happened, in plain English.
    pub what: Option<String>,
    /// Why, in plain English.
    pub why: Option<String>,
    /// What the user can do about it.
    pub todo: Option<String>,
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for PluginError {}

impl PluginError {
    /// An error with a code and a one-line message.
    #[must_use]
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            what: None,
            why: None,
            todo: None,
        }
    }

    /// Any other failure ([`code::FAILED`]).
    #[must_use]
    pub fn failed(message: impl Into<String>) -> Self {
        Self::new(code::FAILED, message)
    }

    /// The call was cancelled.
    #[must_use]
    pub fn cancelled() -> Self {
        Self::new(code::CANCELLED, "Cancelled.")
    }

    /// The method is not implemented.
    #[must_use]
    pub fn method_not_found(method: &str) -> Self {
        Self::new(
            code::METHOD_NOT_FOUND,
            format!("This plugin does not implement {method}."),
        )
    }

    /// The parameters are wrong.
    #[must_use]
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(code::INVALID_PARAMS, message)
    }

    /// Fill in the What / Why / What-to-do parts.
    #[must_use]
    pub fn explain(
        mut self,
        what: impl Into<String>,
        why: impl Into<String>,
        todo: impl Into<String>,
    ) -> Self {
        self.what = Some(what.into());
        self.why = Some(why.into());
        self.todo = Some(todo.into());
        self
    }

    /// The kind, from the code.
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        match self.code {
            code::PARSE_ERROR => ErrorKind::Parse,
            code::INVALID_REQUEST | code::INVALID_PARAMS => ErrorKind::Invalid,
            code::METHOD_NOT_FOUND => ErrorKind::MethodNotFound,
            code::INTERNAL => ErrorKind::Internal,
            code::CANCELLED => ErrorKind::Cancelled,
            code::UNAVAILABLE => ErrorKind::Unavailable,
            code::UNAUTHORISED => ErrorKind::Unauthorised,
            code::UNSUPPORTED_INPUT => ErrorKind::UnsupportedInput,
            code::RESOURCE => ErrorKind::Resource,
            code::INVALID_SETTINGS => ErrorKind::InvalidSettings,
            _ => ErrorKind::Failed,
        }
    }

    /// The JSON-RPC `error` object.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut data = serde_json::Map::new();
        for (k, v) in [
            ("what", &self.what),
            ("why", &self.why),
            ("todo", &self.todo),
        ] {
            if let Some(v) = v {
                data.insert(k.into(), Value::String(v.clone()));
            }
        }
        let mut o = serde_json::Map::new();
        o.insert("code".into(), self.code.into());
        o.insert("message".into(), Value::String(self.message.clone()));
        if !data.is_empty() {
            o.insert("data".into(), Value::Object(data));
        }
        Value::Object(o)
    }

    /// Read a JSON-RPC `error` object leniently: a missing or odd field is ignored, and long
    /// text is cut.
    #[must_use]
    pub fn from_json(v: &Value) -> Self {
        let text = |x: Option<&Value>| x.and_then(Value::as_str).map(|s| clip(s, 2000));
        let code = v
            .get("code")
            .and_then(Value::as_i64)
            .unwrap_or(code::FAILED);
        let message =
            text(v.get("message")).unwrap_or_else(|| "The plugin reported an error.".into());
        let data = v.get("data");
        Self {
            code,
            message,
            what: text(data.and_then(|d| d.get("what"))),
            why: text(data.and_then(|d| d.get("why"))),
            todo: text(data.and_then(|d| d.get("todo"))),
        }
    }
}

/// Cut `s` to at most `max` bytes on a character boundary.
#[must_use]
pub fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

impl From<std::io::Error> for PluginError {
    fn from(e: std::io::Error) -> Self {
        Self::failed(format!("File error: {e}"))
    }
}

/// Parameters of `initialize`.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct InitializeParams {
    /// Protocol major version the host speaks.
    #[serde(default)]
    pub api_version: u32,
    /// The host, `{name, version}`.
    #[serde(default)]
    pub host: Value,
    /// Private working folder for pixel files.
    #[serde(default)]
    pub workdir: String,
    /// The user's validated settings.
    #[serde(default)]
    pub settings: Value,
    /// Limits, such as `max_buffer_bytes`.
    #[serde(default)]
    pub limits: Value,
    /// Locale, such as `en-GB`.
    #[serde(default)]
    pub locale: String,
}

/// What a plugin tells the host in the answer to `initialize`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Info {
    /// Protocol major version the plugin speaks.
    pub api_version: u32,
    /// Plugin name.
    pub name: String,
    /// Plugin version.
    pub version: String,
    /// Capabilities implemented now.
    pub capabilities: Vec<String>,
    /// Step ids implemented now.
    #[serde(default)]
    pub steps: Vec<String>,
}

impl Info {
    /// Info for the current protocol version.
    #[must_use]
    pub fn new(name: &str, version: &str, capabilities: &[&str]) -> Self {
        Self {
            api_version: API_VERSION,
            name: name.into(),
            version: version.into(),
            capabilities: capabilities.iter().map(|s| (*s).into()).collect(),
            steps: Vec::new(),
        }
    }

    /// Name the steps offered.
    #[must_use]
    pub fn with_steps(mut self, steps: &[&str]) -> Self {
        self.steps = steps.iter().map(|s| (*s).into()).collect();
        self
    }
}

/// Answer to `test`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TestResult {
    /// Whether the test passed.
    pub ok: bool,
    /// What to show the user.
    pub message: String,
}

/// Parameters of `stack.run`.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct StackRequest {
    /// The user's settings.
    #[serde(default)]
    pub settings: Value,
    /// Absolute path of the capture.
    pub input: String,
    /// The stacking job, as the `config` of the Helia API.
    #[serde(default)]
    pub job: Value,
    /// Where to write the stack.
    pub output: OutputSpec,
}

/// Answer to `stack.run`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StackResult {
    /// The stack written.
    pub output: BufferRef,
    /// A line for the summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// A free-form record the host keeps beside the result: what produced it (a model, its
    /// checksum, the settings used). The host writes it into the saved file's history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

/// Parameters of `frame.process` and `image.process`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProcessRequest {
    /// Step id.
    #[serde(default)]
    pub step: String,
    /// The user's settings for the plugin.
    #[serde(default)]
    pub settings: Value,
    /// Parameters of this use of the step.
    #[serde(default)]
    pub params: Value,
    /// The input image.
    pub input: BufferRef,
    /// Where to write the result.
    pub output: OutputSpec,
    /// Context: frame number, stage, Bayer pattern.
    #[serde(default)]
    pub meta: Value,
}

/// Answer to `frame.process` and `image.process`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProcessResult {
    /// The image written.
    pub output: BufferRef,
    /// A line for the log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// A free-form record the host keeps beside the result: what produced it (a model, its
    /// checksum, the settings used). The host writes it into the saved file's history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

impl ProcessResult {
    /// A result with no note.
    #[must_use]
    pub fn new(output: BufferRef) -> Self {
        Self {
            output,
            note: None,
            meta: None,
        }
    }

    /// Add a record of how the image was made, for the saved file's history.
    #[must_use]
    pub fn with_meta(mut self, meta: Value) -> Self {
        self.meta = Some(meta);
        self
    }
}

/// Parameters of `analyse.run`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AnalyseRequest {
    /// Step id.
    #[serde(default)]
    pub step: String,
    /// The user's settings for the plugin.
    #[serde(default)]
    pub settings: Value,
    /// Parameters of this use of the step.
    #[serde(default)]
    pub params: Value,
    /// The input image.
    pub input: BufferRef,
    /// Context.
    #[serde(default)]
    pub meta: Value,
}

/// Answer to `analyse.run`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct AnalyseResult {
    /// Named numbers.
    #[serde(default)]
    pub metrics: BTreeMap<String, f64>,
    /// Shapes to draw over the image, see the protocol.
    #[serde(default)]
    pub overlays: Vec<Value>,
    /// Lines of text for the user.
    #[serde(default)]
    pub notes: Vec<String>,
}
