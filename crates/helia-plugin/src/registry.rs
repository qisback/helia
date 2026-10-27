// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Finding plugins and remembering which ones the user approved.
//!
//! `plugins.toml` records, per plugin, the SHA-256 of the program and of the manifest the user
//! approved. Nothing starts unless it is listed there as enabled and both checksums still match.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::HostError;
use crate::manifest::{Digests, Loaded, ManifestError, same_hash};

/// Most plugin folders looked at in the user's plugins folder.
const MAX_FOUND: usize = 256;
/// Largest `plugins.toml` read.
const MAX_STATE: u64 = 1024 * 1024;

/// Where a plugin was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The user's plugins folder.
    UserFolder,
    /// A `--plugin` argument.
    Argument,
}

/// What may be done with a plugin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Listed, never approved (or turned off).
    NotEnabled,
    /// Approved and unchanged: can run.
    Enabled,
    /// Approved, but the program or manifest differs now.
    Changed(String),
    /// The program does not match its manifest's checksum.
    Damaged,
}

impl Status {
    /// Short word for lists.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::NotEnabled => "off",
            Self::Enabled => "on",
            Self::Changed(_) => "changed",
            Self::Damaged => "damaged",
        }
    }
}

/// One discovered plugin.
#[derive(Clone, Debug)]
pub struct Found {
    /// Where it was found.
    pub source: Source,
    /// The manifest, or why it could not be read.
    pub loaded: Result<Loaded, (PathBuf, ManifestError)>,
}

impl Found {
    /// The plugin's name if the manifest loaded.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.loaded.as_ref().ok().map(|l| l.manifest.name.as_str())
    }
}

/// One line of `plugins.toml`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Approval {
    /// Plugin name.
    pub name: String,
    /// Manifest path when approved.
    #[serde(default)]
    pub manifest: String,
    /// SHA-256 of the executable approved.
    #[serde(default)]
    pub sha256: String,
    /// SHA-256 of the manifest parts approved.
    #[serde(default)]
    pub manifest_sha256: String,
    /// Switched on.
    #[serde(default)]
    pub enabled: bool,
    /// When approved, `YYYY-MM-DDTHH:MM:SSZ`.
    #[serde(default)]
    pub approved: String,
    /// The user's settings (may hold secrets).
    #[serde(default)]
    pub settings: toml::Table,
}

#[derive(Default, Deserialize, Serialize)]
struct StateFile {
    #[serde(default)]
    plugin: Vec<Approval>,
}

/// The approvals file.
#[derive(Clone, Debug, Default)]
pub struct Approvals {
    path: Option<PathBuf>,
    /// The approved plugins.
    pub entries: Vec<Approval>,
}

/// The folder Helia keeps its settings in: `$HELIA_CONFIG_DIR`, else `%APPDATA%\helia`,
/// `$XDG_CONFIG_HOME/helia` or `~/.config/helia`.
#[must_use]
pub fn config_dir() -> Option<PathBuf> {
    let var = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if let Some(d) = var("HELIA_CONFIG_DIR") {
        return Some(d);
    }
    let base = var("APPDATA")
        .or_else(|| var("XDG_CONFIG_HOME"))
        .or_else(|| var("HOME").map(|h| h.join(".config")))?;
    Some(base.join("helia"))
}

/// The user's plugins folder.
#[must_use]
pub fn plugins_dir() -> Option<PathBuf> {
    config_dir().map(|d| d.join("plugins"))
}

/// The approvals file.
#[must_use]
pub fn state_file() -> Option<PathBuf> {
    config_dir().map(|d| d.join("plugins.toml"))
}

/// `YYYY-MM-DDTHH:MM:SSZ` for now (UTC), without a time library.
#[must_use]
pub fn timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = i64::try_from(days).unwrap_or(0) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

impl Approvals {
    /// Read the default file; a missing or damaged one gives an empty list.
    #[must_use]
    pub fn load_default() -> Self {
        state_file().map_or_else(Self::default, |p| Self::load(&p))
    }

    /// Read `path`, remembering it for saving.
    #[must_use]
    pub fn load(path: &Path) -> Self {
        let text = std::fs::metadata(path)
            .ok()
            .filter(|m| m.len() <= MAX_STATE)
            .and_then(|_| std::fs::read_to_string(path).ok())
            .unwrap_or_default();
        let file: StateFile = toml::from_str(&text).unwrap_or_default();
        Self {
            path: Some(path.to_owned()),
            entries: file.plugin,
        }
    }

    /// An in-memory list that saves nowhere (tests).
    #[must_use]
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// The entry for `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Approval> {
        self.entries.iter().find(|e| e.name == name)
    }

    /// Save, atomically, readable by the user only on Unix.
    ///
    /// # Errors
    /// The I/O error text.
    pub fn save(&self) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let text = toml::to_string_pretty(&StateFile {
            plugin: self.entries.clone(),
        })
        .map_err(|e| e.to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let tmp = path.with_extension("toml.part");
        write_private(&tmp, text.as_bytes()).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    }

    /// Record approval of what is on disk now and switch the plugin on. The caller has shown the
    /// user the path, hash and capabilities and got a yes.
    ///
    /// # Errors
    /// [`HostError`] if the plugin is damaged or cannot be hashed.
    pub fn approve(&mut self, l: &Loaded) -> Result<Digests, HostError> {
        let name = &l.manifest.name;
        let d = l.digests().map_err(|e| HostError::invalid(name, e.0))?;
        if let Some(want) = &l.manifest.sha256
            && !same_hash(want, &d.exe)
        {
            return Err(HostError::damaged(name));
        }
        let settings = self
            .get(name)
            .map(|e| e.settings.clone())
            .unwrap_or_default();
        let entry = Approval {
            name: name.clone(),
            manifest: l.path.display().to_string(),
            sha256: d.exe.clone(),
            manifest_sha256: d.manifest.clone(),
            enabled: true,
            approved: timestamp(),
            settings,
        };
        match self.entries.iter_mut().find(|e| e.name == *name) {
            Some(e) => *e = entry,
            None => self.entries.push(entry),
        }
        Ok(d)
    }

    /// Switch a plugin off (the approval and settings are kept; enabling again re-checks the hash).
    pub fn disable(&mut self, name: &str) -> bool {
        self.entries
            .iter_mut()
            .find(|e| e.name == name)
            .map(|e| e.enabled = false)
            .is_some()
    }

    /// Forget a plugin entirely, settings included.
    pub fn remove(&mut self, name: &str) -> bool {
        let n = self.entries.len();
        self.entries.retain(|e| e.name != name);
        self.entries.len() != n
    }

    /// Store the plugin's settings (JSON values; nulls are dropped).
    pub fn set_settings(&mut self, name: &str, settings: &Value) {
        let mut t = toml::Table::new();
        if let Value::Object(m) = settings {
            for (k, v) in m {
                if let Ok(tv) = toml::Value::try_from(v) {
                    t.insert(k.clone(), tv);
                }
            }
        }
        match self.entries.iter_mut().find(|e| e.name == name) {
            Some(e) => e.settings = t,
            None => self.entries.push(Approval {
                name: name.to_owned(),
                settings: t,
                ..Approval::default()
            }),
        }
    }

    /// The plugin's stored settings as JSON.
    #[must_use]
    pub fn settings(&self, name: &str) -> Value {
        self.get(name).map_or_else(
            || Value::Object(serde_json::Map::new()),
            |e| serde_json::to_value(&e.settings).unwrap_or(Value::Null),
        )
    }

    /// May this plugin run now? Re-hashes the program and manifest.
    #[must_use]
    pub fn status(&self, l: &Loaded) -> Status {
        let name = &l.manifest.name;
        let Some(e) = self.get(name).filter(|e| e.enabled && !e.sha256.is_empty()) else {
            return Status::NotEnabled;
        };
        let Ok(d) = l.digests() else {
            return Status::Changed("program".into());
        };
        if let Some(want) = &l.manifest.sha256
            && !same_hash(want, &d.exe)
        {
            return Status::Damaged;
        }
        if !same_hash(&e.sha256, &d.exe) {
            return Status::Changed("program".into());
        }
        if !same_hash(&e.manifest_sha256, &d.manifest) {
            return Status::Changed("manifest".into());
        }
        Status::Enabled
    }
}

/// Write a file readable by its owner only (Unix), replacing it.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    let mut f = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?
    };
    #[cfg(not(unix))]
    let mut f = std::fs::File::create(path)?;
    f.write_all(bytes)?;
    f.flush()
}

/// The plugins found, with the approvals.
#[derive(Clone, Debug, Default)]
pub struct Registry {
    /// What was found: `--plugin` arguments first, then the user's folder.
    pub found: Vec<Found>,
    /// The approvals.
    pub approvals: Approvals,
}

impl Registry {
    /// Discover plugins in `plugins_dir` and at the `extra` paths (folders or manifests).
    #[must_use]
    pub fn discover(plugins_dir: Option<&Path>, extra: &[PathBuf], approvals: Approvals) -> Self {
        let mut found = Vec::new();
        for p in extra {
            found.push(Found {
                source: Source::Argument,
                loaded: Loaded::load(p).map_err(|e| (p.clone(), e)),
            });
        }
        if let Some(dir) = plugins_dir
            && let Ok(rd) = std::fs::read_dir(dir)
        {
            let mut dirs: Vec<PathBuf> = rd
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.join("plugin.toml").is_file())
                .collect();
            dirs.sort();
            dirs.truncate(MAX_FOUND);
            for p in dirs {
                found.push(Found {
                    source: Source::UserFolder,
                    loaded: Loaded::load(&p).map_err(|e| (p.clone(), e)),
                });
            }
        }
        Self { found, approvals }
    }

    /// Discover with the default folders and approvals file.
    #[must_use]
    pub fn open(extra: &[PathBuf]) -> Self {
        Self::discover(plugins_dir().as_deref(), extra, Approvals::load_default())
    }

    /// The first plugin called `name` (arguments win over the user's folder).
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Loaded> {
        self.found
            .iter()
            .filter_map(|f| f.loaded.as_ref().ok())
            .find(|l| l.manifest.name == name)
    }

    /// Names of plugins that loaded, in discovery order, without repeats.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for l in self.found.iter().filter_map(|f| f.loaded.as_ref().ok()) {
            if seen.insert(l.manifest.name.clone()) {
                out.push(l.manifest.name.clone());
            }
        }
        out
    }

    /// Status of `name`, or an error saying it was not found.
    ///
    /// # Errors
    /// [`HostError`] if no plugin has this name.
    pub fn status(&self, name: &str) -> Result<Status, HostError> {
        let l = self.get(name).ok_or_else(|| {
            HostError::invalid(
                name,
                "no plugin with this name was found in the plugins folder or on the command line",
            )
        })?;
        Ok(self.approvals.status(l))
    }

    /// The loaded plugin if it may run now.
    ///
    /// # Errors
    /// [`HostError`] saying it is off, changed or damaged.
    pub fn runnable(&self, name: &str) -> Result<&Loaded, HostError> {
        let l = self
            .get(name)
            .ok_or_else(|| HostError::invalid(name, "no plugin with this name was found"))?;
        match self.approvals.status(l) {
            Status::Enabled => Ok(l),
            Status::NotEnabled => Err(HostError::not_enabled(name)),
            Status::Changed(part) => Err(HostError::changed(name, &part)),
            Status::Damaged => Err(HostError::damaged(name)),
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::assert_is_empty,
    clippy::many_single_char_names
)]
mod tests {
    use super::*;

    fn plugin_dir(root: &Path, name: &str, prog: &[u8]) -> PathBuf {
        let d = root.join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("prog"), prog).unwrap();
        std::fs::write(
            d.join("plugin.toml"),
            format!("name = \"{name}\"\napi_version = 1\ncapabilities = [\"stack-backend\"]\nexecutable = \"prog\"\n"),
        )
        .unwrap();
        d
    }

    #[test]
    fn discovery_lists_but_does_not_enable() {
        let t = tempfile::tempdir().unwrap();
        plugin_dir(t.path(), "alpha", b"1");
        let r = Registry::discover(Some(t.path()), &[], Approvals::in_memory());
        assert_eq!(r.names(), vec!["alpha"]);
        assert_eq!(r.status("alpha").unwrap(), Status::NotEnabled);
        assert!(r.runnable("alpha").is_err());
    }

    #[test]
    fn approve_then_change_disables() {
        let t = tempfile::tempdir().unwrap();
        let d = plugin_dir(t.path(), "alpha", b"1");
        let mut r = Registry::discover(Some(t.path()), &[], Approvals::in_memory());
        let l = r.get("alpha").unwrap().clone();
        r.approvals.approve(&l).unwrap();
        assert!(r.runnable("alpha").is_ok());
        std::fs::write(d.join("prog"), b"2").unwrap();
        let e = r.runnable("alpha").unwrap_err();
        assert_eq!(e.kind, crate::error::Kind::Changed);
        // Approving again re-enables it.
        let l = Loaded::load(&d).unwrap();
        r.approvals.approve(&l).unwrap();
        r.found = vec![Found {
            source: Source::Argument,
            loaded: Ok(l),
        }];
        assert!(r.runnable("alpha").is_ok());
    }

    #[test]
    fn disable_and_persist() {
        let t = tempfile::tempdir().unwrap();
        let d = plugin_dir(t.path(), "alpha", b"1");
        let file = t.path().join("plugins.toml");
        let mut a = Approvals::load(&file);
        a.approve(&Loaded::load(&d).unwrap()).unwrap();
        a.set_settings(
            "alpha",
            &serde_json::json!({"token": "s3cret", "n": 4, "skip": null}),
        );
        a.save().unwrap();
        let b = Approvals::load(&file);
        assert!(b.get("alpha").unwrap().enabled);
        assert_eq!(
            b.settings("alpha"),
            serde_json::json!({"token": "s3cret", "n": 4})
        );
        let mut b = b;
        assert!(b.disable("alpha"));
        assert_eq!(b.status(&Loaded::load(&d).unwrap()), Status::NotEnabled);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn damaged_state_file_is_empty() {
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("plugins.toml");
        std::fs::write(&f, "[[plugin]\nbroken").unwrap();
        assert!(Approvals::load(&f).entries.is_empty());
    }

    #[test]
    fn timestamps_look_right() {
        let s = timestamp();
        assert_eq!(s.len(), 20);
        assert!(s.starts_with("20") && s.ends_with('Z'));
    }
}
