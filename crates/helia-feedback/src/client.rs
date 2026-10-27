// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The relay client: fetch a challenge, solve it, post the report.
//!
//! Nothing here runs unless the caller calls [`submit`], which a person does by
//! pressing Send (or running `helia feedback`). Blocking: call it from a
//! worker thread. It checks `cancel` between steps and while solving the proof
//! of work; the network calls are bounded by timeouts instead.
//!
//! # Authentication
//!
//! How the client proves it is not a flood is behind [`RelayAuth`]. Today the
//! only method is [`ProofOfWork`] (fetch a challenge, find a nonce). The owner
//! plans an API that accepts the project with a TLS client certificate
//! (mutual TLS); that will be another [`RelayAuth`] whose
//! [`agent`](RelayAuth::agent) presents the certificate and whose
//! [`prepare`](RelayAuth::prepare) returns an [`AuthProof`] with no `pow`. The
//! dialog and the report format do not change. It is not implemented.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::pow;
use crate::protocol::{
    CHALLENGE_PATH, Challenge, Created, DEFAULT_RELAY_URL, ErrorBody, FEEDBACK_PATH,
    MAX_DIFFICULTY, Pow, Report,
};
use crate::screenshot::Screenshot;

/// What went wrong while sending.
#[derive(Debug, thiserror::Error)]
pub enum FeedbackError {
    /// The report breaks a limit.
    #[error("the report is not valid: {0}")]
    Invalid(String),
    /// The relay could not be reached (no network, DNS, TLS, timeout).
    #[error("could not reach the feedback service: {0}")]
    Unreachable(String),
    /// The person cancelled.
    #[error("cancelled")]
    Cancelled,
    /// HTTP 429.
    #[error("the feedback service is busy; try again{}", .retry_after.map_or_else(String::new, |s| format!(" in {s} seconds")))]
    RateLimited {
        /// The `Retry-After` header, in seconds.
        retry_after: Option<u64>,
    },
    /// HTTP 503.
    #[error("the feedback service is degraded at the moment")]
    Degraded,
    /// The relay refused the report (400, 413, 415 and other 4xx).
    #[error("the feedback service refused the report ({status}): {message}")]
    Rejected {
        /// HTTP status.
        status: u16,
        /// The relay's error code, if it sent one.
        code: String,
        /// The relay's message.
        message: String,
    },
    /// An answer that is not part of the protocol.
    #[error("unexpected answer from the feedback service: {0}")]
    BadResponse(String),
    /// The relay asked for more work than a client will do.
    #[error("the feedback service asked for an unreasonable proof of work ({0} bits)")]
    ChallengeTooHard(u32),
}

impl FeedbackError {
    /// Whether to offer "Save report" and "Open in browser". Everything but a
    /// deliberate cancel and a report that needs editing first.
    #[must_use]
    pub fn offers_fallback(&self) -> bool {
        !matches!(self, Self::Cancelled | Self::Invalid(_))
    }
}

/// Where the relay is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Relay {
    base: String,
}

impl Relay {
    /// A relay at `url`. Must be https; plain http is allowed only for a
    /// loopback address, for tests and local development.
    ///
    /// # Errors
    /// [`FeedbackError::Invalid`] for any other address.
    pub fn new(url: &str) -> Result<Self, FeedbackError> {
        let url = url.trim().trim_end_matches('/');
        let loopback = ["http://127.0.0.1", "http://localhost", "http://[::1]"]
            .iter()
            .any(|p| {
                url.strip_prefix(p)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with([':', '/']))
            });
        if url.starts_with("https://") && url.len() > "https://".len() || loopback {
            Ok(Self {
                base: url.to_owned(),
            })
        } else {
            Err(FeedbackError::Invalid(format!(
                "the feedback address must start with https:// ({url})"
            )))
        }
    }

    /// The address to use: the environment variable `HELIA_FEEDBACK_URL`, else
    /// `setting` (from the settings file or a flag), else the public relay.
    ///
    /// # Errors
    /// As [`Self::new`].
    pub fn configured(setting: Option<&str>) -> Result<Self, FeedbackError> {
        let env = std::env::var("HELIA_FEEDBACK_URL")
            .ok()
            .filter(|s| !s.trim().is_empty());
        Self::new(
            env.as_deref()
                .or(setting.filter(|s| !s.trim().is_empty()))
                .unwrap_or(DEFAULT_RELAY_URL),
        )
    }

    /// The base address, without a trailing slash.
    #[must_use]
    pub fn base(&self) -> &str {
        &self.base
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }
}

/// What the client shows the person while it works.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Fetching the challenge and solving it.
    Preparing,
    /// Uploading the report.
    Sending,
}

/// What an authentication method adds to a submission.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuthProof {
    /// The solved proof of work, for [`ProofOfWork`].
    pub pow: Option<Pow>,
}

/// How a submission proves it may be sent.
pub trait RelayAuth: Send + Sync {
    /// A short name for logs and the docs.
    fn name(&self) -> &'static str;

    /// The HTTP agent to use. The default is a plain one; a client-certificate
    /// method overrides this to present its certificate.
    fn agent(&self) -> ureq::Agent {
        default_agent()
    }

    /// Do whatever the method needs before the report is posted.
    ///
    /// # Errors
    /// Any [`FeedbackError`]; [`FeedbackError::Cancelled`] when `cancel` is set.
    fn prepare(
        &self,
        relay: &Relay,
        agent: &ureq::Agent,
        cancel: &AtomicBool,
    ) -> Result<AuthProof, FeedbackError>;
}

/// An agent with sane timeouts that hands back non-2xx answers as responses.
#[must_use]
pub fn default_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(60)))
        .timeout_connect(Some(Duration::from_secs(10)))
        .user_agent(concat!("helia-feedback/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// The proof-of-work method: `GET` a challenge, find a nonce.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProofOfWork;

impl RelayAuth for ProofOfWork {
    fn name(&self) -> &'static str {
        "proof-of-work"
    }

    fn prepare(
        &self,
        relay: &Relay,
        agent: &ureq::Agent,
        cancel: &AtomicBool,
    ) -> Result<AuthProof, FeedbackError> {
        let mut resp = agent
            .get(&relay.url(CHALLENGE_PATH))
            .call()
            .map_err(|e| FeedbackError::Unreachable(e.to_string()))?;
        let status = resp.status().as_u16();
        let retry = retry_after(&resp);
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        if status != 200 {
            return Err(map_status(status, retry, &text));
        }
        let ch: Challenge = serde_json::from_str(&text)
            .map_err(|e| FeedbackError::BadResponse(format!("challenge: {e}")))?;
        if ch.difficulty > MAX_DIFFICULTY {
            return Err(FeedbackError::ChallengeTooHard(ch.difficulty));
        }
        if pow::decode_hex(&ch.challenge).is_none_or(|c| c.is_empty()) {
            return Err(FeedbackError::BadResponse("challenge is not hex".into()));
        }
        let nonce = pow::solve(&ch.challenge, ch.difficulty, cancel).ok_or_else(|| {
            if cancel.load(Ordering::Relaxed) {
                FeedbackError::Cancelled
            } else {
                FeedbackError::BadResponse("no solution".into())
            }
        })?;
        Ok(AuthProof {
            pow: Some(Pow {
                challenge: ch.challenge,
                nonce,
            }),
        })
    }
}

fn retry_after(resp: &ureq::http::Response<ureq::Body>) -> Option<u64> {
    resp.headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok())
}

fn map_status(status: u16, retry: Option<u64>, body: &str) -> FeedbackError {
    match status {
        429 => FeedbackError::RateLimited { retry_after: retry },
        503 => FeedbackError::Degraded,
        400..=499 => {
            let (code, message) = serde_json::from_str::<ErrorBody>(body).map_or_else(
                |_| (String::new(), format!("HTTP {status}")),
                |b| (b.error.code, b.error.message),
            );
            FeedbackError::Rejected {
                status,
                code,
                message,
            }
        }
        _ => FeedbackError::BadResponse(format!("HTTP {status}")),
    }
}

/// The `multipart/form-data` body: the `report` part, then the `screenshot`
/// part when there is one.
#[must_use]
pub fn multipart_body(boundary: &str, report_json: &str, screenshot_png: Option<&[u8]>) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"report\"\r\nContent-Type: application/json\r\n\r\n"
        )
        .as_bytes(),
    );
    b.extend_from_slice(report_json.as_bytes());
    b.extend_from_slice(b"\r\n");
    if let Some(png) = screenshot_png {
        b.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"screenshot\"; filename=\"screenshot.png\"\r\nContent-Type: image/png\r\n\r\n"
            )
            .as_bytes(),
        );
        b.extend_from_slice(png);
        b.extend_from_slice(b"\r\n");
    }
    b.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    b
}

fn boundary() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("----helia-feedback-{nanos:x}-{:x}", std::process::id())
}

/// Send a report. `report.pow` is filled in by the authentication method.
/// `screenshot` is the clean screenshot; `report.highlights` are in its
/// pixels, and are rescaled if the image has to be shrunk to fit the relay's
/// limit.
///
/// # Errors
/// See [`FeedbackError`]; the caller decides whether to offer the fallbacks
/// (see [`FeedbackError::offers_fallback`]).
pub fn submit(
    relay: &Relay,
    auth: &dyn RelayAuth,
    mut report: Report,
    screenshot: Option<&Screenshot>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Phase),
) -> Result<Created, FeedbackError> {
    report.pow = None;
    report
        .validate()
        .map_err(|e| FeedbackError::Invalid(e.to_string()))?;
    let png = if let Some(shot) = screenshot {
        let (png, highlights) = shot
            .encode_for_relay(&report.highlights)
            .map_err(|e| FeedbackError::Invalid(e.to_string()))?;
        report.highlights = highlights;
        Some(png)
    } else {
        report.highlights.clear();
        None
    };
    let cancelled = || cancel.load(Ordering::Relaxed);
    let agent = auth.agent();
    progress(Phase::Preparing);
    let proof = auth.prepare(relay, &agent, cancel)?;
    if cancelled() {
        return Err(FeedbackError::Cancelled);
    }
    report.pow = proof.pow;
    progress(Phase::Sending);
    let boundary = boundary();
    let body = multipart_body(&boundary, &report.to_json(), png.as_deref());
    let mut resp = agent
        .post(&relay.url(FEEDBACK_PATH))
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .send(&body[..])
        .map_err(|e| FeedbackError::Unreachable(e.to_string()))?;
    let status = resp.status().as_u16();
    let retry = retry_after(&resp);
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    if cancelled() {
        // The relay may already have filed it; say so rather than hide it.
        return Err(FeedbackError::Cancelled);
    }
    if status == 201 {
        serde_json::from_str(&text).map_err(|e| FeedbackError::BadResponse(format!("created: {e}")))
    } else {
        Err(map_status(status, retry, &text))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use super::*;

    #[test]
    fn only_https_or_loopback_addresses_are_accepted() {
        assert!(Relay::new("https://feedback.discworld.casa/").is_ok());
        assert_eq!(
            Relay::new("https://feedback.discworld.casa/")
                .unwrap()
                .base(),
            "https://feedback.discworld.casa"
        );
        assert!(Relay::new("http://127.0.0.1:8080").is_ok());
        assert!(Relay::new("http://localhost").is_ok());
        assert!(Relay::new("http://example.com").is_err());
        assert!(Relay::new("http://127.0.0.1.evil.com").is_err());
        assert!(Relay::new("https://").is_err());
        assert!(Relay::new("ftp://x").is_err());
    }

    #[test]
    fn the_multipart_body_has_the_documented_parts() {
        let b = multipart_body("BND", "{\"a\":1}", Some(&[1, 2, 3]));
        let s = String::from_utf8_lossy(&b);
        assert!(s.starts_with("--BND\r\nContent-Disposition: form-data; name=\"report\"\r\nContent-Type: application/json\r\n\r\n{\"a\":1}\r\n--BND\r\n"));
        assert!(s.contains(
            "name=\"screenshot\"; filename=\"screenshot.png\"\r\nContent-Type: image/png"
        ));
        assert!(s.ends_with("--BND--\r\n"));
        let none = multipart_body("BND", "{}", None);
        assert!(!String::from_utf8_lossy(&none).contains("screenshot"));
    }

    #[test]
    fn statuses_map_to_the_protocol_errors() {
        assert!(matches!(
            map_status(429, Some(30), ""),
            FeedbackError::RateLimited {
                retry_after: Some(30)
            }
        ));
        assert!(matches!(map_status(503, None, ""), FeedbackError::Degraded));
        let body = r#"{"error":{"code":"too_big","message":"Screenshot too large"}}"#;
        match map_status(413, None, body) {
            FeedbackError::Rejected {
                status,
                code,
                message,
            } => {
                assert_eq!((status, code.as_str()), (413, "too_big"));
                assert_eq!(message, "Screenshot too large");
            }
            e => panic!("{e:?}"),
        }
        assert!(matches!(
            map_status(500, None, ""),
            FeedbackError::BadResponse(_)
        ));
        assert!(FeedbackError::Degraded.offers_fallback());
        assert!(!FeedbackError::Cancelled.offers_fallback());
    }
}
