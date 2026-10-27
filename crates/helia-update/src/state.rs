// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The small persisted state: when we last checked, the highest manifest
//! sequence seen (anti-replay), the staged download and the start-up health flag
//! that drives rollback.
//!
//! The health flag works like this. Early in `main` the app calls
//! [`BootHealth::begin_start`] with its own version; once it has shown a window
//! and run for a few seconds it calls [`BootHealth::mark_healthy`]. A start that
//! never got that far leaves `running` set, so the next start counts a failed
//! start. After [`FAILED_STARTS_BEFORE_ROLLBACK`] failed starts of the same
//! version, [`BootHealth::rollback_target`] names the version to restore, if the
//! applier recorded one with [`BootHealth::record_applied`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::download::Staged;
use crate::error::{Error, Result};
use crate::traits::Fs;

/// Failed starts of a freshly updated version before the app rolls back.
pub const FAILED_STARTS_BEFORE_ROLLBACK: u32 = 2;
/// Default gap between automatic checks: once a day.
pub const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;
/// State file format.
pub const STATE_FORMAT: u32 = 1;

/// The start-up health flag.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BootHealth {
    /// The version that started and has not yet reported healthy.
    pub running: Option<String>,
    /// Consecutive starts of `running` that never reported healthy.
    pub failed_starts: u32,
    /// The version that was installed before the last update (what to roll back to).
    pub previous: Option<String>,
    /// Where the applier kept the previous version (backup path), if it did.
    pub previous_path: Option<PathBuf>,
}

impl BootHealth {
    /// Call early at start-up with this app's own version.
    pub fn begin_start(&mut self, version: &str) {
        if self.running.as_deref() == Some(version) {
            self.failed_starts += 1;
        } else {
            self.running = Some(version.to_string());
            self.failed_starts = 0;
        }
    }

    /// Call once the app has run properly (window shown, a few seconds up).
    pub fn mark_healthy(&mut self) {
        self.running = None;
        self.failed_starts = 0;
    }

    /// The applier calls this when it installs `to` over `from`.
    pub fn record_applied(&mut self, from: &str, backup: Option<&Path>) {
        self.previous = Some(from.to_string());
        self.previous_path = backup.map(Path::to_path_buf);
        self.running = None;
        self.failed_starts = 0;
    }

    /// The version to roll back to, when the running version has failed to start
    /// too often and a previous version is known.
    #[must_use]
    pub fn rollback_target(&self) -> Option<&str> {
        (self.failed_starts >= FAILED_STARTS_BEFORE_ROLLBACK && self.running.is_some())
            .then_some(self.previous.as_deref())
            .flatten()
    }

    /// Call after a successful rollback: forget the failure.
    pub fn rolled_back(&mut self) {
        self.running = None;
        self.failed_starts = 0;
        self.previous = None;
        self.previous_path = None;
    }
}

/// The persisted state for one update source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// Format of this file.
    pub format: u32,
    /// App the state belongs to.
    pub app: String,
    /// Channel the state belongs to.
    pub channel: String,
    /// Seconds since the Unix epoch of the last check (successful or not).
    pub last_check: u64,
    /// Highest manifest sequence seen and accepted.
    pub highest_sequence: u64,
    /// A downloaded, verified file waiting to be applied.
    pub staged: Option<Staged>,
    /// Start-up health.
    pub boot: BootHealth,
    /// A version that failed to start here and was rolled back: not offered again by an
    /// automatic check.
    pub skip_version: Option<String>,
    /// A message for the next start (an update was installed, or a rollback happened).
    pub notice: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            format: STATE_FORMAT,
            app: String::new(),
            channel: String::new(),
            last_check: 0,
            highest_sequence: 0,
            staged: None,
            boot: BootHealth::default(),
            skip_version: None,
            notice: None,
        }
    }
}

impl State {
    /// An empty state for an app and channel.
    #[must_use]
    pub fn new(app: &str, channel: &str) -> Self {
        Self {
            app: app.to_string(),
            channel: channel.to_string(),
            ..Self::default()
        }
    }

    /// Is an automatic check due? True when `interval` seconds have passed, or the
    /// clock went backwards.
    #[must_use]
    pub fn check_due(&self, now: u64, interval: u64) -> bool {
        self.last_check == 0 || self.last_check > now || now - self.last_check >= interval
    }
}

/// Loads and saves a [`State`] at a path through an [`Fs`].
pub struct StateStore<'a> {
    fs: &'a dyn Fs,
    path: PathBuf,
}

impl<'a> StateStore<'a> {
    /// A store at `path`.
    #[must_use]
    pub fn new(fs: &'a dyn Fs, path: &Path) -> Self {
        Self {
            fs,
            path: path.to_path_buf(),
        }
    }

    /// Load the state. A missing file, or one for another app or channel, gives a
    /// fresh state. A corrupt file is an error so the caller can decide
    /// (see [`Self::load_or_default`]).
    ///
    /// # Errors
    /// [`Error::State`] for a file that is not valid state, [`Error::Io`] otherwise.
    pub fn load(&self, app: &str, channel: &str) -> Result<State> {
        match self.fs.len(&self.path)? {
            None => Ok(State::new(app, channel)),
            Some(n) if n > 64 * 1024 => Err(Error::State("file is too large".into())),
            Some(_) => {
                let s: State = serde_json::from_slice(&self.fs.read(&self.path)?)
                    .map_err(|e| Error::State(e.to_string()))?;
                if s.app == app && s.channel == channel {
                    Ok(s)
                } else {
                    Ok(State::new(app, channel))
                }
            }
        }
    }

    /// [`Self::load`], but a corrupt or unreadable file gives a fresh state. The
    /// highest-sequence memory is lost then, which is the documented cost.
    #[must_use]
    pub fn load_or_default(&self, app: &str, channel: &str) -> State {
        self.load(app, channel)
            .unwrap_or_else(|_| State::new(app, channel))
    }

    /// Save the state atomically.
    ///
    /// # Errors
    /// [`Error::Io`].
    pub fn save(&self, state: &State) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            self.fs.create_dir_all(dir)?;
        }
        let json = serde_json::to_vec_pretty(state).map_err(|e| Error::State(e.to_string()))?;
        self.fs.write_atomic(&self.path, &json)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mem::MemFs;

    #[test]
    fn health_flag_rolls_back_after_two_failed_restarts() {
        let mut b = BootHealth::default();
        b.record_applied("0.7.0", None);
        b.begin_start("0.8.0");
        assert_eq!(b.rollback_target(), None);
        b.begin_start("0.8.0");
        assert_eq!(b.rollback_target(), None);
        b.begin_start("0.8.0");
        assert_eq!(b.rollback_target(), Some("0.7.0"));
        b.rolled_back();
        assert_eq!(b.rollback_target(), None);
    }

    #[test]
    fn healthy_start_clears_the_count() {
        let mut b = BootHealth::default();
        b.record_applied("0.7.0", None);
        b.begin_start("0.8.0");
        b.begin_start("0.8.0");
        b.mark_healthy();
        b.begin_start("0.8.0");
        b.begin_start("0.8.0");
        assert_eq!(b.rollback_target(), None);
    }

    #[test]
    fn no_previous_means_no_rollback() {
        let mut b = BootHealth::default();
        for _ in 0..5 {
            b.begin_start("0.8.0");
        }
        assert_eq!(b.rollback_target(), None);
    }

    #[test]
    fn store_round_trip_and_mismatch() {
        let fs = MemFs::new();
        let store = StateStore::new(&fs, Path::new("/cfg/update.json"));
        let mut s = store.load("helia", "stable").unwrap();
        s.highest_sequence = 7;
        s.last_check = 100;
        store.save(&s).unwrap();
        assert_eq!(store.load("helia", "stable").unwrap().highest_sequence, 7);
        assert_eq!(
            store.load("helia", "prerelease").unwrap().highest_sequence,
            0
        );
        fs.put(Path::new("/cfg/update.json"), b"{not json");
        assert!(store.load("helia", "stable").is_err());
        assert_eq!(store.load_or_default("helia", "stable").highest_sequence, 0);
    }

    #[test]
    fn due_logic() {
        let mut s = State::new("helia", "stable");
        assert!(s.check_due(1000, CHECK_INTERVAL_SECS));
        s.last_check = 1000;
        assert!(!s.check_due(1000 + 60, CHECK_INTERVAL_SECS));
        assert!(s.check_due(1000 + CHECK_INTERVAL_SECS, CHECK_INTERVAL_SECS));
        assert!(s.check_due(10, CHECK_INTERVAL_SECS), "clock went backwards");
    }
}
