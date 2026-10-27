// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `AppImage`: replace the one file, keep the old one beside it.
//!
//! For an `AppImage` at `/home/me/Helia.AppImage`:
//!
//! 1. the new file is copied to `/home/me/.Helia.AppImage.new`, flushed and made executable;
//! 2. the running file is copied to `.Helia.AppImage.previous.tmp` and renamed to
//!    `Helia.AppImage.previous` (a whole copy or nothing);
//! 3. `.Helia.AppImage.new` is renamed over `Helia.AppImage`. A rename within one
//!    folder is atomic, so the path always holds a complete, working file.
//!
//! The running process keeps its own open file, so it is not disturbed; the new
//! version starts the next time. A failure at any step removes the temporary
//! files and leaves the install exactly as it was.

use std::path::{Path, PathBuf};

use super::{ApplyOutcome, Ctx, Rollback, verify_staged};
use crate::download::Staged;
use crate::error::{Error, Result};
use crate::state::BootHealth;

/// An `AppImage` is an ELF file.
const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];

fn sibling(target: &Path, prefix: &str, suffix: &str) -> PathBuf {
    let name = target.file_name().map_or_else(
        || "Helia.AppImage".into(),
        |n| n.to_string_lossy().into_owned(),
    );
    target.with_file_name(format!("{prefix}{name}{suffix}"))
}

/// Where the previous version is kept.
#[must_use]
pub fn previous_path(target: &Path) -> PathBuf {
    sibling(target, "", ".previous")
}

fn has_elf_magic(ctx: &Ctx<'_>, path: &Path) -> Result<bool> {
    use std::io::Read;
    let mut head = [0u8; 4];
    let mut r = ctx.fs.open_read(path)?;
    Ok(r.read_exact(&mut head).is_ok() && head == ELF_MAGIC)
}

fn cannot(what: &str, e: &std::io::Error, target: &Path) -> Error {
    Error::Apply(format!(
        "{what} ({e}). Nothing was changed: {} is as it was. Check that its folder is writable and has room, then try again.",
        target.display()
    ))
}

/// Replace `target` with the staged `AppImage`.
///
/// # Errors
/// [`Error::Apply`] (nothing changed) or [`Error::Io`] from the verification read.
pub fn apply(
    ctx: &Ctx<'_>,
    target: &Path,
    staged: &Staged,
    from_version: &str,
    health: &mut BootHealth,
) -> Result<ApplyOutcome> {
    verify_staged(ctx.fs, staged, "appimage")?;
    if !has_elf_magic(ctx, &staged.path)? {
        return Err(Error::Apply(
            "the downloaded file is not an AppImage, so it was not used".into(),
        ));
    }
    let new = sibling(target, ".", ".new");
    let prev_tmp = sibling(target, ".", ".previous.tmp");
    let previous = previous_path(target);
    let cleanup = |ctx: &Ctx<'_>| {
        let _ = ctx.fs.remove_file(&new);
        let _ = ctx.fs.remove_file(&prev_tmp);
    };

    let step = (|| -> Result<()> {
        ctx.fs
            .copy_file(&staged.path, &new)
            .map_err(|e| cannot("could not write the new AppImage", &e, target))?;
        ctx.fs
            .make_executable(&new)
            .map_err(|e| cannot("could not make the new AppImage runnable", &e, target))?;
        if ctx.fs.len(target)?.is_some() {
            ctx.fs
                .copy_file(target, &prev_tmp)
                .map_err(|e| cannot("could not keep a copy of the old AppImage", &e, target))?;
            ctx.fs
                .rename(&prev_tmp, &previous)
                .map_err(|e| cannot("could not keep a copy of the old AppImage", &e, target))?;
        }
        ctx.fs
            .rename(&new, target)
            .map_err(|e| cannot("could not put the new AppImage in place", &e, target))?;
        Ok(())
    })();
    if let Err(e) = step {
        cleanup(ctx);
        return Err(e);
    }
    health.record_applied(from_version, Some(&previous));
    let _ = ctx.fs.remove_file(&staged.path);
    Ok(ApplyOutcome::Installed {
        message: format!(
            "Helia {} is installed. It starts the next time you open Helia. The old version is kept as {} in case it is needed.",
            staged.version,
            previous.display()
        ),
    })
}

/// Put the kept previous `AppImage` back over `target`.
///
/// # Errors
/// [`Error::Apply`] when no usable previous file is kept or the rename fails.
pub fn rollback(ctx: &Ctx<'_>, target: &Path, health: &mut BootHealth) -> Result<Rollback> {
    let previous = health
        .previous_path
        .clone()
        .unwrap_or_else(|| previous_path(target));
    let version = health.previous.clone().unwrap_or_default();
    if ctx.fs.len(&previous)?.is_none() || !has_elf_magic(ctx, &previous)? {
        return Err(Error::Apply(format!(
            "the old version is not kept at {} any more, so Helia cannot go back to it. Download the version you want from the Helia website.",
            previous.display()
        )));
    }
    ctx.fs
        .rename(&previous, target)
        .map_err(|e| cannot("could not put the old AppImage back", &e, target))?;
    let failed = health.running.clone();
    health.rolled_back();
    let message = match failed {
        Some(f) => format!(
            "Helia {f} did not start properly twice, so I put the old version {version} back. Nothing of yours was changed."
        ),
        None => format!("Helia is back on version {version}. Nothing of yours was changed."),
    };
    Ok(Rollback {
        to_version: version,
        message,
        relaunch: Some(target.to_path_buf()),
        exit_now: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply::fake::{FakeProcess, TestFs};
    use crate::verify::sha256_hex;

    const TARGET: &str = "/home/me/Helia.AppImage";

    fn elf(tag: &str) -> Vec<u8> {
        let mut v = vec![0x7f, b'E', b'L', b'F'];
        v.extend_from_slice(tag.as_bytes());
        v
    }

    fn staged(fs: &TestFs, body: &[u8]) -> Staged {
        let path = PathBuf::from("/stage/abcd1234-Helia-0.8.0.AppImage");
        fs.mem.put(&path, body);
        Staged {
            version: "0.8.0".into(),
            kind: "appimage".into(),
            path,
            sha256: sha256_hex(body),
            size: body.len() as u64,
        }
    }

    fn ctx<'a>(fs: &'a TestFs, p: &'a FakeProcess) -> Ctx<'a> {
        Ctx {
            fs,
            process: p,
            backup_dir: Path::new("/backup"),
            relaunch_gui: true,
        }
    }

    #[test]
    fn replaces_atomically_and_keeps_the_old_one() {
        let fs = TestFs::default();
        let p = FakeProcess::default();
        fs.mem.put(Path::new(TARGET), &elf("old"));
        let s = staged(&fs, &elf("new"));
        let mut h = BootHealth::default();
        let out = apply(&ctx(&fs, &p), Path::new(TARGET), &s, "0.7.1", &mut h).unwrap();
        assert!(matches!(out, ApplyOutcome::Installed { .. }));
        assert_eq!(fs.mem.get(Path::new(TARGET)).unwrap(), elf("new"));
        assert!(
            fs.is_exec(Path::new(TARGET)),
            "the executable bit survives the rename"
        );
        let prev = Path::new("/home/me/Helia.AppImage.previous");
        assert_eq!(fs.mem.get(prev).unwrap(), elf("old"));
        assert_eq!(h.previous.as_deref(), Some("0.7.1"));
        assert_eq!(h.previous_path.as_deref(), Some(prev));
        // No temporary files are left, and the staged file is gone.
        let left: Vec<_> = fs.mem.paths();
        assert_eq!(left.len(), 2, "{left:?}");
        assert!(p.spawned.lock().unwrap().is_empty());
    }

    #[test]
    fn failure_leaves_the_install_untouched() {
        let fs = TestFs::default();
        let p = FakeProcess::default();
        fs.mem.put(Path::new(TARGET), &elf("old"));
        let s = staged(&fs, &elf("new"));
        *fs.fail_copy_to.lock().unwrap() =
            Some(PathBuf::from("/home/me/.Helia.AppImage.previous.tmp"));
        let mut h = BootHealth::default();
        let e = apply(&ctx(&fs, &p), Path::new(TARGET), &s, "0.7.1", &mut h).unwrap_err();
        assert!(e.to_string().contains("Nothing was changed"), "{e}");
        assert_eq!(fs.mem.get(Path::new(TARGET)).unwrap(), elf("old"));
        assert_eq!(
            fs.mem.paths().len(),
            2,
            "only the target and the staged file remain"
        );
        assert_eq!(h, BootHealth::default());
    }

    #[test]
    fn refuses_a_file_that_is_not_what_was_verified() {
        let fs = TestFs::default();
        let p = FakeProcess::default();
        fs.mem.put(Path::new(TARGET), &elf("old"));
        let mut s = staged(&fs, &elf("new"));
        // Tampered in the staging folder after the download was checked.
        fs.mem.put(&s.path, &elf("evil"));
        let mut h = BootHealth::default();
        assert!(apply(&ctx(&fs, &p), Path::new(TARGET), &s, "0.7.1", &mut h).is_err());
        assert_eq!(fs.mem.get(Path::new(TARGET)).unwrap(), elf("old"));
        // Right hash but not an ELF file.
        s = staged(&fs, b"#!/bin/sh\necho hi\n");
        assert!(apply(&ctx(&fs, &p), Path::new(TARGET), &s, "0.7.1", &mut h).is_err());
        // Wrong kind.
        s = staged(&fs, &elf("new"));
        s.kind = "deb".into();
        assert!(apply(&ctx(&fs, &p), Path::new(TARGET), &s, "0.7.1", &mut h).is_err());
        assert_eq!(fs.mem.get(Path::new(TARGET)).unwrap(), elf("old"));
    }

    #[test]
    fn rollback_after_two_failed_starts_restores_the_old_file() {
        let fs = TestFs::default();
        let p = FakeProcess::default();
        fs.mem.put(Path::new(TARGET), &elf("old"));
        let s = staged(&fs, &elf("new"));
        let mut h = BootHealth::default();
        apply(&ctx(&fs, &p), Path::new(TARGET), &s, "0.7.1", &mut h).unwrap();
        for _ in 0..3 {
            h.begin_start("0.8.0");
        }
        assert_eq!(h.rollback_target(), Some("0.7.1"));
        let r = rollback(&ctx(&fs, &p), Path::new(TARGET), &mut h).unwrap();
        assert_eq!(r.to_version, "0.7.1");
        assert_eq!(r.relaunch.as_deref(), Some(Path::new(TARGET)));
        assert!(
            r.message.contains("0.8.0") && r.message.contains("0.7.1"),
            "{}",
            r.message
        );
        assert_eq!(fs.mem.get(Path::new(TARGET)).unwrap(), elf("old"));
        assert_eq!(h.rollback_target(), None);
        assert_eq!(h.previous, None);
    }

    #[test]
    fn rollback_without_a_kept_file_says_so() {
        let fs = TestFs::default();
        let p = FakeProcess::default();
        fs.mem.put(Path::new(TARGET), &elf("new"));
        let mut h = BootHealth::default();
        h.record_applied("0.7.1", Some(Path::new("/home/me/Helia.AppImage.previous")));
        let e = rollback(&ctx(&fs, &p), Path::new(TARGET), &mut h).unwrap_err();
        assert!(e.to_string().contains("not kept"), "{e}");
        assert_eq!(fs.mem.get(Path::new(TARGET)).unwrap(), elf("new"));
    }
}
