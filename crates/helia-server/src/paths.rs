// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Data-root sandbox: every client-supplied path resolves inside one directory.
//!
//! Rules, applied in order:
//! 1. `..` components are rejected outright (`path_traversal`), even if the
//!    result would land back inside the root.
//! 2. The path is joined to the root (absolute paths replace it) and
//!    canonicalised through its deepest existing ancestor, so symlinks are
//!    followed before the containment check.
//! 3. The canonical result must start with the canonical root
//!    (`path_outside_root`).
//!
//! Paths are checked at request time. A symlink swapped in between the check
//! and the use is not defended against; the data root must not be writable by
//! untrusted local users.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use crate::error::{ApiError, ApiResult};

/// A canonical directory that bounds file access.
#[derive(Clone, Debug)]
pub struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    /// Canonicalise `root`.
    ///
    /// # Errors
    /// The directory does not exist or is not a directory.
    pub fn new(root: &Path) -> std::io::Result<Self> {
        let root = root.canonicalize()?;
        if !root.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                format!("data root {} is not a directory", root.display()),
            ));
        }
        Ok(Self { root })
    }

    /// The canonical root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve a client path to a canonical path inside the root. The target
    /// itself may be missing (for outputs); its parent chain is canonicalised.
    ///
    /// # Errors
    /// 400 for malformed input, 403 for traversal or escape.
    pub fn resolve(&self, raw: &str) -> ApiResult<PathBuf> {
        if raw.is_empty() || raw.contains('\0') {
            return Err(ApiError::bad_request(
                "invalid_path",
                "path must be a non-empty string without NUL bytes",
            ));
        }
        let p = Path::new(raw);
        if p.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(ApiError::forbidden(
                "path_traversal",
                "`..` is not allowed in paths",
            ));
        }
        let canon = canonicalise_lenient(&self.root.join(p))?;
        if canon.starts_with(&self.root) {
            Ok(canon)
        } else {
            Err(ApiError::forbidden(
                "path_outside_root",
                "path resolves outside the data root",
            ))
        }
    }

    /// Resolve a path that must exist as a regular file.
    ///
    /// # Errors
    /// As [`Self::resolve`], plus 404 when missing or not a file.
    pub fn resolve_file(&self, raw: &str) -> ApiResult<PathBuf> {
        let p = self.resolve(raw)?;
        if p.is_file() {
            Ok(p)
        } else {
            Err(ApiError::not_found("not_found", "no such file"))
        }
    }

    /// Path relative to the root with forward slashes, for responses.
    #[must_use]
    pub fn display(&self, p: &Path) -> String {
        p.strip_prefix(&self.root)
            .unwrap_or(p)
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
    }
}

/// Canonicalise the deepest existing ancestor and re-append the missing tail.
fn canonicalise_lenient(p: &Path) -> ApiResult<PathBuf> {
    let mut tail: Vec<OsString> = Vec::new();
    let mut cur = p.to_path_buf();
    loop {
        match cur.canonicalize() {
            Ok(mut c) => {
                for t in tail.iter().rev() {
                    c.push(t);
                }
                return Ok(c);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // A dangling symlink canonicalises to NotFound but still
                // redirects writes; refuse it.
                if std::fs::symlink_metadata(&cur).is_ok() {
                    return Err(ApiError::forbidden(
                        "path_outside_root",
                        "dangling symbolic link",
                    ));
                }
                let Some(name) = cur.file_name().map(OsString::from) else {
                    return Err(ApiError::bad_request("invalid_path", "unresolvable path"));
                };
                tail.push(name);
                if !cur.pop() {
                    return Err(ApiError::bad_request("invalid_path", "unresolvable path"));
                }
            }
            Err(e) => {
                return Err(ApiError::bad_request(
                    "invalid_path",
                    format!("cannot resolve path: {e}"),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inside_and_missing_resolve() {
        let d = tempfile::tempdir().expect("tmp");
        std::fs::write(d.path().join("a.ser"), b"x").expect("write");
        let s = Sandbox::new(d.path()).expect("sandbox");
        assert!(s.resolve_file("a.ser").is_ok());
        assert!(s.resolve("new/out.tif").is_ok());
        assert_eq!(
            s.resolve_file("nope.ser").expect_err("must fail").status,
            404
        );
    }

    #[test]
    fn traversal_and_outside_are_403() {
        let d = tempfile::tempdir().expect("tmp");
        let s = Sandbox::new(d.path()).expect("sandbox");
        assert_eq!(
            s.resolve("../x").expect_err("must fail").code,
            "path_traversal"
        );
        assert_eq!(
            s.resolve("a/../b").expect_err("must fail").code,
            "path_traversal"
        );
        let other = tempfile::tempdir().expect("tmp2");
        let abs = other.path().join("x.ser");
        assert_eq!(
            s.resolve(abs.to_str().expect("utf8"))
                .expect_err("must fail")
                .code,
            "path_outside_root"
        );
    }
}
