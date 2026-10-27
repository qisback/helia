// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
#![allow(
    clippy::unwrap_used,
    clippy::many_single_char_names,
    clippy::too_many_lines,
    reason = "tests"
)]

//! The `helia-manifest` tool end to end with throwaway keys.

use std::path::Path;
use std::process::Command;

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_helia-manifest"))
        .args(args)
        .output()
        .unwrap()
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

#[test]
fn build_sign_verify_and_tamper() {
    let dir = std::env::temp_dir().join(format!("helia-manifest-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let asset = dir.join("Helia-0.8.0-x86_64.AppImage");
    std::fs::write(&asset, b"pretend appimage").unwrap();
    assert!(
        run(&["keygen-for-tests", "--out-dir", s(&dir)])
            .status
            .success()
    );
    let (key, publ, man) = (
        dir.join("test.key"),
        dir.join("test.pub"),
        dir.join("manifest.json"),
    );

    let spec = format!("linux-x86_64:appimage:{}", s(&asset));
    let b = run(&[
        "build",
        "--app",
        "helia",
        "--version",
        "0.8.0",
        "--sequence",
        "7",
        "--date",
        "2026-10-24T18:00:00Z",
        "--expires-days",
        "30",
        "--base-url",
        "https://downloads.example.org/helia/0.8.0/",
        "--asset",
        &spec,
        "--out",
        s(&man),
    ]);
    assert!(b.status.success(), "{}", String::from_utf8_lossy(&b.stderr));
    let text = std::fs::read_to_string(&man).unwrap();
    assert!(text.contains("https://downloads.example.org/helia/0.8.0/Helia-0.8.0-x86_64.AppImage"));
    assert!(text.contains("2026-11-23T18:00:00Z"));

    // Signing needs a key; the secret never appears in the output.
    let out = run(&["sign", "--manifest", s(&man), "--secret-key-file", s(&key)]);
    assert!(out.status.success());
    let secret = std::fs::read_to_string(&key).unwrap();
    let secret_line = secret.lines().nth(1).unwrap();
    assert!(!String::from_utf8_lossy(&out.stdout).contains(secret_line));
    assert!(!String::from_utf8_lossy(&out.stderr).contains(secret_line));
    assert!(
        !run(&["sign", "--manifest", s(&man)]).status.success(),
        "no key given"
    );

    let v = run(&[
        "verify",
        "--manifest",
        s(&man),
        "--key",
        s(&publ),
        "--at",
        "2026-10-25T00:00:00Z",
    ]);
    assert!(v.status.success(), "{}", String::from_utf8_lossy(&v.stderr));
    // After expiry, and after tampering, verification fails.
    assert!(
        !run(&[
            "verify",
            "--manifest",
            s(&man),
            "--key",
            s(&publ),
            "--at",
            "2027-01-01T00:00:00Z"
        ])
        .status
        .success()
    );
    std::fs::write(&man, text.replace("\"sequence\": 7", "\"sequence\": 8")).unwrap();
    assert!(
        !run(&[
            "verify",
            "--manifest",
            s(&man),
            "--key",
            s(&publ),
            "--at",
            "2026-10-25T00:00:00Z"
        ])
        .status
        .success()
    );
    std::fs::remove_dir_all(&dir).ok();
}
