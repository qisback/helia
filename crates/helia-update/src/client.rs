// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The whole client flow in one place, shared by `helia update ...` and the GUI:
//! settings, the daily check, download, apply, start-up health and rollback.
//!
//! Network rules, in one place:
//!
//! * mode `off`: nothing runs on its own ([`Client::check`] with `explicit = false`
//!   returns [`CheckReport::Disabled`] without touching the network);
//! * an automatic check runs at most once a day ([`CheckReport::NotDue`] otherwise);
//! * a check the user asked for (`explicit = true`) always runs, whatever the mode;
//! * only the manifest, its signature and the chosen file are ever fetched.

use std::path::{Path, PathBuf};

use crate::apply::{self, ApplyFs, ApplyOutcome, Ctx, Process, Rollback};
use crate::check::{CheckOptions, Outcome, Update, Updater};
use crate::download::Staged;
use crate::error::{Error, Result};
use crate::install::{self, HostOs, InstallKind, Probe};
use crate::manifest::{Asset, Platform};
use crate::settings::{Channel, Loaded, UpdateMode, UpdateSettings};
use crate::source::UpdateSource;
use crate::state::{CHECK_INTERVAL_SECS, StateStore};
use crate::traits::{Clock, Http};

/// The app name in manifests and state files.
pub const APP: &str = "helia";

/// Where the client keeps its files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    /// The settings file (`update.conf`).
    pub settings: PathBuf,
    /// The state file (`update-state.json`).
    pub state: PathBuf,
    /// Downloaded updates wait here.
    pub staging: PathBuf,
    /// The Windows strategy keeps the programs it replaces here.
    pub backup: PathBuf,
}

impl Paths {
    /// Everything under one folder.
    #[must_use]
    pub fn under(dir: &Path) -> Self {
        Self {
            settings: dir.join("update.conf"),
            state: dir.join("update-state.json"),
            staging: dir.join("updates"),
            backup: dir.join("previous"),
        }
    }

    /// The usual places: settings and state with the other settings
    /// (`%APPDATA%\helia`, `$XDG_CONFIG_HOME/helia`, `~/.config/helia`), downloads and
    /// kept copies in local data (`%LOCALAPPDATA%\helia`, `$XDG_DATA_HOME/helia`,
    /// `~/.local/share/helia`) so a roaming profile never carries them.
    /// `$HELIA_UPDATE_DIR` puts everything under one folder (tests, portable use).
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let var = |k: &str| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        if let Some(d) = var("HELIA_UPDATE_DIR") {
            return Some(Self::under(&d));
        }
        let home = var("HOME");
        let config = var("APPDATA")
            .or_else(|| var("XDG_CONFIG_HOME"))
            .or_else(|| home.as_ref().map(|h| h.join(".config")))?
            .join("helia");
        let data = var("LOCALAPPDATA")
            .or_else(|| var("XDG_DATA_HOME"))
            .or_else(|| home.as_ref().map(|h| h.join(".local").join("share")))?
            .join("helia");
        Some(Self {
            settings: config.join("update.conf"),
            state: config.join("update-state.json"),
            staging: data.join("updates"),
            backup: data.join("previous"),
        })
    }
}

/// An update that can be had.
#[derive(Clone, Debug)]
pub struct Available {
    /// The verified manifest and what it means.
    pub update: Update,
    /// Version offered.
    pub version: String,
    /// Release notes address.
    pub notes_url: Option<String>,
    /// The running version is below the manifest's minimum supported one.
    pub required: bool,
    /// The file for this system and install, if the manifest lists one.
    pub asset: Option<Asset>,
    /// How the running copy was installed.
    pub install: InstallKind,
    /// The file is already downloaded and verified.
    pub staged: Option<Staged>,
}

impl Available {
    /// Can the app install this itself (rather than show a command or a file)?
    #[must_use]
    pub fn self_applies(&self) -> bool {
        self.install.can_self_apply() && self.asset.is_some()
    }
}

/// The answer to a check.
#[derive(Clone, Debug)]
pub enum CheckReport {
    /// Automatic checks are off; nothing was fetched.
    Disabled,
    /// An automatic check, but one ran less than a day ago; nothing was fetched.
    NotDue,
    /// Running the newest version.
    UpToDate {
        /// The version.
        version: String,
    },
    /// The server offers an older version than the one running (ignored).
    OlderOnServer {
        /// The older version on the server.
        version: String,
    },
    /// The update details need a newer Helia to read.
    SchemaTooNew {
        /// The format found.
        schema: u32,
    },
    /// A newer version.
    Available(Box<Available>),
}

/// What start-up found.
#[derive(Clone, Debug, Default)]
pub struct Startup {
    /// A notice left for this start (an update was installed, or a rollback happened).
    pub notice: Option<String>,
    /// A rollback was done.
    pub rollback: Option<Rollback>,
    /// A rollback was due but could not be done, in plain English.
    pub rollback_error: Option<String>,
}

/// Everything `helia update status` shows.
#[derive(Clone, Debug)]
pub struct Status {
    /// The running version.
    pub version: String,
    /// How it is installed.
    pub install: InstallKind,
    /// The settings in force.
    pub settings: UpdateSettings,
    /// The settings file is from a newer Helia and was not used.
    pub newer_settings_schema: Option<u32>,
    /// Seconds since the epoch of the last check, 0 for never.
    pub last_check: u64,
    /// The highest manifest sequence seen.
    pub highest_sequence: u64,
    /// A verified download waiting.
    pub staged: Option<Staged>,
    /// The version an update replaced, when one did and its copy is kept.
    pub previous: Option<String>,
}

/// The pieces the client works with, injected so tests need no network or disk.
#[derive(Clone)]
pub struct Client<'a> {
    /// HTTP.
    pub http: &'a dyn Http,
    /// File access.
    pub fs: &'a dyn ApplyFs,
    /// Time.
    pub clock: &'a dyn Clock,
    /// Install detection.
    pub probe: &'a dyn Probe,
    /// Starting programs.
    pub process: &'a dyn Process,
    /// Where files live.
    pub paths: &'a Paths,
    /// The running version.
    pub version: &'a str,
    /// The system family.
    pub os: HostOs,
    /// The platform assets are chosen for.
    pub platform: Option<Platform>,
    /// The update source for a channel (`stable`, `prerelease`).
    pub source_for: &'a dyn Fn(&str) -> UpdateSource,
    /// Plain http to loopback is allowed (tests only).
    pub loopback: bool,
    /// Start the GUI again after a Windows update.
    pub relaunch_gui: bool,
    /// Use this channel instead of the saved one (for one command).
    pub channel_override: Option<Channel>,
}

impl Client<'_> {
    /// The same client on another channel for its whole life; `None` keeps the saved one.
    #[must_use]
    pub fn with_channel(mut self, channel: Option<Channel>) -> Self {
        self.channel_override = channel;
        self
    }

    fn load_settings(&self) -> Loaded {
        UpdateSettings::load(self.fs, &self.paths.settings)
    }

    /// The settings in force.
    #[must_use]
    pub fn settings(&self) -> UpdateSettings {
        let mut s = self.load_settings().settings;
        if let Some(ch) = self.channel_override {
            s.channel = ch;
        }
        s
    }

    /// Save new settings.
    ///
    /// # Errors
    /// [`Error::Settings`] when the file is from a newer Helia; [`Error::Io`].
    pub fn save_settings(&self, s: &UpdateSettings) -> Result<()> {
        s.save(self.fs, &self.paths.settings)
    }

    /// How the running copy is installed.
    #[must_use]
    pub fn install_kind(&self) -> InstallKind {
        install::detect(self.probe, self.os)
    }

    fn store(&self) -> StateStore<'_> {
        StateStore::new(self.fs, &self.paths.state)
    }

    fn apply_ctx(&self) -> Ctx<'_> {
        Ctx {
            fs: self.fs,
            process: self.process,
            backup_dir: &self.paths.backup,
            relaunch_gui: self.relaunch_gui,
        }
    }

    /// Check for an update. `explicit` is true when the user asked.
    ///
    /// # Errors
    /// Any [`Error`] from fetching or verifying; the check time is still recorded,
    /// so a failing server is not asked again until the next day.
    pub fn check(&self, explicit: bool) -> Result<CheckReport> {
        let settings = self.settings();
        if !explicit && settings.mode == UpdateMode::Off {
            return Ok(CheckReport::Disabled);
        }
        let channel = settings.channel.as_str();
        let source = (self.source_for)(channel);
        let store = self.store();
        let state = store.load_or_default(APP, channel);
        if !explicit && !state.check_due(self.clock.now_unix(), CHECK_INTERVAL_SECS) {
            return Ok(CheckReport::NotDue);
        }
        let updater = Updater {
            http: self.http,
            fs: self.fs,
            clock: self.clock,
        };
        let opts = CheckOptions {
            allow_downgrade: false,
            allow_loopback_http: self.loopback,
        };
        Ok(
            match updater.check_stored(&source, self.version, &store, opts)? {
                Outcome::UpToDate { manifest } => CheckReport::UpToDate {
                    version: manifest.version.to_string(),
                },
                Outcome::OlderOnServer { manifest } => CheckReport::OlderOnServer {
                    version: manifest.version.to_string(),
                },
                Outcome::SchemaTooNew { schema } => CheckReport::SchemaTooNew { schema },
                Outcome::Available(update) => {
                    let version = update.manifest.version.to_string();
                    let state = store.load_or_default(APP, channel);
                    if !explicit && state.skip_version.as_deref() == Some(version.as_str()) {
                        // A version that failed to start here and was rolled back is not pushed again.
                        return Ok(CheckReport::UpToDate {
                            version: self.version.to_string(),
                        });
                    }
                    let install = self.install_kind();
                    let asset = self
                        .platform
                        .and_then(|p| update.asset(p, install.asset_kinds()))
                        .cloned();
                    let staged = state.staged.filter(|s| {
                        s.version == version && self.fs.len(&s.path).ok().flatten() == Some(s.size)
                    });
                    CheckReport::Available(Box::new(Available {
                        notes_url: update.manifest.notes_url.clone(),
                        required: update.required,
                        version,
                        update,
                        asset,
                        install,
                        staged,
                    }))
                }
            },
        )
    }

    /// Download the file for `available`, resuming a partial one, and remember it.
    ///
    /// # Errors
    /// [`Error::NoAsset`] when the manifest has no file for this system; any
    /// download error ([`Error::Truncated`] keeps the part for the next try).
    pub fn download(
        &self,
        available: &Available,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<Staged> {
        if let Some(s) = &available.staged {
            return Ok(s.clone());
        }
        let asset = available.asset.as_ref().ok_or(Error::NoAsset)?;
        let channel = self.settings().channel.as_str();
        let source = (self.source_for)(channel);
        let updater = Updater {
            http: self.http,
            fs: self.fs,
            clock: self.clock,
        };
        let staged = updater.download(
            &source,
            &available.update,
            asset,
            &self.paths.staging,
            self.loopback,
            progress,
        )?;
        let store = self.store();
        let mut state = store.load_or_default(APP, channel);
        state.staged = Some(staged.clone());
        store.save(&state)?;
        Ok(staged)
    }

    /// Apply a downloaded file. For a self-applying install the outcome is
    /// `Installed` or `Scheduled`; otherwise it says what the user does.
    ///
    /// # Errors
    /// [`Error::Apply`] with what failed and what was left untouched.
    pub fn apply(&self, staged: &Staged, notes_url: Option<&str>) -> Result<ApplyOutcome> {
        let channel = self.settings().channel.as_str();
        let store = self.store();
        let mut state = store.load_or_default(APP, channel);
        let kind = self.install_kind();
        let out = apply::apply(
            &self.apply_ctx(),
            &kind,
            staged,
            self.version,
            &mut state.boot,
        )?;
        if !matches!(out, ApplyOutcome::Manual { .. }) {
            state.staged = None;
            state.notice = Some(format!(
                "Helia was updated from {} to {}.{}",
                self.version,
                staged.version,
                notes_url.map_or_else(String::new, |u| format!(" What changed: {u}"))
            ));
        }
        store.save(&state)?;
        Ok(out)
    }

    /// Call early at start-up (GUI): count the start, roll back a version that
    /// keeps failing, drop a download that is no longer needed, return any notice.
    ///
    /// # Errors
    /// [`Error::Io`] when the state cannot be saved.
    pub fn startup(&self) -> Result<Startup> {
        let channel = self.settings().channel.as_str();
        let store = self.store();
        let mut state = store.load_or_default(APP, channel);
        state.boot.begin_start(self.version);
        let mut out = Startup {
            notice: state.notice.take(),
            ..Startup::default()
        };
        if state.boot.rollback_target().is_some() {
            let failed = state.boot.running.clone();
            match apply::rollback(&self.apply_ctx(), &self.install_kind(), &mut state.boot) {
                Ok(r) => {
                    state.skip_version = failed;
                    out.notice = Some(r.message.clone());
                    out.rollback = Some(r);
                }
                Err(e) => out.rollback_error = Some(e.to_string()),
            }
        }
        if let Some(s) = &state.staged {
            let obsolete = semver::Version::parse(&s.version)
                .ok()
                .zip(semver::Version::parse(self.version).ok())
                .is_none_or(|(offered, running)| offered <= running);
            if obsolete {
                let _ = self.fs.remove_file(&s.path);
                state.staged = None;
            }
        }
        store.save(&state)?;
        Ok(out)
    }

    /// Call once the app has run properly.
    ///
    /// # Errors
    /// [`Error::Io`] when the state cannot be saved.
    pub fn mark_healthy(&self) -> Result<()> {
        let channel = self.settings().channel.as_str();
        let store = self.store();
        let mut state = store.load_or_default(APP, channel);
        state.boot.mark_healthy();
        store.save(&state)
    }

    /// Go back to the kept previous version now (`helia update rollback`).
    ///
    /// # Errors
    /// [`Error::Apply`] when nothing is kept or it cannot be put back.
    pub fn rollback_now(&self) -> Result<Rollback> {
        let channel = self.settings().channel.as_str();
        let store = self.store();
        let mut state = store.load_or_default(APP, channel);
        let r = apply::rollback(&self.apply_ctx(), &self.install_kind(), &mut state.boot)?;
        // The version being left is not offered again until a newer one appears.
        state.skip_version = Some(self.version.to_string());
        store.save(&state)?;
        Ok(r)
    }

    /// What is configured and remembered.
    #[must_use]
    pub fn status(&self) -> Status {
        let loaded = self.load_settings();
        let settings = self.settings();
        let state = self.store().load_or_default(APP, settings.channel.as_str());
        Status {
            version: self.version.to_string(),
            install: self.install_kind(),
            settings,
            newer_settings_schema: loaded.newer_schema,
            last_check: state.last_check,
            highest_sequence: state.highest_sequence,
            staged: state.staged,
            previous: state.boot.previous,
        }
    }
}

/// The real system, owned, for a [`Client`] to borrow.
#[cfg(feature = "client")]
pub struct System {
    http: crate::ureq_http::UreqHttp,
    fs: crate::traits::StdFs,
    clock: crate::traits::SystemClock,
    probe: install::SystemProbe,
    process: apply::StdProcess,
    paths: Paths,
    loopback: bool,
    relaunch_gui: bool,
}

#[cfg(feature = "client")]
impl System {
    /// The real system, or `None` when no place for the files can be worked out.
    /// `relaunch_gui` is true in the GUI.
    #[must_use]
    pub fn new(relaunch_gui: bool) -> Option<Self> {
        let loopback = crate::helia::allow_loopback_http();
        Some(Self {
            http: crate::ureq_http::UreqHttp::new(loopback),
            fs: crate::traits::StdFs,
            clock: crate::traits::SystemClock,
            probe: install::SystemProbe,
            process: apply::StdProcess,
            paths: Paths::from_env()?,
            loopback,
            relaunch_gui,
        })
    }

    /// Run `f` with a client for the running version.
    pub fn with_client<T>(&self, version: &str, f: impl FnOnce(&Client<'_>) -> T) -> T {
        let source_for = |c: &str| crate::helia::source(c);
        let client = Client {
            http: &self.http,
            fs: &self.fs,
            clock: &self.clock,
            probe: &self.probe,
            process: &self.process,
            paths: &self.paths,
            version,
            os: HostOs::current(),
            platform: Platform::current(),
            source_for: &source_for,
            loopback: self.loopback,
            relaunch_gui: self.relaunch_gui,
            channel_override: None,
        };
        f(&client)
    }
}
