// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Plugin errors in Helia's standard form: what happened, why, what to do.

use std::fmt;

use helia_plugin_sdk::{ErrorKind, PluginError};

/// Why a plugin call failed, for the code that has to decide what to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The user has not enabled the plugin.
    NotEnabled,
    /// The program or manifest changed since it was approved.
    Changed,
    /// The program does not match the checksum in its manifest.
    Damaged,
    /// The manifest or the program cannot be used.
    Invalid,
    /// The process could not be started.
    Spawn,
    /// The process ended unexpectedly.
    Crashed,
    /// The process stopped answering.
    Timeout,
    /// The process broke the protocol.
    Protocol,
    /// The process returned data that does not fit.
    BadResult,
    /// Helia refused the request before sending it.
    Refused,
    /// The call was cancelled.
    Cancelled,
    /// The plugin reported an error of this kind.
    Plugin(ErrorKind),
}

/// An error with the three parts Helia's error standard asks for.
#[derive(Clone, Debug)]
pub struct HostError {
    /// What kind of failure.
    pub kind: Kind,
    /// The plugin's name.
    pub plugin: String,
    /// What happened.
    pub what: String,
    /// Why.
    pub why: String,
    /// What to do about it.
    pub todo: String,
}

impl HostError {
    /// Build an error.
    pub fn new(
        kind: Kind,
        plugin: &str,
        what: impl Into<String>,
        why: impl Into<String>,
        todo: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            plugin: plugin.to_owned(),
            what: what.into(),
            why: why.into(),
            todo: todo.into(),
        }
    }

    /// The plugin is not enabled.
    #[must_use]
    pub fn not_enabled(plugin: &str) -> Self {
        Self::new(
            Kind::NotEnabled,
            plugin,
            format!("The plugin {plugin} is not turned on."),
            "Plugins only run after you approve them.",
            format!("Open the Plugins page and enable it, or run `helia plugins enable {plugin}`."),
        )
    }

    /// The plugin changed since it was approved.
    #[must_use]
    pub fn changed(plugin: &str, part: &str) -> Self {
        Self::new(
            Kind::Changed,
            plugin,
            format!("The plugin {plugin} has changed since you approved it."),
            format!("Its {part} no longer matches the checksum you approved."),
            format!(
                "If you updated it on purpose, review it and enable it again with `helia plugins enable {plugin}`."
            ),
        )
    }

    /// The program does not match its manifest.
    #[must_use]
    pub fn damaged(plugin: &str) -> Self {
        Self::new(
            Kind::Damaged,
            plugin,
            format!("The plugin {plugin} looks damaged."),
            "The program's checksum does not match the one in its manifest.",
            "Download the plugin again from its author.",
        )
    }

    /// The plugin stopped unexpectedly.
    #[must_use]
    pub fn crashed(plugin: &str, exit: &str, log_tail: &str) -> Self {
        let why = if log_tail.is_empty() {
            format!("It {exit}.")
        } else {
            format!("It {exit}. Its last message was: {log_tail}")
        };
        Self::new(
            Kind::Crashed,
            plugin,
            format!("The plugin {plugin} stopped unexpectedly."),
            why,
            "See the plugin's log on the Plugins page. Your capture is untouched; try again.",
        )
    }

    /// The plugin stopped answering.
    #[must_use]
    pub fn timeout(plugin: &str, secs: u64) -> Self {
        Self::new(
            Kind::Timeout,
            plugin,
            format!("The plugin {plugin} stopped answering."),
            format!("It sent nothing for {secs} seconds."),
            "Try again. If it keeps happening, raise timeout_s in its plugin.toml or ask its author.",
        )
    }

    /// The plugin broke the protocol.
    #[must_use]
    pub fn protocol(plugin: &str, detail: impl fmt::Display) -> Self {
        Self::new(
            Kind::Protocol,
            plugin,
            format!("The plugin {plugin} sent something Helia cannot read."),
            format!("{detail}."),
            "Update the plugin, and report this to its author.",
        )
    }

    /// The plugin returned a picture that does not fit.
    #[must_use]
    pub fn bad_result(plugin: &str, detail: impl fmt::Display) -> Self {
        Self::new(
            Kind::BadResult,
            plugin,
            format!("The plugin {plugin} returned a picture that does not fit."),
            format!("{detail}."),
            "Update the plugin, and report this to its author. Your capture is untouched.",
        )
    }

    /// Helia refused to send the request.
    #[must_use]
    pub fn refused(
        plugin: &str,
        what: impl Into<String>,
        why: impl Into<String>,
        todo: impl Into<String>,
    ) -> Self {
        Self::new(Kind::Refused, plugin, what, why, todo)
    }

    /// The manifest or program cannot be used.
    #[must_use]
    pub fn invalid(plugin: &str, why: impl Into<String>) -> Self {
        Self::new(
            Kind::Invalid,
            plugin,
            format!("The plugin {plugin} cannot be used."),
            why,
            "Check its plugin.toml, or download the plugin again from its author.",
        )
    }

    /// The call was cancelled.
    #[must_use]
    pub fn cancelled(plugin: &str) -> Self {
        Self::new(Kind::Cancelled, plugin, "Cancelled.", "", "")
    }

    /// The plugin reported an error; its own wording wins where it gave any.
    #[must_use]
    pub fn from_plugin(plugin: &str, e: &PluginError) -> Self {
        if e.kind() == ErrorKind::Cancelled {
            return Self::cancelled(plugin);
        }
        let what = e
            .what
            .clone()
            .unwrap_or_else(|| format!("The plugin {plugin} could not finish: {}", e.message));
        let why = e.why.clone().unwrap_or_else(|| match e.kind() {
            ErrorKind::Unavailable => "Something it needs is not reachable.".into(),
            ErrorKind::Unauthorised => "Its credentials were refused.".into(),
            ErrorKind::UnsupportedInput => "It cannot handle this kind of data.".into(),
            ErrorKind::Resource => "It ran out of memory, GPU memory or disk space.".into(),
            ErrorKind::InvalidSettings => "One of its settings is wrong.".into(),
            ErrorKind::MethodNotFound => "It does not offer this operation.".into(),
            _ => e.message.clone(),
        });
        let todo = e.todo.clone().unwrap_or_else(|| match e.kind() {
            ErrorKind::Unavailable => "Check the connection and try again.".into(),
            ErrorKind::Unauthorised => {
                "Check the token or password in the plugin's settings.".into()
            }
            ErrorKind::InvalidSettings => "Open the plugin's settings and correct them.".into(),
            ErrorKind::Resource => "Free some memory or space, or use a smaller capture.".into(),
            _ => "See the plugin's log on the Plugins page.".into(),
        });
        Self::new(Kind::Plugin(e.kind()), plugin, what, why, todo)
    }

    /// True for a cancellation.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.kind == Kind::Cancelled
    }
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.kind == Kind::Cancelled {
            return write!(f, "{}", self.what);
        }
        write!(f, "{} {} What to do: {}", self.what, self.why, self.todo)
    }
}

impl std::error::Error for HostError {}

impl From<HostError> for helia_core::Error {
    fn from(e: HostError) -> Self {
        if e.is_cancelled() {
            Self::Cancelled
        } else {
            Self::Backend(e.to_string())
        }
    }
}
