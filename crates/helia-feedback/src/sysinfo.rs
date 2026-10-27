// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The operating system, described in a line for a report. Best effort: it
//! reads the system's own release file or asks its own version command, and
//! falls back to the bare name. It never includes a host name or user name.

use std::process::Command;

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (out.status.success() && !text.is_empty()).then_some(text)
}

/// The `PRETTY_NAME` of an os-release file's text.
#[must_use]
pub fn pretty_name(os_release: &str) -> Option<String> {
    os_release.lines().find_map(|l| {
        let v = l.strip_prefix("PRETTY_NAME=")?;
        let v = v.trim().trim_matches('"').trim_matches('\'');
        (!v.is_empty()).then(|| v.to_owned())
    })
}

/// A short description such as `Debian GNU/Linux 13 (trixie) (x86_64)`.
#[must_use]
pub fn os_description() -> String {
    let arch = std::env::consts::ARCH;
    let name = match std::env::consts::OS {
        "linux" => std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|t| pretty_name(&t))
            .unwrap_or_else(|| "Linux".into()),
        "windows" => command_output("cmd", &["/C", "ver"]).map_or_else(
            || "Windows".into(),
            |v| v.trim_matches(['\r', '\n']).to_owned(),
        ),
        "macos" => command_output("sw_vers", &["-productVersion"])
            .map_or_else(|| "macOS".into(), |v| format!("macOS {v}")),
        other => other.to_owned(),
    };
    format!("{name} ({arch})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pretty_name_is_read_from_os_release() {
        let text =
            "NAME=\"Debian GNU/Linux\"\nPRETTY_NAME=\"Debian GNU/Linux 13 (trixie)\"\nID=debian\n";
        assert_eq!(
            pretty_name(text).as_deref(),
            Some("Debian GNU/Linux 13 (trixie)")
        );
        assert_eq!(pretty_name("ID=x\n"), None);
        assert_eq!(pretty_name("PRETTY_NAME=\n"), None);
    }

    #[test]
    fn the_description_names_the_architecture_and_no_host() {
        let d = os_description();
        assert!(d.ends_with(&format!("({})", std::env::consts::ARCH)), "{d}");
        assert_ne!(d, "");
    }
}
