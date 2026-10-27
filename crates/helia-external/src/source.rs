// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Where an app gets its manifest, which key signs it, and which hosts it may talk to.
//!
//! The host lists live in the application, not in the manifest: a manifest can name
//! any URL, but a download is only made when its host is on the app's own list, so a
//! signed manifest cannot widen what the app is willing to contact.

use helia_update::manifest::{check_url, url_host};
use helia_update::verify::parse_public_key;

use crate::error::{Error, Result};

/// A source of external items for one app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalSource {
    /// App name as written in the manifest, for example `helia`.
    pub app: String,
    /// URL of the manifest; the signature is that URL plus `.minisig`.
    pub manifest_url: String,
    /// One or two minisign public keys (the second is for rotation): the same pinned
    /// release keys the update check uses.
    pub pinned_keys: Vec<String>,
    /// Hosts the manifest and its signature may come from, redirect hops included.
    pub manifest_hosts: Vec<String>,
    /// Hosts the downloads, redirect hops included, may come from.
    pub asset_hosts: Vec<String>,
    /// Allow plain `http://127.0.0.1` (tests only).
    pub allow_loopback_http: bool,
}

impl ExternalSource {
    /// A source whose manifest host is also the only download host. Replace the
    /// download hosts with [`Self::with_asset_hosts`].
    #[must_use]
    pub fn new(app: &str, manifest_url: &str, pinned_keys: &[&str]) -> Self {
        let host: Vec<String> = url_host(manifest_url).into_iter().collect();
        Self {
            app: app.to_string(),
            manifest_url: manifest_url.to_string(),
            pinned_keys: pinned_keys.iter().map(|k| (*k).to_string()).collect(),
            manifest_hosts: host.clone(),
            asset_hosts: host,
            allow_loopback_http: false,
        }
    }

    /// Replace the download host list (exact match, lower case, no port).
    #[must_use]
    pub fn with_asset_hosts(mut self, hosts: &[&str]) -> Self {
        self.asset_hosts = hosts.iter().map(|h| (*h).to_ascii_lowercase()).collect();
        self
    }

    /// Replace the manifest host list.
    #[must_use]
    pub fn with_manifest_hosts(mut self, hosts: &[&str]) -> Self {
        self.manifest_hosts = hosts.iter().map(|h| (*h).to_ascii_lowercase()).collect();
        self
    }

    /// Allow plain http to `127.0.0.1` (tests only).
    #[must_use]
    pub fn with_loopback_http(mut self, allow: bool) -> Self {
        self.allow_loopback_http = allow;
        self
    }

    /// The URL of the detached signature.
    #[must_use]
    pub fn signature_url(&self) -> String {
        format!("{}.minisig", self.manifest_url)
    }

    /// Is `url` on a host the manifest may come from?
    #[must_use]
    pub fn manifest_host_allowed(&self, url: &str) -> bool {
        url_host(url).is_some_and(|h| self.manifest_hosts.contains(&h))
    }

    /// Is `url` on a host downloads may come from?
    #[must_use]
    pub fn asset_host_allowed(&self, url: &str) -> bool {
        url_host(url).is_some_and(|h| self.asset_hosts.contains(&h))
    }

    /// Check the source: a name, one or two parseable keys, one to eight hosts in each
    /// list, an https manifest URL on an allowed host.
    ///
    /// # Errors
    /// [`Error::BadSource`], or [`Error::Transport`] for a bad key or address.
    pub fn validate(&self) -> Result<()> {
        let ok = !self.app.is_empty()
            && self.app.len() <= 64
            && self.app.bytes().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-')
            });
        if !ok {
            return Err(Error::BadSource(
                "the app name must be 1 to 64 characters of a-z 0-9 . _ -".into(),
            ));
        }
        if self.pinned_keys.is_empty() || self.pinned_keys.len() > 2 {
            return Err(Error::Transport(helia_update::Error::KeyCount(
                self.pinned_keys.len(),
            )));
        }
        for k in &self.pinned_keys {
            parse_public_key(k)?;
        }
        for list in [&self.manifest_hosts, &self.asset_hosts] {
            if list.is_empty() || list.len() > 8 {
                return Err(Error::BadSource(
                    "one to eight allowed hosts are required in each list".into(),
                ));
            }
        }
        check_url(&self.manifest_url, self.allow_loopback_http)?;
        if !self.manifest_host_allowed(&self.manifest_url) {
            return Err(Error::BadSource(
                "the manifest address is not on an allowed host".into(),
            ));
        }
        Ok(())
    }
}
