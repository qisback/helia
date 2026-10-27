// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Framing and parsing of the plugin protocol: one JSON-RPC 2.0 message per line.
//!
//! These functions read untrusted input (a plugin's output on the host, the host's input in a
//! plugin), so they bound everything before they allocate: the line length while it is being read,
//! the nesting depth before parsing, and UTF-8 before JSON.

use std::fmt;
use std::io::BufRead;

use serde_json::{Map, Value};

use crate::types::PluginError;

/// Longest line accepted, in bytes.
pub const MAX_LINE: usize = 4 << 20;
/// Deepest JSON nesting accepted.
pub const MAX_DEPTH: usize = 32;

/// Why a line was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WireError {
    /// The line is longer than the limit (it was not buffered further).
    LineTooLong,
    /// The line is not valid UTF-8.
    BadUtf8,
    /// The JSON nests deeper than [`MAX_DEPTH`].
    TooDeep,
    /// The text is not JSON.
    BadJson(String),
    /// The JSON is not a single object (arrays, batches and scalars are not messages).
    NotObject,
    /// `jsonrpc` is missing or not `"2.0"`.
    BadVersion,
    /// The object is not a request, notification or response.
    Invalid(String),
    /// The line is empty.
    Empty,
    /// A read failed.
    Io(String),
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LineTooLong => write!(f, "a message is longer than {MAX_LINE} bytes"),
            Self::BadUtf8 => write!(f, "a message is not valid UTF-8"),
            Self::TooDeep => write!(f, "a message nests deeper than {MAX_DEPTH} levels"),
            Self::BadJson(e) => write!(f, "a message is not valid JSON ({e})"),
            Self::NotObject => write!(f, "a message is not a single JSON object"),
            Self::BadVersion => write!(f, "a message does not say jsonrpc 2.0"),
            Self::Invalid(e) => write!(f, "a message is not a valid request or response ({e})"),
            Self::Empty => write!(f, "an empty line"),
            Self::Io(e) => write!(f, "reading failed ({e})"),
        }
    }
}

impl std::error::Error for WireError {}

/// One parsed message.
#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    /// A call that wants an answer.
    Request {
        /// The caller's id (an integer from the host).
        id: Value,
        /// Method name.
        method: String,
        /// Parameters (`Null` if absent).
        params: Value,
    },
    /// A call that wants no answer.
    Notification {
        /// Method name.
        method: String,
        /// Parameters (`Null` if absent).
        params: Value,
    },
    /// An answer.
    Response {
        /// The id of the request answered (`Null` for an error to an unreadable request).
        id: Value,
        /// The result or the error.
        result: Result<Value, PluginError>,
    },
}

/// Read one line of at most `max` bytes (the newline is not returned; a `\r` before it is
/// dropped). `Ok(None)` at end of input with nothing read.
///
/// # Errors
/// [`WireError::LineTooLong`] as soon as `max` bytes pass without a newline (the rest is not
/// read), or [`WireError::Io`].
pub fn read_line_limited<R: BufRead + ?Sized>(
    r: &mut R,
    max: usize,
) -> Result<Option<Vec<u8>>, WireError> {
    let mut line: Vec<u8> = Vec::new();
    loop {
        let buf = match r.fill_buf() {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(WireError::Io(e.to_string())),
        };
        if buf.is_empty() {
            return Ok(if line.is_empty() { None } else { Some(line) });
        }
        if let Some(i) = buf.iter().position(|&b| b == b'\n') {
            if line.len() + i > max {
                return Err(WireError::LineTooLong);
            }
            line.extend_from_slice(&buf[..i]);
            r.consume(i + 1);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Some(line));
        }
        let n = buf.len();
        if line.len() + n > max {
            return Err(WireError::LineTooLong);
        }
        line.extend_from_slice(buf);
        r.consume(n);
    }
}

/// True if no JSON array or object in `text` nests deeper than `max` (strings are skipped).
#[must_use]
pub fn depth_ok(text: &[u8], max: usize) -> bool {
    let mut depth = 0usize;
    let mut in_str = false;
    let mut esc = false;
    for &b in text {
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'[' | b'{' => {
                depth += 1;
                if depth > max {
                    return false;
                }
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    true
}

/// Parse one line (without its newline).
///
/// # Errors
/// See [`WireError`].
pub fn parse_message(line: &[u8]) -> Result<Message, WireError> {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    if line.iter().all(u8::is_ascii_whitespace) {
        return Err(WireError::Empty);
    }
    if line.len() > MAX_LINE {
        return Err(WireError::LineTooLong);
    }
    let text = std::str::from_utf8(line).map_err(|_| WireError::BadUtf8)?;
    if !depth_ok(line, MAX_DEPTH) {
        return Err(WireError::TooDeep);
    }
    let v: Value = serde_json::from_str(text).map_err(|e| WireError::BadJson(e.to_string()))?;
    let Value::Object(mut o) = v else {
        return Err(WireError::NotObject);
    };
    if o.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(WireError::BadVersion);
    }
    let method = match o.remove("method") {
        Some(Value::String(m)) if !m.is_empty() && m.len() <= 128 => Some(m),
        Some(_) => return Err(WireError::Invalid("method must be a short string".into())),
        None => None,
    };
    let id = o.remove("id");
    let params = o.remove("params").unwrap_or(Value::Null);
    if !matches!(params, Value::Null | Value::Object(_) | Value::Array(_)) {
        return Err(WireError::Invalid(
            "params must be an object or array".into(),
        ));
    }
    if let Some(id) = &id
        && !matches!(id, Value::Number(_) | Value::String(_) | Value::Null)
    {
        return Err(WireError::Invalid("id must be a number or string".into()));
    }
    match (method, id) {
        (Some(method), Some(id)) if !id.is_null() => Ok(Message::Request { id, method, params }),
        (Some(method), None) => Ok(Message::Notification { method, params }),
        (Some(_), Some(_)) => Err(WireError::Invalid("a request id may not be null".into())),
        (None, id) => {
            let id = id.unwrap_or(Value::Null);
            match (o.remove("result"), o.remove("error")) {
                (Some(r), None) => Ok(Message::Response { id, result: Ok(r) }),
                (None, Some(e)) if e.is_object() => Ok(Message::Response {
                    id,
                    result: Err(PluginError::from_json(&e)),
                }),
                _ => Err(WireError::Invalid(
                    "a response has exactly one of result and error".into(),
                )),
            }
        }
    }
}

fn line(m: Map<String, Value>) -> String {
    let mut s = Value::Object(m).to_string();
    s.push('\n');
    s
}

fn base() -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("jsonrpc".into(), Value::String("2.0".into()));
    m
}

/// A request line, ending in a newline.
#[must_use]
pub fn request(id: i64, method: &str, params: &Value) -> String {
    let mut m = base();
    m.insert("id".into(), id.into());
    m.insert("method".into(), method.into());
    m.insert("params".into(), params.clone());
    line(m)
}

/// A notification line, ending in a newline.
#[must_use]
pub fn notification(method: &str, params: &Value) -> String {
    let mut m = base();
    m.insert("method".into(), method.into());
    m.insert("params".into(), params.clone());
    line(m)
}

/// A success response line, ending in a newline.
#[must_use]
pub fn response_ok(id: &Value, result: &Value) -> String {
    let mut m = base();
    m.insert("id".into(), id.clone());
    m.insert("result".into(), result.clone());
    line(m)
}

/// An error response line, ending in a newline.
#[must_use]
pub fn response_err(id: &Value, err: &PluginError) -> String {
    let mut m = base();
    m.insert("id".into(), id.clone());
    m.insert("error".into(), err.to_json());
    line(m)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn lines_are_bounded() {
        let mut c = Cursor::new(b"ab\ncd".to_vec());
        assert_eq!(read_line_limited(&mut c, 10).unwrap(), Some(b"ab".to_vec()));
        assert_eq!(read_line_limited(&mut c, 10).unwrap(), Some(b"cd".to_vec()));
        assert_eq!(read_line_limited(&mut c, 10).unwrap(), None);
        let mut c = Cursor::new(vec![b'x'; 100]);
        assert_eq!(read_line_limited(&mut c, 10), Err(WireError::LineTooLong));
        let mut c = Cursor::new(b"abcdefghijkl\n".to_vec());
        assert_eq!(read_line_limited(&mut c, 10), Err(WireError::LineTooLong));
    }

    #[test]
    fn crlf() {
        let mut c = Cursor::new(b"{}\r\n".to_vec());
        assert_eq!(read_line_limited(&mut c, 10).unwrap(), Some(b"{}".to_vec()));
    }

    #[test]
    fn classifies() {
        let r =
            parse_message(br#"{"jsonrpc":"2.0","id":1,"method":"m","params":{"a":1}}"#).unwrap();
        assert!(matches!(r, Message::Request { .. }));
        let n = parse_message(br#"{"jsonrpc":"2.0","method":"progress","params":{}}"#).unwrap();
        assert!(matches!(n, Message::Notification { .. }));
        let ok = parse_message(br#"{"jsonrpc":"2.0","id":1,"result":null}"#).unwrap();
        assert!(matches!(ok, Message::Response { result: Ok(_), .. }));
        let e = parse_message(br#"{"jsonrpc":"2.0","id":1,"error":{"code":-1,"message":"x"}}"#)
            .unwrap();
        assert!(matches!(e, Message::Response { result: Err(_), .. }));
    }

    #[test]
    fn refuses_bad_input() {
        assert_eq!(parse_message(b""), Err(WireError::Empty));
        assert_eq!(parse_message(&[0xff, 0xfe]), Err(WireError::BadUtf8));
        assert_eq!(parse_message(b"[]"), Err(WireError::NotObject));
        assert_eq!(parse_message(b"{\"id\":1}"), Err(WireError::BadVersion));
        let deep = format!("{}{}", "[".repeat(40), "]".repeat(40));
        assert_eq!(parse_message(deep.as_bytes()), Err(WireError::TooDeep));
        assert!(matches!(parse_message(b"{"), Err(WireError::BadJson(_))));
        assert!(parse_message(br#"{"jsonrpc":"2.0","id":1,"result":1,"error":{}}"#).is_err());
        assert!(parse_message(br#"{"jsonrpc":"2.0","id":{},"method":"m"}"#).is_err());
    }

    #[test]
    fn deep_in_string_is_fine() {
        let s = format!(
            r#"{{"jsonrpc":"2.0","method":"m","params":{{"t":"{}"}}}}"#,
            "[".repeat(100)
        );
        assert!(parse_message(s.as_bytes()).is_ok());
    }
}
