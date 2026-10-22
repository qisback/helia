// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Server settings and the builder.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

/// Why a server could not be configured.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The data root is missing or not a directory.
    #[error("data root {path}: {source}")]
    DataRoot {
        /// Offending path.
        path: PathBuf,
        /// Cause.
        source: std::io::Error,
    },
    /// Binding beyond loopback without authentication.
    #[error("refusing to bind {0}: a non-loopback address needs a token (--token or --token-file)")]
    NonLoopbackWithoutToken(SocketAddr),
    /// A token that is empty after trimming.
    #[error("the token must not be empty")]
    EmptyToken,
    /// Token file unreadable.
    #[error("cannot read token file {path}: {source}")]
    TokenFile {
        /// Offending path.
        path: PathBuf,
        /// Cause.
        source: std::io::Error,
    },
    /// An origin that is not `scheme://host[:port]`.
    #[error("invalid CORS origin {0:?}; expected scheme://host[:port]")]
    CorsOrigin(String),
}

/// Resolved settings.
#[derive(Clone)]
pub struct ServerConfig {
    /// Listen address.
    pub bind: SocketAddr,
    /// Directory every path must stay inside.
    pub data_root: PathBuf,
    /// Bearer token; no authentication when absent.
    pub token: Option<String>,
    /// Concurrent jobs.
    pub workers: usize,
    /// Queued plus running jobs allowed.
    pub max_queue: usize,
    /// Finished jobs remembered.
    pub max_retained_jobs: usize,
    /// Largest request body in bytes.
    pub max_body_bytes: usize,
    /// Origins allowed by CORS; none by default.
    pub cors_origins: Vec<String>,
    /// Extra `Host` values accepted on a loopback bind.
    pub allowed_hosts: Vec<String>,
    /// Directory for compressed video decoded by ffmpeg; the operating system's
    /// cache directory when absent.
    pub cache_dir: Option<PathBuf>,
}

impl std::fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerConfig")
            .field("bind", &self.bind)
            .field("data_root", &self.data_root)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("workers", &self.workers)
            .field("max_queue", &self.max_queue)
            .field("max_retained_jobs", &self.max_retained_jobs)
            .field("max_body_bytes", &self.max_body_bytes)
            .field("cors_origins", &self.cors_origins)
            .field("allowed_hosts", &self.allowed_hosts)
            .field("cache_dir", &self.cache_dir)
            .finish()
    }
}

/// Default port.
pub const DEFAULT_PORT: u16 = 7870;

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], DEFAULT_PORT)),
            data_root: PathBuf::from("."),
            token: None,
            workers: 1,
            max_queue: 16,
            max_retained_jobs: 100,
            max_body_bytes: 64 * 1024,
            cors_origins: Vec::new(),
            allowed_hosts: Vec::new(),
            cache_dir: None,
        }
    }
}

/// Fluent construction of a [`crate::Server`].
#[derive(Debug, Default)]
pub struct ServerBuilder {
    cfg: ServerConfig,
}

impl ServerBuilder {
    /// Listen address (default `127.0.0.1:7870`).
    #[must_use]
    pub fn bind(mut self, addr: SocketAddr) -> Self {
        self.cfg.bind = addr;
        self
    }

    /// Directory all inputs, outputs and previews must stay inside (default
    /// the current directory).
    #[must_use]
    pub fn data_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.cfg.data_root = root.into();
        self
    }

    /// Require this bearer token.
    #[must_use]
    pub fn token(mut self, token: impl Into<String>) -> Self {
        self.cfg.token = Some(token.into());
        self
    }

    /// Read the bearer token from a file (surrounding whitespace is trimmed).
    ///
    /// # Errors
    /// [`ConfigError::TokenFile`] when unreadable.
    pub fn token_file(self, path: &Path) -> Result<Self, ConfigError> {
        let t = std::fs::read_to_string(path).map_err(|source| ConfigError::TokenFile {
            path: path.to_owned(),
            source,
        })?;
        Ok(self.token(t.trim()))
    }

    /// Concurrent jobs (default 1: the GPU is one device).
    #[must_use]
    pub fn workers(mut self, n: usize) -> Self {
        self.cfg.workers = n.max(1);
        self
    }

    /// Queued plus running jobs allowed before 429 (default 16).
    #[must_use]
    pub fn max_queue(mut self, n: usize) -> Self {
        self.cfg.max_queue = n.max(1);
        self
    }

    /// Finished jobs remembered (default 100).
    #[must_use]
    pub fn max_retained_jobs(mut self, n: usize) -> Self {
        self.cfg.max_retained_jobs = n;
        self
    }

    /// Request body limit in bytes (default 64 KiB).
    #[must_use]
    pub fn max_body_bytes(mut self, n: usize) -> Self {
        self.cfg.max_body_bytes = n;
        self
    }

    /// Allow cross-origin browser access from this origin.
    #[must_use]
    pub fn cors_origin(mut self, origin: impl Into<String>) -> Self {
        self.cfg.cors_origins.push(origin.into());
        self
    }

    /// Directory for compressed video decoded by ffmpeg (default: the operating
    /// system's cache directory, or `HELIA_CACHE_DIR`).
    #[must_use]
    pub fn cache_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cfg.cache_dir = Some(dir.into());
        self
    }

    /// Accept this `Host` value on a loopback bind (reverse proxies).
    #[must_use]
    pub fn allow_host(mut self, host: impl Into<String>) -> Self {
        self.cfg
            .allowed_hosts
            .push(host.into().to_ascii_lowercase());
        self
    }

    /// Settings so far.
    #[must_use]
    pub fn config(&self) -> &ServerConfig {
        &self.cfg
    }

    /// Validate and create the server.
    ///
    /// # Errors
    /// [`ConfigError`] for a bad data root, an empty token, an unsafe bind or
    /// a malformed CORS origin.
    pub fn build(self) -> Result<crate::Server, ConfigError> {
        let cfg = self.cfg;
        if cfg.token.as_deref().is_some_and(|t| t.trim().is_empty()) {
            return Err(ConfigError::EmptyToken);
        }
        if !cfg.bind.ip().is_loopback() && cfg.token.is_none() {
            return Err(ConfigError::NonLoopbackWithoutToken(cfg.bind));
        }
        for o in &cfg.cors_origins {
            let ok = o
                .split_once("://")
                .is_some_and(|(s, h)| !s.is_empty() && !h.is_empty() && !h.contains('/'));
            if !ok {
                return Err(ConfigError::CorsOrigin(o.clone()));
            }
        }
        crate::Server::from_config(cfg)
    }
}
