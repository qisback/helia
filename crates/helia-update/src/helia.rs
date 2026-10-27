// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Helia's own update sources, configured at build time.
//!
//! The keys are in [`crate::pinned_keys`]. The addresses come from environment
//! variables read when the crate is compiled (`option_env!`), so a release build,
//! the fallback hosting and the end-to-end test need no code change:
//!
//! | Variable | Default | Meaning |
//! |---|---|---|
//! | `HELIA_UPDATE_BASE_URL` | `https://12hobbies.studio/updates` (fallback hosting; R2 follows) | the manifest is `<base>/helia/<channel>/manifest.json` |
//! | `HELIA_UPDATE_MANIFEST_HOSTS` | `12hobbies.studio` when the base URL is the default, else the host of the base URL | comma-separated hosts the manifest may come from |
//! | `HELIA_UPDATE_ASSET_HOSTS` | `github.com,objects.githubusercontent.com` when the base URL is the default, else the host of the base URL | comma-separated hosts the files may come from |
//! | `HELIA_UPDATE_PINNED_KEYS` | the keys in `pinned_keys.rs` | comma-separated minisign public keys; end-to-end tests only, a release build must not set it ([`keys_overridden`]) |
//!
//! Fallback hosting for a first release: base `https://12hobbies.studio/updates`,
//! manifest hosts `12hobbies.studio`, asset hosts
//! `github.com,objects.githubusercontent.com`. End-to-end test: base
//! `http://127.0.0.1:PORT`, both host lists `127.0.0.1` (plain http is then
//! allowed to loopback only: see [`allow_loopback_http`]).

use crate::manifest::url_host;
use crate::source::UpdateSource;

pub use crate::pinned_keys::{KEYS_ARE_PLACEHOLDER, PINNED_KEYS};

/// Default base URL.
pub const DEFAULT_BASE_URL: &str = "https://12hobbies.studio/updates";

/// Default manifest hosts (fallback hosting, used only when `HELIA_UPDATE_BASE_URL` is unset).
pub const DEFAULT_MANIFEST_HOSTS: &str = "12hobbies.studio";

/// Default file hosts (fallback hosting, used only when `HELIA_UPDATE_BASE_URL` is unset).
pub const DEFAULT_ASSET_HOSTS: &str = "github.com,objects.githubusercontent.com";

/// The base URL this build uses.
#[must_use]
pub fn base_url() -> &'static str {
    option_env!("HELIA_UPDATE_BASE_URL").unwrap_or(DEFAULT_BASE_URL)
}

/// The public keys this build trusts: [`PINNED_KEYS`], or the build-time
/// `HELIA_UPDATE_PINNED_KEYS` list when one is set (end-to-end tests).
#[must_use]
pub fn pinned_keys() -> Vec<&'static str> {
    match option_env!("HELIA_UPDATE_PINNED_KEYS") {
        Some(l) if !l.trim().is_empty() => l
            .split(',')
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .collect(),
        _ => PINNED_KEYS.to_vec(),
    }
}

/// True when this build trusts a key list set at build time instead of the pinned keys.
/// The release workflow refuses to publish such a build.
#[must_use]
pub fn keys_overridden() -> bool {
    option_env!("HELIA_UPDATE_PINNED_KEYS").is_some_and(|l| !l.trim().is_empty())
}

/// Does this build talk to a loopback test server (plain http to `127.0.0.1`)?
/// Pass the result as `allow_loopback_http` to the checker, the downloader and
/// `UreqHttp::new`.
#[must_use]
pub fn allow_loopback_http() -> bool {
    base_url().starts_with("http://127.0.0.1")
}

/// Helia's update source for `channel` (`stable` or `prerelease`).
#[must_use]
pub fn source(channel: &str) -> UpdateSource {
    // The default hosts belong to the default base URL; a build that sets its own base
    // URL and no host lists gets that URL's host, as before.
    let default_base = option_env!("HELIA_UPDATE_BASE_URL").is_none();
    let pick = |env: Option<&'static str>, dflt: &'static str| {
        env.or_else(|| default_base.then_some(dflt))
    };
    source_from(
        base_url(),
        pick(
            option_env!("HELIA_UPDATE_MANIFEST_HOSTS"),
            DEFAULT_MANIFEST_HOSTS,
        ),
        pick(option_env!("HELIA_UPDATE_ASSET_HOSTS"), DEFAULT_ASSET_HOSTS),
        channel,
    )
}

/// The same with explicit configuration (what [`source`] passes; for tests).
/// Host lists are comma separated; absent or empty means the base URL's host.
#[must_use]
pub fn source_from(
    base: &str,
    manifest_hosts: Option<&str>,
    asset_hosts: Option<&str>,
    channel: &str,
) -> UpdateSource {
    let url = format!(
        "{}/helia/{channel}/manifest.json",
        base.trim_end_matches('/')
    );
    let default = url_host(&url).unwrap_or_default();
    let list = |l: Option<&str>| -> Vec<String> {
        let v: Vec<String> = l
            .unwrap_or_default()
            .split(',')
            .map(|h| h.trim().to_ascii_lowercase())
            .filter(|h| !h.is_empty())
            .collect();
        if v.is_empty() {
            vec![default.clone()]
        } else {
            v
        }
    };
    let (mh, ah) = (list(manifest_hosts), list(asset_hosts));
    let mut s = UpdateSource::new("helia", channel, &url, &pinned_keys());
    s.manifest_hosts = mh;
    s.asset_hosts = ah;
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_build_uses_the_fallback_hosting() {
        if option_env!("HELIA_UPDATE_BASE_URL").is_some() {
            return;
        }
        let s = source("stable");
        assert_eq!(
            s.manifest_url,
            "https://12hobbies.studio/updates/helia/stable/manifest.json"
        );
        assert_eq!(s.manifest_hosts, ["12hobbies.studio"]);
        assert_eq!(
            s.asset_hosts,
            ["github.com", "objects.githubusercontent.com"]
        );
    }
}
