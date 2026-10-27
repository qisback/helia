// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
#![allow(clippy::unwrap_used, reason = "tests")]

//! Unpack a REAL provider archive and run the program. Manual: it needs a download of
//! several hundred megabytes, so it is ignored unless asked for.
//!
//! ```text
//! HELIA_EXTERNAL_REAL_ARCHIVE=/path/ffmpeg-...-linux64-lgpl-9.0.tar.xz \
//!   cargo test -p helia-external --test real_archive -- --ignored
//! ```
//! The archive kind comes from the file name (`.zip` or `.tar.xz`).

use std::path::PathBuf;
use std::process::Command;

use helia_external::archive::extract;
use helia_external::{ArchiveKind, Member};

#[test]
#[ignore = "needs a real provider archive; see the module comment"]
fn a_real_archive_unpacks_and_the_program_runs() {
    let Some(path) = std::env::var_os("HELIA_EXTERNAL_REAL_ARCHIVE").map(PathBuf::from) else {
        eprintln!("HELIA_EXTERNAL_REAL_ARCHIVE is not set: skipping");
        return;
    };
    let name = path.to_string_lossy().to_string();
    let (kind, exe) = if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
    {
        (ArchiveKind::Zip, name.contains("win64"))
    } else {
        (ArchiveKind::TarXz, false)
    };
    let e = if exe { ".exe" } else { "" };
    let members = ["ffmpeg", "ffprobe"].map(|n| Member {
        find: format!("bin/{n}{e}"),
        save_as: format!("{n}{e}"),
        provides: Some(n.into()),
        executable: true,
    });
    let out = tempfile::tempdir().unwrap();
    let t = std::time::Instant::now();
    let got = extract(&path, kind, &members, out.path()).unwrap();
    eprintln!("unpacked {} files in {:?}", got.len(), t.elapsed());
    assert_eq!(got.len(), 2);
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) && name.contains("linux64") {
        let v = Command::new(out.path().join("ffprobe"))
            .arg("-version")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&v.stdout).to_string();
        assert!(text.starts_with("ffprobe version"), "{text}");
        eprintln!("{}", text.lines().next().unwrap());
    }
}
