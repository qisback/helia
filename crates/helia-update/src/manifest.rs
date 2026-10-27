// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The manifest v1 format and its strict parser.
//!
//! The wire form ([`ManifestDoc`]) is what the release tool writes. [`parse`]
//! turns bytes into a validated [`Manifest`]. It does not check the signature or
//! the policy (see [`crate::check`]): verify first, then parse.
//!
//! Limits: 64 KiB, nesting depth 8, https-only URLs (plain `http` only to
//! `127.0.0.1`, and only when the caller allows it for tests), unknown fields
//! ignored, assets with an unknown platform or kind skipped.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::time;

/// Largest manifest accepted, in bytes.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
/// Deepest JSON nesting accepted.
pub const MAX_DEPTH: usize = 8;
/// The newest manifest schema this crate reads.
pub const SUPPORTED_SCHEMA: u32 = 1;
/// Largest asset accepted, in bytes (4 GiB).
pub const MAX_ASSET_BYTES: u64 = 4 << 30;
/// Most assets accepted in one manifest.
pub const MAX_ASSETS: usize = 64;

/// A platform an asset is built for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Platform {
    /// `windows-x86_64`
    WindowsX86_64,
    /// `linux-x86_64`
    LinuxX86_64,
    /// `linux-aarch64`
    LinuxAarch64,
    /// `macos-aarch64`
    MacosAarch64,
    /// `macos-x86_64`
    MacosX86_64,
}

impl Platform {
    /// The manifest spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WindowsX86_64 => "windows-x86_64",
            Self::LinuxX86_64 => "linux-x86_64",
            Self::LinuxAarch64 => "linux-aarch64",
            Self::MacosAarch64 => "macos-aarch64",
            Self::MacosX86_64 => "macos-x86_64",
        }
    }

    /// Parse the manifest spelling; `None` for an unknown platform.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "windows-x86_64" => Self::WindowsX86_64,
            "linux-x86_64" => Self::LinuxX86_64,
            "linux-aarch64" => Self::LinuxAarch64,
            "macos-aarch64" => Self::MacosAarch64,
            "macos-x86_64" => Self::MacosX86_64,
            _ => return None,
        })
    }

    /// The platform this build runs on, if it is one the manifest names.
    #[must_use]
    pub fn current() -> Option<Self> {
        if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            Some(Self::WindowsX86_64)
        } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            Some(Self::LinuxX86_64)
        } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
            Some(Self::LinuxAarch64)
        } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            Some(Self::MacosAarch64)
        } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
            Some(Self::MacosX86_64)
        } else {
            None
        }
    }
}

/// What an asset is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Windows NSIS installer.
    Nsis,
    /// Linux `AppImage`.
    Appimage,
    /// Debian package.
    Deb,
    /// RPM package.
    Rpm,
    /// Tarball.
    Targz,
    /// Zip archive.
    Zip,
    /// macOS disk image.
    Dmg,
}

impl Kind {
    /// The manifest spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Nsis => "nsis",
            Self::Appimage => "appimage",
            Self::Deb => "deb",
            Self::Rpm => "rpm",
            Self::Targz => "targz",
            Self::Zip => "zip",
            Self::Dmg => "dmg",
        }
    }

    /// Parse the manifest spelling; `None` for an unknown kind.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "nsis" => Self::Nsis,
            "appimage" => Self::Appimage,
            "deb" => Self::Deb,
            "rpm" => Self::Rpm,
            "targz" => Self::Targz,
            "zip" => Self::Zip,
            "dmg" => Self::Dmg,
            _ => return None,
        })
    }
}

/// One downloadable file, validated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    /// Platform it is built for.
    pub platform: Platform,
    /// What it is.
    pub kind: Kind,
    /// Where to fetch it (https).
    pub url: String,
    /// Lower-case hex sha256 of the file.
    pub sha256: String,
    /// Size in bytes.
    pub size: u64,
}

/// A validated manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    /// Schema number.
    pub schema: u32,
    /// App name, for example `helia`.
    pub app: String,
    /// Channel, for example `stable`.
    pub channel: String,
    /// The version this manifest offers.
    pub version: semver::Version,
    /// When the manifest was made, seconds since the Unix epoch.
    pub date: u64,
    /// Monotonic counter: anti-rollback and anti-replay.
    pub sequence: u64,
    /// When the manifest stops being trusted, seconds since the Unix epoch.
    pub expires: u64,
    /// Oldest version still supported, if stated.
    pub min_supported: Option<semver::Version>,
    /// Release notes page (https), if stated.
    pub notes_url: Option<String>,
    /// Assets this client understands.
    pub assets: Vec<Asset>,
    /// How many assets were skipped for an unknown platform or kind.
    pub skipped_assets: usize,
}

/// The wire form of an asset. Strings, so unknown kinds survive parsing.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AssetDoc {
    /// Platform spelling.
    pub platform: String,
    /// Kind spelling.
    pub kind: String,
    /// URL.
    #[serde(default)]
    pub url: String,
    /// Hex sha256.
    #[serde(default)]
    pub sha256: String,
    /// Size in bytes.
    #[serde(default)]
    pub size: u64,
}

/// The wire form of a manifest, as written by the release tool.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ManifestDoc {
    /// Schema number.
    pub schema: u32,
    /// App name.
    pub app: String,
    /// Channel.
    pub channel: String,
    /// Version.
    pub version: String,
    /// `YYYY-MM-DDTHH:MM:SSZ`.
    pub date: String,
    /// Sequence number.
    pub sequence: u64,
    /// `YYYY-MM-DDTHH:MM:SSZ`.
    pub expires: String,
    /// Oldest supported version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_supported: Option<String>,
    /// Release notes URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes_url: Option<String>,
    /// Assets.
    #[serde(default)]
    pub assets: Vec<AssetDoc>,
}

/// Parser options.
#[derive(Clone, Copy, Debug, Default)]
pub struct ParseOptions {
    /// Allow plain `http://127.0.0.1` URLs. For tests only.
    pub allow_loopback_http: bool,
}

/// Is `url` acceptable: https with a host and no credentials, or (when allowed)
/// `http://127.0.0.1`.
///
/// # Errors
/// [`Error::InsecureUrl`] when it is not.
pub fn check_url(url: &str, allow_loopback_http: bool) -> Result<()> {
    let bad = || Error::InsecureUrl(url.chars().take(200).collect());
    if url.len() > 2048 || url.bytes().any(|c| c <= b' ' || c == 0x7f || !c.is_ascii()) {
        return Err(bad());
    }
    let (rest, loopback_only) = if let Some(r) = url.strip_prefix("https://") {
        (r, false)
    } else if allow_loopback_http && let Some(r) = url.strip_prefix("http://") {
        (r, true)
    } else {
        return Err(bad());
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') || authority.starts_with(':') {
        return Err(bad());
    }
    if loopback_only {
        let host = authority.split(':').next().unwrap_or("");
        if host != "127.0.0.1" {
            return Err(bad());
        }
    }
    Ok(())
}

/// The host of an `http(s)` URL, without port, user info or path. Lower-cased.
#[must_use]
pub fn url_host(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?.split(':').next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// Reject input over the size limit or nested deeper than [`MAX_DEPTH`].
///
/// # Errors
/// [`Error::TooLarge`] or [`Error::TooDeep`].
pub fn check_shape(bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(Error::TooLarge {
            size: bytes.len(),
            max: MAX_MANIFEST_BYTES,
        });
    }
    let (mut depth, mut in_str, mut esc) = (0usize, false, false);
    for &c in bytes {
        if in_str {
            match (esc, c) {
                (true, _) => esc = false,
                (false, b'\\') => esc = true,
                (false, b'"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > MAX_DEPTH {
                    return Err(Error::TooDeep);
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

/// Read only the `schema` field.
///
/// # Errors
/// Anything [`check_shape`] rejects, or [`Error::Malformed`].
pub fn peek_schema(bytes: &[u8]) -> Result<u32> {
    #[derive(Deserialize)]
    struct Peek {
        schema: u32,
    }
    check_shape(bytes)?;
    serde_json::from_slice::<Peek>(bytes)
        .map(|p| p.schema)
        .map_err(|e| Error::Malformed(e.to_string()))
}

fn name_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-')
        })
}

fn version(field: &str, s: &str) -> Result<semver::Version> {
    if s.len() > 64 {
        return Err(Error::Malformed(format!("{field} is too long")));
    }
    semver::Version::parse(s).map_err(|e| Error::Malformed(format!("{field} {s:?}: {e}")))
}

/// Parse and validate a manifest.
///
/// # Errors
/// [`Error::TooLarge`], [`Error::TooDeep`], [`Error::SchemaTooNew`],
/// [`Error::Malformed`] or [`Error::InsecureUrl`].
pub fn parse(bytes: &[u8], opts: ParseOptions) -> Result<Manifest> {
    check_shape(bytes)?;
    let schema = peek_schema(bytes)?;
    if schema > SUPPORTED_SCHEMA {
        return Err(Error::SchemaTooNew(schema));
    }
    if schema == 0 {
        return Err(Error::Malformed("schema 0".into()));
    }
    let doc: ManifestDoc =
        serde_json::from_slice(bytes).map_err(|e| Error::Malformed(e.to_string()))?;
    from_doc(&doc, opts)
}

/// Validate a wire-form manifest.
///
/// # Errors
/// [`Error::Malformed`] or [`Error::InsecureUrl`].
pub fn from_doc(doc: &ManifestDoc, opts: ParseOptions) -> Result<Manifest> {
    let bad = |m: &str| Error::Malformed(m.to_string());
    if !name_ok(&doc.app) || !name_ok(&doc.channel) {
        return Err(bad(
            "app and channel must be 1 to 64 characters of a-z 0-9 . _ -",
        ));
    }
    let date = time::parse_utc(&doc.date).ok_or_else(|| bad("date is not YYYY-MM-DDTHH:MM:SSZ"))?;
    let expires =
        time::parse_utc(&doc.expires).ok_or_else(|| bad("expires is not YYYY-MM-DDTHH:MM:SSZ"))?;
    if expires <= date {
        return Err(bad("expires is not after date"));
    }
    let min_supported = doc
        .min_supported
        .as_deref()
        .map(|s| version("min_supported", s))
        .transpose()?;
    if let Some(n) = &doc.notes_url {
        check_url(n, opts.allow_loopback_http)?;
    }
    if doc.assets.len() > MAX_ASSETS {
        return Err(bad("too many assets"));
    }
    let mut assets = Vec::new();
    let mut skipped = 0;
    for a in &doc.assets {
        let (Some(platform), Some(kind)) = (Platform::parse(&a.platform), Kind::parse(&a.kind))
        else {
            skipped += 1;
            continue;
        };
        check_url(&a.url, opts.allow_loopback_http)?;
        let hex_ok = a.sha256.len() == 64
            && a.sha256
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c));
        if !hex_ok {
            return Err(bad("sha256 must be 64 lower-case hex digits"));
        }
        if a.size == 0 || a.size > MAX_ASSET_BYTES {
            return Err(bad("asset size is zero or over the limit"));
        }
        assets.push(Asset {
            platform,
            kind,
            url: a.url.clone(),
            sha256: a.sha256.clone(),
            size: a.size,
        });
    }
    Ok(Manifest {
        schema: doc.schema,
        app: doc.app.clone(),
        channel: doc.channel.clone(),
        version: version("version", &doc.version)?,
        date,
        sequence: doc.sequence,
        expires,
        min_supported,
        notes_url: doc.notes_url.clone(),
        assets,
        skipped_assets: skipped,
    })
}

impl Manifest {
    /// The first asset for `platform` of the first of `kinds` that has one, in the
    /// order of `kinds` (the caller's preference for this install type).
    #[must_use]
    pub fn select_asset(&self, platform: Platform, kinds: &[Kind]) -> Option<&Asset> {
        kinds.iter().find_map(|k| {
            self.assets
                .iter()
                .find(|a| a.platform == platform && a.kind == *k)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) const GOOD: &str = r#"{
  "schema": 1, "app": "helia", "channel": "stable", "version": "0.8.0",
  "date": "2026-10-24T18:00:00Z", "sequence": 42, "expires": "2026-12-01T00:00:00Z",
  "min_supported": "0.7.0", "notes_url": "https://example.org/notes",
  "assets": [
    {"platform": "windows-x86_64", "kind": "nsis", "url": "https://example.org/a.exe", "sha256": "0000000000000000000000000000000000000000000000000000000000000000", "size": 10},
    {"platform": "linux-x86_64", "kind": "appimage", "url": "https://example.org/a.AppImage", "sha256": "1111111111111111111111111111111111111111111111111111111111111111", "size": 20, "future": true},
    {"platform": "haiku-x86_64", "kind": "hpkg"},
    {"platform": "linux-x86_64", "kind": "snap"}
  ],
  "extra_top_level": {"anything": [1, 2]}
}"#;

    #[test]
    fn parses_and_ignores_unknowns() {
        let m = parse(GOOD.as_bytes(), ParseOptions::default()).unwrap();
        assert_eq!(m.version.to_string(), "0.8.0");
        assert_eq!(m.assets.len(), 2);
        assert_eq!(m.skipped_assets, 2);
        assert_eq!(m.sequence, 42);
        let a = m
            .select_asset(Platform::LinuxX86_64, &[Kind::Deb, Kind::Appimage])
            .unwrap();
        assert_eq!(a.kind, Kind::Appimage);
        assert!(
            m.select_asset(Platform::MacosAarch64, &[Kind::Dmg])
                .is_none()
        );
    }

    #[test]
    fn newer_schema_is_reported() {
        let s = GOOD.replace("\"schema\": 1", "\"schema\": 2");
        assert!(matches!(
            parse(s.as_bytes(), ParseOptions::default()),
            Err(Error::SchemaTooNew(2))
        ));
    }

    #[test]
    fn size_and_depth_limits() {
        let big = vec![b' '; MAX_MANIFEST_BYTES + 1];
        assert!(matches!(
            parse(&big, ParseOptions::default()),
            Err(Error::TooLarge { .. })
        ));
        let deep = format!("{}{}", "[".repeat(100), "]".repeat(100));
        assert!(matches!(check_shape(deep.as_bytes()), Err(Error::TooDeep)));
        // Brackets inside strings do not count.
        assert!(check_shape(br#"{"a": "[[[[[[[[[[[[[[[[[[[[[[["}"#).is_ok());
        assert!(check_shape(br#"{"a": "\"[[[[[[[[[[[[[[[[[[["}"#).is_ok());
    }

    #[test]
    fn urls_must_be_https() {
        assert!(check_url("https://example.org/x", false).is_ok());
        for u in [
            "http://example.org/x",
            "ftp://example.org/x",
            "https://",
            "https://user:pw@example.org/",
            "https://exa mple.org/",
            "http://127.0.0.1:80/x",
            "http://localhost/x",
            "file:///etc/passwd",
        ] {
            assert!(check_url(u, false).is_err(), "{u}");
        }
        assert!(check_url("http://127.0.0.1:8080/x", true).is_ok());
        assert!(check_url("http://example.org/x", true).is_err());
        assert!(check_url("http://127.0.0.1.evil.org/x", true).is_err());
    }

    #[test]
    fn rejects_bad_fields() {
        let cases = [
            ("\"sequence\": 42", "\"sequence\": -1"),
            ("\"version\": \"0.8.0\"", "\"version\": \"0.8\""),
            ("\"2026-12-01T00:00:00Z\"", "\"2026-10-01T00:00:00Z\""),
            ("https://example.org/a.exe", "http://example.org/a.exe"),
            ("\"size\": 10", "\"size\": 0"),
            ("\"app\": \"helia\"", "\"app\": \"../x\""),
        ];
        for (from, to) in cases {
            let s = GOOD.replace(from, to);
            assert!(
                parse(s.as_bytes(), ParseOptions::default()).is_err(),
                "{to}"
            );
        }
        let upper = GOOD.replace(&"0".repeat(64), &"A".repeat(64));
        assert!(parse(upper.as_bytes(), ParseOptions::default()).is_err());
        assert!(parse(b"[]", ParseOptions::default()).is_err());
        assert!(parse(b"{\"schema\":1,\"schema\":1}", ParseOptions::default()).is_err());
    }
}
