// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Applying a downloaded update, one strategy per install kind.
//!
//! * [`appimage`]: write the new file beside the old one, flush it, mark it
//!   executable, keep the old file as `<name>.previous`, then atomically rename
//!   the new file over the old.
//! * [`windows`]: keep copies of the two programs, then start a hidden helper that
//!   waits for this process to exit, runs the new installer silently (`/S`, same
//!   folder, settings untouched), puts the old programs back if the installer
//!   fails, and starts the app again.
//! * [`package`]: never touches files; gives the command to run.
//! * anything else: says where the download is.
//!
//! All file and process access goes through [`ApplyFs`] and [`Process`], so every
//! strategy is tested without root, a real installer or a second process.

pub mod appimage;
pub mod package;
pub mod windows;

use std::ffi::OsString;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::download::Staged;
use crate::error::{Error, Result};
use crate::install::InstallKind;
use crate::state::BootHealth;
use crate::traits::{Fs, StdFs};
use crate::verify::hex;

/// File operations the appliers need beyond [`Fs`].
pub trait ApplyFs: Fs {
    /// Copy a file's contents to `to` and flush them to disk. Replaces `to`.
    fn copy_file(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Mark a file executable (a no-op where there is no such bit).
    fn make_executable(&self, path: &Path) -> io::Result<()>;
}

impl ApplyFs for StdFs {
    fn copy_file(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::copy(from, to)?;
        // Write access: on Windows FlushFileBuffers fails on a read-only handle.
        fs::OpenOptions::new().write(true).open(to)?.sync_all()
    }

    fn make_executable(&self, path: &Path) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Ok(())
        }
    }
}

/// Starting another program.
pub trait Process {
    /// Start `program` with `args`, detached: it keeps running after this process exits.
    fn spawn_detached(&self, program: &Path, args: &[OsString]) -> io::Result<()>;
    /// This process's id.
    fn pid(&self) -> u32;
}

/// The real thing.
#[derive(Clone, Copy, Debug, Default)]
pub struct StdProcess;

impl Process for StdProcess {
    fn spawn_detached(&self, program: &Path, args: &[OsString]) -> io::Result<()> {
        use std::process::{Command, Stdio};
        let mut c = Command::new(program);
        c.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW. Not DETACHED_PROCESS: the
            // PowerShell helper needs a console, and a hidden one is what NO_WINDOW gives it.
            c.creation_flags(0x0000_0200 | 0x0800_0000);
        }
        c.spawn().map(|_| ())
    }

    fn pid(&self) -> u32 {
        std::process::id()
    }
}

/// What the strategies work with.
pub struct Ctx<'a> {
    /// File access.
    pub fs: &'a dyn ApplyFs,
    /// Process access.
    pub process: &'a dyn Process,
    /// Where the Windows strategy keeps the programs it replaces.
    pub backup_dir: &'a Path,
    /// Start the GUI again after a Windows update (true for the GUI, false for the CLI).
    pub relaunch_gui: bool,
}

/// What applying did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// Done. The new version runs the next time the app starts.
    Installed {
        /// Plain English.
        message: String,
    },
    /// Will be done when this process exits.
    Scheduled {
        /// Plain English.
        message: String,
    },
    /// Not done by the app: the user finishes it.
    Manual {
        /// Plain English.
        message: String,
        /// A command to copy, when there is one.
        command: Option<String>,
        /// The downloaded file.
        file: Option<PathBuf>,
    },
}

impl ApplyOutcome {
    /// The message.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Installed { message }
            | Self::Scheduled { message }
            | Self::Manual { message, .. } => message,
        }
    }
}

/// A rollback that was done.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rollback {
    /// The version put back.
    pub to_version: String,
    /// Plain English, for a notice.
    pub message: String,
    /// Start this program now (the restored one).
    pub relaunch: Option<PathBuf>,
    /// The caller must exit now so a helper can finish (Windows).
    pub exit_now: bool,
}

/// Re-check a staged file just before it is used: its kind, size and sha256.
/// The download was verified, but the staging folder is writable by the user, so
/// this closes the gap between download and use.
///
/// # Errors
/// [`Error::Apply`] when it does not match; [`Error::Io`] when it cannot be read.
pub fn verify_staged(fs: &dyn Fs, staged: &Staged, want_kind: &str) -> Result<()> {
    if staged.kind != want_kind {
        return Err(Error::Apply(format!(
            "the downloaded file is a {} but this install needs a {want_kind}",
            staged.kind
        )));
    }
    if fs.len(&staged.path)? != Some(staged.size) {
        return Err(Error::Apply(
            "the downloaded file is missing or has changed size, so it was not used".into(),
        ));
    }
    let mut r = fs.open_read(&staged.path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    if hex(&h.finalize()) != staged.sha256 {
        return Err(Error::Apply(
            "the downloaded file has changed since it was checked, so it was not used".into(),
        ));
    }
    Ok(())
}

/// Apply `staged` to the running install. `from_version` is what is running now.
///
/// # Errors
/// [`Error::Apply`] with what failed and what was left untouched; [`Error::Io`].
pub fn apply(
    ctx: &Ctx<'_>,
    kind: &InstallKind,
    staged: &Staged,
    from_version: &str,
    health: &mut BootHealth,
) -> Result<ApplyOutcome> {
    match kind {
        InstallKind::AppImage { path } => appimage::apply(ctx, path, staged, from_version, health),
        InstallKind::Nsis { dir, all_users } => {
            windows::apply(ctx, dir, *all_users, staged, from_version, health)
        }
        InstallKind::Deb | InstallKind::Rpm => Ok(package::apply(kind, staged)),
        InstallKind::Flatpak => Ok(ApplyOutcome::Manual {
            message: "This copy of Helia is installed with Flatpak. Update it with: flatpak update casa.discworld.helia".into(),
            command: Some("flatpak update casa.discworld.helia".into()),
            file: None,
        }),
        InstallKind::MacApp => Ok(ApplyOutcome::Manual {
            message: format!(
                "The new Helia is downloaded to {}. Open it and drag Helia into Applications, replacing the old one.",
                staged.path.display()
            ),
            command: None,
            file: Some(staged.path.clone()),
        }),
        InstallKind::Portable => Ok(ApplyOutcome::Manual {
            message: format!(
                "The new Helia is downloaded to {}. Unpack it where the old one is, replacing the old files.",
                staged.path.display()
            ),
            command: None,
            file: Some(staged.path.clone()),
        }),
    }
}

/// Put the previous version back.
///
/// # Errors
/// [`Error::Apply`] when there is nothing to put back or it cannot be done.
pub fn rollback(ctx: &Ctx<'_>, kind: &InstallKind, health: &mut BootHealth) -> Result<Rollback> {
    match kind {
        InstallKind::AppImage { path } => appimage::rollback(ctx, path, health),
        InstallKind::Nsis {
            dir,
            all_users: false,
        } => windows::rollback(ctx, dir, health),
        _ => Err(Error::Apply(
            "this install was not updated by Helia, so there is nothing for it to put back".into(),
        )),
    }
}

/// The test file system and process.
#[cfg(any(test, feature = "testutil"))]
#[allow(
    clippy::unwrap_used,
    clippy::missing_panics_doc,
    missing_docs,
    reason = "test doubles"
)]
pub mod fake {
    use std::collections::BTreeSet;
    use std::ffi::OsString;
    use std::io::{self, Read, Write};
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use super::{ApplyFs, Process};
    use crate::mem::MemFs;
    use crate::traits::Fs;

    /// [`MemFs`] plus the executable bit and failure injection.
    #[derive(Default)]
    pub struct TestFs {
        pub mem: MemFs,
        pub exec: Mutex<BTreeSet<PathBuf>>,
        /// Fail `copy_file` into this path.
        pub fail_copy_to: Mutex<Option<PathBuf>>,
    }

    impl TestFs {
        pub fn is_exec(&self, p: &Path) -> bool {
            self.exec.lock().unwrap().contains(p)
        }
    }

    impl Fs for TestFs {
        fn read(&self, p: &Path) -> io::Result<Vec<u8>> {
            self.mem.read(p)
        }
        fn write_atomic(&self, p: &Path, d: &[u8]) -> io::Result<()> {
            self.mem.write_atomic(p, d)
        }
        fn create_dir_all(&self, p: &Path) -> io::Result<()> {
            self.mem.create_dir_all(p)
        }
        fn len(&self, p: &Path) -> io::Result<Option<u64>> {
            self.mem.len(p)
        }
        fn open_read(&self, p: &Path) -> io::Result<Box<dyn Read>> {
            self.mem.open_read(p)
        }
        fn open_append(&self, p: &Path) -> io::Result<Box<dyn Write>> {
            self.mem.open_append(p)
        }
        fn rename(&self, a: &Path, b: &Path) -> io::Result<()> {
            let was_exec = self.exec.lock().unwrap().remove(a);
            self.mem.rename(a, b)?;
            if was_exec {
                self.exec.lock().unwrap().insert(b.to_path_buf());
            }
            Ok(())
        }
        fn remove_file(&self, p: &Path) -> io::Result<()> {
            self.exec.lock().unwrap().remove(p);
            self.mem.remove_file(p)
        }
    }

    impl ApplyFs for TestFs {
        fn copy_file(&self, from: &Path, to: &Path) -> io::Result<()> {
            if self.fail_copy_to.lock().unwrap().as_deref() == Some(to) {
                return Err(io::Error::other("disk full"));
            }
            let d = self.mem.read(from)?;
            self.mem.put(to, &d);
            Ok(())
        }
        fn make_executable(&self, p: &Path) -> io::Result<()> {
            self.exec.lock().unwrap().insert(p.to_path_buf());
            Ok(())
        }
    }

    /// Records what would have been started.
    #[derive(Default)]
    pub struct FakeProcess {
        pub spawned: Mutex<Vec<(PathBuf, Vec<OsString>)>>,
    }

    impl Process for FakeProcess {
        fn spawn_detached(&self, program: &Path, args: &[OsString]) -> io::Result<()> {
            self.spawned
                .lock()
                .unwrap()
                .push((program.to_path_buf(), args.to_vec()));
            Ok(())
        }
        fn pid(&self) -> u32 {
            4242
        }
    }
}
