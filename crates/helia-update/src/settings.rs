// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The user's update choices: the mode and the channel.
//!
//! One small `key=value` file shared by the command line tool and the GUI, so a
//! choice made in one holds in the other:
//!
//! ```text
//! schema=1
//! mode=notify
//! channel=stable
//! ```
//!
//! Unknown lines are skipped. A file written by a newer Helia (`schema` above
//! [`SCHEMA`]) is never rewritten: it is read as the defaults and
//! [`Loaded::newer_schema`] says so, so a downgrade cannot lose newer settings.

use std::path::Path;

use crate::error::{Error, Result};
use crate::traits::Fs;

/// The settings file format this build writes and reads.
pub const SCHEMA: u32 = 1;

/// What Helia may do about updates on its own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UpdateMode {
    /// Check once a day and say so when there is an update; download only when asked.
    #[default]
    Notify,
    /// Check once a day, download in the background and install on restart or exit.
    Automatic,
    /// Never contact the network on its own. Checks run only when you ask.
    Off,
}

impl UpdateMode {
    /// Every mode, in the order a settings page lists them.
    pub const ALL: [Self; 3] = [Self::Notify, Self::Automatic, Self::Off];

    /// The file spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Notify => "notify",
            Self::Automatic => "automatic",
            Self::Off => "off",
        }
    }

    /// Parse the file spelling.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "notify" => Some(Self::Notify),
            "automatic" => Some(Self::Automatic),
            "off" => Some(Self::Off),
            _ => None,
        }
    }

    /// One line for a settings page.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::Notify => "Tell me when an update is available (checks once a day)",
            Self::Automatic => {
                "Download updates and install them when I restart (checks once a day)"
            }
            Self::Off => "Never check on my own",
        }
    }
}

/// Which stream of releases to follow.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Channel {
    /// Released versions only.
    #[default]
    Stable,
    /// Pre-release versions as well.
    Prerelease,
}

impl Channel {
    /// The file and manifest spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Prerelease => "prerelease",
        }
    }

    /// Parse the spelling.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "stable" => Some(Self::Stable),
            "prerelease" => Some(Self::Prerelease),
            _ => None,
        }
    }
}

/// The choices.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UpdateSettings {
    /// What may happen on its own.
    pub mode: UpdateMode,
    /// Which releases.
    pub channel: Channel,
}

/// The result of reading the file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Loaded {
    /// The choices (the defaults when the file is missing, damaged or newer).
    pub settings: UpdateSettings,
    /// The file's schema when it is newer than this build understands.
    pub newer_schema: Option<u32>,
}

impl UpdateSettings {
    /// Parse file text. Unknown or malformed lines are skipped.
    #[must_use]
    pub fn parse(text: &str) -> Loaded {
        let mut s = Self::default();
        let mut schema = SCHEMA;
        for line in text.lines().map(str::trim) {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            match k.trim() {
                "schema" => schema = v.trim().parse().unwrap_or(SCHEMA),
                "mode" => s.mode = UpdateMode::parse(v).unwrap_or_default(),
                "channel" => s.channel = Channel::parse(v).unwrap_or_default(),
                _ => {}
            }
        }
        if schema > SCHEMA {
            Loaded {
                settings: Self::default(),
                newer_schema: Some(schema),
            }
        } else {
            Loaded {
                settings: s,
                newer_schema: None,
            }
        }
    }

    /// The file text.
    #[must_use]
    pub fn to_text(&self) -> String {
        format!(
            "# Helia update settings. mode: notify, automatic or off. channel: stable or prerelease.\nschema={SCHEMA}\nmode={}\nchannel={}\n",
            self.mode.as_str(),
            self.channel.as_str()
        )
    }

    /// Read `path`. A missing or unreadable file gives the defaults.
    #[must_use]
    pub fn load(fs: &dyn Fs, path: &Path) -> Loaded {
        match fs.len(path) {
            Ok(Some(n)) if n <= 16 * 1024 => fs
                .read(path)
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
                .map(|t| Self::parse(&t))
                .unwrap_or_default(),
            _ => Loaded::default(),
        }
    }

    /// Write `path`, unless the file there is from a newer Helia.
    ///
    /// # Errors
    /// [`Error::Settings`] when the file is from a newer Helia, [`Error::Io`] otherwise.
    pub fn save(&self, fs: &dyn Fs, path: &Path) -> Result<()> {
        if let Some(n) = Self::load(fs, path).newer_schema {
            return Err(Error::Settings(format!(
                "the update settings file was written by a newer Helia (format {n}), so it was left alone"
            )));
        }
        if let Some(dir) = path.parent() {
            fs.create_dir_all(dir)?;
        }
        fs.write_atomic(path, self.to_text().as_bytes())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mem::MemFs;

    #[test]
    fn defaults_are_notify_and_stable() {
        let l = UpdateSettings::parse("");
        assert_eq!(l.settings.mode, UpdateMode::Notify);
        assert_eq!(l.settings.channel, Channel::Stable);
        assert_eq!(l.newer_schema, None);
    }

    #[test]
    fn round_trip_and_bad_values() {
        let s = UpdateSettings {
            mode: UpdateMode::Off,
            channel: Channel::Prerelease,
        };
        assert_eq!(UpdateSettings::parse(&s.to_text()).settings, s);
        let l = UpdateSettings::parse("junk\nmode=banana\nchannel=nightly\n");
        assert_eq!(l.settings, UpdateSettings::default());
    }

    #[test]
    fn newer_schema_is_read_as_defaults_and_never_overwritten() {
        let fs = MemFs::new();
        let p = Path::new("/c/update.conf");
        fs.put(p, b"schema=9\nmode=off\nfuture=1\n");
        let l = UpdateSettings::load(&fs, p);
        assert_eq!(l.newer_schema, Some(9));
        assert_eq!(l.settings, UpdateSettings::default());
        assert!(UpdateSettings::default().save(&fs, p).is_err());
        assert_eq!(fs.get(p).unwrap(), b"schema=9\nmode=off\nfuture=1\n");
    }

    #[test]
    fn save_then_load() {
        let fs = MemFs::new();
        let p = Path::new("/c/update.conf");
        let s = UpdateSettings {
            mode: UpdateMode::Automatic,
            channel: Channel::Stable,
        };
        s.save(&fs, p).unwrap();
        assert_eq!(UpdateSettings::load(&fs, p).settings, s);
    }
}
