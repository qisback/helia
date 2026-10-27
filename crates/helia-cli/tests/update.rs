// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `helia update`: the parts that need no network. The full check, apply and
//! rollback run against a local server in `tools/update-e2e.py`.

use assert_cmd::Command;
use predicates::prelude::*;

fn helia(dir: &std::path::Path) -> Command {
    let mut c = Command::cargo_bin("helia").expect("helia binary");
    c.env("HELIA_UPDATE_DIR", dir)
        .env_remove("APPIMAGE")
        .env_remove("FLATPAK_ID");
    c
}

#[test]
fn status_shows_the_defaults_and_makes_no_request() {
    let d = tempfile::tempdir().expect("tempdir");
    helia(d.path())
        .args(["update", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Mode: notify"))
        .stdout(predicate::str::contains("Channel: stable"))
        .stdout(predicate::str::contains("Last check: never"));
}

#[test]
fn config_saves_mode_and_channel_and_status_json_reads_them() {
    let d = tempfile::tempdir().expect("tempdir");
    helia(d.path())
        .args([
            "update",
            "config",
            "--mode",
            "off",
            "--channel",
            "prerelease",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("mode=off"))
        .stdout(predicate::str::contains("channel=prerelease"));
    let out = helia(d.path())
        .args(["update", "status", "--json"])
        .output()
        .expect("run");
    let text = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&text).expect("json");
    assert_eq!(v["mode"], "off");
    assert_eq!(v["channel"], "prerelease");
    let saved = std::fs::read_to_string(d.path().join("update.conf")).expect("update.conf");
    assert!(
        saved.contains("schema=1") && saved.contains("mode=off"),
        "{saved}"
    );
}

#[test]
fn a_settings_file_from_a_newer_helia_is_not_overwritten() {
    let d = tempfile::tempdir().expect("tempdir");
    let path = d.path().join("update.conf");
    std::fs::write(&path, "schema=9\nmode=off\n").expect("write");
    helia(d.path())
        .args(["update", "config", "--mode", "automatic"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("newer Helia"));
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "schema=9\nmode=off\n"
    );
    helia(d.path())
        .args(["update", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("newer Helia"));
}

#[test]
fn rollback_with_nothing_kept_says_so_and_fails() {
    let d = tempfile::tempdir().expect("tempdir");
    helia(d.path())
        .args(["update", "rollback", "--yes"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("no older version kept"));
}

#[test]
fn the_help_documents_the_exit_codes() {
    Command::cargo_bin("helia")
        .expect("helia binary")
        .args(["update", "check", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("10"));
}
