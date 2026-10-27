// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
#![allow(clippy::unwrap_used, reason = "tests")]

//! The shipped item list (`packaging/external/helia-items.json`) built the way the release
//! builds it must parse, be offered on every platform Helia ships for, and name only hosts
//! that Helia's source allows. Needs `python3`; without it the test says so and passes.

use std::path::Path;
use std::process::Command;

use helia_external::{Manifest, Route, helia};
use helia_update::Platform;

#[test]
fn the_shipped_items_build_parse_and_use_allowed_hosts() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = tempfile::tempdir().unwrap();
    let file = out.path().join("external.json");
    let run = Command::new("python3")
        .arg(root.join("packaging/external/build_manifest.py"))
        .args(["--items"])
        .arg(root.join("packaging/external/helia-items.json"))
        .args(["--sequence", "1", "--date", "2026-10-26T12:00:00Z", "--out"])
        .arg(&file)
        .output();
    let Ok(run) = run else {
        eprintln!("python3 not found: skipping");
        return;
    };
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );

    let bytes = std::fs::read(&file).unwrap();
    let m = Manifest::parse(&bytes, false).unwrap();
    assert_eq!(m.app, helia::APP);
    assert_eq!(m.skipped, 0);

    let source = helia::source();
    for p in [
        Platform::WindowsX86_64,
        Platform::LinuxX86_64,
        Platform::LinuxAarch64,
        Platform::MacosAarch64,
        Platform::MacosX86_64,
    ] {
        let offers = m.offers("ffmpeg", Some(p)).unwrap();
        let dl = offers
            .iter()
            .find(|o| matches!(o.route, Route::Download(_)))
            .unwrap();
        let Route::Download(parts) = &dl.route else {
            unreachable!()
        };
        // Both programs are provided, from hosts the app allows.
        let provided: Vec<_> = parts
            .iter()
            .flat_map(|d| d.members.iter().filter_map(|m| m.provides.clone()))
            .collect();
        assert!(provided.contains(&"ffmpeg".to_string()), "{p:?}");
        assert!(provided.contains(&"ffprobe".to_string()), "{p:?}");
        for d in parts {
            assert!(source.asset_host_allowed(&d.url), "{}", d.url);
        }
    }
}
