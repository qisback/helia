// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! deb and rpm: never touch the package manager's files, show the command.

use std::path::Path;

use super::ApplyOutcome;
use crate::download::Staged;
use crate::install::InstallKind;

/// `s` as one shell word: single-quoted, with embedded quotes escaped.
#[must_use]
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+' | ':'))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The command that installs the package file at `path` for `kind`, if `kind`
/// is a package install.
#[must_use]
pub fn command(kind: &InstallKind, path: &Path) -> Option<String> {
    let p = shell_quote(&path.to_string_lossy());
    match kind {
        InstallKind::Deb => Some(format!("sudo apt install {p}")),
        InstallKind::Rpm => Some(format!("sudo dnf upgrade {p}")),
        _ => None,
    }
}

/// The outcome for a package install: the command to run.
#[must_use]
pub fn apply(kind: &InstallKind, staged: &Staged) -> ApplyOutcome {
    let cmd = command(kind, &staged.path);
    ApplyOutcome::Manual {
        message: format!(
            "Helia {} is downloaded to {}. Helia was installed by your package manager, so it leaves the files to it. Run this in a terminal to install it{}.",
            staged.version,
            staged.path.display(),
            if matches!(kind, InstallKind::Deb) {
                ""
            } else {
                " (use zypper install instead on openSUSE)"
            }
        ),
        command: cmd,
        file: Some(staged.path.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn staged(name: &str, kind: &str) -> Staged {
        Staged {
            version: "0.8.0".into(),
            kind: kind.into(),
            path: PathBuf::from(name),
            sha256: "0".repeat(64),
            size: 1,
        }
    }

    #[test]
    fn deb_and_rpm_commands() {
        let d = apply(
            &InstallKind::Deb,
            &staged(
                "/home/me/.local/share/helia/updates/ab12-helia_0.8.0_amd64.deb",
                "deb",
            ),
        );
        match d {
            ApplyOutcome::Manual {
                command: Some(c), ..
            } => {
                assert_eq!(
                    c,
                    "sudo apt install /home/me/.local/share/helia/updates/ab12-helia_0.8.0_amd64.deb"
                );
            }
            other => panic!("{other:?}"),
        }
        let r = command(&InstallKind::Rpm, Path::new("/tmp/helia-0.8.0.x86_64.rpm")).unwrap();
        assert_eq!(r, "sudo dnf upgrade /tmp/helia-0.8.0.x86_64.rpm");
    }

    #[test]
    fn awkward_paths_are_quoted() {
        let c = command(&InstallKind::Deb, Path::new("/home/o'brien/my files/h.deb")).unwrap();
        assert_eq!(c, r"sudo apt install '/home/o'\''brien/my files/h.deb'");
        assert_eq!(command(&InstallKind::Portable, Path::new("/x")), None);
    }
}
