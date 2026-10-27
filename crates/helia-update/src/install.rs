// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! How this copy of the app was installed, which decides how it may be updated.
//!
//! | Install | How it is recognised | Update |
//! |---|---|---|
//! | Windows installer | `uninstall.exe` beside the program | run the new installer silently when the app exits |
//! | `AppImage` | `$APPIMAGE` names an existing file | replace that file, keeping the old one |
//! | deb / rpm | program under `/usr` and owned by `dpkg` / `rpm` | show the package command, touch nothing |
//! | Flatpak | `$FLATPAK_ID` is set | leave it to Flatpak |
//! | macOS app | program inside `X.app/Contents/MacOS` | tell the user, download only |
//! | anything else | tar.gz / zip, `/usr/local`, a build tree | tell the user, download only |
//!
//! The questions are asked through [`Probe`], so the detection is tested without
//! the real systems.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::manifest::Kind;

/// The operating system family, as far as installs differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostOs {
    /// Windows.
    Windows,
    /// Linux.
    Linux,
    /// macOS.
    Macos,
    /// Anything else.
    Other,
}

impl HostOs {
    /// The system this build runs on.
    #[must_use]
    pub fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "macos") {
            Self::Macos
        } else {
            Self::Other
        }
    }
}

/// A package manager that owns the program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageManager {
    /// `dpkg` (Debian, Ubuntu).
    Dpkg,
    /// `rpm` (Fedora, openSUSE, RHEL).
    Rpm,
}

/// How the running copy was installed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallKind {
    /// Installed by the Windows installer.
    Nsis {
        /// The install folder.
        dir: PathBuf,
        /// Installed for all users (needs administrator rights to change).
        all_users: bool,
    },
    /// A single-file `AppImage`.
    AppImage {
        /// The file `$APPIMAGE` names.
        path: PathBuf,
    },
    /// Installed by a Debian package.
    Deb,
    /// Installed by an RPM package.
    Rpm,
    /// Installed by Flatpak.
    Flatpak,
    /// A macOS application bundle.
    MacApp,
    /// Unpacked from an archive, or built from source.
    Portable,
}

impl InstallKind {
    /// The asset kinds that suit this install, best first.
    #[must_use]
    pub fn asset_kinds(&self) -> &'static [Kind] {
        match self {
            Self::Nsis { .. } => &[Kind::Nsis],
            Self::AppImage { .. } => &[Kind::Appimage],
            Self::Deb => &[Kind::Deb],
            Self::Rpm => &[Kind::Rpm],
            Self::Flatpak => &[],
            Self::MacApp => &[Kind::Dmg],
            Self::Portable => &[Kind::Targz, Kind::Zip],
        }
    }

    /// Can the app replace itself? Only for the Windows installer (per user) and `AppImage`.
    #[must_use]
    pub fn can_self_apply(&self) -> bool {
        matches!(
            self,
            Self::Nsis {
                all_users: false,
                ..
            } | Self::AppImage { .. }
        )
    }

    /// A short name for messages.
    #[must_use]
    pub fn describe(&self) -> &'static str {
        match self {
            Self::Nsis { .. } => "Windows installer",
            Self::AppImage { .. } => "AppImage",
            Self::Deb => "Debian package",
            Self::Rpm => "RPM package",
            Self::Flatpak => "Flatpak",
            Self::MacApp => "macOS app",
            Self::Portable => "portable copy",
        }
    }
}

/// The questions detection asks of the system.
pub trait Probe {
    /// An environment variable.
    fn env(&self, key: &str) -> Option<OsString>;
    /// The running program's path.
    fn current_exe(&self) -> Option<PathBuf>;
    /// Does the path exist?
    fn exists(&self, path: &Path) -> bool;
    /// Which package manager, if any, owns this file (`dpkg -S`, `rpm -qf`).
    fn package_owner(&self, path: &Path) -> Option<PackageManager>;
}

/// The real system.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemProbe;

fn succeeds(program: &str, args: &[&OsString]) -> bool {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

impl Probe for SystemProbe {
    fn env(&self, key: &str) -> Option<OsString> {
        std::env::var_os(key).filter(|v| !v.is_empty())
    }

    fn current_exe(&self) -> Option<PathBuf> {
        std::env::current_exe().ok()
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn package_owner(&self, path: &Path) -> Option<PackageManager> {
        let p = path.as_os_str().to_owned();
        let dash_s = OsString::from("-S");
        let qf = OsString::from("-qf");
        if succeeds("dpkg", &[&dash_s, &p]) {
            Some(PackageManager::Dpkg)
        } else if succeeds("rpm", &[&qf, &p]) {
            Some(PackageManager::Rpm)
        } else {
            None
        }
    }
}

fn under(path: &Path, prefix: &Path) -> bool {
    let lower = |p: &Path| p.to_string_lossy().replace('/', "\\").to_ascii_lowercase();
    let (p, pre) = (lower(path), lower(prefix));
    !pre.is_empty() && (p == pre || p.starts_with(&format!("{}\\", pre.trim_end_matches('\\'))))
}

/// Work out how the running copy was installed.
#[must_use]
pub fn detect(probe: &dyn Probe, os: HostOs) -> InstallKind {
    let exe = probe.current_exe();
    match os {
        HostOs::Windows => {
            let Some(dir) = exe.as_deref().and_then(Path::parent) else {
                return InstallKind::Portable;
            };
            if !probe.exists(&dir.join("uninstall.exe")) {
                return InstallKind::Portable;
            }
            let all_users = ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"]
                .iter()
                .filter_map(|k| probe.env(k))
                .any(|pf| under(dir, Path::new(&pf)));
            InstallKind::Nsis {
                dir: dir.to_path_buf(),
                all_users,
            }
        }
        HostOs::Linux => {
            if let Some(a) = probe.env("APPIMAGE") {
                let path = PathBuf::from(a);
                // has_root: the Linux rules are also tested on Windows, where "/x" has no drive letter.
                if path.has_root() && probe.exists(&path) {
                    return InstallKind::AppImage { path };
                }
            }
            if probe.env("FLATPAK_ID").is_some() {
                return InstallKind::Flatpak;
            }
            if let Some(exe) = exe.as_deref()
                && exe.starts_with("/usr")
                && !exe.starts_with("/usr/local")
            {
                return match probe.package_owner(exe) {
                    Some(PackageManager::Dpkg) => InstallKind::Deb,
                    Some(PackageManager::Rpm) => InstallKind::Rpm,
                    None => InstallKind::Portable,
                };
            }
            InstallKind::Portable
        }
        HostOs::Macos => {
            if exe
                .as_deref()
                .is_some_and(|e| e.to_string_lossy().contains(".app/Contents/MacOS/"))
            {
                InstallKind::MacApp
            } else {
                InstallKind::Portable
            }
        }
        HostOs::Other => InstallKind::Portable,
    }
}

#[cfg(any(test, feature = "testutil"))]
#[allow(missing_docs, clippy::wildcard_imports, reason = "test doubles")]
pub mod fake {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    /// A probe with canned answers.
    #[derive(Default)]
    pub struct FakeProbe {
        pub env: BTreeMap<String, String>,
        pub exe: Option<PathBuf>,
        pub files: BTreeSet<PathBuf>,
        pub owner: Option<PackageManager>,
    }

    impl Probe for FakeProbe {
        fn env(&self, key: &str) -> Option<OsString> {
            self.env.get(key).map(OsString::from)
        }
        fn current_exe(&self) -> Option<PathBuf> {
            self.exe.clone()
        }
        fn exists(&self, path: &Path) -> bool {
            self.files.contains(path)
        }
        fn package_owner(&self, _: &Path) -> Option<PackageManager> {
            self.owner
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeProbe;
    use super::*;

    fn probe(exe: &str) -> FakeProbe {
        FakeProbe {
            exe: Some(PathBuf::from(exe)),
            ..FakeProbe::default()
        }
    }

    #[test]
    fn windows_installer_per_user_and_all_users() {
        let mut p = probe(r"C:/Users/me/AppData/Local/Programs/Helia/helia-gui.exe");
        p.files.insert(PathBuf::from(
            r"C:/Users/me/AppData/Local/Programs/Helia/uninstall.exe",
        ));
        p.env
            .insert("ProgramFiles".into(), r"C:\Program Files".into());
        match detect(&p, HostOs::Windows) {
            InstallKind::Nsis { all_users, .. } => assert!(!all_users),
            other => panic!("{other:?}"),
        }
        let mut q = probe(r"C:/Program Files/Helia/helia-gui.exe");
        q.files
            .insert(PathBuf::from(r"C:/Program Files/Helia/uninstall.exe"));
        q.env
            .insert("ProgramFiles".into(), r"c:\program files".into());
        match detect(&q, HostOs::Windows) {
            InstallKind::Nsis { all_users, dir } => {
                assert!(all_users);
                assert!(!InstallKind::Nsis { dir, all_users }.can_self_apply());
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn windows_without_uninstaller_is_portable() {
        let p = probe(r"D:/tools/helia/helia-gui.exe");
        assert_eq!(detect(&p, HostOs::Windows), InstallKind::Portable);
    }

    #[test]
    fn appimage_needs_an_existing_absolute_file() {
        let mut p = probe("/tmp/.mount_abc/usr/bin/helia-gui");
        p.env
            .insert("APPIMAGE".into(), "/home/me/Helia.AppImage".into());
        assert_eq!(detect(&p, HostOs::Linux), InstallKind::Portable);
        p.files.insert(PathBuf::from("/home/me/Helia.AppImage"));
        assert_eq!(
            detect(&p, HostOs::Linux),
            InstallKind::AppImage {
                path: PathBuf::from("/home/me/Helia.AppImage")
            }
        );
        p.env.insert("APPIMAGE".into(), "relative.AppImage".into());
        assert_eq!(detect(&p, HostOs::Linux), InstallKind::Portable);
    }

    #[test]
    fn packages_are_told_apart_by_their_owner() {
        let mut p = probe("/usr/bin/helia-gui");
        p.owner = Some(PackageManager::Dpkg);
        assert_eq!(detect(&p, HostOs::Linux), InstallKind::Deb);
        p.owner = Some(PackageManager::Rpm);
        assert_eq!(detect(&p, HostOs::Linux), InstallKind::Rpm);
        p.owner = None;
        assert_eq!(detect(&p, HostOs::Linux), InstallKind::Portable);
        let mut local = probe("/usr/local/bin/helia-gui");
        local.owner = Some(PackageManager::Dpkg);
        assert_eq!(detect(&local, HostOs::Linux), InstallKind::Portable);
    }

    #[test]
    fn flatpak_and_mac_bundle() {
        let mut p = probe("/app/bin/helia-gui");
        p.env
            .insert("FLATPAK_ID".into(), "casa.discworld.helia".into());
        assert_eq!(detect(&p, HostOs::Linux), InstallKind::Flatpak);
        let m = probe("/Applications/Helia.app/Contents/MacOS/helia-gui");
        assert_eq!(detect(&m, HostOs::Macos), InstallKind::MacApp);
    }

    #[test]
    fn only_two_installs_replace_themselves() {
        assert!(InstallKind::AppImage { path: "/a".into() }.can_self_apply());
        for k in [
            InstallKind::Deb,
            InstallKind::Rpm,
            InstallKind::Flatpak,
            InstallKind::MacApp,
            InstallKind::Portable,
        ] {
            assert!(!k.can_self_apply(), "{k:?}");
        }
    }
}
