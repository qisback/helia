// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Who wrote Helia and under what terms it is shared.
//!
//! One place for the notices that `helia --version`, `helia-gui --version`
//! and the About dialog print, so they cannot drift apart.

/// Product name.
pub const NAME: &str = "Helia";

/// The author, with no contact details.
pub const AUTHOR: &str = "Laurence Horrocks-Barlow";

/// The copyright line.
pub const COPYRIGHT: &str = "Copyright (C) 2026 Laurence Horrocks-Barlow";

/// SPDX identifier of the licence.
pub const LICENCE_ID: &str = "GPL-3.0-or-later";

/// Where the source code lives.
pub const SOURCE_URL: &str = "https://github.com/qisback/helia";

/// The author's other work, offered as a link and never fetched.
pub const HOBBIES_URL: &str = "https://12hobbies.studio";

/// Where donations go (Ko-fi); opened in the browser only when the user clicks.
pub const KOFI_URL: &str = "https://ko-fi.com/qisback";

/// Where the additional terms are published.
pub const ADDITIONAL_TERMS_URL: &str =
    "https://github.com/qisback/helia/blob/main/ADDITIONAL-TERMS.md";

/// Notice of the additional terms (GPL section 7), for the About page and `--version`.
pub const ADDITIONAL_TERMS_NOTICE: &str = "Additional terms under section 7 of the GNU General Public License apply to Helia and to every work based on it: the copyright line and the dedication must be preserved, and modified versions must be marked as changed and must not be presented as the original or as made or endorsed by Laurence Horrocks-Barlow. See ADDITIONAL-TERMS.md.";

/// The licence text.
pub const LICENCE_URL: &str = "https://www.gnu.org/licenses/gpl-3.0.html";

/// The licence paragraph from the GPL's "How to Apply" section.
pub const LICENCE_NOTICE: &str = "Helia is free software: you can redistribute it and/or modify it under the \
terms of the GNU General Public License as published by the Free Software \
Foundation, either version 3 of the License, or (at your option) any later \
version.";

/// The no-warranty paragraph from the GPL's "How to Apply" section.
pub const WARRANTY_NOTICE: &str = "Helia is distributed in the hope that it will be useful, but WITHOUT ANY \
WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR \
A PARTICULAR PURPOSE. See the GNU General Public License for more details.";

/// How a binary was built, recorded by `build.rs`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuildInfo {
    /// Short git commit hash, or `unknown` outside a git checkout.
    pub commit: &'static str,
    /// The working tree had uncommitted changes to tracked files.
    pub dirty: bool,
    /// Build time in UTC (`YYYY-MM-DD HH:MM UTC`); `SOURCE_DATE_EPOCH` applies.
    pub built: &'static str,
    /// Cargo profile (`debug` or `release`).
    pub profile: &'static str,
    /// Target triple.
    pub target: &'static str,
    /// Output of `rustc --version`.
    pub rustc: &'static str,
}

impl BuildInfo {
    /// The commit with `+dirty` when built from a modified tree.
    #[must_use]
    pub fn commit_label(&self) -> String {
        if self.dirty {
            format!("{}+dirty", self.commit)
        } else {
            self.commit.to_owned()
        }
    }
}

/// The build record of this crate (and so of the workspace build it is in).
#[must_use]
pub fn build_info() -> BuildInfo {
    BuildInfo {
        commit: env!("HELIA_BUILD_COMMIT"),
        dirty: matches!(env!("HELIA_BUILD_DIRTY").as_bytes(), b"true"),
        built: env!("HELIA_BUILD_DATE"),
        profile: env!("HELIA_BUILD_PROFILE"),
        target: env!("HELIA_BUILD_TARGET"),
        rustc: env!("HELIA_BUILD_RUSTC"),
    }
}

/// This build is a pre-release: the workspace version is below 1.0.0 (or has a
/// pre-release suffix). Decided once, in `build.rs`; every place that says
/// "pre-release" reads this.
pub const PRE_RELEASE: bool = matches!(env!("HELIA_PRE_RELEASE").as_bytes(), b"true");

/// The word shown beside the name and in the version block.
pub const PRE_RELEASE_LABEL: &str = "pre-release";

/// `version` with ` (pre-release)` after it when this build is one, for
/// window titles: `0.3.0 (pre-release)`.
#[must_use]
pub fn display_version(version: &str) -> String {
    if PRE_RELEASE {
        format!("{version} ({PRE_RELEASE_LABEL})")
    } else {
        version.to_owned()
    }
}

/// The window title: `Helia 0.3.0 (pre-release)`.
#[must_use]
pub fn window_title(version: &str) -> String {
    format!("{NAME} {}", display_version(version))
}

/// The version lines shared by `--version`, the About page's copy button and
/// bug reports. `gpu` is the active adapter or backend when known.
#[must_use]
pub fn version_block(binary: &str, version: &str, wgpu: &str, gpu: Option<&str>) -> String {
    let b = build_info();
    let mut s = format!(
        "{binary} {version}{}\ncommit: {}\nbuilt: {}\nprofile: {}\ntarget: {}\nrust: {}\nwgpu: {wgpu}",
        if PRE_RELEASE {
            format!("\nrelease: {PRE_RELEASE_LABEL}")
        } else {
            String::new()
        },
        b.commit_label(),
        b.built,
        b.profile,
        b.target,
        b.rustc,
    );
    if let Some(g) = gpu {
        s.push_str("\ngpu: ");
        s.push_str(g);
    }
    s
}

/// The dedication in full, as in the README and the book.
pub const DEDICATION_FULL: &str = "Helia is dedicated to my daughter, the light of my life. Her middle name is Helia, because she has an energetic spark. She has not stayed still since the day she was born, which suits software whose whole job is catching the sharp moments in something that never stops moving.";

/// Text for `--version`: name and version on the first line (what scripts
/// compare), then the build record, copyright, licence and warranty notices.
#[must_use]
pub fn long_version(binary: &str, version: &str, wgpu: &str) -> String {
    let block = version_block(binary, version, wgpu, None);
    format!(
        "{block}\n\n{COPYRIGHT}\nLicence: {LICENCE_ID}\n\n{DEDICATION_FULL}\n\n{LICENCE_NOTICE}\n\n{WARRANTY_NOTICE}\n\n{ADDITIONAL_TERMS_NOTICE}\nTerms: {ADDITIONAL_TERMS_URL}\n\nSource: {SOURCE_URL}\nMore from {AUTHOR}: {HOBBIES_URL}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collapse whitespace so a wrapped paragraph compares equal to one line.
    fn flat(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn the_author_line_and_dedication_cannot_be_dropped() {
        // ADDITIONAL-TERMS.md term 1 needs them in the source notices and the
        // program's legal notices: this fails if any copy loses them.
        let terms = flat(include_str!("../../../ADDITIONAL-TERMS.md"));
        let readme = flat(include_str!("../../../README.md"));
        let intro = flat(include_str!("../../../docs/book/src/index.md"));
        for (name, text) in [
            ("ADDITIONAL-TERMS.md", &terms),
            ("README.md", &readme),
            ("index.md", &intro),
        ] {
            assert!(text.contains(COPYRIGHT), "{name} lacks the copyright line");
            assert!(
                text.contains(DEDICATION_FULL),
                "{name} lacks the dedication"
            );
        }
        let version = long_version("helia", "1.0.0", "1");
        for needle in [
            COPYRIGHT,
            DEDICATION_FULL,
            WARRANTY_NOTICE,
            LICENCE_ID,
            "ADDITIONAL-TERMS.md",
        ] {
            assert!(version.contains(needle), "--version lacks {needle}");
        }
        assert!(ADDITIONAL_TERMS_NOTICE.contains("dedication"));
    }

    #[test]
    fn pre_release_follows_the_version_and_is_said_in_the_version_block() {
        // One constant decides; below 1.0.0 means pre-release.
        let major: u32 = env!("CARGO_PKG_VERSION_MAJOR").parse().expect("numeric");
        assert_eq!(
            PRE_RELEASE,
            major == 0 || !env!("CARGO_PKG_VERSION_PRE").is_empty()
        );
        let block = version_block("helia", "0.3.0", "1", None);
        assert_eq!(block.contains("\nrelease: pre-release\n"), PRE_RELEASE);
        assert_eq!(block.lines().next(), Some("helia 0.3.0"));
        assert_eq!(
            window_title("0.3.0"),
            if PRE_RELEASE {
                "Helia 0.3.0 (pre-release)"
            } else {
                "Helia 0.3.0"
            }
        );
        assert_eq!(
            display_version("2.0.0").contains("pre-release"),
            PRE_RELEASE
        );
    }

    #[test]
    fn dirty_commit_is_labelled() {
        let mut b = build_info();
        b.commit = "abc12345";
        b.dirty = true;
        assert_eq!(b.commit_label(), "abc12345+dirty");
        b.dirty = false;
        assert_eq!(b.commit_label(), "abc12345");
    }

    #[test]
    fn first_line_is_name_and_version() {
        let text = long_version("helia", "1.2.3", "30.0.0");
        assert_eq!(text.lines().next(), Some("helia 1.2.3"));
        assert!(text.contains("\ncommit: "));
        assert!(text.contains("\nwgpu: 30.0.0"));
        assert!(text.contains(COPYRIGHT));
        assert!(text.contains("WITHOUT ANY WARRANTY"));
        assert!(!text.contains('@'), "no email address in notices");
    }
}
