// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! In-memory [`Clock`], [`Fs`] and [`Http`] for tests of this crate and of the
//! applications that use it.

use std::collections::BTreeMap;
use std::io::{self, Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::error::{Error, Result};
use crate::traits::{Clock, Fs, Http, Request, Response};

/// A clock you set by hand.
#[derive(Debug, Default)]
pub struct FixedClock(AtomicU64);

impl FixedClock {
    /// A clock at `t` seconds since the Unix epoch.
    #[must_use]
    pub fn new(t: u64) -> Self {
        Self(AtomicU64::new(t))
    }

    /// Move the clock.
    pub fn set(&self, t: u64) {
        self.0.store(t, Ordering::SeqCst);
    }
}

impl Clock for FixedClock {
    fn now_unix(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// A request seen by [`MemHttp`]: URL and `Range` start.
pub type Seen = (String, Option<u64>);

/// A request seen by [`MemHttp`]: URL, bearer token and bearer host it asked with.
pub type BearerSeen = (String, Option<String>, Option<String>);

type Files = Arc<Mutex<BTreeMap<PathBuf, Vec<u8>>>>;

/// A file system in memory.
#[derive(Clone, Debug, Default)]
pub struct MemFs {
    files: Files,
}

impl MemFs {
    /// An empty file system.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The bytes of a file, if present.
    #[must_use]
    pub fn get(&self, path: &Path) -> Option<Vec<u8>> {
        self.files.lock().ok()?.get(path).cloned()
    }

    /// Put a file in place.
    pub fn put(&self, path: &Path, data: &[u8]) {
        if let Ok(mut f) = self.files.lock() {
            f.insert(path.to_path_buf(), data.to_vec());
        }
    }

    /// Every path, sorted.
    #[must_use]
    pub fn paths(&self) -> Vec<PathBuf> {
        self.files
            .lock()
            .map(|f| f.keys().cloned().collect())
            .unwrap_or_default()
    }
}

struct Appender {
    files: Files,
    path: PathBuf,
}

impl Write for Appender {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.files
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?
            .entry(self.path.clone())
            .or_default()
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn not_found() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "no such file")
}

impl Fs for MemFs {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.get(path).ok_or_else(not_found)
    }

    fn write_atomic(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        self.put(path, data);
        Ok(())
    }

    fn create_dir_all(&self, _: &Path) -> io::Result<()> {
        Ok(())
    }

    fn len(&self, path: &Path) -> io::Result<Option<u64>> {
        Ok(self.get(path).map(|d| d.len() as u64))
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read>> {
        Ok(Box::new(Cursor::new(self.get(path).ok_or_else(not_found)?)))
    }

    fn open_append(&self, path: &Path) -> io::Result<Box<dyn Write>> {
        self.files
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?
            .entry(path.to_path_buf())
            .or_default();
        Ok(Box::new(Appender {
            files: self.files.clone(),
            path: path.to_path_buf(),
        }))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let mut f = self
            .files
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?;
        let d = f.remove(from).ok_or_else(not_found)?;
        f.insert(to.to_path_buf(), d);
        Ok(())
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.files
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?
            .remove(path);
        Ok(())
    }
}

/// How [`MemHttp`] serves one URL.
#[derive(Clone, Debug)]
pub struct MemRoute {
    /// The body.
    pub body: Vec<u8>,
    /// Ignore `Range` and always answer `200` with the whole body.
    pub ignore_range: bool,
    /// End the body after this many bytes (a dropped connection) on the next request only.
    pub cut_next_at: Option<usize>,
    /// Answer with this status instead of 200 or 206.
    pub status: Option<u16>,
}

/// A scripted HTTP server in memory.
#[derive(Clone, Debug, Default)]
pub struct MemHttp {
    routes: Arc<Mutex<BTreeMap<String, MemRoute>>>,
    log: Arc<Mutex<Vec<Seen>>>,
    redirects: Arc<Mutex<BTreeMap<String, String>>>,
    bearers: Arc<Mutex<Vec<BearerSeen>>>,
    fail: Arc<Mutex<std::collections::BTreeSet<String>>>,
}

impl MemHttp {
    /// No routes.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Serve `body` at `url`.
    pub fn serve(&self, url: &str, body: &[u8]) {
        if let Ok(mut r) = self.routes.lock() {
            r.insert(
                url.to_string(),
                MemRoute {
                    body: body.to_vec(),
                    ignore_range: false,
                    cut_next_at: None,
                    status: None,
                },
            );
        }
    }

    /// Answer `url` with `status` and `body` (for example a 403 with a JSON reason).
    pub fn serve_status(&self, url: &str, status: u16, body: &[u8]) {
        self.serve(url, body);
        self.tweak(url, |r| r.status = Some(status));
    }

    /// Make requests for `url` fail as if the network were down.
    pub fn fail(&self, url: &str) {
        if let Ok(mut f) = self.fail.lock() {
            f.insert(url.to_string());
        }
    }

    /// Let `url` work again.
    pub fn heal(&self, url: &str) {
        if let Ok(mut f) = self.fail.lock() {
            f.remove(url);
        }
    }

    /// The bearer token and bearer host each request carried, by URL.
    #[must_use]
    pub fn bearers(&self) -> Vec<BearerSeen> {
        self.bearers.lock().map(|l| l.clone()).unwrap_or_default()
    }

    /// Make `url` answer with a redirect to `target` (the body is then `target`'s).
    pub fn redirect(&self, url: &str, target: &str) {
        if let Ok(mut r) = self.redirects.lock() {
            r.insert(url.to_string(), target.to_string());
        }
    }

    /// Change how a served URL behaves.
    pub fn tweak(&self, url: &str, f: impl FnOnce(&mut MemRoute)) {
        if let Some(r) = self
            .routes
            .lock()
            .ok()
            .as_mut()
            .and_then(|r| r.get_mut(url))
        {
            f(r);
        }
    }

    /// Requests seen so far: URL and `Range` start.
    #[must_use]
    pub fn requests(&self) -> Vec<Seen> {
        self.log.lock().map(|l| l.clone()).unwrap_or_default()
    }
}

impl Http for MemHttp {
    fn get(&self, req: &Request<'_>) -> Result<Response> {
        let poisoned = || Error::Http("poisoned".into());
        self.log
            .lock()
            .map_err(|_| poisoned())?
            .push((req.url.to_string(), req.range_from));
        self.bearers.lock().map_err(|_| poisoned())?.push((
            req.url.to_string(),
            req.bearer.map(str::to_string),
            req.bearer_host.map(str::to_string),
        ));
        if self.fail.lock().map_err(|_| poisoned())?.contains(req.url) {
            return Err(Error::Http("the network is down".into()));
        }
        if let Some(u) = self.redirects.lock().map_err(|_| poisoned())?.get(req.url) {
            // A redirect to another host is refused, as the real client does.
            let host = crate::manifest::url_host(u);
            if !host.is_some_and(|h| req.allowed_hosts.contains(&h)) {
                return Err(Error::ForeignHost(u.clone()));
            }
        }
        let mut routes = self.routes.lock().map_err(|_| poisoned())?;
        let Some(r) = routes.get_mut(req.url) else {
            return Ok(Response {
                status: 404,
                content_range_start: None,
                body: Box::new(Cursor::new(Vec::new())),
            });
        };
        if let Some(s) = r.status {
            return Ok(Response {
                status: s,
                content_range_start: None,
                body: Box::new(Cursor::new(r.body.clone())),
            });
        }
        let (status, start) = match req.range_from {
            Some(n) if !r.ignore_range && (n as usize) < r.body.len() => (206, n as usize),
            Some(n) if !r.ignore_range && (n as usize) >= r.body.len() => {
                return Ok(Response {
                    status: 416,
                    content_range_start: None,
                    body: Box::new(Cursor::new(Vec::new())),
                });
            }
            _ => (200, 0),
        };
        let mut body = r.body[start..].to_vec();
        if let Some(cut) = r.cut_next_at.take() {
            body.truncate(cut);
        }
        Ok(Response {
            status,
            content_range_start: (status == 206).then_some(start as u64),
            body: Box::new(Cursor::new(body)),
        })
    }
}
