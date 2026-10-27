// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The three things the updater needs from outside: a clock, a file system and
//! HTTP. Injected so tests need no network, disk or wall clock.

use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::Result;

/// Wall-clock time.
pub trait Clock {
    /// Seconds since the Unix epoch.
    fn now_unix(&self) -> u64;
}

/// The system clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }
}

/// The file operations the updater uses.
/// Errors are plain `std::io::Error`s from the operation.
pub trait Fs {
    /// Read a whole (small) file.
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    /// Write a whole file atomically (temporary file in the same directory, then rename).
    fn write_atomic(&self, path: &Path, data: &[u8]) -> io::Result<()>;
    /// `mkdir -p`.
    fn create_dir_all(&self, path: &Path) -> io::Result<()>;
    /// Size of the file, or `None` when it does not exist.
    fn len(&self, path: &Path) -> io::Result<Option<u64>>;
    /// Open for streaming reads.
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read>>;
    /// Open for appending, creating the file when absent.
    fn open_append(&self, path: &Path) -> io::Result<Box<dyn Write>>;
    /// Atomic rename, replacing the target.
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Remove a file; a missing file is not an error.
    fn remove_file(&self, path: &Path) -> io::Result<()>;
    /// [`Self::write_atomic`] for a secret: readable by the owner only where the
    /// platform has file modes (0600). The default is `write_atomic`.
    fn write_private(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        self.write_atomic(path, data)
    }
}

/// The real file system.
#[derive(Clone, Copy, Debug, Default)]
pub struct StdFs;

impl Fs for StdFs {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        fs::read(path)
    }

    fn write_atomic(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = std::path::PathBuf::from(tmp);
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(data)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, path)
    }

    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        fs::create_dir_all(path)
    }

    fn len(&self, path: &Path) -> io::Result<Option<u64>> {
        match fs::metadata(path) {
            Ok(m) => Ok(Some(m.len())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read>> {
        Ok(Box::new(fs::File::open(path)?))
    }

    fn open_append(&self, path: &Path) -> io::Result<Box<dyn Write>> {
        Ok(Box::new(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?,
        ))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }

    fn write_private(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = std::path::PathBuf::from(tmp);
        {
            let mut o = fs::OpenOptions::new();
            o.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                o.mode(0o600);
            }
            let mut f = o.open(&tmp)?;
            f.write_all(data)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, path)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        match fs::remove_file(path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

/// One `GET`.
#[derive(Clone, Copy)]
pub struct Request<'a> {
    /// The URL.
    pub url: &'a str,
    /// Ask for `Range: bytes=<n>-`.
    pub range_from: Option<u64>,
    /// Hosts the request and every redirect hop may go to. An implementation must
    /// refuse a redirect to any other host.
    pub allowed_hosts: &'a [String],
    /// A bearer token to send as `Authorization: Bearer ...`, only on the hop whose
    /// host is `bearer_host`. `None` sends no `Authorization` header at all.
    pub bearer: Option<&'a str>,
    /// The one host the token may be sent to. A redirect hop to any other host
    /// carries no token. A value with a port (tests) must match host and port.
    pub bearer_host: Option<&'a str>,
}

impl std::fmt::Debug for Request<'_> {
    // The token is never printed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("url", &self.url)
            .field("range_from", &self.range_from)
            .field("allowed_hosts", &self.allowed_hosts)
            .field("bearer", &self.bearer.map(|_| "<redacted>"))
            .field("bearer_host", &self.bearer_host)
            .finish()
    }
}

/// The answer to a [`Request`].
pub struct Response {
    /// HTTP status.
    pub status: u16,
    /// The first byte of the body within the file, from `Content-Range` (206 only).
    pub content_range_start: Option<u64>,
    /// The body.
    pub body: Box<dyn Read>,
}

/// An HTTP `GET` client. The real one is `ureq_http::UreqHttp`.
/// Errors are [`crate::Error`]: refused address, network failure.
pub trait Http {
    /// Fetch `req`. A non-2xx status is returned as a [`Response`], not an error.
    fn get(&self, req: &Request<'_>) -> Result<Response>;
}
