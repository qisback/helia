// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Resumable, verified download into a staging directory.
//!
//! The file is written to `<name>.part`; a restart resumes with an HTTP `Range`
//! request from the size on disk. When the declared size is reached the sha256 is
//! checked and the part is renamed atomically to its final name. A server that
//! ignores `Range` (status 200) makes the download start again from zero. More
//! bytes than the manifest declared, or a wrong hash, discards the file.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::check::{Update, Updater};
use crate::error::{Error, Result};
use crate::manifest::{Asset, check_url};
use crate::source::UpdateSource;
use crate::traits::{Fs, Request};
use crate::verify::hex;

/// A downloaded and verified file, waiting to be applied.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Staged {
    /// The version it installs.
    pub version: String,
    /// The kind spelling (`nsis`, `appimage`, ...).
    pub kind: String,
    /// Where it is.
    pub path: PathBuf,
    /// Its sha256 (lower-case hex).
    pub sha256: String,
    /// Its size in bytes.
    pub size: u64,
}

/// A safe file name from the last URL segment: `[A-Za-z0-9._-]` only.
fn file_name(url: &str, sha256: &str) -> String {
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
        "update.bin"
    } else {
        clean
    };
    format!("{}-{clean}", &sha256[..8])
}

fn hash_file(fs: &dyn Fs, path: &Path) -> Result<String> {
    let mut r = fs.open_read(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

impl Updater<'_> {
    /// Download `asset` of `update` into `staging_dir`, resuming a partial file.
    /// `progress(done, total)` is called as bytes arrive. On success the file is
    /// verified (size and sha256) and renamed into place.
    ///
    /// # Errors
    /// [`Error::InsecureUrl`], [`Error::ForeignHost`], [`Error::HttpStatus`],
    /// [`Error::Truncated`] (the part is kept, call again to resume),
    /// [`Error::TooMuchData`], [`Error::HashMismatch`] (both discard the file),
    /// [`Error::Io`].
    pub fn download(
        &self,
        source: &UpdateSource,
        update: &Update,
        asset: &Asset,
        staging_dir: &Path,
        allow_loopback_http: bool,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<Staged> {
        check_url(&asset.url, allow_loopback_http)?;
        if !source.asset_host_allowed(&asset.url) {
            return Err(Error::ForeignHost(asset.url.chars().take(200).collect()));
        }
        let fs = self.fs;
        fs.create_dir_all(staging_dir)?;
        let name = file_name(&asset.url, &asset.sha256);
        let done = staging_dir.join(&name);
        let part = staging_dir.join(format!("{name}.part"));
        let staged = Staged {
            version: update.manifest.version.to_string(),
            kind: asset.kind.as_str().to_string(),
            path: done.clone(),
            sha256: asset.sha256.clone(),
            size: asset.size,
        };

        // A finished file from an earlier run.
        if let Some(n) = fs.len(&done)? {
            if n == asset.size && hash_file(fs, &done)? == asset.sha256 {
                return Ok(staged);
            }
            fs.remove_file(&done)?;
        }

        let mut have = fs.len(&part)?.unwrap_or(0);
        if have > asset.size {
            fs.remove_file(&part)?;
            have = 0;
        }
        if have < asset.size {
            let resp = self.http.get(&Request {
                url: &asset.url,
                range_from: (have > 0).then_some(have),
                allowed_hosts: &source.asset_hosts,
            })?;
            match resp.status {
                206 if resp.content_range_start == Some(have) => {}
                200 => {
                    // The server ignored Range (or this is the first request): from the top.
                    if have > 0 {
                        fs.remove_file(&part)?;
                        have = 0;
                    }
                }
                206 => return Err(Error::Http("the server resumed at the wrong place".into())),
                416 => {
                    fs.remove_file(&part)?;
                    return Err(Error::HttpStatus(416));
                }
                s => return Err(Error::HttpStatus(s)),
            }
            let mut out = fs.open_append(&part)?;
            let mut body = resp.body.take(asset.size - have + 1);
            let mut buf = vec![0u8; 64 * 1024];
            // A read error is a dropped connection: keep what arrived, let the caller resume.
            while let Ok(n) = body.read(&mut buf) {
                if n == 0 {
                    break;
                }
                have += n as u64;
                if have > asset.size {
                    drop(out);
                    fs.remove_file(&part)?;
                    return Err(Error::TooMuchData);
                }
                std::io::Write::write_all(&mut out, &buf[..n])?;
                progress(have, asset.size);
            }
            std::io::Write::flush(&mut out)?;
            drop(out);
            if have < asset.size {
                return Err(Error::Truncated {
                    got: have,
                    expected: asset.size,
                });
            }
        }

        if hash_file(fs, &part)? != asset.sha256 {
            fs.remove_file(&part)?;
            return Err(Error::HashMismatch);
        }
        fs.rename(&part, &done)?;
        Ok(staged)
    }
}
