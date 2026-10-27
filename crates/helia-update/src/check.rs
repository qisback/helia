// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The update policy: fetch, verify, then decide.
//!
//! The order matters and follows the TUF client workflow: the signature is
//! checked on the raw bytes first, so nothing unsigned is ever parsed beyond a
//! size check. Then the manifest must be for this app and channel (mix-and-match),
//! not expired (freeze), not older than the highest sequence seen (replay and
//! rollback), and carry no pre-release on the stable channel.

use std::io::Read;

use crate::error::{Error, Result};
use crate::manifest::{self, Asset, Kind, MAX_MANIFEST_BYTES, Manifest, ParseOptions, Platform};
use crate::source::UpdateSource;
use crate::state::{State, StateStore};
use crate::traits::{Clock, Fs, Http, Request};
use crate::verify;

/// Largest signature file read, in bytes.
pub const MAX_SIGNATURE_BYTES: usize = 4096;

/// Options for a check.
#[derive(Clone, Copy, Debug, Default)]
pub struct CheckOptions {
    /// The user explicitly asked to roll back: an older version in a correctly
    /// signed, fresh manifest is then offered (flagged `downgrade`).
    pub allow_downgrade: bool,
    /// Allow plain `http://127.0.0.1` URLs. For tests only.
    pub allow_loopback_http: bool,
}

/// An update on offer.
#[derive(Clone, Debug)]
pub struct Update {
    /// The verified manifest.
    pub manifest: Manifest,
    /// The offered version is older than the installed one (explicit rollback only).
    pub downgrade: bool,
    /// The installed version is older than the manifest's `min_supported`.
    pub required: bool,
}

impl Update {
    /// The asset for `platform`, preferring `kinds` in order.
    #[must_use]
    pub fn asset(&self, platform: Platform, kinds: &[Kind]) -> Option<&Asset> {
        self.manifest.select_asset(platform, kinds)
    }
}

/// What a check found.
#[derive(Clone, Debug)]
pub enum Outcome {
    /// The installed version is the offered one.
    UpToDate {
        /// The verified manifest.
        manifest: Manifest,
    },
    /// A newer version (or, with `allow_downgrade`, a different one) is on offer.
    Available(Update),
    /// The server's version is older than the installed one and a downgrade was
    /// not asked for. Nothing to do.
    OlderOnServer {
        /// The verified manifest.
        manifest: Manifest,
    },
    /// The manifest uses a newer format than this app reads: update by hand.
    SchemaTooNew {
        /// The schema number seen.
        schema: u32,
    },
}

/// Verify and validate manifest bytes against a source and the current knowledge.
///
/// This is the whole trust decision, as a pure function (the fuzz target calls
/// it). Order: size, signature, schema and parse, app and channel, expiry,
/// sequence, pre-release rule.
///
/// # Errors
/// Any of the errors in [`Error`] named above.
pub fn verify_manifest(
    source: &UpdateSource,
    bytes: &[u8],
    signature: &str,
    now: u64,
    highest_sequence: u64,
    allow_loopback_http: bool,
) -> Result<Manifest> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(Error::TooLarge {
            size: bytes.len(),
            max: MAX_MANIFEST_BYTES,
        });
    }
    verify::verify(bytes, signature, &source.pinned_keys)?;
    let m = manifest::parse(
        bytes,
        ParseOptions {
            allow_loopback_http,
        },
    )?;
    if let Some(a) = m.assets.iter().find(|a| !source.asset_host_allowed(&a.url)) {
        return Err(Error::ForeignHost(a.url.chars().take(200).collect()));
    }
    if m.app != source.app || m.channel != source.channel {
        return Err(Error::WrongTarget {
            expected: format!("{}/{}", source.app, source.channel),
            found: format!("{}/{}", m.app, m.channel),
        });
    }
    if m.expires <= now {
        return Err(Error::Expired {
            expires: m.expires,
            now,
        });
    }
    if m.sequence < highest_sequence {
        return Err(Error::Replay {
            seen: m.sequence,
            highest: highest_sequence,
        });
    }
    if source.channel == "stable" && !m.version.pre.is_empty() {
        return Err(Error::PrereleaseOnStable(m.version.to_string()));
    }
    Ok(m)
}

/// Read at most `max` bytes; one more than that is an error.
pub(crate) fn read_limited(mut r: impl Read, max: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    (&mut r)
        .take(max as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|e| Error::Http(e.to_string()))?;
    if out.len() > max {
        return Err(Error::TooLarge {
            size: out.len(),
            max,
        });
    }
    Ok(out)
}

/// Ties the three injected parts together.
pub struct Updater<'a> {
    /// HTTP client.
    pub http: &'a dyn Http,
    /// File system.
    pub fs: &'a dyn Fs,
    /// Clock.
    pub clock: &'a dyn Clock,
}

impl Updater<'_> {
    /// Fetch the manifest and its signature. Returns the exact bytes and the signature text.
    ///
    /// # Errors
    /// [`Error::InsecureUrl`], [`Error::Http`], [`Error::HttpStatus`], [`Error::TooLarge`].
    pub fn fetch(
        &self,
        source: &UpdateSource,
        allow_loopback_http: bool,
    ) -> Result<(Vec<u8>, String)> {
        let get = |url: &str, max: usize| -> Result<Vec<u8>> {
            manifest::check_url(url, allow_loopback_http)?;
            if !source.manifest_host_allowed(url) {
                return Err(Error::ForeignHost(url.chars().take(200).collect()));
            }
            let resp = self.http.get(&Request {
                url,
                range_from: None,
                allowed_hosts: &source.manifest_hosts,
            })?;
            if resp.status != 200 {
                return Err(Error::HttpStatus(resp.status));
            }
            read_limited(resp.body, max)
        };
        let bytes = get(&source.manifest_url, MAX_MANIFEST_BYTES)?;
        let sig = get(&source.signature_url(), MAX_SIGNATURE_BYTES)?;
        let sig =
            String::from_utf8(sig).map_err(|_| Error::BadSignatureFormat("not text".into()))?;
        Ok((bytes, sig))
    }

    /// Check for an update. Records the attempt in `state.last_check` and, when the
    /// manifest verifies, its sequence in `state.highest_sequence`. The caller saves
    /// the state (or use [`Self::check_stored`]).
    ///
    /// `current` is the installed version.
    ///
    /// # Errors
    /// Any [`Error`] from fetching, verifying or the policy; [`Error::Malformed`]
    /// when `current` is not a version.
    pub fn check(
        &self,
        source: &UpdateSource,
        current: &str,
        state: &mut State,
        opts: CheckOptions,
    ) -> Result<Outcome> {
        source.validate(opts.allow_loopback_http)?;
        let current = semver::Version::parse(current)
            .map_err(|e| Error::Malformed(format!("installed version {current:?}: {e}")))?;
        let now = self.clock.now_unix();
        state.last_check = now;
        let (bytes, sig) = self.fetch(source, opts.allow_loopback_http)?;
        let m = match verify_manifest(
            source,
            &bytes,
            &sig,
            now,
            state.highest_sequence,
            opts.allow_loopback_http,
        ) {
            Ok(m) => m,
            Err(Error::SchemaTooNew(schema)) => return Ok(Outcome::SchemaTooNew { schema }),
            Err(e) => return Err(e),
        };
        state.highest_sequence = state.highest_sequence.max(m.sequence);
        let required = m.min_supported.as_ref().is_some_and(|min| current < *min);
        Ok(match m.version.cmp_precedence(&current) {
            std::cmp::Ordering::Equal => Outcome::UpToDate { manifest: m },
            std::cmp::Ordering::Greater => Outcome::Available(Update {
                manifest: m,
                downgrade: false,
                required,
            }),
            std::cmp::Ordering::Less if opts.allow_downgrade => Outcome::Available(Update {
                manifest: m,
                downgrade: true,
                required,
            }),
            std::cmp::Ordering::Less => Outcome::OlderOnServer { manifest: m },
        })
    }

    /// [`Self::check`] with the state loaded from and saved to `store`. The state is
    /// saved even when the check fails, so a failing server is not retried at once.
    ///
    /// # Errors
    /// As [`Self::check`], and [`Error::Io`] when saving fails.
    pub fn check_stored(
        &self,
        source: &UpdateSource,
        current: &str,
        store: &StateStore<'_>,
        opts: CheckOptions,
    ) -> Result<Outcome> {
        let mut state = store.load_or_default(&source.app, &source.channel);
        let r = self.check(source, current, &mut state, opts);
        store.save(&state)?;
        r
    }
}
