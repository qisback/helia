// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Where things live, and the two small per-user records: the highest manifest
//! sequence seen, and the licence acceptances.
//!
//! Acceptances are kept on this computer only and are never sent anywhere. A record
//! says which item and version, which licence (by id and by the sha256 of its exact
//! text), when, and through which front end. [`Acceptances::is_accepted`] compares
//! the hash, so a changed licence text asks again.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use helia_update::Fs;

use crate::error::{Error, Result};
use crate::manifest::Offer;

/// Format number of both files.
pub const FORMAT: u32 = 1;

/// The folders and files of one app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dirs {
    /// The highest-sequence record.
    pub state: PathBuf,
    /// The acceptance records.
    pub accepted: PathBuf,
    /// Installed items: one folder per item id.
    pub tools: PathBuf,
    /// Downloads in progress and finished downloads waiting to be unpacked.
    pub staging: PathBuf,
}

impl Dirs {
    /// Everything under one folder (tests, portable use).
    #[must_use]
    pub fn under(dir: &Path) -> Self {
        Self {
            state: dir.join("external-state.json"),
            accepted: dir.join("external-accepted.json"),
            tools: dir.join("tools"),
            staging: dir.join("tools").join(".staging"),
        }
    }

    /// The usual places for an app called `app` (`helia`): the records with the other
    /// settings (`%APPDATA%\<app>`, `$XDG_CONFIG_HOME/<app>`, `~/.config/<app>`) and the
    /// tools in local data (`%LOCALAPPDATA%\<app>\tools`, `$XDG_DATA_HOME/<app>/tools`,
    /// `~/.local/share/<app>/tools`), so a roaming profile never carries programs.
    /// The environment variable `override_var` (for example `HELIA_EXTERNAL_DIR`) puts
    /// everything under one folder.
    #[must_use]
    pub fn from_env(app: &str, override_var: &str) -> Option<Self> {
        let var = |k: &str| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        if let Some(d) = var(override_var) {
            return Some(Self::under(&d));
        }
        let home = var("HOME");
        let config = var("APPDATA")
            .or_else(|| var("XDG_CONFIG_HOME"))
            .or_else(|| home.as_ref().map(|h| h.join(".config")))?
            .join(app);
        let data = var("LOCALAPPDATA")
            .or_else(|| var("XDG_DATA_HOME"))
            .or_else(|| home.as_ref().map(|h| h.join(".local").join("share")))?
            .join(app);
        Some(Self {
            state: config.join("external-state.json"),
            accepted: config.join("external-accepted.json"),
            tools: data.join("tools"),
            staging: data.join("tools").join(".staging"),
        })
    }

    /// The folder of one installed item.
    #[must_use]
    pub fn item_dir(&self, item_id: &str) -> PathBuf {
        self.tools.join(item_id)
    }
}

/// What the client remembers about the manifest.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// File format.
    pub format: u32,
    /// Highest manifest sequence accepted so far (anti-replay).
    pub highest_sequence: u64,
    /// When the manifest was last fetched, seconds since the Unix epoch.
    pub last_fetch: u64,
}

impl State {
    /// Read the state; a missing file is the empty state.
    ///
    /// # Errors
    /// [`Error::State`] when the file exists and cannot be read.
    pub fn load(fs: &dyn Fs, path: &Path) -> Result<Self> {
        load_json(fs, path)
    }

    /// Write the state.
    ///
    /// # Errors
    /// [`Error::State`] or [`Error::Io`].
    pub fn save(&self, fs: &dyn Fs, path: &Path) -> Result<()> {
        let mut s = self.clone();
        s.format = FORMAT;
        save_json(fs, path, &s)
    }
}

/// One accepted licence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Acceptance {
    /// Item id.
    pub item: String,
    /// Item version at the time.
    pub version: String,
    /// Licence id.
    pub licence: String,
    /// sha256 of the exact licence text that was shown.
    pub licence_sha256: String,
    /// When, seconds since the Unix epoch.
    pub accepted_at: u64,
    /// Which front end took the answer (`cli`, `gui`).
    pub via: String,
}

/// The acceptance records of this user.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Acceptances {
    /// File format.
    pub format: u32,
    /// Records, newest last.
    pub records: Vec<Acceptance>,
}

impl Acceptances {
    /// Read the records; a missing file is no records.
    ///
    /// # Errors
    /// [`Error::State`] when the file exists and cannot be read.
    pub fn load(fs: &dyn Fs, path: &Path) -> Result<Self> {
        load_json(fs, path)
    }

    /// Write the records (readable by the owner only where the system has file modes).
    ///
    /// # Errors
    /// [`Error::State`] or [`Error::Io`].
    pub fn save(&self, fs: &dyn Fs, path: &Path) -> Result<()> {
        let mut s = self.clone();
        s.format = FORMAT;
        let bytes = serde_json::to_vec_pretty(&s).map_err(|e| Error::State(e.to_string()))?;
        if let Some(dir) = path.parent() {
            fs.create_dir_all(dir)?;
        }
        fs.write_private(path, &bytes)?;
        Ok(())
    }

    /// Has this user accepted exactly this licence text for this item? Only the item id and
    /// the licence text hash count, so a new item version under the same text does not ask again,
    /// and a changed text always does.
    #[must_use]
    pub fn is_accepted(&self, offer: &Offer<'_>) -> bool {
        self.records
            .iter()
            .any(|r| r.item == offer.item.id && r.licence_sha256 == offer.licence.sha256)
    }

    /// Record that the user accepted the licence of `offer` at `now`, through `via`.
    /// Call this only after the user has been shown the full text and said yes.
    pub fn accept(&mut self, offer: &Offer<'_>, now: u64, via: &str) {
        self.records
            .retain(|r| !(r.item == offer.item.id && r.licence == offer.licence.id));
        self.records.push(Acceptance {
            item: offer.item.id.clone(),
            version: offer.item.version.clone(),
            licence: offer.licence.id.clone(),
            licence_sha256: offer.licence.sha256.clone(),
            accepted_at: now,
            via: via.chars().take(16).collect(),
        });
    }

    /// Forget the acceptances for an item (removing the tool and starting clean).
    /// Returns how many were dropped.
    pub fn forget(&mut self, item_id: &str) -> usize {
        let before = self.records.len();
        self.records.retain(|r| r.item != item_id);
        before - self.records.len()
    }
}

fn load_json<T: Default + for<'de> Deserialize<'de>>(fs: &dyn Fs, path: &Path) -> Result<T> {
    match fs.len(path)? {
        None => Ok(T::default()),
        Some(n) if n > 1 << 20 => Err(Error::State(format!("{} is too large", path.display()))),
        Some(_) => {
            let bytes = fs.read(path)?;
            serde_json::from_slice(&bytes)
                .map_err(|e| Error::State(format!("{}: {e}", path.display())))
        }
    }
}

fn save_json<T: Serialize>(fs: &dyn Fs, path: &Path, v: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(v).map_err(|e| Error::State(e.to_string()))?;
    if let Some(dir) = path.parent() {
        fs.create_dir_all(dir)?;
    }
    fs.write_atomic(path, &bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{Manifest, tests_support};
    use helia_update::Platform;
    use helia_update::mem::MemFs;

    fn manifest() -> Manifest {
        Manifest::parse(&tests_support::bytes(&tests_support::doc()), false).unwrap()
    }

    #[test]
    fn acceptance_is_recorded_and_asked_again_when_the_text_changes() {
        let fs = MemFs::new();
        let path = Path::new("/cfg/external-accepted.json");
        let m = manifest();
        let offer = m
            .offers("ffmpeg", Some(Platform::LinuxX86_64))
            .unwrap()
            .remove(1);
        let mut a = Acceptances::load(&fs, path).unwrap();
        assert!(!a.is_accepted(&offer));
        a.accept(&offer, 1_800_000_000, "cli");
        a.save(&fs, path).unwrap();

        let a = Acceptances::load(&fs, path).unwrap();
        assert!(a.is_accepted(&offer));
        assert_eq!(a.records[0].via, "cli");
        assert_eq!(a.records[0].version, "9.0.2");

        // The provider changes the licence text: the hash differs, so ask again.
        let mut d = tests_support::doc();
        d.licences[0].text.push_str("\nNew clause.\n");
        let m2 = Manifest::parse(&tests_support::bytes(&d), false).unwrap();
        let offer2 = m2
            .offers("ffmpeg", Some(Platform::LinuxX86_64))
            .unwrap()
            .remove(1);
        assert!(!a.is_accepted(&offer2));

        // A new item version under the same text does not ask again.
        let mut d = tests_support::doc();
        d.items[0].version = "9.1.0".into();
        let m3 = Manifest::parse(&tests_support::bytes(&d), false).unwrap();
        let offer3 = m3
            .offers("ffmpeg", Some(Platform::LinuxX86_64))
            .unwrap()
            .remove(1);
        assert!(a.is_accepted(&offer3));
    }

    #[test]
    fn accepting_again_replaces_the_old_record_and_forget_drops_it() {
        let m = manifest();
        let offer = m
            .offers("ffmpeg", Some(Platform::LinuxX86_64))
            .unwrap()
            .remove(1);
        let mut a = Acceptances::default();
        a.accept(&offer, 1, "gui");
        a.accept(&offer, 2, "gui");
        assert_eq!(a.records.len(), 1);
        assert_eq!(a.records[0].accepted_at, 2);
        assert_eq!(a.forget("ffmpeg"), 1);
        assert!(!a.is_accepted(&offer));
    }

    #[test]
    fn a_damaged_file_is_an_error_and_a_missing_file_is_empty() {
        let fs = MemFs::new();
        let p = Path::new("/x/state.json");
        assert_eq!(State::load(&fs, p).unwrap(), State::default());
        fs.put(p, b"{not json");
        assert!(matches!(State::load(&fs, p), Err(Error::State(_))));
    }

    #[test]
    fn dirs_under_one_folder() {
        let d = Dirs::under(Path::new("/t"));
        assert_eq!(d.item_dir("ffmpeg"), Path::new("/t/tools/ffmpeg"));
    }
}
