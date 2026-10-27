// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The extension point for a gated update service.
//!
//! Public Helia checks the signed manifest and tells you when an update exists. It
//! does not fetch installers from a paid service by itself. An official build can
//! plug a [`UpdateBackend`] in at start-up ([`install`]) that decides whether an
//! update may be downloaded, supplies the credentials for the download and adds its
//! own settings page and command. Nothing is installed in a source build: every
//! method has a no-op default and [`installed`] returns `None`.
//!
//! The trait is deliberately small and generic. It knows nothing about any service:
//! it passes plain data (a bearer token for one host, a sentence for the user, a
//! link) between the updater and whatever sits behind it.

use std::ffi::OsString;
use std::io::Read;
use std::path::Path;
use std::sync::OnceLock;

use crate::error::{Error, Result};
use crate::source::UpdateSource;
use crate::traits::{Clock, Fs, Http};

/// What a backend may use. Borrowed from the running [`Client`](crate::client::Client).
pub struct BackendCx<'a> {
    /// HTTP.
    pub http: &'a dyn Http,
    /// File access.
    pub fs: &'a dyn Fs,
    /// Time.
    pub clock: &'a dyn Clock,
    /// The folder of the settings and state files. A backend keeps its own files here.
    pub config_dir: &'a Path,
    /// The app name, for example `helia`.
    pub app: &'a str,
    /// The channel in force, `stable` or `prerelease`.
    pub channel: &'a str,
    /// The update source, after [`UpdateBackend::extend_source`].
    pub source: &'a UpdateSource,
    /// Plain http to loopback is allowed (tests only).
    pub loopback: bool,
}

/// Credentials for the download of one asset.
pub struct AssetAccess {
    /// Sent as `Authorization: Bearer ...`, only to `bearer_host`.
    pub bearer: String,
    /// The one host the bearer may go to. A redirect to any other host carries none.
    pub bearer_host: String,
    /// Turns a 401 or 403 answer into the error to show (the answer's body is passed).
    pub refusal: Box<dyn Fn(u16, Box<dyn Read>) -> Error + Send + Sync>,
}

impl std::fmt::Debug for AssetAccess {
    // The bearer is never printed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssetAccess")
            .field("bearer", &"<redacted>")
            .field("bearer_host", &self.bearer_host)
            .finish_non_exhaustive()
    }
}

/// What a backend decides about one available version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gate {
    /// Automatic mode may download it without being asked.
    pub auto_download: bool,
    /// A plain sentence for the user, when something other than a download happens.
    pub note: Option<String>,
    /// Where to read more, buy, or fetch it by hand.
    pub link: Option<String>,
}

/// The result of [`UpdateBackend::refresh`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Refresh {
    /// The backend's answer was read in this check (a remembered one is never fresh).
    pub fresh: bool,
    /// A notice to show once at the next start.
    pub notice: Option<String>,
}

/// A backend's line in `helia update status`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendStatus {
    /// Label for the line, for example `Plan`.
    pub label: String,
    /// The line.
    pub line: String,
    /// The same, for `--json`.
    pub json: serde_json::Value,
}

/// A settings block the GUI draws on the About page, from plain data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Panel {
    /// The block heading.
    pub heading: String,
    /// Lines of text, the first in the normal colour.
    pub lines: Vec<PanelLine>,
    /// A link: label and address.
    pub link: Option<(String, String)>,
    /// A one-line input, kept hidden as it is typed.
    pub input: Option<PanelInput>,
    /// Buttons, in order.
    pub buttons: Vec<PanelButton>,
    /// Small print under the block.
    pub footer: Option<String>,
}

/// One line of a [`Panel`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelLine {
    /// The text.
    pub text: String,
    /// Draw it in the muted colour.
    pub muted: bool,
}

/// The input of a [`Panel`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelInput {
    /// Grey hint text.
    pub hint: String,
}

/// A button of a [`Panel`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelButton {
    /// Passed back to [`UpdateBackend::panel_action`].
    pub id: String,
    /// The label.
    pub label: String,
    /// Hover text.
    pub hover: String,
    /// Enabled only while the input holds text.
    pub needs_input: bool,
}

/// The result of [`UpdateBackend::panel_action`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelDone {
    /// A message for a toast.
    pub message: String,
    /// Check for updates now.
    pub recheck: bool,
}

/// A command the backend adds to the `helia` command line.
#[derive(Clone, Copy)]
pub struct CliHook {
    /// The subcommand name.
    pub name: &'static str,
    /// One line for `helia --help`.
    pub about: &'static str,
    /// Runs the command with the arguments after its name and returns the exit code.
    /// The backend parses its own arguments, including `--help`.
    pub run: fn(&[OsString]) -> i32,
}

/// A gated update service. Every method has a no-op default.
pub trait UpdateBackend: Send + Sync {
    /// Adjust the source, for example add the hosts downloads may come from.
    fn extend_source(&self, source: UpdateSource) -> UpdateSource {
        source
    }

    /// Credentials for the download of an asset, or `None` for a plain download.
    fn asset_access(&self, _cx: &BackendCx<'_>) -> Option<AssetAccess> {
        None
    }

    /// Called after every check: read what the backend needs to know and remember it.
    fn refresh(&self, _cx: &BackendCx<'_>) -> Refresh {
        Refresh::default()
    }

    /// May `version` be downloaded, and what should the user be told? `None` leaves
    /// the decision to the default.
    fn gate(&self, _cx: &BackendCx<'_>, _version: &semver::Version, _fresh: bool) -> Option<Gate> {
        None
    }

    /// The backend's line for `helia update status`.
    fn status(&self, _cx: &BackendCx<'_>) -> Option<BackendStatus> {
        None
    }

    /// The settings block for the About page.
    fn panel(&self, _cx: &BackendCx<'_>) -> Option<Panel> {
        None
    }

    /// A button of the [`Panel`] was pressed; `input` is what was typed.
    ///
    /// # Errors
    /// An [`Error`] whose text is shown to the user.
    fn panel_action(&self, _cx: &BackendCx<'_>, _id: &str, _input: &str) -> Result<PanelDone> {
        Err(Error::Settings("there is nothing to do here".into()))
    }

    /// A command to add to the command line.
    fn cli(&self) -> Option<CliHook> {
        None
    }

    /// A line for the build record in `helia --version`, for example the backend's name
    /// and version.
    fn build_line(&self) -> Option<String> {
        None
    }

    /// A sentence for the Privacy paragraph: what the backend sends and where.
    fn privacy(&self) -> Option<String> {
        None
    }
}

static INSTALLED: OnceLock<Box<dyn UpdateBackend>> = OnceLock::new();

/// Plug a backend in. Call once, first thing in `main`. Returns false when one is
/// already installed (the first stays).
pub fn install(backend: Box<dyn UpdateBackend>) -> bool {
    INSTALLED.set(backend).is_ok()
}

/// The installed backend, if any.
#[must_use]
pub fn installed() -> Option<&'static dyn UpdateBackend> {
    INSTALLED.get().map(AsRef::as_ref)
}
