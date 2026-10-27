// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Helia's own source of external items, configured at build time.
//!
//! The keys are the update keys (`helia_update::helia::pinned_keys`). The manifest
//! address and the hosts come from environment variables read when the crate is compiled
//! (`option_env!`), so a release build, the fallback hosting and the end-to-end test need
//! no code change:
//!
//! | Variable | Default | Meaning |
//! |---|---|---|
//! | `HELIA_EXTERNAL_MANIFEST_URL` | `<update base URL>/helia/external.json` | the manifest; its signature is that address plus `.minisig` |
//! | `HELIA_EXTERNAL_MANIFEST_HOSTS` | the update manifest hosts | comma-separated hosts the manifest may come from |
//! | `HELIA_EXTERNAL_ASSET_HOSTS` | [`DEFAULT_ASSET_HOSTS`] | comma-separated hosts the downloads may come from |
//!
//! A test build points the manifest at `http://127.0.0.1:PORT/...`; the manifest host is then
//! `127.0.0.1` and plain http is allowed to loopback only.

use crate::source::ExternalSource;

/// The app name written in Helia's manifest.
pub const APP: &str = "helia";

/// Environment variable that puts the records and tools under one folder (tests, portable use).
pub const DIR_ENV: &str = "HELIA_EXTERNAL_DIR";

/// Hosts downloads may come from by default: GitHub release assets (`BtbN` `FFmpeg-Builds`,
/// reached through a redirect to the GitHub asset host) evermeet.cx (Intel macOS builds) and ffmpeg.martin-riedl.de (Apple silicon builds).
pub const DEFAULT_ASSET_HOSTS: &str = "github.com,objects.githubusercontent.com,release-assets.githubusercontent.com,evermeet.cx,ffmpeg.martin-riedl.de";

fn list(s: &str) -> Vec<&str> {
    s.split(',')
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .collect()
}

/// The manifest address this build uses.
#[must_use]
pub fn manifest_url() -> String {
    option_env!("HELIA_EXTERNAL_MANIFEST_URL")
        .filter(|u| !u.trim().is_empty())
        .map_or_else(
            || format!("{}/helia/external.json", helia_update::helia::base_url()),
            |u| u.trim().to_string(),
        )
}

/// Helia's source: pinned keys, manifest address, allowed hosts.
#[must_use]
pub fn source() -> ExternalSource {
    let url = manifest_url();
    let mut s = ExternalSource::new(APP, &url, &helia_update::helia::pinned_keys());
    let manifest_hosts = option_env!("HELIA_EXTERNAL_MANIFEST_HOSTS")
        .map(list)
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| list(helia_update::helia::DEFAULT_MANIFEST_HOSTS));
    let asset_hosts = option_env!("HELIA_EXTERNAL_ASSET_HOSTS")
        .map(list)
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| list(DEFAULT_ASSET_HOSTS));
    // A build that points the manifest at a test server on loopback trusts that host for the
    // manifest, and allows it for downloads as well as the configured hosts, so an end-to-end
    // test can serve a local manifest and still fetch the real files. A normal build never has
    // a loopback manifest address, so this changes nothing for users.
    if url.starts_with("http://127.0.0.1") {
        let mut hosts = asset_hosts;
        hosts.push("127.0.0.1");
        s = s
            .with_manifest_hosts(&["127.0.0.1"])
            .with_asset_hosts(&hosts)
            .with_loopback_http(true);
    } else {
        s = s
            .with_manifest_hosts(&manifest_hosts)
            .with_asset_hosts(&asset_hosts);
    }
    s
}

/// The `ffmpeg` and `ffprobe` installed with Helia by download, if both records and files are
/// there. A package manager install leaves nothing here: that copy is found on the `PATH`.
#[must_use]
pub fn installed_ffmpeg() -> (Option<std::path::PathBuf>, Option<std::path::PathBuf>) {
    let Some(d) = dirs() else {
        return (None, None);
    };
    (
        crate::installed_program(&d, "ffmpeg", "ffmpeg"),
        crate::installed_program(&d, "ffmpeg", "ffprobe"),
    )
}

/// The usual folders for Helia.
#[must_use]
pub fn dirs() -> Option<crate::store::Dirs> {
    crate::store::Dirs::from_env(APP, DIR_ENV)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_source_is_valid() {
        let s = source();
        s.validate().unwrap();
        assert_eq!(s.app, "helia");
        assert!(s.manifest_url.ends_with("/helia/external.json"));
        assert!(
            s.asset_host_allowed("https://github.com/BtbN/FFmpeg-Builds/releases/download/x/y")
        );
        assert!(!s.asset_host_allowed("https://example.com/x"));
    }
}
