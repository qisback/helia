// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The external-item manifest (schema 1) and its strict parser.
//!
//! The wire form ([`ManifestDoc`]) is what the release tooling writes and what the
//! signature covers. [`Manifest::parse`] turns bytes into a validated
//! [`Manifest`]; it does not check the signature or the policy (see
//! [`crate::client`]): verify first, then parse.
//!
//! One manifest serves one app. It lists licences (full text inline) and items
//! (ffmpeg, a data pack). Each item has per-platform downloads (https URL, sha256,
//! size, how to unpack, which files to keep) and optionally package manager
//! commands. Every download and command names the licence that must be accepted
//! before it is used.
//!
//! Limits: 512 KiB, nesting depth 8, 32 items, 16 downloads and 16 commands per
//! item, 128 KiB per licence text, https-only URLs (plain `http` only to
//! `127.0.0.1` when the caller allows it, for tests), unknown fields ignored,
//! downloads for an unknown platform or archive kind skipped.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use helia_update::Platform;
use helia_update::manifest::check_url;
use helia_update::time;

use crate::error::{Error, Result};

/// Largest manifest accepted, in bytes.
pub const MAX_MANIFEST_BYTES: usize = 512 * 1024;
/// Largest licence text accepted, in bytes.
pub const MAX_LICENCE_BYTES: usize = 128 * 1024;
/// Deepest JSON nesting accepted.
pub const MAX_DEPTH: usize = 8;
/// The newest schema this crate reads.
pub const SUPPORTED_SCHEMA: u32 = 1;
/// Most items accepted in one manifest.
pub const MAX_ITEMS: usize = 32;
/// Most downloads, and most commands, accepted per item.
pub const MAX_PER_ITEM: usize = 16;
/// Largest download accepted, in bytes (4 GiB).
pub const MAX_DOWNLOAD_BYTES: u64 = 4 << 30;
/// Programs a package manager command may start with. `sudo` must be followed by another entry.
pub const COMMAND_PROGRAMS: &[&str] =
    &["sudo", "apt", "apt-get", "dnf", "pacman", "zypper", "brew"];

/// How a download is packed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ArchiveKind {
    /// The file is the program (or data file) itself.
    None,
    /// A zip archive.
    Zip,
    /// A tar archive compressed with xz.
    TarXz,
}

impl ArchiveKind {
    /// The manifest spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Zip => "zip",
            Self::TarXz => "tar.xz",
        }
    }

    /// Parse the manifest spelling; `None` for an unknown kind.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "none" => Self::None,
            "zip" => Self::Zip,
            "tar.xz" => Self::TarXz,
            _ => return None,
        })
    }
}

/// A licence the user must read and accept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Licence {
    /// Id the items refer to.
    pub id: String,
    /// Human name, for example `GNU Lesser General Public License, version 3`.
    pub name: String,
    /// SPDX identifier, where one exists.
    pub spdx: Option<String>,
    /// The full text, shown to the user unchanged.
    pub text: String,
    /// Lower-case hex sha256 of `text` (computed here, not taken from the file).
    pub sha256: String,
    /// Where the provider publishes the licence and the source.
    pub source_url: String,
    /// Attribution the licence requires to be shown, if any.
    pub attribution: Option<String>,
    /// Show the attribution after install (About, status).
    pub show_attribution: bool,
}

/// One file to keep from a download.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    /// Path of the file inside the archive, matched as a whole-component suffix
    /// (`bin/ffmpeg.exe` matches `ffmpeg-n9/bin/ffmpeg.exe`). Ignored for [`ArchiveKind::None`].
    pub find: String,
    /// Name to save it as in the item folder.
    pub save_as: String,
    /// Logical name the app looks it up by (`ffmpeg`), if it is a program or file the app uses.
    pub provides: Option<String>,
    /// Mark the saved file executable (a program, not data).
    pub executable: bool,
}

/// One downloadable file, validated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Download {
    /// Platform it is built for.
    pub platform: Platform,
    /// Where to fetch it (https).
    pub url: String,
    /// Lower-case hex sha256 of the file.
    pub sha256: String,
    /// Size in bytes.
    pub size: u64,
    /// How it is packed.
    pub archive: ArchiveKind,
    /// Files to keep.
    pub members: Vec<Member>,
    /// Id of the licence to accept first.
    pub licence: String,
    /// Who provides this build, when it is not the item's provider.
    pub provider: Option<String>,
}

/// A package manager command the user may allow the app to run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageCommand {
    /// Platform it is for (`linux-x86_64`, ...).
    pub platform: Platform,
    /// The package manager program, for example `apt`.
    pub manager: String,
    /// The full command as an argument list, never run through a shell.
    pub argv: Vec<String>,
    /// Id of the licence to accept first.
    pub licence: String,
    /// Who provides the package, when it is not the item's provider.
    pub provider: Option<String>,
}

impl PackageCommand {
    /// The command as one line for display (arguments are plain words by construction).
    #[must_use]
    pub fn display(&self) -> String {
        self.argv.join(" ")
    }
}

/// One external item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// Id, for example `ffmpeg`.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Version of what the downloads contain.
    pub version: String,
    /// Who provides it (not Helia).
    pub provider: String,
    /// The provider's page, if stated.
    pub provider_url: Option<String>,
    /// What it is for, in plain words.
    pub description: String,
    /// The app cannot work without it (else optional).
    pub required: bool,
    /// Downloads, all platforms.
    pub downloads: Vec<Download>,
    /// Package manager commands, all platforms.
    pub packages: Vec<PackageCommand>,
}

/// A validated manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    /// Schema number.
    pub schema: u32,
    /// App name, for example `helia`.
    pub app: String,
    /// When the manifest was made, seconds since the Unix epoch.
    pub date: u64,
    /// Monotonic counter: anti-rollback and anti-replay.
    pub sequence: u64,
    /// When the manifest stops being trusted, seconds since the Unix epoch.
    pub expires: u64,
    /// Licences.
    pub licences: Vec<Licence>,
    /// Items.
    pub items: Vec<Item>,
    /// Downloads and commands skipped for an unknown platform or kind.
    pub skipped: usize,
}

/// The way an item can be had on this system, with the licence that goes with it.
#[derive(Clone, Debug)]
pub struct Offer<'m> {
    /// The item.
    pub item: &'m Item,
    /// How.
    pub route: Route<'m>,
    /// The licence to show and accept first.
    pub licence: &'m Licence,
}

/// How an [`Offer`] is carried out.
#[derive(Clone, Debug)]
pub enum Route<'m> {
    /// Download these files (one or more parts) from the provider.
    Download(Vec<&'m Download>),
    /// Run this package manager command, with the user's consent.
    Package(&'m PackageCommand),
}

impl Offer<'_> {
    /// Who provides what this offer fetches: the download's or command's own provider, else the item's.
    #[must_use]
    pub fn provider(&self) -> &str {
        let own = match &self.route {
            Route::Download(d) => d.first().and_then(|d| d.provider.as_deref()),
            Route::Package(p) => p.provider.as_deref(),
        };
        own.unwrap_or(&self.item.provider)
    }

    /// Bytes to download (zero for a package manager route).
    #[must_use]
    pub fn download_bytes(&self) -> u64 {
        match &self.route {
            Route::Download(d) => d.iter().map(|d| d.size).sum(),
            Route::Package(_) => 0,
        }
    }
}

// ---- wire form -----------------------------------------------------------------

/// Wire form of a licence.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LicenceDoc {
    /// Id.
    pub id: String,
    /// Name.
    pub name: String,
    /// SPDX id.
    #[serde(default)]
    pub spdx: Option<String>,
    /// Full text.
    pub text: String,
    /// Source link.
    pub source_url: String,
    /// Attribution text.
    #[serde(default)]
    pub attribution: Option<String>,
    /// Show the attribution after install.
    #[serde(default)]
    pub show_attribution: bool,
}

/// Wire form of a member.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MemberDoc {
    /// Path inside the archive.
    #[serde(default)]
    pub find: String,
    /// Saved name.
    pub save_as: String,
    /// Logical name.
    #[serde(default)]
    pub provides: Option<String>,
    /// Mark the saved file executable.
    #[serde(default)]
    pub executable: bool,
}

/// Wire form of a download.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DownloadDoc {
    /// Platform spelling.
    pub platform: String,
    /// URL.
    #[serde(default)]
    pub url: String,
    /// Hex sha256.
    #[serde(default)]
    pub sha256: String,
    /// Size in bytes.
    #[serde(default)]
    pub size: u64,
    /// Archive spelling: `none`, `zip` or `tar.xz`.
    #[serde(default)]
    pub archive: String,
    /// Files to keep.
    #[serde(default)]
    pub members: Vec<MemberDoc>,
    /// Licence id; the item's own when absent.
    #[serde(default)]
    pub licence: Option<String>,
    /// Provider of this build; the item's own when absent.
    #[serde(default)]
    pub provider: Option<String>,
}

/// Wire form of a package manager command.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PackageDoc {
    /// Platform spelling.
    pub platform: String,
    /// Package manager program.
    pub manager: String,
    /// The command.
    pub argv: Vec<String>,
    /// Licence id; the item's own when absent.
    #[serde(default)]
    pub licence: Option<String>,
    /// Provider of the package; the item's own when absent.
    #[serde(default)]
    pub provider: Option<String>,
}

/// Wire form of an item.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ItemDoc {
    /// Id.
    pub id: String,
    /// Name.
    pub name: String,
    /// Version.
    pub version: String,
    /// Provider.
    pub provider: String,
    /// Provider page.
    #[serde(default)]
    pub provider_url: Option<String>,
    /// Description.
    #[serde(default)]
    pub description: String,
    /// Required (else optional).
    #[serde(default)]
    pub required: bool,
    /// Default licence id.
    pub licence: String,
    /// Downloads.
    #[serde(default)]
    pub downloads: Vec<DownloadDoc>,
    /// Package commands.
    #[serde(default)]
    pub packages: Vec<PackageDoc>,
}

/// Wire form of the manifest.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ManifestDoc {
    /// Schema number.
    pub schema: u32,
    /// App name.
    pub app: String,
    /// `YYYY-MM-DDTHH:MM:SSZ`.
    pub date: String,
    /// Sequence.
    pub sequence: u64,
    /// `YYYY-MM-DDTHH:MM:SSZ`.
    pub expires: String,
    /// Licences.
    #[serde(default)]
    pub licences: Vec<LicenceDoc>,
    /// Items.
    #[serde(default)]
    pub items: Vec<ItemDoc>,
}

// ---- validation ----------------------------------------------------------------

fn bad(msg: impl Into<String>) -> Error {
    Error::Malformed(msg.into())
}

/// `[a-z0-9._-]`, 1 to 64 characters: ids and the app name.
pub(crate) fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-')
        })
}

/// A plain file name: `[A-Za-z0-9._-]`, 1 to 100 characters, no leading dot.
fn is_file_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && !s.starts_with('.')
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

/// A relative archive path: components of `[A-Za-z0-9._+-]`, no `..`, no leading `/`.
fn is_archive_path(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && s.split('/').all(|c| {
            !c.is_empty()
                && c != ".."
                && c != "."
                && c.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
        })
}

fn is_sha256(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn printable(s: &str, max: usize) -> bool {
    !s.trim().is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}

/// Reject input over the size limit or nested deeper than [`MAX_DEPTH`].
///
/// # Errors
/// [`Error::TooLarge`] or [`Error::TooDeep`].
pub fn check_shape(bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(Error::TooLarge {
            size: bytes.len(),
            max: MAX_MANIFEST_BYTES,
        });
    }
    let (mut depth, mut in_str, mut esc) = (0usize, false, false);
    for &c in bytes {
        if in_str {
            match (esc, c) {
                (true, _) => esc = false,
                (false, b'\\') => esc = true,
                (false, b'"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > MAX_DEPTH {
                    return Err(Error::TooDeep);
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

/// Check a package manager argument list: an allow-listed program (and, after
/// `sudo`, an allow-listed package manager), and plain words only.
fn check_command(manager: &str, argv: &[String]) -> Result<()> {
    let cmd_bad = |m: &str| Error::BadCommand(m.to_string());
    let program = argv.first().ok_or_else(|| cmd_bad("empty"))?;
    if argv.len() < 3 || argv.len() > 16 {
        return Err(cmd_bad("a command has 3 to 16 words"));
    }
    if !COMMAND_PROGRAMS.contains(&program.as_str()) {
        return Err(cmd_bad("the program is not a known package manager"));
    }
    let real = if program == "sudo" { &argv[1] } else { program };
    if real == "sudo" || !COMMAND_PROGRAMS.contains(&real.as_str()) {
        return Err(cmd_bad("sudo must be followed by a known package manager"));
    }
    if real != manager {
        return Err(cmd_bad("the manager does not match the command"));
    }
    for a in argv {
        if a.is_empty()
            || a.len() > 64
            || !a.bytes().all(|c| {
                c.is_ascii_alphanumeric()
                    || matches!(c, b'-' | b'_' | b'.' | b'+' | b':' | b'=' | b'@')
            })
        {
            return Err(cmd_bad(
                "a word in the command has characters that are not allowed",
            ));
        }
    }
    Ok(())
}

impl Manifest {
    /// Parse and validate manifest bytes. Does not check the signature or the policy.
    ///
    /// # Errors
    /// [`Error::TooLarge`], [`Error::TooDeep`], [`Error::Malformed`], [`Error::SchemaTooNew`],
    /// [`Error::BadCommand`] or [`Error::Transport`] for an insecure address.
    pub fn parse(bytes: &[u8], allow_loopback_http: bool) -> Result<Self> {
        check_shape(bytes)?;
        let doc: ManifestDoc = serde_json::from_slice(bytes).map_err(|e| bad(e.to_string()))?;
        Self::from_doc(doc, allow_loopback_http)
    }

    /// Validate a wire document.
    ///
    /// # Errors
    /// As [`Self::parse`].
    #[allow(clippy::too_many_lines)]
    pub fn from_doc(doc: ManifestDoc, allow_loopback_http: bool) -> Result<Self> {
        if doc.schema > SUPPORTED_SCHEMA {
            return Err(Error::SchemaTooNew(doc.schema));
        }
        if doc.schema == 0 {
            return Err(bad("schema must be 1"));
        }
        if !is_ident(&doc.app) {
            return Err(bad("app must be 1 to 64 characters of a-z 0-9 . _ -"));
        }
        let date =
            time::parse_utc(&doc.date).ok_or_else(|| bad("date is not YYYY-MM-DDTHH:MM:SSZ"))?;
        let expires = time::parse_utc(&doc.expires)
            .ok_or_else(|| bad("expires is not YYYY-MM-DDTHH:MM:SSZ"))?;
        if doc.items.len() > MAX_ITEMS || doc.licences.len() > MAX_ITEMS * 2 {
            return Err(bad("too many items or licences"));
        }

        let mut licences = Vec::new();
        for l in doc.licences {
            if !is_ident(&l.id) || licences.iter().any(|x: &Licence| x.id == l.id) {
                return Err(bad(format!(
                    "licence id {:?} is not valid or is repeated",
                    l.id
                )));
            }
            if !printable(&l.name, 200) {
                return Err(bad("a licence has no usable name"));
            }
            if l.text.trim().is_empty() || l.text.len() > MAX_LICENCE_BYTES {
                return Err(bad(format!("licence {} has no text or too much", l.id)));
            }
            check_url(&l.source_url, allow_loopback_http)?;
            if l.attribution.as_deref().is_some_and(|a| a.len() > 2000) {
                return Err(bad("an attribution is longer than 2000 characters"));
            }
            let sha256 = hex(&Sha256::digest(l.text.as_bytes()));
            licences.push(Licence {
                id: l.id,
                name: l.name,
                spdx: l.spdx.filter(|s| printable(s, 64)),
                text: l.text,
                sha256,
                source_url: l.source_url,
                attribution: l.attribution,
                show_attribution: l.show_attribution,
            });
        }
        let have_licence = |id: &str| licences.iter().any(|l| l.id == id);

        let mut skipped = 0usize;
        let mut items: Vec<Item> = Vec::new();
        for it in doc.items {
            if !is_ident(&it.id) || items.iter().any(|x| x.id == it.id) {
                return Err(bad(format!(
                    "item id {:?} is not valid or is repeated",
                    it.id
                )));
            }
            if !printable(&it.name, 100)
                || !printable(&it.version, 64)
                || !printable(&it.provider, 100)
                || it.description.len() > 2000
            {
                return Err(bad(format!(
                    "item {} has a name, version or provider that is not usable",
                    it.id
                )));
            }
            if let Some(u) = &it.provider_url {
                check_url(u, allow_loopback_http)?;
            }
            if !have_licence(&it.licence) {
                return Err(bad(format!("item {} names an unknown licence", it.id)));
            }
            if it.downloads.len() > MAX_PER_ITEM || it.packages.len() > MAX_PER_ITEM {
                return Err(bad(format!(
                    "item {} has too many downloads or commands",
                    it.id
                )));
            }
            let mut downloads = Vec::new();
            for d in it.downloads {
                let (Some(platform), Some(archive)) =
                    (Platform::parse(&d.platform), ArchiveKind::parse(&d.archive))
                else {
                    skipped += 1;
                    continue;
                };
                check_url(&d.url, allow_loopback_http)?;
                if !is_sha256(&d.sha256) {
                    return Err(bad(format!(
                        "item {}: sha256 must be 64 lower-case hex digits",
                        it.id
                    )));
                }
                if d.size == 0 || d.size > MAX_DOWNLOAD_BYTES {
                    return Err(bad(format!("item {}: size must be 1 byte to 4 GiB", it.id)));
                }
                if d.members.is_empty()
                    || d.members.len() > 16
                    || (archive == ArchiveKind::None && d.members.len() != 1)
                {
                    return Err(bad(format!(
                        "item {}: a download lists 1 to 16 files to keep",
                        it.id
                    )));
                }
                let mut members = Vec::new();
                for m in d.members {
                    if !is_file_name(&m.save_as)
                        || (archive != ArchiveKind::None && !is_archive_path(&m.find))
                        || m.provides.as_deref().is_some_and(|p| !is_ident(p))
                        || members.iter().any(|x: &Member| x.save_as == m.save_as)
                    {
                        return Err(bad(format!(
                            "item {}: a file to keep has a name or path that is not allowed",
                            it.id
                        )));
                    }
                    members.push(Member {
                        find: m.find,
                        save_as: m.save_as,
                        provides: m.provides,
                        executable: m.executable,
                    });
                }
                let licence = d.licence.unwrap_or_else(|| it.licence.clone());
                if !have_licence(&licence) {
                    return Err(bad(format!("item {} names an unknown licence", it.id)));
                }
                if d.provider.as_deref().is_some_and(|p| !printable(p, 100)) {
                    return Err(bad(format!(
                        "item {}: a download has a provider name that is not usable",
                        it.id
                    )));
                }
                downloads.push(Download {
                    platform,
                    url: d.url,
                    sha256: d.sha256,
                    size: d.size,
                    archive,
                    members,
                    licence,
                    provider: d.provider,
                });
            }
            // The parts of one platform share a licence, so one acceptance covers them all.
            for p in downloads.iter().map(|d| d.platform) {
                let mut l = downloads
                    .iter()
                    .filter(|d| d.platform == p)
                    .map(|d| &d.licence);
                if let Some(first) = l.next()
                    && l.any(|x| x != first)
                {
                    return Err(bad(format!(
                        "item {}: the parts for one platform must share a licence",
                        it.id
                    )));
                }
            }
            let mut packages = Vec::new();
            for p in it.packages {
                let Some(platform) = Platform::parse(&p.platform) else {
                    skipped += 1;
                    continue;
                };
                check_command(&p.manager, &p.argv)?;
                let licence = p.licence.unwrap_or_else(|| it.licence.clone());
                if !have_licence(&licence) {
                    return Err(bad(format!("item {} names an unknown licence", it.id)));
                }
                if p.provider.as_deref().is_some_and(|p| !printable(p, 100)) {
                    return Err(bad(format!(
                        "item {}: a command has a provider name that is not usable",
                        it.id
                    )));
                }
                packages.push(PackageCommand {
                    platform,
                    manager: p.manager,
                    argv: p.argv,
                    licence,
                    provider: p.provider,
                });
            }
            items.push(Item {
                id: it.id,
                name: it.name,
                version: it.version,
                provider: it.provider,
                provider_url: it.provider_url,
                description: it.description,
                required: it.required,
                downloads,
                packages,
            });
        }
        Ok(Self {
            schema: doc.schema,
            app: doc.app,
            date,
            sequence: doc.sequence,
            expires,
            licences,
            items,
            skipped,
        })
    }

    /// The item with this id.
    #[must_use]
    pub fn item(&self, id: &str) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    /// The licence with this id.
    #[must_use]
    pub fn licence(&self, id: &str) -> Option<&Licence> {
        self.licences.iter().find(|l| l.id == id)
    }

    /// Ways to get `item_id` on `platform`: package manager commands first (the app
    /// decides which are usable on this machine), then the provider download.
    ///
    /// # Errors
    /// [`Error::UnknownItem`] or [`Error::NotOffered`] when there is no way at all.
    pub fn offers(&self, item_id: &str, platform: Option<Platform>) -> Result<Vec<Offer<'_>>> {
        let item = self
            .item(item_id)
            .ok_or_else(|| Error::UnknownItem(item_id.to_string()))?;
        let not_offered = || Error::NotOffered {
            item: item.id.clone(),
            platform: platform.map_or("unknown", Platform::as_str).to_string(),
        };
        let platform = platform.ok_or_else(not_offered)?;
        let mut out = Vec::new();
        for p in item.packages.iter().filter(|p| p.platform == platform) {
            if let Some(licence) = self.licence(&p.licence) {
                out.push(Offer {
                    item,
                    route: Route::Package(p),
                    licence,
                });
            }
        }
        let parts: Vec<&Download> = item
            .downloads
            .iter()
            .filter(|d| d.platform == platform)
            .collect();
        if let Some(first) = parts.first()
            && let Some(licence) = self.licence(&first.licence)
        {
            out.push(Offer {
                item,
                route: Route::Download(parts),
                licence,
            });
        }
        if out.is_empty() {
            return Err(not_offered());
        }
        Ok(out)
    }

    /// Policy: the manifest is for `app`, has not expired at `now`, and is not older
    /// than `highest_seen` (the highest sequence this client has accepted).
    ///
    /// # Errors
    /// [`Error::WrongApp`], [`Error::Expired`] or [`Error::Replay`].
    pub fn check_policy(&self, app: &str, now: u64, highest_seen: u64) -> Result<()> {
        if self.app != app {
            return Err(Error::WrongApp {
                expected: app.to_string(),
                found: self.app.clone(),
            });
        }
        if now >= self.expires {
            return Err(Error::Expired {
                expires: self.expires,
                now,
            });
        }
        if self.sequence < highest_seen {
            return Err(Error::Replay {
                seen: self.sequence,
                highest: highest_seen,
            });
        }
        Ok(())
    }
}

/// Lower-case hex.
pub(crate) fn hex(bytes: &[u8]) -> String {
    helia_update::verify::hex(bytes)
}

/// A small valid manifest and its bytes, for the unit tests of this crate.
#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;

    const LGPL: &str = "GNU LESSER GENERAL PUBLIC LICENSE\nVersion 3\n";

    pub(crate) fn doc() -> ManifestDoc {
        ManifestDoc {
            schema: 1,
            app: "helia".into(),
            date: "2026-10-26T12:00:00Z".into(),
            sequence: 5,
            expires: "2027-01-26T12:00:00Z".into(),
            licences: vec![LicenceDoc {
                id: "lgpl-3.0".into(),
                name: "GNU Lesser General Public License, version 3".into(),
                spdx: Some("LGPL-3.0-or-later".into()),
                text: LGPL.into(),
                source_url: "https://www.gnu.org/licenses/lgpl-3.0.html".into(),
                attribution: Some("FFmpeg is a trademark of Fabrice Bellard.".into()),
                show_attribution: true,
            }],
            items: vec![ItemDoc {
                id: "ffmpeg".into(),
                name: "FFmpeg".into(),
                version: "9.0.2".into(),
                provider: "BtbN FFmpeg-Builds".into(),
                provider_url: Some("https://github.com/BtbN/FFmpeg-Builds".into()),
                description: "Reads MP4 and MOV video.".into(),
                required: false,
                licence: "lgpl-3.0".into(),
                downloads: vec![DownloadDoc {
                    platform: "linux-x86_64".into(),
                    url: "https://github.com/BtbN/FFmpeg-Builds/releases/download/x/ffmpeg.tar.xz"
                        .into(),
                    sha256: "a".repeat(64),
                    size: 1000,
                    archive: "tar.xz".into(),
                    members: vec![
                        MemberDoc {
                            find: "bin/ffmpeg".into(),
                            save_as: "ffmpeg".into(),
                            provides: Some("ffmpeg".into()),
                            executable: true,
                        },
                        MemberDoc {
                            find: "bin/ffprobe".into(),
                            save_as: "ffprobe".into(),
                            provides: Some("ffprobe".into()),
                            executable: true,
                        },
                    ],
                    licence: None,
                    provider: Some("BtbN".into()),
                }],
                packages: vec![PackageDoc {
                    platform: "linux-x86_64".into(),
                    manager: "apt".into(),
                    argv: ["sudo", "apt", "install", "ffmpeg"]
                        .map(String::from)
                        .to_vec(),
                    licence: None,
                    provider: None,
                }],
            }],
        }
    }

    pub(crate) fn bytes(d: &ManifestDoc) -> Vec<u8> {
        serde_json::to_vec_pretty(d).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::{bytes, doc};
    use super::*;

    #[test]
    fn a_good_manifest_parses_and_offers_package_first() {
        let m = Manifest::parse(&bytes(&doc()), false).unwrap();
        assert_eq!(m.items[0].version, "9.0.2");
        assert_eq!(m.licences[0].sha256.len(), 64);
        let offers = m.offers("ffmpeg", Some(Platform::LinuxX86_64)).unwrap();
        assert!(matches!(offers[0].route, Route::Package(_)));
        assert!(matches!(offers[1].route, Route::Download(_)));
        assert_eq!(offers[1].download_bytes(), 1000);
        // A download names its own provider; a command falls back to the item's.
        assert_eq!(offers[1].provider(), "BtbN");
        assert_eq!(offers[0].provider(), "BtbN FFmpeg-Builds");
        assert!(matches!(
            m.offers("ffmpeg", Some(Platform::WindowsX86_64)),
            Err(Error::NotOffered { .. })
        ));
        assert!(matches!(
            m.offers("nope", Some(Platform::LinuxX86_64)),
            Err(Error::UnknownItem(_))
        ));
    }

    #[test]
    fn the_licence_hash_follows_the_text() {
        let a = Manifest::parse(&bytes(&doc()), false).unwrap();
        let mut d = doc();
        d.licences[0].text.push_str("Amended.\n");
        let b = Manifest::parse(&bytes(&d), false).unwrap();
        assert_ne!(a.licences[0].sha256, b.licences[0].sha256);
    }

    #[test]
    fn insecure_and_malformed_input_is_refused() {
        let mut d = doc();
        d.items[0].downloads[0].url = "http://example.com/x".into();
        assert!(matches!(
            Manifest::parse(&bytes(&d), false),
            Err(Error::Transport(_))
        ));
        let mut d = doc();
        d.items[0].downloads[0].sha256 = "ABC".into();
        assert!(matches!(
            Manifest::parse(&bytes(&d), false),
            Err(Error::Malformed(_))
        ));
        let mut d = doc();
        d.items[0].downloads[0].members[0].find = "../bin/ffmpeg".into();
        assert!(matches!(
            Manifest::parse(&bytes(&d), false),
            Err(Error::Malformed(_))
        ));
        let mut d = doc();
        d.items[0].downloads[0].members[0].save_as = "../ffmpeg".into();
        assert!(matches!(
            Manifest::parse(&bytes(&d), false),
            Err(Error::Malformed(_))
        ));
        let mut d = doc();
        d.items[0].licence = "missing".into();
        assert!(matches!(
            Manifest::parse(&bytes(&d), false),
            Err(Error::Malformed(_))
        ));
        let mut d = doc();
        d.schema = 2;
        assert!(matches!(
            Manifest::parse(&bytes(&d), false),
            Err(Error::SchemaTooNew(2))
        ));
        assert!(Manifest::parse(b"not json", false).is_err());
        assert!(matches!(
            Manifest::parse(&vec![b' '; MAX_MANIFEST_BYTES + 1], false),
            Err(Error::TooLarge { .. })
        ));
        assert!(matches!(
            Manifest::parse("[".repeat(20).as_bytes(), false),
            Err(Error::TooDeep)
        ));
    }

    #[test]
    fn package_commands_are_plain_words_from_known_managers() {
        for argv in [
            vec!["sh", "-c", "evil"],
            vec!["sudo", "sh", "-c"],
            vec!["sudo", "sudo", "apt"],
            vec!["apt", "install", "ffmpeg;rm"],
            vec!["apt", "install", "$(id)"],
            vec!["apt", "install"],
        ] {
            let mut d = doc();
            d.items[0].packages[0].argv = argv.iter().map(|s| (*s).to_string()).collect();
            d.items[0].packages[0].manager = argv[0].to_string();
            assert!(Manifest::parse(&bytes(&d), false).is_err(), "{argv:?}");
        }
        let mut d = doc();
        d.items[0].packages[0].manager = "dnf".into();
        assert!(matches!(
            Manifest::parse(&bytes(&d), false),
            Err(Error::BadCommand(_))
        ));
    }

    #[test]
    fn unknown_platforms_and_kinds_are_skipped() {
        let mut d = doc();
        d.items[0].downloads[0].platform = "plan9-mips".into();
        let mut e = d.items[0].downloads[0].clone();
        e.platform = "linux-x86_64".into();
        e.archive = "7z".into();
        d.items[0].downloads.push(e);
        let m = Manifest::parse(&bytes(&d), false).unwrap();
        assert_eq!(m.skipped, 2);
        assert_eq!(m.items[0].downloads.len(), 0);
    }

    #[test]
    fn policy_checks_app_expiry_and_replay() {
        let m = Manifest::parse(&bytes(&doc()), false).unwrap();
        m.check_policy("helia", m.date, 5).unwrap();
        assert!(matches!(
            m.check_policy("valerian", m.date, 0),
            Err(Error::WrongApp { .. })
        ));
        assert!(matches!(
            m.check_policy("helia", m.expires, 0),
            Err(Error::Expired { .. })
        ));
        assert!(matches!(
            m.check_policy("helia", m.date, 6),
            Err(Error::Replay { .. })
        ));
    }

    #[test]
    fn parts_of_one_platform_must_share_a_licence() {
        let mut d = doc();
        d.licences.push(LicenceDoc {
            id: "gpl-3.0".into(),
            name: "GPL".into(),
            text: "GPL text".into(),
            source_url: "https://www.gnu.org/licenses/gpl-3.0.html".into(),
            ..LicenceDoc::default()
        });
        let mut e = d.items[0].downloads[0].clone();
        e.licence = Some("gpl-3.0".into());
        d.items[0].downloads.push(e);
        assert!(matches!(
            Manifest::parse(&bytes(&d), false),
            Err(Error::Malformed(_))
        ));
    }
}
