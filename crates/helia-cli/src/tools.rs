// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `helia tools`: install, check and remove optional programs (ffmpeg), from a terminal.
//!
//! Nothing here installs anything on its own. `status` never uses the network. `licence`,
//! `install` and `remove` contact the server only when you run them, and `install` shows the
//! provider's full licence and waits for you to type `yes` before a single byte is fetched.
//! Helia never ships these programs: they come from the provider, and the licence you accept
//! is the provider's.
//!
//! Exit codes:
//!
//! | Code | Meaning |
//! |---|---|
//! | 0 | success |
//! | 1 | failed: the message says what, why and what to do |
//! | 3 | the licence was not accepted (answered no, or no terminal and no `--accept-licence`) |

use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Result, anyhow, bail};
use clap::{Args, Subcommand, ValueEnum};
use helia_core::io::ffmpeg::{Source, Tools};
use helia_external::manifest::Offer;
use helia_external::{
    Acceptances, Client, CommandRunner, Hooks, Phase, Progress, Route, SystemRunner,
};
use helia_update::ureq_http::UreqHttp;
use helia_update::{Platform, SystemClock};

/// Exit code: the licence was not accepted.
pub const EXIT_DECLINED: i32 = 3;

/// How to get the program.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum MethodArg {
    /// Use the system package manager when there is one (Linux, Homebrew), else download.
    Auto,
    /// Use the system package manager; its command is shown and run only if you agree.
    Package,
    /// Download the provider's own build into your Helia tools folder.
    Download,
}

/// Options of `helia tools`.
#[derive(Args)]
pub struct ToolsArgs {
    /// What to do.
    #[command(subcommand)]
    pub command: ToolsCommand,
}

/// The `helia tools` subcommands.
#[derive(Subcommand, Clone)]
pub enum ToolsCommand {
    /// Show which optional tools are installed and where Helia finds ffmpeg.
    ///
    /// Uses no network. Helia looks for ffmpeg in `HELIA_FFMPEG` and `HELIA_FFPROBE`
    /// first, then in a copy installed with `helia tools install`, then on the `PATH`.
    Status {
        /// Print as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Print the full licence text of a tool, with its provider and source link.
    ///
    /// Fetches the signed list of tools to get the text. Nothing is installed.
    Licence {
        /// The tool: `ffmpeg`.
        item: String,
    },
    /// Install a tool: show its licence, ask, then fetch it from the provider.
    ///
    /// You are shown the provider, the size, the source link and the whole licence (in a
    /// pager when there is a terminal) and must type `yes`. Declining leaves everything as
    /// it was and Helia keeps working without the tool. Exit code 3: the licence was not
    /// accepted.
    Install {
        /// The tool: `ffmpeg` (installs `ffmpeg` and `ffprobe`).
        item: String,
        /// How to get it: the package manager, a download, or choose for me.
        #[arg(long, value_enum, default_value = "auto")]
        method: MethodArg,
        /// Accept the licence without a prompt, for scripts: the first 12 or more characters
        /// of the licence hash that `helia tools licence` prints. If the licence text has
        /// changed since you read it, the hash differs and nothing is installed.
        #[arg(long, value_name = "HASH")]
        accept_licence: Option<String>,
    },
    /// Remove an installed tool.
    ///
    /// Deletes the copy in your Helia tools folder. A tool installed by a package manager is
    /// left on the system (Helia says how to remove it); only Helia's note of it is dropped.
    Remove {
        /// The tool: `ffmpeg`.
        item: String,
        /// Also forget that you accepted its licence, so installing again asks again.
        #[arg(long)]
        forget_licence: bool,
        /// Go ahead without asking.
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

/// Shows long text. The real one uses the system pager on a terminal.
pub trait Pager {
    /// Show `text`; fall back to writing it to `out`.
    ///
    /// # Errors
    /// A failure to write to `out`.
    fn show(&self, text: &str, out: &mut dyn Write) -> std::io::Result<()>;
}

/// The system pager (`$PAGER`, else `less`, else `more`) when output is a terminal.
pub struct SystemPager;

impl Pager for SystemPager {
    fn show(&self, text: &str, out: &mut dyn Write) -> std::io::Result<()> {
        if std::io::stdout().is_terminal() {
            let candidates: Vec<String> = std::env::var("PAGER")
                .ok()
                .filter(|p| !p.trim().is_empty())
                .into_iter()
                .chain(["less".to_string(), "more".to_string()])
                .collect();
            for c in candidates {
                let mut words = c.split_whitespace();
                let Some(program) = words.next() else {
                    continue;
                };
                let spawned = Command::new(program)
                    .args(words)
                    .stdin(Stdio::piped())
                    .spawn();
                if let Ok(mut child) = spawned {
                    if let Some(mut stdin) = child.stdin.take() {
                        // The reader may quit early; that is not an error.
                        let _ = stdin.write_all(text.as_bytes());
                    }
                    let _ = child.wait();
                    return Ok(());
                }
            }
        }
        out.write_all(text.as_bytes())
    }
}

/// Everything `run_with` needs from outside, so tests can supply a local server and fake answers.
pub struct Ctx<'a> {
    /// The installer client (source, folders, HTTP).
    pub client: &'a Client<'a>,
    /// The platform to look for offers on.
    pub platform: Option<Platform>,
    /// Is a person at the keyboard (stdin is a terminal)?
    pub interactive: bool,
    /// Is this program on the path? Used to choose a package manager.
    pub has_program: &'a dyn Fn(&str) -> bool,
    /// Runs a package manager command.
    pub runner: &'a dyn CommandRunner,
    /// Shows the licence.
    pub pager: &'a dyn Pager,
    /// Where installed `ffmpeg` is currently found (for `status`).
    pub found_ffmpeg: &'a dyn Fn() -> Option<(PathBuf, Source)>,
}

fn size_text(bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss, reason = "a display figure")]
    let mb = bytes as f64 / (1024.0 * 1024.0);
    format!("{mb:.1} MB")
}

fn date_text(secs: u64) -> String {
    helia_update::time::format_utc(secs)
}

/// Can this package command run here: the manager is installed, and `sudo` too when it is used?
fn usable(cmd: &helia_external::PackageCommand, has: &dyn Fn(&str) -> bool) -> bool {
    has(&cmd.manager) && (cmd.argv.first().map(String::as_str) != Some("sudo") || has("sudo"))
}

fn choose<'m>(
    offers: &'m [Offer<'m>],
    method: MethodArg,
    has: &dyn Fn(&str) -> bool,
) -> Result<&'m Offer<'m>> {
    let package = offers
        .iter()
        .find(|o| matches!(&o.route, Route::Package(c) if usable(c, has)));
    let download = offers
        .iter()
        .find(|o| matches!(o.route, Route::Download(_)));
    match method {
        MethodArg::Package => package.ok_or_else(|| {
            anyhow!(
                "no supported package manager was found on this system. Try `--method download`, or install the tool yourself."
            )
        }),
        MethodArg::Download => download.ok_or_else(|| {
            anyhow!("no download is offered for this system. Try `--method package`, or install the tool yourself.")
        }),
        MethodArg::Auto => package
            .or(download)
            .ok_or_else(|| anyhow!("this tool is not offered for this system; install it yourself and set HELIA_FFMPEG and HELIA_FFPROBE")),
    }
}

/// Run a command. Returns the process exit code.
///
/// # Errors
/// Any failure, as a message that says what happened, why and what to do.
pub fn run_with(
    ctx: &Ctx<'_>,
    cmd: &ToolsCommand,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32> {
    match cmd {
        ToolsCommand::Status { json } => status(ctx, *json, out),
        ToolsCommand::Licence { item } => {
            let manifest = ctx.client.fetch_manifest()?;
            let offers = manifest.offers(item, ctx.platform)?;
            for o in &offers {
                writeln!(
                    out,
                    "{} {}, from {}.\nLicence: {}\nHash: {}\nProvider and source: {}\n",
                    o.item.name,
                    o.item.version,
                    o.item.provider,
                    o.licence.name,
                    o.licence.sha256,
                    o.licence.source_url
                )?;
                ctx.pager.show(&format!("{}\n", o.licence.text), out)?;
                writeln!(out)?;
            }
            Ok(0)
        }
        ToolsCommand::Install {
            item,
            method,
            accept_licence,
        } => install(
            ctx,
            item,
            *method,
            accept_licence.as_deref(),
            input,
            out,
            err,
        ),
        ToolsCommand::Remove {
            item,
            forget_licence,
            yes,
        } => remove(ctx, item, *forget_licence, *yes, input, out),
    }
}

fn status(ctx: &Ctx<'_>, json: bool, out: &mut dyn Write) -> Result<i32> {
    let health = ctx.client.list(false)?;
    let found = (ctx.found_ffmpeg)();
    if json {
        let v = serde_json::json!({
            "installed": health.iter().map(|h| serde_json::json!({
                "item": h.installed.item,
                "name": h.installed.name,
                "version": h.installed.version,
                "provider": h.installed.provider,
                "route": h.installed.route,
                "licence": h.installed.licence_name,
                "installed_at": date_text(h.installed.installed_at),
                "ok": h.ok(),
                "problems": h.problems,
                "files": h.installed.files.iter().map(|f| f.name.clone()).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "ffmpeg": found.as_ref().map(|(p, s)| serde_json::json!({
                "path": p.display().to_string(),
                "from": source_text(*s),
            })),
            "folder": ctx.client.dirs.tools.display().to_string(),
        });
        writeln!(out, "{}", serde_json::to_string_pretty(&v)?)?;
        return Ok(0);
    }
    writeln!(out, "Optional tools")?;
    if health.is_empty() {
        writeln!(
            out,
            "  Nothing installed with Helia. `helia tools install ffmpeg` installs ffmpeg."
        )?;
    }
    for h in &health {
        let i = &h.installed;
        writeln!(
            out,
            "  {} {} ({}), installed {} from {}{}",
            i.name,
            i.version,
            if i.route == "package" {
                "package manager"
            } else {
                "download"
            },
            date_text(i.installed_at),
            i.provider,
            if h.ok() { "" } else { ": NEEDS ATTENTION" }
        )?;
        writeln!(
            out,
            "    Licence: {} ({})",
            i.licence_name, i.licence_source_url
        )?;
        for p in &h.problems {
            writeln!(
                out,
                "    Problem: {p}. Run `helia tools install {}` again.",
                i.item
            )?;
        }
        if i.show_attribution
            && let Some(a) = &i.attribution
        {
            writeln!(out, "    {a}")?;
        }
    }
    match found {
        Some((p, s)) => writeln!(
            out,
            "Helia finds ffmpeg at {} ({}).",
            p.display(),
            source_text(s)
        )?,
        None => writeln!(
            out,
            "Helia cannot find ffmpeg. Video files need it; `helia tools install ffmpeg` fetches it."
        )?,
    }
    writeln!(out, "Tools folder: {}", ctx.client.dirs.tools.display())?;
    Ok(0)
}

fn source_text(s: Source) -> &'static str {
    match s {
        Source::Environment => "from HELIA_FFMPEG",
        Source::Installed => "installed with Helia",
        Source::Path => "on the PATH",
    }
}

fn read_line(input: &mut dyn BufRead) -> String {
    let mut s = String::new();
    let _ = input.read_line(&mut s);
    s.trim().to_string()
}

/// What is on offer, in plain words.
fn describe(ctx: &Ctx<'_>, offer: &Offer<'_>, out: &mut dyn Write) -> Result<()> {
    writeln!(out, "{} {}", offer.item.name, offer.item.version)?;
    writeln!(out, "  {}", offer.item.description)?;
    writeln!(out, "  Provided by: {}", offer.provider())?;
    match &offer.route {
        Route::Download(parts) => {
            let hosts: Vec<String> = parts
                .iter()
                .filter_map(|d| helia_update::manifest::url_host(&d.url))
                .collect();
            writeln!(
                out,
                "  Helia would download {} from {} into {}.",
                size_text(offer.download_bytes()),
                hosts.join(", "),
                ctx.client.dirs.tools.display()
            )?;
        }
        Route::Package(c) => writeln!(
            out,
            "  Helia would run this command, which asks your system for permission if it needs it: {}",
            c.display()
        )?,
    }
    writeln!(out, "  Licence: {}", offer.licence.name)?;
    writeln!(out, "  Licence and source: {}", offer.licence.source_url)?;
    writeln!(
        out,
        "  Helia is not affiliated with the provider and does not distribute this tool."
    )?;
    Ok(())
}

/// Get the user's acceptance of the licence, by record, by hash or by asking. `Some(code)` means stop
/// with that exit code; `None` means the licence is accepted and recorded.
fn obtain_acceptance(
    ctx: &Ctx<'_>,
    offer: &Offer<'_>,
    accepted: &mut Acceptances,
    accept_licence: Option<&str>,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<Option<i32>> {
    let path = &ctx.client.dirs.accepted;
    if accepted.is_accepted(offer) {
        let when = accepted
            .records
            .iter()
            .find(|r| r.item == offer.item.id && r.licence_sha256 == offer.licence.sha256)
            .map_or_else(String::new, |r| format!(" on {}", date_text(r.accepted_at)));
        writeln!(out, "You accepted this licence{when}; it has not changed.")?;
    } else if let Some(hash) = accept_licence {
        let h = hash.trim().to_ascii_lowercase();
        if h.len() < 12 || !offer.licence.sha256.starts_with(&h) {
            writeln!(
                err,
                "The licence hash you gave does not match the licence now on offer (hash {}). It may have changed. Read it with `helia tools licence {}` and accept it again if you agree.",
                &offer.licence.sha256[..12],
                offer.item.id
            )?;
            return Ok(Some(EXIT_DECLINED));
        }
        accepted.accept(offer, ctx.client.clock.now_unix(), "cli-flag");
        accepted.save(ctx.client.fs, path)?;
    } else if ctx.interactive {
        writeln!(
            out,
            "\nThe licence follows. Read it, then answer the question at the end.\n"
        )?;
        ctx.pager.show(&format!("{}\n", offer.licence.text), out)?;
        write!(
            out,
            "\nType yes to accept the licence above and install {}. Anything else cancels: ",
            offer.item.name
        )?;
        out.flush()?;
        if !read_line(input).eq_ignore_ascii_case("yes") {
            writeln!(
                out,
                "Cancelled. Nothing was downloaded. Helia keeps working without {}.",
                offer.item.name
            )?;
            return Ok(Some(EXIT_DECLINED));
        }
        accepted.accept(offer, ctx.client.clock.now_unix(), "cli");
        accepted.save(ctx.client.fs, path)?;
    } else {
        writeln!(
            err,
            "{} needs you to accept its licence, and there is no terminal to ask on, so nothing was downloaded.\nRead the licence with `helia tools licence {}`, then run `helia tools install {} --accept-licence {}` (the first 12 characters of the hash it prints).",
            offer.item.name,
            offer.item.id,
            offer.item.id,
            &offer.licence.sha256[..12]
        )?;
        return Ok(Some(EXIT_DECLINED));
    }
    Ok(None)
}

/// Download, check and unpack, reporting progress on `err`.
fn fetch(
    ctx: &Ctx<'_>,
    offer: &Offer<'_>,
    accepted: &Acceptances,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<()> {
    let mut last = 101u64;
    let mut progress = |p: &Progress| {
        if p.total == 0 {
            return;
        }
        let step = p.done.saturating_mul(100) / p.total / 10 * 10;
        let label = match p.phase {
            Phase::Downloading => "Downloading",
            Phase::Verifying => "Checking the download",
            Phase::Unpacking => "Unpacking",
        };
        if p.phase != Phase::Downloading {
            let _ = writeln!(err, "{label}...");
        } else if step != last {
            last = step;
            let _ = writeln!(
                err,
                "{label} part {}/{}: {} of {} ({step}%)",
                p.part,
                p.parts,
                size_text(p.done),
                size_text(p.total)
            );
        }
    };
    let cancelled = || false;
    let rec = ctx.client.install_with(
        offer,
        accepted,
        &mut Hooks {
            progress: &mut progress,
            cancelled: &cancelled,
        },
    )?;
    writeln!(
        out,
        "Installed {} {} in {}.",
        rec.name,
        rec.version,
        ctx.client.dirs.item_dir(&rec.item).display()
    )?;
    if rec.show_attribution
        && let Some(a) = &rec.attribution
    {
        writeln!(out, "{a}")?;
    }
    writeln!(
        out,
        "Helia uses it automatically; HELIA_FFMPEG and HELIA_FFPROBE still win if you set them."
    )?;
    Ok(())
}

/// Run the package manager command after a second, separate yes.
fn run_command(
    ctx: &Ctx<'_>,
    offer: &Offer<'_>,
    cmd: &helia_external::PackageCommand,
    accepted: &Acceptances,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<i32> {
    if !ctx.interactive {
        bail!(
            "installing with a package manager needs a terminal, because it may ask for your password. Run it yourself ({}) or use `--method download`.",
            cmd.display()
        );
    }
    write!(out, "Run `{}` now? Type yes to run it: ", cmd.display())?;
    out.flush()?;
    if !read_line(input).eq_ignore_ascii_case("yes") {
        writeln!(
            out,
            "Not run. The licence is recorded; run the command yourself, or run this again."
        )?;
        return Ok(EXIT_DECLINED);
    }
    let rec = ctx.client.run_package(offer, accepted, ctx.runner)?;
    writeln!(out, "Installed {} with your package manager.", rec.name)?;
    if rec.show_attribution
        && let Some(a) = &rec.attribution
    {
        writeln!(out, "{a}")?;
    }
    Ok(0)
}

fn install(
    ctx: &Ctx<'_>,
    item: &str,
    method: MethodArg,
    accept_licence: Option<&str>,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32> {
    let manifest = ctx.client.fetch_manifest()?;
    let offers = manifest.offers(item, ctx.platform)?;
    let offer = choose(&offers, method, ctx.has_program)?;
    let mut accepted = Acceptances::load(ctx.client.fs, &ctx.client.dirs.accepted)?;
    describe(ctx, offer, out)?;
    if let Some(code) =
        obtain_acceptance(ctx, offer, &mut accepted, accept_licence, input, out, err)?
    {
        return Ok(code);
    }
    match &offer.route {
        Route::Download(_) => fetch(ctx, offer, &accepted, out, err)?,
        Route::Package(c) => return run_command(ctx, offer, c, &accepted, input, out),
    }
    Ok(0)
}

fn remove(
    ctx: &Ctx<'_>,
    item: &str,
    forget: bool,
    yes: bool,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<i32> {
    let Some(h) = ctx.client.check(item, false)? else {
        bail!(
            "{item} is not installed with Helia, so there is nothing to remove. `helia tools status` lists what is."
        );
    };
    if !yes {
        if !ctx.interactive {
            bail!("removing {item} needs a yes: add --yes, or run this in a terminal.");
        }
        write!(
            out,
            "Remove {} {} from {}? Type yes to confirm: ",
            h.installed.name,
            h.installed.version,
            ctx.client.dirs.item_dir(item).display()
        )?;
        out.flush()?;
        if !read_line(input).eq_ignore_ascii_case("yes") {
            writeln!(out, "Nothing was removed.")?;
            return Ok(EXIT_DECLINED);
        }
    }
    let removal = ctx.client.remove(item)?;
    writeln!(out, "Removed {}.", removal.installed.name)?;
    if let Some(n) = removal.note {
        writeln!(out, "{n}")?;
    }
    if forget {
        let mut a = Acceptances::load(ctx.client.fs, &ctx.client.dirs.accepted)?;
        let n = a.forget(item);
        a.save(ctx.client.fs, &ctx.client.dirs.accepted)?;
        writeln!(
            out,
            "Forgot {n} licence acceptance(s); installing again asks again."
        )?;
    }
    Ok(0)
}

/// `helia tools`, with the real network, folders, terminal and package manager.
///
/// # Errors
/// As [`run_with`].
pub fn run(args: &ToolsArgs) -> Result<i32> {
    let source = helia_external::helia::source();
    let dirs = helia_external::helia::dirs().ok_or_else(|| {
        anyhow!(
            "cannot find a home folder to keep the tools in; set HELIA_EXTERNAL_DIR to a folder"
        )
    })?;
    let http = UreqHttp::new(source.allow_loopback_http);
    let clock = SystemClock;
    let client = Client {
        http: &http,
        fs: &helia_update::StdFs,
        clock: &clock,
        source: &source,
        dirs: &dirs,
    };
    let has = |p: &str| which(p);
    let found = || {
        Tools::locate()
            .ok()
            .and_then(|t| Tools::source_of_ffmpeg().map(|s| (t.ffmpeg, s)))
    };
    let ctx = Ctx {
        client: &client,
        platform: Platform::current(),
        interactive: std::io::stdin().is_terminal(),
        has_program: &has,
        runner: &SystemRunner,
        pager: &SystemPager,
        found_ffmpeg: &found,
    };
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let (stdout, stderr) = (std::io::stdout(), std::io::stderr());
    let (mut out, mut err) = (stdout.lock(), stderr.lock());
    run_with(&ctx, &args.command, &mut input, &mut out, &mut err)
}

/// The programs installed with Helia, for [`helia_core::io::ffmpeg::set_installed_locator`].
#[must_use]
pub fn installed_programs() -> helia_core::io::ffmpeg::InstalledPrograms {
    let (ffmpeg, ffprobe) = helia_external::helia::installed_ffmpeg();
    helia_core::io::ffmpeg::InstalledPrograms { ffmpeg, ffprobe }
}

/// Is `program` an executable on the `PATH`?
fn which(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|d| {
        let p = d.join(program);
        p.is_file() || (cfg!(windows) && d.join(format!("{program}.exe")).is_file())
    })
}
