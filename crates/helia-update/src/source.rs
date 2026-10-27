// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Where an app, plugin or add-on gets its updates.

use crate::error::{Error, Result};
use crate::manifest::{check_url, url_host};
use crate::verify::parse_public_key;

/// An update source. The same type serves Helia, Valerian, Annora, plugins and
/// add-ons: each has its own manifest URL and its own pinned keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateSource {
    /// App (or plugin) name as written in the manifest, for example `helia`.
    pub app: String,
    /// Channel, for example `stable`. `stable` refuses pre-release versions.
    pub channel: String,
    /// URL of `update-<app>-<channel>.json`; the signature is that URL plus `.minisig`.
    pub manifest_url: String,
    /// One or two minisign public keys (the second is for rotation), each the
    /// `.pub` file contents or its bare base64 line.
    pub pinned_keys: Vec<String>,
    /// Hosts the manifest and its signature may come from (exact match, lower
    /// case, no port), including every redirect hop. [`Self::new`] sets it to the
    /// manifest URL's host.
    pub manifest_hosts: Vec<String>,
    /// Hosts the asset URLs, and every redirect hop of a download, must be on.
    /// [`Self::new`] sets it to the manifest URL's host; a GitHub release needs
    /// `github.com` and `objects.githubusercontent.com` here.
    pub asset_hosts: Vec<String>,
}

impl UpdateSource {
    /// Build a source from string slices.
    #[must_use]
    pub fn new(app: &str, channel: &str, manifest_url: &str, pinned_keys: &[&str]) -> Self {
        Self {
            app: app.to_string(),
            channel: channel.to_string(),
            manifest_url: manifest_url.to_string(),
            pinned_keys: pinned_keys.iter().map(|k| (*k).to_string()).collect(),
            manifest_hosts: url_host(manifest_url).into_iter().collect(),
            asset_hosts: url_host(manifest_url).into_iter().collect(),
        }
    }

    /// The URL of the detached signature.
    #[must_use]
    pub fn signature_url(&self) -> String {
        format!("{}.minisig", self.manifest_url)
    }

    /// Replace both host lists (build-time configuration).
    #[must_use]
    pub fn with_hosts(mut self, manifest_hosts: &[&str], asset_hosts: &[&str]) -> Self {
        self.manifest_hosts = manifest_hosts
            .iter()
            .map(|h| (*h).to_ascii_lowercase())
            .collect();
        self.asset_hosts = asset_hosts
            .iter()
            .map(|h| (*h).to_ascii_lowercase())
            .collect();
        self
    }

    /// Is `url` on a host the manifest may come from?
    #[must_use]
    pub fn manifest_host_allowed(&self, url: &str) -> bool {
        url_host(url).is_some_and(|h| self.manifest_hosts.contains(&h))
    }

    /// Is `url` on a host assets may come from?
    #[must_use]
    pub fn asset_host_allowed(&self, url: &str) -> bool {
        url_host(url).is_some_and(|h| self.asset_hosts.contains(&h))
    }

    /// Check the source: names, one or two parseable keys, https manifest URL.
    ///
    /// # Errors
    /// [`Error::BadSource`], [`Error::KeyCount`], [`Error::BadKey`] or [`Error::InsecureUrl`].
    pub fn validate(&self, allow_loopback_http: bool) -> Result<()> {
        let name_ok = |s: &str| {
            !s.is_empty()
                && s.len() <= 64
                && s.bytes().all(|c| {
                    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-')
                })
        };
        if !name_ok(&self.app) || !name_ok(&self.channel) {
            return Err(Error::BadSource(
                "app and channel must be 1 to 64 characters of a-z 0-9 . _ -".into(),
            ));
        }
        if self.pinned_keys.is_empty() || self.pinned_keys.len() > 2 {
            return Err(Error::KeyCount(self.pinned_keys.len()));
        }
        for k in &self.pinned_keys {
            parse_public_key(k)?;
        }
        for list in [&self.manifest_hosts, &self.asset_hosts] {
            if list.is_empty() || list.len() > 4 {
                return Err(Error::BadSource(
                    "one to four allowed hosts are required in each list".into(),
                ));
            }
        }
        check_url(&self.manifest_url, allow_loopback_http)?;
        if !self.manifest_host_allowed(&self.manifest_url) {
            return Err(Error::BadSource(
                "the manifest URL is not on an allowed host".into(),
            ));
        }
        Ok(())
    }
}
