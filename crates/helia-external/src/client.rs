// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The client: fetch and verify the manifest, then install, check and remove items.
//!
//! The order of an install is fixed and cannot be skipped:
//!
//! 1. [`Client::fetch_manifest`]: manifest and detached signature over https from an
//!    allowed host, signature checked against the pinned release keys, strict parse,
//!    then the policy (right app, not expired, not a replay).
//! 2. The caller shows [`Offer::licence`] in full and asks. Only after a yes does it
//!    call [`Acceptances::accept`] and [`Acceptances::save`].
//! 3. [`Client::install`] checks the acceptance FIRST, before any request is made,
//!    then checks every address, downloads (resumable, sha256 and size checked),
//!    unpacks only the named files into a fresh folder and renames it into place.
//!
//! Helia does not redistribute anything: every byte comes from the provider named in
//! the signed manifest.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use helia_update::manifest::check_url;
use helia_update::{Clock, Fs, Http, Request};

use crate::archive::{self, Extracted};
use crate::error::{Error, Result};
use crate::manifest::{Download, MAX_MANIFEST_BYTES, Manifest, Offer, Route, hex, is_ident};
use crate::source::ExternalSource;
use crate::store::{Acceptances, Dirs, State};

/// What an install is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Bytes are arriving.
    Downloading,
    /// The checksum is being checked.
    Verifying,
    /// The named files are being unpacked.
    Unpacking,
}

/// A progress report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Progress {
    /// What is happening.
    pub phase: Phase,
    /// Which part (1-based) of how many.
    pub part: usize,
    /// How many parts the item has on this system.
    pub parts: usize,
    /// Bytes so far in this part.
    pub done: u64,
    /// Bytes in this part.
    pub total: u64,
}

/// Progress and cancellation for a long call.
pub struct Hooks<'a> {
    /// Called as work advances.
    pub progress: &'a mut dyn FnMut(&Progress),
    /// Polled between chunks; return true to stop. The partial download is kept.
    pub cancelled: &'a dyn Fn() -> bool,
}

/// One file of an installed item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledFile {
    /// Name in the item folder.
    pub name: String,
    /// Logical name the app looks it up by.
    pub provides: Option<String>,
    /// sha256 of the file when installed.
    pub sha256: String,
    /// Size in bytes.
    pub size: u64,
}

/// The record written next to an installed item (`installed.json`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    /// Item id.
    pub item: String,
    /// Display name.
    pub name: String,
    /// Version installed.
    pub version: String,
    /// Provider.
    pub provider: String,
    /// Provider page.
    pub provider_url: Option<String>,
    /// Licence id.
    pub licence_id: String,
    /// Licence name.
    pub licence_name: String,
    /// sha256 of the licence text that was accepted.
    pub licence_sha256: String,
    /// Where the provider publishes the licence and source.
    pub licence_source_url: String,
    /// Attribution to show, if the licence needs it.
    pub attribution: Option<String>,
    /// Show the attribution (About, status).
    pub show_attribution: bool,
    /// When, seconds since the Unix epoch.
    pub installed_at: u64,
    /// `download` or `package`.
    pub route: String,
    /// The package manager command, for `package`.
    pub command: Option<String>,
    /// Files, for `download`.
    pub files: Vec<InstalledFile>,
}

/// An installed item and anything wrong with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Health {
    /// The record.
    pub installed: Installed,
    /// Plain-words problems (a missing or changed file); empty when all is well.
    pub problems: Vec<String>,
}

impl Health {
    /// Is everything in place?
    #[must_use]
    pub fn ok(&self) -> bool {
        self.problems.is_empty()
    }
}

/// What [`Client::remove`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Removal {
    /// The record that was removed.
    pub installed: Installed,
    /// Said when a package manager installed it: Helia does not uninstall system packages.
    pub note: Option<String>,
}

/// Runs a package manager command. The real one is [`SystemRunner`]; tests use a fake.
pub trait CommandRunner {
    /// Run `argv` (never through a shell) and return its exit code.
    fn run(&self, argv: &[String]) -> std::io::Result<i32>;
}

/// Runs the command with the terminal's own input and output, so `sudo` asks for the
/// password itself and the user sees what the package manager prints.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&self, argv: &[String]) -> std::io::Result<i32> {
        let (prog, rest) = argv
            .split_first()
            .ok_or_else(|| std::io::Error::other("empty command"))?;
        let status = std::process::Command::new(prog).args(rest).status()?;
        Ok(status.code().unwrap_or(-1))
    }
}

/// The program or file an installed item provides, for example
/// `installed_program(&dirs, "ffmpeg", "ffprobe")`. `None` when the item is not
/// installed by download, the record cannot be read, or the file has gone.
#[must_use]
pub fn installed_program(dirs: &Dirs, item_id: &str, provides: &str) -> Option<PathBuf> {
    if !is_ident(item_id) {
        return None;
    }
    let dir = dirs.item_dir(item_id);
    let rec: Installed =
        serde_json::from_slice(&fs::read(dir.join("installed.json")).ok()?).ok()?;
    let f = rec
        .files
        .iter()
        .find(|f| f.provides.as_deref() == Some(provides))?;
    let p = dir.join(&f.name);
    p.is_file().then_some(p)
}

/// The client. All outside effects come in through the three traits of `helia-update`.
pub struct Client<'a> {
    /// HTTP `GET` (the real one is `helia_update::ureq_http::UreqHttp`).
    pub http: &'a dyn Http,
    /// Small-file storage for the records.
    pub fs: &'a dyn Fs,
    /// The clock.
    pub clock: &'a dyn Clock,
    /// Where the manifest comes from and who may serve downloads.
    pub source: &'a ExternalSource,
    /// Folders.
    pub dirs: &'a Dirs,
}

/// A safe staging file name from the last URL segment: `[A-Za-z0-9._-]` only.
fn staging_name(url: &str, sha256: &str) -> String {
    let seg = url
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('/')
        .next()
        .unwrap_or("");
    let clean: String = seg
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(100)
        .collect();
    let clean = clean.trim_start_matches('.');
    let clean = if clean.is_empty() {
        "download.bin"
    } else {
        clean
    };
    format!("{}-{clean}", &sha256[..8])
}

fn hash_file(path: &Path) -> Result<String> {
    let mut f = File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

impl Client<'_> {
    /// Fetch, verify and parse the manifest, then apply the policy and remember its
    /// sequence.
    ///
    /// # Errors
    /// [`Error::Transport`] (address, host, network, signature), [`Error::TooLarge`],
    /// [`Error::TooDeep`], [`Error::Malformed`], [`Error::SchemaTooNew`],
    /// [`Error::WrongApp`], [`Error::Expired`], [`Error::Replay`], [`Error::State`].
    pub fn fetch_manifest(&self) -> Result<Manifest> {
        self.source.validate()?;
        let bytes = self.get_small(&self.source.manifest_url, MAX_MANIFEST_BYTES)?;
        let sig = self.get_small(&self.source.signature_url(), 4096)?;
        let sig = String::from_utf8(sig).map_err(|_| {
            Error::Transport(helia_update::Error::BadSignatureFormat("not text".into()))
        })?;
        // Verify the exact bytes before parsing them.
        helia_update::verify::verify(&bytes, &sig, &self.source.pinned_keys)?;
        let manifest = Manifest::parse(&bytes, self.source.allow_loopback_http)?;
        let mut state = State::load(self.fs, &self.dirs.state)?;
        let now = self.clock.now_unix();
        manifest.check_policy(&self.source.app, now, state.highest_sequence)?;
        state.highest_sequence = state.highest_sequence.max(manifest.sequence);
        state.last_fetch = now;
        state.save(self.fs, &self.dirs.state)?;
        Ok(manifest)
    }

    fn get_small(&self, url: &str, max: usize) -> Result<Vec<u8>> {
        check_url(url, self.source.allow_loopback_http)?;
        if !self.source.manifest_host_allowed(url) {
            return Err(helia_update::Error::ForeignHost(url.chars().take(200).collect()).into());
        }
        let resp = self.http.get(&Request {
            url,
            range_from: None,
            allowed_hosts: &self.source.manifest_hosts,
            bearer: None,
            bearer_host: None,
        })?;
        if resp.status != 200 {
            return Err(helia_update::Error::HttpStatus(resp.status).into());
        }
        let mut buf = Vec::new();
        resp.body.take(max as u64 + 1).read_to_end(&mut buf)?;
        if buf.len() > max {
            return Err(Error::TooLarge {
                size: buf.len(),
                max,
            });
        }
        Ok(buf)
    }

    /// Install a download offer. Refuses, before any request, unless `accepted` holds the
    /// user's acceptance of exactly this licence text.
    ///
    /// # Errors
    /// [`Error::NotAccepted`], [`Error::WrongRoute`], [`Error::Cancelled`], [`Error::Archive`],
    /// [`Error::Member`], [`Error::Transport`] (address, host, network, hash mismatch,
    /// truncated download: call again to resume), [`Error::Io`].
    pub fn install(&self, offer: &Offer<'_>, accepted: &Acceptances) -> Result<Installed> {
        let mut progress = |_: &Progress| {};
        let cancelled = || false;
        self.install_with(
            offer,
            accepted,
            &mut Hooks {
                progress: &mut progress,
                cancelled: &cancelled,
            },
        )
    }

    /// [`Self::install`] with progress and cancellation.
    ///
    /// # Errors
    /// As [`Self::install`].
    pub fn install_with(
        &self,
        offer: &Offer<'_>,
        accepted: &Acceptances,
        hooks: &mut Hooks<'_>,
    ) -> Result<Installed> {
        if !accepted.is_accepted(offer) {
            return Err(Error::NotAccepted(offer.item.name.clone()));
        }
        let Route::Download(parts) = &offer.route else {
            return Err(Error::WrongRoute(offer.item.name.clone()));
        };
        // Every address is checked before the first byte is fetched.
        for d in parts {
            check_url(&d.url, self.source.allow_loopback_http)?;
            if !self.source.asset_host_allowed(&d.url) {
                return Err(
                    helia_update::Error::ForeignHost(d.url.chars().take(200).collect()).into(),
                );
            }
        }
        let mut staged = Vec::new();
        for (i, d) in parts.iter().enumerate() {
            staged.push((*d, self.download_part(d, i + 1, parts.len(), hooks)?));
        }

        fs::create_dir_all(&self.dirs.tools)?;
        let work = self.dirs.tools.join(format!(".incoming-{}", offer.item.id));
        if work.exists() {
            fs::remove_dir_all(&work)?;
        }
        fs::create_dir_all(&work)?;
        let built = (|| -> Result<Vec<Extracted>> {
            let mut files = Vec::new();
            for (i, (d, path)) in staged.iter().enumerate() {
                (hooks.progress)(&Progress {
                    phase: Phase::Unpacking,
                    part: i + 1,
                    parts: staged.len(),
                    done: 0,
                    total: d.size,
                });
                files.extend(archive::extract(path, d.archive, &d.members, &work)?);
            }
            Ok(files)
        })();
        let files = match built {
            Ok(f) => f,
            Err(e) => {
                let _ = fs::remove_dir_all(&work);
                return Err(e);
            }
        };

        let record = Installed {
            item: offer.item.id.clone(),
            name: offer.item.name.clone(),
            version: offer.item.version.clone(),
            provider: offer.provider().to_string(),
            provider_url: offer.item.provider_url.clone(),
            licence_id: offer.licence.id.clone(),
            licence_name: offer.licence.name.clone(),
            licence_sha256: offer.licence.sha256.clone(),
            licence_source_url: offer.licence.source_url.clone(),
            attribution: offer.licence.attribution.clone(),
            show_attribution: offer.licence.show_attribution,
            installed_at: self.clock.now_unix(),
            route: "download".into(),
            command: None,
            files: files
                .into_iter()
                .map(|f| InstalledFile {
                    name: f.save_as,
                    provides: f.provides,
                    sha256: f.sha256,
                    size: f.size,
                })
                .collect(),
        };
        let notes = source_note(offer, parts);
        let finish = (|| -> Result<()> {
            fs::write(work.join("LICENCE.txt"), offer.licence.text.as_bytes())?;
            fs::write(work.join("SOURCE.txt"), notes.as_bytes())?;
            let json =
                serde_json::to_vec_pretty(&record).map_err(|e| Error::State(e.to_string()))?;
            fs::write(work.join("installed.json"), json)?;
            let dest = self.dirs.item_dir(&offer.item.id);
            if dest.exists() {
                fs::remove_dir_all(&dest)?;
            }
            fs::rename(&work, &dest)?;
            Ok(())
        })();
        if let Err(e) = finish {
            let _ = fs::remove_dir_all(&work);
            return Err(e);
        }
        for (_, path) in &staged {
            let _ = fs::remove_file(path);
        }
        Ok(record)
    }

    /// Run a package manager offer, with the user's consent already recorded in `accepted`.
    /// The command is exactly the manifest's argument list, started without a shell.
    ///
    /// # Errors
    /// [`Error::NotAccepted`], [`Error::WrongRoute`], [`Error::CommandFailed`], [`Error::Io`].
    pub fn run_package(
        &self,
        offer: &Offer<'_>,
        accepted: &Acceptances,
        runner: &dyn CommandRunner,
    ) -> Result<Installed> {
        if !accepted.is_accepted(offer) {
            return Err(Error::NotAccepted(offer.item.name.clone()));
        }
        let Route::Package(cmd) = &offer.route else {
            return Err(Error::WrongRoute(offer.item.name.clone()));
        };
        let code = runner.run(&cmd.argv)?;
        if code != 0 {
            return Err(Error::CommandFailed(format!(
                "{} exited with {code}",
                cmd.display()
            )));
        }
        let record = Installed {
            item: offer.item.id.clone(),
            name: offer.item.name.clone(),
            version: offer.item.version.clone(),
            provider: offer.provider().to_string(),
            provider_url: offer.item.provider_url.clone(),
            licence_id: offer.licence.id.clone(),
            licence_name: offer.licence.name.clone(),
            licence_sha256: offer.licence.sha256.clone(),
            licence_source_url: offer.licence.source_url.clone(),
            attribution: offer.licence.attribution.clone(),
            show_attribution: offer.licence.show_attribution,
            installed_at: self.clock.now_unix(),
            route: "package".into(),
            command: Some(cmd.display()),
            files: Vec::new(),
        };
        let dir = self.dirs.item_dir(&offer.item.id);
        fs::create_dir_all(&dir)?;
        let json = serde_json::to_vec_pretty(&record).map_err(|e| Error::State(e.to_string()))?;
        fs::write(dir.join("installed.json"), json)?;
        Ok(record)
    }

    /// The installed items and their health. `deep` re-hashes every file; otherwise
    /// only presence and size are checked.
    ///
    /// # Errors
    /// [`Error::Io`] when the tools folder cannot be listed.
    pub fn list(&self, deep: bool) -> Result<Vec<Health>> {
        let mut out = Vec::new();
        let Ok(rd) = fs::read_dir(&self.dirs.tools) else {
            return Ok(out);
        };
        let mut names: Vec<String> = rd
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| is_ident(n))
            .collect();
        names.sort();
        for n in names {
            if let Some(h) = self.check(&n, deep)? {
                out.push(h);
            }
        }
        Ok(out)
    }

    /// The health of one installed item, or `None` when it is not installed.
    ///
    /// # Errors
    /// [`Error::State`] when the record exists but cannot be read.
    pub fn check(&self, item_id: &str, deep: bool) -> Result<Option<Health>> {
        if !is_ident(item_id) {
            return Ok(None);
        }
        let dir = self.dirs.item_dir(item_id);
        let Ok(bytes) = fs::read(dir.join("installed.json")) else {
            return Ok(None);
        };
        let installed: Installed = serde_json::from_slice(&bytes)
            .map_err(|e| Error::State(format!("installed.json of {item_id}: {e}")))?;
        let mut problems = Vec::new();
        for f in &installed.files {
            let p = dir.join(&f.name);
            match fs::metadata(&p) {
                Ok(m) if m.len() == f.size => {
                    if deep && hash_file(&p)? != f.sha256 {
                        problems.push(format!(
                            "{} has been changed since it was installed",
                            f.name
                        ));
                    }
                }
                Ok(_) => problems.push(format!(
                    "{} has been changed since it was installed",
                    f.name
                )),
                Err(_) => problems.push(format!("{} is missing", f.name)),
            }
        }
        Ok(Some(Health {
            installed,
            problems,
        }))
    }

    /// Remove an installed item. For a download this deletes its folder. For a package
    /// manager install only Helia's record is removed; system packages are left alone and
    /// the note says so.
    ///
    /// # Errors
    /// [`Error::NotInstalled`] or [`Error::Io`].
    pub fn remove(&self, item_id: &str) -> Result<Removal> {
        let health = self
            .check(item_id, false)?
            .ok_or_else(|| Error::NotInstalled(item_id.to_string()))?;
        fs::remove_dir_all(self.dirs.item_dir(item_id))?;
        let note = (health.installed.route == "package").then(|| {
            format!(
                "{} was installed by your package manager ({}). Helia has forgotten it, but the package is still on this computer; remove it with the same package manager if you want it gone.",
                health.installed.name,
                health.installed.command.clone().unwrap_or_default()
            )
        });
        Ok(Removal {
            installed: health.installed,
            note,
        })
    }

    /// Delete finished and partial downloads left in the staging folder.
    ///
    /// # Errors
    /// [`Error::Io`].
    pub fn clean_staging(&self) -> Result<()> {
        if self.dirs.staging.exists() {
            fs::remove_dir_all(&self.dirs.staging)?;
        }
        Ok(())
    }

    fn download_part(
        &self,
        d: &Download,
        part_no: usize,
        parts: usize,
        hooks: &mut Hooks<'_>,
    ) -> Result<PathBuf> {
        fs::create_dir_all(&self.dirs.staging)?;
        let name = staging_name(&d.url, &d.sha256);
        let done = self.dirs.staging.join(&name);
        let part = self.dirs.staging.join(format!("{name}.part"));
        let mut report = |phase, done: u64| {
            (hooks.progress)(&Progress {
                phase,
                part: part_no,
                parts,
                done,
                total: d.size,
            });
        };

        // A finished file from an earlier run.
        if let Ok(m) = fs::metadata(&done) {
            if m.len() == d.size && hash_file(&done)? == d.sha256 {
                return Ok(done);
            }
            fs::remove_file(&done)?;
        }
        let mut have = fs::metadata(&part).map_or(0, |m| m.len());
        if have > d.size {
            fs::remove_file(&part)?;
            have = 0;
        }
        if have < d.size {
            let resp = self.http.get(&Request {
                url: &d.url,
                range_from: (have > 0).then_some(have),
                allowed_hosts: &self.source.asset_hosts,
                bearer: None,
                bearer_host: None,
            })?;
            match resp.status {
                206 if resp.content_range_start == Some(have) => {}
                // The server ignored Range (or this is the first request): from the top.
                200 => have = 0,
                206 => {
                    return Err(helia_update::Error::Http(
                        "the server resumed at the wrong place".into(),
                    )
                    .into());
                }
                416 => {
                    fs::remove_file(&part)?;
                    return Err(helia_update::Error::HttpStatus(416).into());
                }
                s => return Err(helia_update::Error::HttpStatus(s).into()),
            }
            let mut out = if have == 0 {
                File::create(&part)?
            } else {
                OpenOptions::new().append(true).open(&part)?
            };
            let mut body = resp.body.take(d.size - have + 1);
            let mut buf = vec![0u8; 64 * 1024];
            // A read error is a dropped connection: keep what arrived, let the caller resume.
            while let Ok(n) = body.read(&mut buf) {
                if n == 0 {
                    break;
                }
                have += n as u64;
                if have > d.size {
                    drop(out);
                    fs::remove_file(&part)?;
                    return Err(helia_update::Error::TooMuchData.into());
                }
                out.write_all(&buf[..n])?;
                report(Phase::Downloading, have);
                if (hooks.cancelled)() {
                    out.flush()?;
                    return Err(Error::Cancelled);
                }
            }
            out.flush()?;
            drop(out);
            if have < d.size {
                return Err(helia_update::Error::Truncated {
                    got: have,
                    expected: d.size,
                }
                .into());
            }
        }

        report(Phase::Verifying, d.size);
        if hash_file(&part)? != d.sha256 {
            fs::remove_file(&part)?;
            return Err(helia_update::Error::HashMismatch.into());
        }
        fs::rename(&part, &done)?;
        Ok(done)
    }
}

/// The text of `SOURCE.txt`: where the files came from, so anyone can find the provider's
/// source and licence.
fn source_note(offer: &Offer<'_>, parts: &[&Download]) -> String {
    use std::fmt::Write as _;
    let mut s = format!(
        "{} {}\nProvided by: {}\n",
        offer.item.name,
        offer.item.version,
        offer.provider()
    );
    if let Some(u) = &offer.item.provider_url {
        let _ = writeln!(s, "Provider page: {u}");
    }
    let _ = writeln!(
        s,
        "Licence: {} (the text is in LICENCE.txt)\nLicence and source: {}",
        offer.licence.name, offer.licence.source_url
    );
    for d in parts {
        let _ = writeln!(s, "Downloaded from: {}\nsha256: {}", d.url, d.sha256);
    }
    s.push_str("This was downloaded at the user's request. It is not part of the application and is not distributed with it.\n");
    s
}
