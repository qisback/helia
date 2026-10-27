// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The real `helia` binary and `helia tools`: the parts that need no network. A copy
//! installed with Helia is found without any environment variable, an environment variable
//! still wins, and an install without a terminal and without `--accept-licence` is refused
//! before any request.

#![cfg(unix)]

use assert_cmd::Command;
use predicates::prelude::*;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

fn helia(dir: &Path) -> Command {
    let mut c = Command::cargo_bin("helia").expect("helia binary");
    c.env("HELIA_EXTERNAL_DIR", dir)
        .env_remove("HELIA_FFMPEG")
        .env_remove("HELIA_FFPROBE")
        .env("PATH", dir.join("empty-path"));
    c
}

fn fake_program(path: &Path, banner: &str) {
    std::fs::write(path, format!("#!/bin/sh\necho '{banner}'\n")).expect("write");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// A tools folder as `helia tools install` leaves it.
fn installed_copy(dir: &Path) {
    let item = dir.join("tools").join("ffmpeg");
    std::fs::create_dir_all(&item).expect("mkdir");
    std::fs::create_dir_all(dir.join("empty-path")).expect("mkdir");
    fake_program(&item.join("ffmpeg"), "ffmpeg version 9.0.2 test");
    fake_program(&item.join("ffprobe"), "ffprobe version 9.0.2 test");
    let rec = serde_json::json!({
        "item": "ffmpeg", "name": "FFmpeg", "version": "9.0.2", "provider": "Test",
        "provider_url": null, "licence_id": "lgpl-3.0",
        "licence_name": "GNU Lesser General Public License, version 3",
        "licence_sha256": "0".repeat(64),
        "licence_source_url": "https://www.gnu.org/licenses/lgpl-3.0.html",
        "attribution": "FFmpeg is a trademark of Fabrice Bellard.", "show_attribution": true,
        "installed_at": 1_792_864_800u64, "route": "download", "command": null,
        "files": [
            {"name": "ffmpeg", "provides": "ffmpeg", "sha256": "0".repeat(64), "size": 26},
            {"name": "ffprobe", "provides": "ffprobe", "sha256": "0".repeat(64), "size": 28},
        ],
    });
    std::fs::write(item.join("installed.json"), rec.to_string()).expect("write");
}

#[test]
fn an_installed_copy_is_found_with_no_environment_variable() {
    let d = tempfile::tempdir().expect("tempdir");
    installed_copy(d.path());
    helia(d.path())
        .args(["tools", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("FFmpeg 9.0.2 (download)"))
        .stdout(predicate::str::contains("(installed with Helia)"))
        .stdout(predicate::str::contains("trademark of Fabrice Bellard"));
}

#[test]
fn the_environment_variable_wins_over_the_installed_copy() {
    let d = tempfile::tempdir().expect("tempdir");
    installed_copy(d.path());
    let mine = d.path().join("my-ffmpeg");
    fake_program(&mine, "ffmpeg version 8.0 mine");
    helia(d.path())
        .env("HELIA_FFMPEG", &mine)
        .args(["tools", "status", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"from\": \"from HELIA_FFMPEG\""));
}

#[test]
fn without_any_copy_status_says_how_to_get_one() {
    let d = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(d.path().join("empty-path")).expect("mkdir");
    helia(d.path())
        .args(["tools", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("helia tools install ffmpeg"));
}

#[test]
fn remove_of_something_not_installed_explains() {
    let d = tempfile::tempdir().expect("tempdir");
    helia(d.path())
        .args(["tools", "remove", "ffmpeg", "--yes"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("not installed with Helia"));
}

#[test]
fn the_help_documents_the_exit_code_and_the_licence_step() {
    Command::cargo_bin("helia")
        .expect("helia binary")
        .args(["tools", "install", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("type `yes`"))
        .stdout(predicate::str::contains("--accept-licence"))
        .stdout(predicate::str::contains("3"));
}
