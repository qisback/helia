// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The plugin manifest, `plugin.toml`: parsing, validation and the checksums that approval rests on.
//!
//! A manifest is untrusted input (it arrives with a downloaded plugin), so everything is bounded
//! and validated before use, and loading it never runs the plugin.

use std::io::Read;
use std::path::{Path, PathBuf};

use helia_plugin_sdk::hlpx::check_relative_path;
use helia_plugin_sdk::wire;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::schema::Schema;

/// Most bytes in a manifest or a schema file.
pub const MAX_FILE: u64 = 256 * 1024;
/// The protocol major version this host supports.
pub const API_VERSION: u32 = helia_plugin_sdk::API_VERSION;
/// Capabilities a plugin may declare.
pub const CAPABILITIES: &[&str] = &["stack-backend", "frame-op", "image-op", "analyse"];

const KNOWN_KEYS: &[&str] = &[
    "name",
    "version",
    "api_version",
    "title",
    "description",
    "author",
    "licence",
    "homepage",
    "capabilities",
    "executable",
    "interpreter",
    "args",
    "files",
    "sha256",
    "signature",
    "settings_schema",
    "resources",
    "limits",
    "steps",
    "update",
];

/// Why a manifest could not be loaded.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ManifestError(pub String);

/// An update source must be an HTTPS address without credentials, plus a 32-byte hex key.
fn check_update(u: &Update) -> Result<(), ManifestError> {
    let url = u.manifest_url.as_str();
    let host_ok = url.strip_prefix("https://").is_some_and(|rest| {
        let host = rest.split(['/', '?', '#']).next().unwrap_or("");
        !host.is_empty() && !host.contains('@') && host.bytes().all(|c| c.is_ascii_graphic())
    });
    if !host_ok || url.len() > 400 || url.bytes().any(|c| c.is_ascii_control() || c == b' ') {
        return bad("update.manifest_url must be an https:// address without a user name");
    }
    if !(u.signing_key.len() == 64 && u.signing_key.bytes().all(|c| c.is_ascii_hexdigit())) {
        return bad("update.signing_key must be 64 hexadecimal digits (an Ed25519 public key)");
    }
    if u.channel.len() > 32
        || !u
            .channel
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
    {
        return bad("update.channel must be a short word");
    }
    Ok(())
}

fn bad<T>(s: impl Into<String>) -> Result<T, ManifestError> {
    Err(ManifestError(s.into()))
}

/// Resource hints.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Resources {
    /// The plugin wants a GPU.
    #[serde(default)]
    pub gpu: bool,
    /// Typical peak memory, MB.
    #[serde(default)]
    pub memory_mb: u64,
}

/// Time limits.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Limits {
    /// Seconds without any message before a call fails.
    #[serde(default = "default_timeout")]
    pub timeout_s: u64,
    /// Seconds a call may take in all.
    #[serde(default = "default_total")]
    pub max_total_s: u64,
}

fn default_timeout() -> u64 {
    120
}
fn default_total() -> u64 {
    86_400
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            timeout_s: default_timeout(),
            max_total_s: default_total(),
        }
    }
}

/// Where the automatic updater finds newer versions of the plugin (validated here, used elsewhere).
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Update {
    /// HTTPS address of the signed update manifest.
    #[serde(default)]
    pub manifest_url: String,
    /// The Ed25519 public key (64 hex digits) the update manifest must be signed with.
    #[serde(default)]
    pub signing_key: String,
    /// Optional release channel, for example `stable`.
    #[serde(default)]
    pub channel: String,
}

/// One processing step a plugin offers.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Step {
    /// Id, unique in the plugin.
    pub id: String,
    /// `frame-op`, `image-op` or `analyse`.
    pub capability: String,
    /// Shown in lists.
    #[serde(default)]
    pub title: String,
    /// One or two sentences.
    #[serde(default)]
    pub description: String,
    /// Relative path of a JSON Schema for the parameters.
    #[serde(default)]
    pub params_schema: Option<String>,
}

/// The manifest as written.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Manifest {
    /// Name.
    pub name: String,
    /// Version of the plugin.
    #[serde(default)]
    pub version: String,
    /// Protocol major version.
    pub api_version: u32,
    /// Shown in lists.
    #[serde(default)]
    pub title: String,
    /// One or two sentences.
    #[serde(default)]
    pub description: String,
    /// Author.
    #[serde(default)]
    pub author: String,
    /// SPDX licence expression.
    #[serde(default)]
    pub licence: String,
    /// Web page.
    #[serde(default)]
    pub homepage: String,
    /// Capabilities.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Relative path of the program (or the script, with an interpreter).
    pub executable: String,
    /// A program looked up on the `PATH` that runs `executable` (for example `python3`).
    #[serde(default)]
    pub interpreter: Option<String>,
    /// Fixed arguments, after the executable.
    #[serde(default)]
    pub args: Vec<String>,
    /// Further files the program needs; they are covered by the approval checksum.
    #[serde(default)]
    pub files: Vec<String>,
    /// SHA-256 of the executable, optional; if given it must match.
    #[serde(default)]
    pub sha256: Option<String>,
    /// Reserved.
    #[serde(default)]
    pub signature: String,
    /// Relative path of the settings schema.
    #[serde(default)]
    pub settings_schema: Option<String>,
    /// Resource hints.
    #[serde(default)]
    pub resources: Resources,
    /// Time limits.
    #[serde(default)]
    pub limits: Limits,
    /// Steps.
    #[serde(default)]
    pub steps: Vec<Step>,
    /// Update source, if the author publishes one.
    #[serde(default)]
    pub update: Option<Update>,
}

/// A step with its parameter schema loaded.
#[derive(Clone, Debug)]
pub struct LoadedStep {
    /// The step as written.
    pub step: Step,
    /// Its parameter form.
    pub params: Schema,
}

/// The two checksums approval records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Digests {
    /// SHA-256 of the executable, lower-case hex.
    pub exe: String,
    /// SHA-256 over the manifest, schema files and extra files.
    pub manifest: String,
}

/// A manifest read from disk and checked.
#[derive(Clone, Debug)]
pub struct Loaded {
    /// The manifest.
    pub manifest: Manifest,
    /// Folder holding it.
    pub dir: PathBuf,
    /// Path of `plugin.toml`.
    pub path: PathBuf,
    /// Settings form.
    pub settings: Schema,
    /// Steps with their forms.
    pub steps: Vec<LoadedStep>,
    /// Things worth telling the user (unknown keys, ignored capabilities).
    pub notes: Vec<String>,
    exe: PathBuf,
    parts: Vec<(String, Vec<u8>)>,
}

fn valid_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
}

fn read_limited(path: &Path, what: &str) -> Result<Vec<u8>, ManifestError> {
    let meta = std::fs::symlink_metadata(path)
        .map_err(|e| ManifestError(format!("cannot read {what}: {e}")))?;
    if !meta.is_file() {
        return bad(format!("{what} is not a regular file"));
    }
    if meta.len() > MAX_FILE {
        return bad(format!("{what} is larger than {} KiB", MAX_FILE / 1024));
    }
    let mut buf = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(MAX_FILE + 1).read_to_end(&mut buf))
        .map_err(|e| ManifestError(format!("cannot read {what}: {e}")))?;
    if buf.len() as u64 > MAX_FILE {
        return bad(format!("{what} is larger than {} KiB", MAX_FILE / 1024));
    }
    Ok(buf)
}

/// A path named in the manifest: relative, plain, and inside `dir` once links are resolved.
fn inside(dir: &Path, rel: &str, what: &str) -> Result<PathBuf, ManifestError> {
    check_relative_path(rel).map_err(|e| ManifestError(format!("{what}: {e}")))?;
    let root = std::fs::canonicalize(dir).map_err(|e| ManifestError(format!("{what}: {e}")))?;
    let full = std::fs::canonicalize(root.join(rel))
        .map_err(|e| ManifestError(format!("{what} {rel}: {e}")))?;
    if !full.starts_with(&root) {
        return bad(format!("{what} {rel} is outside the plugin folder"));
    }
    Ok(full)
}

fn schema_file(dir: &Path, rel: &str, what: &str) -> Result<(Schema, Vec<u8>), ManifestError> {
    let p = inside(dir, rel, what)?;
    let bytes = read_limited(&p, what)?;
    if !wire::depth_ok(&bytes, wire::MAX_DEPTH) {
        return bad(format!("{what} nests too deeply"));
    }
    let v: Value = serde_json::from_slice(&bytes)
        .map_err(|e| ManifestError(format!("{what} is not JSON: {e}")))?;
    let s = Schema::from_json(&v).map_err(|e| ManifestError(format!("{what}: {e}")))?;
    Ok((s, bytes))
}

impl Loaded {
    /// Load a manifest from a folder or from the `plugin.toml` itself.
    ///
    /// # Errors
    /// [`ManifestError`] with a sentence saying what is wrong.
    pub fn load(dir_or_file: &Path) -> Result<Self, ManifestError> {
        let path = if dir_or_file.is_dir() {
            dir_or_file.join("plugin.toml")
        } else {
            dir_or_file.to_owned()
        };
        let dir = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let bytes = read_limited(&path, "plugin.toml")?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| ManifestError("plugin.toml is not UTF-8".into()))?;
        Self::from_text(text, &path, &dir, bytes.clone())
    }

    #[allow(clippy::too_many_lines)]
    fn from_text(
        text: &str,
        path: &Path,
        dir: &Path,
        bytes: Vec<u8>,
    ) -> Result<Self, ManifestError> {
        let table: toml::Table = text
            .parse()
            .map_err(|e| ManifestError(format!("plugin.toml: {e}")))?;
        let mut notes: Vec<String> = table
            .keys()
            .filter(|k| !KNOWN_KEYS.contains(&k.as_str()))
            .map(|k| format!("Ignored unknown key {}.", helia_plugin_sdk::clip(k, 40)))
            .collect();
        let m: Manifest = table
            .try_into()
            .map_err(|e| ManifestError(format!("plugin.toml: {e}")))?;
        if !valid_name(&m.name) {
            return bad("name must be 1 to 32 characters: lower-case letters, digits, - and _");
        }
        if m.api_version != API_VERSION {
            return bad(format!(
                "it speaks plugin API version {}; this Helia supports version {API_VERSION}",
                m.api_version
            ));
        }
        for (k, v, max) in [
            ("version", &m.version, 64),
            ("title", &m.title, 200),
            ("description", &m.description, 4096),
            ("author", &m.author, 200),
            ("licence", &m.licence, 200),
            ("homepage", &m.homepage, 400),
        ] {
            if v.len() > max {
                return bad(format!("{k} is longer than {max} characters"));
            }
        }
        let mut caps = Vec::new();
        for c in &m.capabilities {
            if CAPABILITIES.contains(&c.as_str()) {
                caps.push(c.clone());
            } else {
                notes.push(format!(
                    "Ignored unknown capability {}.",
                    helia_plugin_sdk::clip(c, 40)
                ));
            }
        }
        if caps.is_empty() {
            return bad("it declares no capability this Helia knows");
        }
        if let Some(i) = &m.interpreter
            && (i.is_empty()
                || i.len() > 32
                || !i
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c)))
        {
            return bad("interpreter must be a bare program name such as python3");
        }
        if m.args.len() > 32 || m.args.iter().any(|a| a.len() > 512 || a.contains('\0')) {
            return bad("args has more than 32 entries or an entry is too long");
        }
        if m.files.len() > 16 {
            return bad("files has more than 16 entries");
        }
        if let Some(h) = &m.sha256
            && !(h.len() == 64 && h.bytes().all(|c| c.is_ascii_hexdigit()))
        {
            return bad("sha256 must be 64 hexadecimal digits");
        }
        if m.limits.timeout_s == 0
            || m.limits.max_total_s == 0
            || m.limits.timeout_s > 86_400
            || m.limits.max_total_s > 7 * 86_400
        {
            return bad("limits are out of range");
        }
        if let Some(u) = &m.update {
            check_update(u)?;
        }
        if m.steps.len() > 64 {
            return bad("more than 64 steps");
        }
        let exe = inside(dir, &m.executable, "executable")?;
        if !std::fs::metadata(&exe).is_ok_and(|x| x.is_file()) {
            return bad("executable is not a regular file");
        }
        let mut parts = vec![("plugin.toml".to_owned(), bytes)];
        let settings = if let Some(rel) = &m.settings_schema {
            let (s, b) = schema_file(dir, rel, "settings_schema")?;
            parts.push((rel.clone(), b));
            s
        } else {
            Schema::default()
        };
        let mut steps = Vec::new();
        for st in &m.steps {
            if !valid_name(&st.id) {
                return bad(format!(
                    "step id {:?} is not valid",
                    helia_plugin_sdk::clip(&st.id, 40)
                ));
            }
            if steps.iter().any(|s: &LoadedStep| s.step.id == st.id) {
                return bad(format!("step id {} appears twice", st.id));
            }
            if !matches!(st.capability.as_str(), "frame-op" | "image-op" | "analyse") {
                return bad(format!(
                    "step {} has capability {:?}; steps are frame-op, image-op or analyse",
                    st.id,
                    helia_plugin_sdk::clip(&st.capability, 40)
                ));
            }
            if !caps.contains(&st.capability) {
                return bad(format!(
                    "step {} is {} but the plugin does not declare that capability",
                    st.id, st.capability
                ));
            }
            let params = if let Some(rel) = &st.params_schema {
                let (s, b) = schema_file(dir, rel, "params_schema")?;
                parts.push((rel.clone(), b));
                s
            } else {
                Schema::default()
            };
            steps.push(LoadedStep {
                step: st.clone(),
                params,
            });
        }
        for cap in ["frame-op", "image-op", "analyse"] {
            if caps.contains(&cap.to_owned()) && !steps.iter().any(|s| s.step.capability == cap) {
                return bad(format!(
                    "it declares {cap} but lists no step with that capability"
                ));
            }
        }
        for f in &m.files {
            let p = inside(dir, f, "files")?;
            parts.push((f.clone(), read_limited(&p, "a listed file")?));
        }
        let mut manifest = m;
        manifest.capabilities = caps;
        Ok(Self {
            manifest,
            dir: dir.to_owned(),
            path: path.to_owned(),
            settings,
            steps,
            notes,
            exe,
            parts,
        })
    }

    /// The executable's real path.
    #[must_use]
    pub fn exe_path(&self) -> &Path {
        &self.exe
    }

    /// Hash the executable and the manifest parts now (the manifest parts were read at load).
    ///
    /// # Errors
    /// [`ManifestError`] if the executable cannot be read.
    pub fn digests(&self) -> Result<Digests, ManifestError> {
        Ok(Digests {
            exe: sha256_file(&self.exe)?,
            manifest: {
                let mut h = Sha256::new();
                for (name, bytes) in &self.parts {
                    h.update((name.len() as u64).to_le_bytes());
                    h.update(name.as_bytes());
                    h.update((bytes.len() as u64).to_le_bytes());
                    h.update(bytes);
                }
                hex(&h.finalize())
            },
        })
    }

    /// The step with this id.
    #[must_use]
    pub fn step(&self, id: &str) -> Option<&LoadedStep> {
        self.steps.iter().find(|s| s.step.id == id)
    }

    /// True if the plugin declares the capability.
    #[must_use]
    pub fn has(&self, cap: &str) -> bool {
        self.manifest.capabilities.iter().any(|c| c == cap)
    }
}

/// Lower-case hex of `bytes`.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// SHA-256 of a file, streamed.
///
/// # Errors
/// [`ManifestError`] if it cannot be read.
pub fn sha256_file(p: &Path) -> Result<String, ManifestError> {
    let mut f = std::fs::File::open(p)
        .map_err(|e| ManifestError(format!("cannot read {}: {e}", p.display())))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| ManifestError(format!("cannot read {}: {e}", p.display())))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

/// Compare two hex strings in constant time (for the lengths we use).
#[must_use]
pub fn same_hash(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| {
        acc | (x.to_ascii_lowercase() ^ y.to_ascii_lowercase())
    }) == 0
}

/// Parse a manifest from memory (for the fuzz target and tests): the folder must hold the files
/// it names.
///
/// # Errors
/// [`ManifestError`].
pub fn parse_in(dir: &Path, text: &str) -> Result<Loaded, ManifestError> {
    Loaded::from_text(
        text,
        &dir.join("plugin.toml"),
        dir,
        text.as_bytes().to_vec(),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn folder(manifest: &str) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("prog"), b"#!/bin/sh\n").unwrap();
        std::fs::write(d.path().join("plugin.toml"), manifest).unwrap();
        d
    }

    const OK: &str = "name = \"inv\"\nversion = \"1\"\napi_version = 1\ncapabilities = [\"image-op\"]\nexecutable = \"prog\"\n[[steps]]\nid = \"inv\"\ncapability = \"image-op\"\n";

    #[test]
    fn loads_and_hashes() {
        let d = folder(OK);
        let l = Loaded::load(d.path()).unwrap();
        assert_eq!(l.manifest.name, "inv");
        let a = l.digests().unwrap();
        assert_eq!(a.exe.len(), 64);
        std::fs::write(d.path().join("prog"), b"changed").unwrap();
        assert_ne!(l.digests().unwrap().exe, a.exe);
    }

    #[test]
    fn refuses_bad_manifests() {
        for (name, text) in [
            ("name", OK.replace("name = \"inv\"", "name = \"Bad Name\"")),
            ("api", OK.replace("api_version = 1", "api_version = 9")),
            ("traversal", OK.replace("\"prog\"", "\"../prog\"")),
            ("absolute", OK.replace("\"prog\"", "\"/bin/sh\"")),
            (
                "nocap",
                OK.replace("[\"image-op\"]", "[\"launch-missiles\"]"),
            ),
            ("nostep", OK.split("[[steps]]").next().unwrap().to_owned()),
            (
                "stepcap",
                OK.replace("capability = \"image-op\"\n", "capability = \"analyse\"\n"),
            ),
            (
                "hash",
                format!("{OK}\n").replace("version = \"1\"", "version = \"1\"\nsha256 = \"zz\""),
            ),
            ("missing", OK.replace("\"prog\"", "\"nothere\"")),
            ("interp", format!("interpreter = \"../x\"\n{OK}")),
            ("notoml", "not = [toml".to_owned()),
        ] {
            let d = folder(&text);
            assert!(Loaded::load(d.path()).is_err(), "{name} should be refused");
        }
    }

    #[test]
    fn huge_and_binary_manifests() {
        let d = folder(OK);
        std::fs::write(d.path().join("plugin.toml"), vec![b'a'; 300 * 1024]).unwrap();
        assert!(Loaded::load(d.path()).is_err());
        std::fs::write(d.path().join("plugin.toml"), [0xff, 0xfe, 0x00]).unwrap();
        assert!(Loaded::load(d.path()).is_err());
    }

    #[test]
    fn unknown_keys_are_noted() {
        let d = folder(&format!("future = 1\n{OK}"));
        let l = Loaded::load(d.path()).unwrap();
        assert_eq!(l.notes.len(), 1);
    }

    #[test]
    fn update_section_is_validated() {
        let key = "ab".repeat(32);
        let good = format!(
            "{OK}
[update]
manifest_url = \"https://example.org/p/update.json\"
signing_key = \"{key}\"
channel = \"stable\"
"
        );
        let l = Loaded::load(folder(&good).path()).unwrap();
        let u = l.manifest.update.unwrap();
        assert_eq!(u.channel, "stable");
        for (url, k) in [
            ("http://example.org/u.json", key.as_str()),
            ("https://user@example.org/u.json", key.as_str()),
            ("https:///u.json", key.as_str()),
            ("file:///etc/passwd", key.as_str()),
            ("https://example.org/u.json", "abcd"),
        ] {
            let t = format!(
                "{OK}
[update]
manifest_url = \"{url}\"
signing_key = \"{k}\"
"
            );
            assert!(Loaded::load(folder(&t).path()).is_err(), "{url} {k}");
        }
    }

    #[test]
    fn constant_time_compare() {
        assert!(same_hash("ab", "AB"));
        assert!(!same_hash("ab", "ac"));
        assert!(!same_hash("ab", "abc"));
    }
}
