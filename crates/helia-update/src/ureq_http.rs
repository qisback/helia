// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The real [`Http`]: `ureq` with rustls. Redirects are followed by hand so that
//! every hop is checked against the source's allowed hosts and must be https.
//!
//! Headers sent: `Host`, `User-Agent: helia-update/<version>`, `Accept: */*`, and
//! `Range` when resuming (plus what the HTTP library adds for framing). No cookies,
//! no `Authorization`, no identifiers.

use std::time::Duration;

use crate::error::{Error, Result};
use crate::manifest::{check_url, url_host};
use crate::traits::{Http, Request, Response};

const MAX_HOPS: usize = 5;

/// `ureq` based client.
pub struct UreqHttp {
    agent: ureq::Agent,
    allow_loopback_http: bool,
}

impl UreqHttp {
    /// A client. `allow_loopback_http` permits `http://127.0.0.1` and exists for tests.
    #[must_use]
    pub fn new(allow_loopback_http: bool) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .https_only(!allow_loopback_http)
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .timeout_recv_body(Some(Duration::from_secs(60)))
            .user_agent(crate::USER_AGENT)
            .build()
            .into();
        Self {
            agent,
            allow_loopback_http,
        }
    }
}

fn content_range_start(v: &str) -> Option<u64> {
    v.strip_prefix("bytes ")?.split('-').next()?.parse().ok()
}

impl Http for UreqHttp {
    fn get(&self, req: &Request<'_>) -> Result<Response> {
        let mut url = req.url.to_string();
        for _ in 0..=MAX_HOPS {
            check_url(&url, self.allow_loopback_http)?;
            if !url_host(&url).is_some_and(|h| req.allowed_hosts.contains(&h)) {
                return Err(Error::ForeignHost(url.chars().take(200).collect()));
            }
            let mut r = self.agent.get(&url);
            if let Some(n) = req.range_from {
                r = r.header("Range", format!("bytes={n}-"));
            }
            let resp = r.call().map_err(|e| Error::Http(e.to_string()))?;
            let status = resp.status().as_u16();
            if matches!(status, 301 | 302 | 303 | 307 | 308) {
                let loc = resp
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| Error::Http("redirect without a location".into()))?;
                // Only absolute locations: a relative one stays on this host by construction.
                url = if loc.starts_with('/') {
                    let base = url
                        .split("://")
                        .nth(1)
                        .unwrap_or("")
                        .split('/')
                        .next()
                        .unwrap_or("");
                    format!(
                        "{}://{base}{loc}",
                        if url.starts_with("https") {
                            "https"
                        } else {
                            "http"
                        }
                    )
                } else {
                    loc.to_string()
                };
                continue;
            }
            let start = resp
                .headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok())
                .and_then(content_range_start);
            return Ok(Response {
                status,
                content_range_start: start,
                body: Box::new(resp.into_body().into_reader()),
            });
        }
        Err(Error::Http("too many redirects".into()))
    }
}
