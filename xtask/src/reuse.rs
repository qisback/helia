// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `cargo xtask reuse-check`: every tracked file must carry licensing.
//!
//! Source files (Rust, WGSL, Python, shell, PowerShell, YAML) need an in-file
//! `SPDX-FileCopyrightText` and `SPDX-License-Identifier` header near the top.
//! Everything else must be covered by an annotation in `REUSE.toml`. The
//! licence texts under `LICENSES/` are exempt, as in the REUSE specification.
//!
//! Helia carries additional terms under GPLv3 section 7 (`ADDITIONAL-TERMS.md`):
//! each header must point to them, and the author's copyright line and the
//! dedication must stay in the README, the book introduction and the terms file.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Extensions whose files carry their own header.
const HEADER_EXTENSIONS: &[&str] = &["rs", "wgsl", "py", "sh", "ps1", "yml", "yaml"];

/// The pointer every header carries to the additional terms.
const TERMS_POINTER: &str = "ADDITIONAL-TERMS.md";

/// The author attribution that the additional terms require to be preserved.
const COPYRIGHT_LINE: &str = "Copyright (C) 2026 Laurence Horrocks-Barlow";

/// The dedication that the additional terms require to be preserved.
const DEDICATION: &str = "Helia is dedicated to my daughter, the light of my life. Her middle name is Helia, because she has an energetic spark. She has not stayed still since the day she was born, which suits software whose whole job is catching the sharp moments in something that never stops moving.";

/// Files that must carry the copyright line and the dedication.
const NOTICE_FILES: &[&str] = &[
    "README.md",
    "ADDITIONAL-TERMS.md",
    "docs/book/src/index.md",
    "crates/helia-core/src/about.rs",
];

/// How far into a file the header must appear.
const HEADER_WINDOW: usize = 1024;

/// Does `glob` (`*` within a segment, `**` across segments, `?` one character)
/// match the repository-relative `path`?
fn glob_match(glob: &str, path: &str) -> bool {
    fn go(g: &[u8], p: &[u8]) -> bool {
        match g.first() {
            None => p.is_empty(),
            Some(b'*') if g.get(1) == Some(&b'*') => {
                // `**/` also matches zero directories.
                let rest = match g.get(2) {
                    Some(b'/') => &g[3..],
                    _ => &g[2..],
                };
                (0..=p.len()).any(|i| go(rest, &p[i..]))
            }
            Some(b'*') => {
                let end = p.iter().position(|&c| c == b'/').unwrap_or(p.len());
                (0..=end).any(|i| go(&g[1..], &p[i..]))
            }
            Some(b'?') => p.first().is_some_and(|&c| c != b'/') && go(&g[1..], &p[1..]),
            Some(&c) => p.first() == Some(&c) && go(&g[1..], &p[1..]),
        }
    }
    go(glob.as_bytes(), path.as_bytes())
}

/// The path globs of every `[[annotations]]` that carries both SPDX fields.
fn annotation_globs(root: &Path) -> Result<Vec<String>> {
    let text =
        std::fs::read_to_string(root.join("REUSE.toml")).context("cannot read REUSE.toml")?;
    let doc: toml::Table = text.parse().context("REUSE.toml is not valid TOML")?;
    let mut globs = Vec::new();
    let annotations = doc
        .get("annotations")
        .and_then(toml::Value::as_array)
        .context("REUSE.toml has no [[annotations]]")?;
    for a in annotations {
        let has = |k: &str| {
            a.get(k)
                .is_some_and(|v| !v.to_string().trim_matches('"').is_empty())
        };
        if !has("SPDX-FileCopyrightText") || !has("SPDX-License-Identifier") {
            bail!(
                "a REUSE.toml annotation lacks SPDX-FileCopyrightText or SPDX-License-Identifier"
            );
        }
        match a.get("path") {
            Some(toml::Value::String(s)) => globs.push(s.clone()),
            Some(toml::Value::Array(v)) => {
                globs.extend(v.iter().filter_map(|x| x.as_str().map(str::to_owned)));
            }
            _ => bail!("a REUSE.toml annotation has no path"),
        }
    }
    Ok(globs)
}

/// Does the head of `text` hold a complete SPDX header?
fn has_header(text: &str) -> bool {
    let mut end = text.len().min(HEADER_WINDOW);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let head = &text[..end];
    let value = |tag: &str| {
        head.lines()
            .find_map(|l| l.split_once(tag).map(|(_, v)| v.trim().to_owned()))
            .is_some_and(|v| !v.is_empty())
    };
    value("SPDX-FileCopyrightText:")
        && value("SPDX-License-Identifier:")
        && head.contains(TERMS_POINTER)
}

/// Collapse whitespace, so a paragraph wrapped in a Markdown file compares equal.
fn flat(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Names of the files in `NOTICE_FILES` that lost the copyright line or the dedication.
fn missing_notices(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for rel in NOTICE_FILES {
        let text = std::fs::read_to_string(root.join(rel))
            .map(|t| flat(&t))
            .unwrap_or_default();
        if !text.contains(COPYRIGHT_LINE) {
            out.push(format!("{rel}: the copyright line is missing"));
        }
        if !text.contains(DEDICATION) {
            out.push(format!("{rel}: the dedication is missing"));
        }
    }
    out
}

/// Fail listing every tracked (or untracked, unignored) file without licensing.
pub fn check(root: &Path) -> Result<()> {
    let out = Command::new("git")
        .args(["ls-files", "--cached", "--others", "--exclude-standard"])
        .current_dir(root)
        .output()
        .context("cannot run git ls-files")?;
    if !out.status.success() {
        bail!("git ls-files failed");
    }
    let globs = annotation_globs(root)?;
    let mut missing = Vec::new();
    let mut checked = 0usize;
    for rel in String::from_utf8(out.stdout)?.lines() {
        if rel.starts_with("LICENSES/") || !root.join(rel).is_file() {
            continue;
        }
        checked += 1;
        let ext = rel.rsplit_once('.').map_or("", |(_, e)| e);
        if HEADER_EXTENSIONS.contains(&ext) {
            let bytes = std::fs::read(root.join(rel))?;
            if !has_header(&String::from_utf8_lossy(&bytes)) {
                missing.push(format!(
                    "{rel}: no SPDX header, or no pointer to {TERMS_POINTER}"
                ));
            }
        } else if !globs.iter().any(|g| glob_match(g, rel)) {
            missing.push(format!("{rel}: not covered by REUSE.toml"));
        }
    }
    missing.extend(missing_notices(root));
    if !missing.is_empty() {
        bail!(
            "{} file(s) without licensing information:\n  {}\nadd the SPDX header and terms pointer (see CONTRIBUTING.md), a REUSE.toml annotation, or restore the copyright line and dedication",
            missing.len(),
            missing.join("\n  ")
        );
    }
    println!("reuse-check: {checked} files, all licensed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{DEDICATION, flat, glob_match, has_header, missing_notices};

    #[test]
    fn globs() {
        assert!(glob_match("*.md", "README.md"));
        assert!(!glob_match("*.md", "docs/a.md"));
        assert!(glob_match("docs/**", "docs/book/src/a.md"));
        assert!(glob_match("**/Cargo.toml", "Cargo.toml"));
        assert!(glob_match("**/Cargo.toml", "crates/x/Cargo.toml"));
        assert!(glob_match("a/IBMPlex*.ttf", "a/IBMPlexMono-Regular.ttf"));
        assert!(!glob_match("a/*.ttf", "a/b/c.ttf"));
    }

    #[test]
    fn headers() {
        let pointer = "// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.\n";
        let bare =
            "// SPDX-FileCopyrightText: 2026 A B\n// SPDX-License-Identifier: GPL-3.0-or-later\n";
        let ok = format!("{bare}{pointer}");
        assert!(has_header(&ok));
        assert!(!has_header(bare), "the terms pointer is required");
        assert!(!has_header(
            "// SPDX-License-Identifier: GPL-3.0-or-later\n"
        ));
        assert!(!has_header(
            "// SPDX-FileCopyrightText:\n// SPDX-License-Identifier: X\n"
        ));
        assert!(!has_header("fn main() {}\n"));
    }

    #[test]
    fn notices_survive_wrapping_and_fail_when_dropped() {
        let dir = std::env::temp_dir().join(format!("helia-notices-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("docs/book/src")).expect("dirs");
        std::fs::create_dir_all(dir.join("crates/helia-core/src")).expect("dirs");
        let wrapped = DEDICATION.replace(
            ", ", ",
",
        );
        let good = format!(
            "Copyright (C) 2026 Laurence Horrocks-Barlow

{wrapped}
"
        );
        for f in [
            "README.md",
            "ADDITIONAL-TERMS.md",
            "docs/book/src/index.md",
            "crates/helia-core/src/about.rs",
        ] {
            std::fs::write(dir.join(f), &good).expect("write");
        }
        assert!(
            missing_notices(&dir).is_empty(),
            "{:?}",
            missing_notices(&dir)
        );
        std::fs::write(
            dir.join("README.md"),
            "Copyright (C) 2026 Laurence Horrocks-Barlow
",
        )
        .expect("write");
        let bad = missing_notices(&dir);
        assert_eq!(bad, ["README.md: the dedication is missing"]);
        assert_eq!(
            flat(
                " a 
 b "
            ),
            "a b"
        );
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }
}
