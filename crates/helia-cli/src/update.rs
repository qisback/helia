// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `helia update`: check for, download and install a new Helia, from a terminal.
//!
//! The command line never checks on its own: nothing goes over the network
//! unless you run `check` or `apply`. Those two contact only the update server
//! the build was made for (the signed manifest and its signature, and the file
//! when you apply), send no identifiers, and identify themselves as
//! `helia-update/<version>`.
//!
//! Exit codes:
//!
//! | Code | Meaning |
//! |---|---|
//! | 0 | success; for `check`, Helia is up to date |
//! | 1 | failed: the message says what, why and what to do |
//! | 3 | `apply` or `rollback` was not confirmed (no `--yes` and no terminal, or answered no) |
//! | 10 | `check` only: an update is available |
//! | 12 | `check` or `apply`: the update details need a newer Helia to read; update by hand |

#![allow(
    clippy::doc_markdown,
    reason = "AppImage is a product name in the help text"
)]

use std::io::{BufRead, IsTerminal, Write};

use anyhow::{Result, anyhow};
use clap::{Args, Subcommand, ValueEnum};
use helia_update::apply::ApplyOutcome;
use helia_update::client::{Available, CheckReport, Client, System};
use helia_update::settings::{Channel, UpdateMode, UpdateSettings};

/// The version of this build.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Exit code: an update is available (`check`).
pub const EXIT_AVAILABLE: i32 = 10;
/// Exit code: not confirmed.
pub const EXIT_DECLINED: i32 = 3;
/// Exit code: the update details need a newer Helia to read.
pub const EXIT_MANUAL: i32 = 12;

/// Which releases.
#[derive(Clone, Copy, ValueEnum)]
pub enum ChannelArg {
    /// Released versions only.
    Stable,
    /// Pre-release versions as well.
    Prerelease,
}

impl From<ChannelArg> for Channel {
    fn from(c: ChannelArg) -> Self {
        match c {
            ChannelArg::Stable => Self::Stable,
            ChannelArg::Prerelease => Self::Prerelease,
        }
    }
}

/// What Helia may do about updates on its own.
#[derive(Clone, Copy, ValueEnum)]
pub enum ModeArg {
    /// Tell me when an update is available; download only when asked.
    Notify,
    /// Download updates and install them when Helia restarts.
    Automatic,
    /// Never check on my own.
    Off,
}

impl From<ModeArg> for UpdateMode {
    fn from(m: ModeArg) -> Self {
        match m {
            ModeArg::Notify => Self::Notify,
            ModeArg::Automatic => Self::Automatic,
            ModeArg::Off => Self::Off,
        }
    }
}

/// Options of `helia update`.
///
/// The command line never checks for updates on its own. Settings are shared with
/// the GUI (`update.conf` in the Helia settings folder).
#[derive(Args)]
pub struct UpdateArgs {
    /// What to do.
    #[command(subcommand)]
    pub command: UpdateCommand,
}

/// The `helia update` subcommands.
#[derive(Subcommand)]
pub enum UpdateCommand {
    /// Ask the update server whether a newer Helia exists.
    ///
    /// Exit code 0: up to date. 10: an update is available. 12: the update details
    /// need a newer Helia to read. 1: the check failed.
    Check {
        /// Look at this channel for this run only (the saved channel is unchanged).
        #[arg(long, value_enum)]
        channel: Option<ChannelArg>,
        /// Print the answer as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Download the newer Helia and install it.
    ///
    /// How it installs depends on how this copy was installed: an AppImage is
    /// replaced (the old file is kept beside it), the Windows installer is run
    /// silently when this command ends, and for deb and rpm installs the package
    /// command is shown and no file is touched.
    Apply {
        /// Go ahead without asking.
        #[arg(long, short = 'y')]
        yes: bool,
        /// Use this channel for this run only.
        #[arg(long, value_enum)]
        channel: Option<ChannelArg>,
    },
    /// Show the update settings, how this copy is installed and what is remembered.
    Status {
        /// Print as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Go back to the version an update replaced, when it was kept.
    Rollback {
        /// Go ahead without asking.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Show or change the update mode and channel.
    ///
    /// The mode is `notify` (the default: tell me, download when I ask), `automatic`
    /// (download and install on restart) or `off` (never check on my own). With no
    /// option this shows the current settings.
    Config {
        /// Set the mode.
        #[arg(long, value_enum)]
        mode: Option<ModeArg>,
        /// Set the channel.
        #[arg(long, value_enum)]
        channel: Option<ChannelArg>,
    },
}

fn json_string(v: &serde_json::Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

fn available_json(a: &Available) -> serde_json::Value {
    serde_json::json!({
        "status": "available",
        "current": VERSION,
        "version": a.version,
        "required": a.required,
        "notes_url": a.notes_url,
        "install": a.install.describe(),
        "installs_itself": a.self_applies(),
        "file": a.asset.as_ref().map(|f| serde_json::json!({
            "kind": f.kind.as_str(),
            "size": f.size,
            "sha256": f.sha256,
        })),
        "downloaded": a.staged.is_some(),
    })
}

fn size_text(bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss, reason = "a display figure")]
    let mb = bytes as f64 / (1024.0 * 1024.0);
    format!("{mb:.1} MB")
}

fn describe(a: &Available, out: &mut dyn Write) -> std::io::Result<()> {
    writeln!(
        out,
        "Helia {} is available (you have {VERSION}).",
        a.version
    )?;
    if a.required {
        writeln!(
            out,
            "This version is required: the one you have is no longer supported."
        )?;
    }
    if let Some(u) = &a.notes_url {
        writeln!(out, "What changed: {u}")?;
    }
    match (&a.asset, a.self_applies()) {
        (None, _) => writeln!(
            out,
            "There is no download for this system listed yet ({}). Get it from the Helia website.",
            a.install.describe()
        ),
        (Some(f), true) => writeln!(
            out,
            "This copy is a {}: `helia update apply` downloads {} and installs it for you.",
            a.install.describe(),
            size_text(f.size)
        ),
        (Some(f), false) => writeln!(
            out,
            "This copy is a {}: `helia update apply` downloads {} and shows you how to install it.",
            a.install.describe(),
            size_text(f.size)
        ),
    }
}

fn client_run<T>(
    channel: Option<ChannelArg>,
    f: impl FnOnce(&Client<'_>) -> Result<T>,
) -> Result<T> {
    let system = System::new(false).ok_or_else(|| {
        anyhow!(
            "I could not work out where to keep update files. Set HELIA_UPDATE_DIR to a folder you can write to, then try again."
        )
    })?;
    system.with_client(VERSION, |c| {
        let c = c.clone().with_channel(channel.map(Channel::from));
        f(&c)
    })
}

fn confirm(question: &str, yes: bool) -> bool {
    if yes {
        return true;
    }
    if !std::io::stdin().is_terminal() {
        eprintln!("{question} Run again with --yes to go ahead.");
        return false;
    }
    eprint!("{question} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).is_ok()
        && matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn check(channel: Option<ChannelArg>, json: bool) -> Result<i32> {
    client_run(channel, |c| {
        let report = c.check(true)?;
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        Ok(match report {
            CheckReport::Available(a) => {
                if json {
                    writeln!(out, "{}", json_string(&available_json(&a)))?;
                } else {
                    describe(&a, &mut out)?;
                }
                EXIT_AVAILABLE
            }
            CheckReport::UpToDate { version } | CheckReport::OlderOnServer { version } => {
                if json {
                    let v = serde_json::json!({"status": "up-to-date", "current": VERSION, "latest": version});
                    writeln!(out, "{}", json_string(&v))?;
                } else {
                    writeln!(out, "Helia {VERSION} is up to date.")?;
                }
                0
            }
            CheckReport::SchemaTooNew { schema } => {
                if json {
                    let v = serde_json::json!({"status": "update-by-hand", "current": VERSION, "format": schema});
                    writeln!(out, "{}", json_string(&v))?;
                } else {
                    writeln!(
                        out,
                        "There is a newer Helia, but its update details use format {schema}, which this version cannot read. Download the new version from the Helia website."
                    )?;
                }
                EXIT_MANUAL
            }
            CheckReport::Disabled | CheckReport::NotDue => 0,
        })
    })
}

fn apply(yes: bool, channel: Option<ChannelArg>) -> Result<i32> {
    client_run(channel, |c| {
        let a = match c.check(true)? {
            CheckReport::Available(a) => a,
            CheckReport::SchemaTooNew { schema } => {
                eprintln!(
                    "There is a newer Helia, but its update details use format {schema}, which this version cannot read. Download the new version from the Helia website."
                );
                return Ok(EXIT_MANUAL);
            }
            _ => {
                println!("Helia {VERSION} is up to date. Nothing to do.");
                return Ok(0);
            }
        };
        describe(&a, &mut std::io::stdout())?;
        if !confirm(&format!("Download and install Helia {}?", a.version), yes) {
            return Ok(EXIT_DECLINED);
        }
        let bar = indicatif::ProgressBar::new(a.asset.as_ref().map_or(0, |f| f.size));
        bar.set_style(
            indicatif::ProgressStyle::with_template("{bytes}/{total_bytes} {wide_bar} {eta}")
                .unwrap_or_else(|_| indicatif::ProgressStyle::default_bar()),
        );
        let staged = c.download(&a, &mut |done, total| {
            bar.set_length(total);
            bar.set_position(done);
        })?;
        bar.finish_and_clear();
        println!("Downloaded and checked (signature and checksum).");
        let outcome = c.apply(&staged, a.notes_url.as_deref())?;
        println!("{}", outcome.message());
        if let ApplyOutcome::Manual {
            command: Some(cmd), ..
        } = &outcome
        {
            println!("\n    {cmd}\n");
        }
        Ok(0)
    })
}

fn status(json: bool) -> Result<i32> {
    client_run(None, |c| {
        let s = c.status();
        if json {
            let v = serde_json::json!({
                "version": s.version,
                "install": s.install.describe(),
                "mode": s.settings.mode.as_str(),
                "channel": s.settings.channel.as_str(),
                "settings_from_newer_helia": s.newer_settings_schema,
                "last_check_unix": (s.last_check != 0).then_some(s.last_check),
                "downloaded": s.staged.as_ref().map(|d| d.version.clone()),
                "previous_version_kept": s.previous,
            });
            println!("{}", json_string(&v));
            return Ok(0);
        }
        println!("Helia {} ({})", s.version, s.install.describe());
        println!(
            "Mode: {}  ({})",
            s.settings.mode.as_str(),
            s.settings.mode.describe()
        );
        println!("Channel: {}", s.settings.channel.as_str());
        if let Some(n) = s.newer_settings_schema {
            println!(
                "The update settings file was written by a newer Helia (format {n}); it is left alone and the defaults are used."
            );
        }
        if s.last_check == 0 {
            println!("Last check: never");
        } else {
            println!(
                "Last check: {}",
                helia_update::time::format_utc(s.last_check)
            );
        }
        if let Some(d) = &s.staged {
            println!("Downloaded and waiting: Helia {}", d.version);
        }
        if let Some(p) = &s.previous {
            println!("Previous version kept: {p} (`helia update rollback` goes back to it)");
        }
        Ok(0)
    })
}

fn rollback(yes: bool) -> Result<i32> {
    client_run(None, |c| {
        let s = c.status();
        let Some(prev) = s.previous else {
            return Err(anyhow!(
                "Helia has no older version kept, so it cannot go back. Updates only keep the previous version when Helia installed them itself. Download the version you want from the Helia website."
            ));
        };
        if !confirm(&format!("Go back from Helia {VERSION} to {prev}?"), yes) {
            return Ok(EXIT_DECLINED);
        }
        let r = c.rollback_now()?;
        println!("{}", r.message);
        Ok(0)
    })
}

fn config(mode: Option<ModeArg>, channel: Option<ChannelArg>) -> Result<i32> {
    client_run(None, |c| {
        let mut s: UpdateSettings = c.settings();
        if mode.is_some() || channel.is_some() {
            if let Some(m) = mode {
                s.mode = m.into();
            }
            if let Some(ch) = channel {
                s.channel = ch.into();
            }
            c.save_settings(&s)?;
        }
        println!("mode={}", s.mode.as_str());
        println!("channel={}", s.channel.as_str());
        Ok(0)
    })
}

/// Run `helia update ...`; returns the exit code.
///
/// # Errors
/// Anything that went wrong, in words that say what, why and what to do.
pub fn run(a: &UpdateArgs) -> Result<i32> {
    match &a.command {
        UpdateCommand::Check { channel, json } => check(*channel, *json),
        UpdateCommand::Apply { yes, channel } => apply(*yes, *channel),
        UpdateCommand::Status { json } => status(*json),
        UpdateCommand::Rollback { yes } => rollback(*yes),
        UpdateCommand::Config { mode, channel } => config(*mode, *channel),
    }
}
