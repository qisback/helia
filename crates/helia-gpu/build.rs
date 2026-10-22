// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Records the version of wgpu this build links, read from the workspace
//! `Cargo.lock`, for `WGPU_VERSION`. `unknown` when the lock file is absent.

use std::path::PathBuf;

fn wgpu_version(lock: &str) -> Option<String> {
    let mut lines = lock.lines();
    while let Some(l) = lines.next() {
        if l.trim() == "name = \"wgpu\"" {
            let v = lines.next()?.trim();
            return v
                .strip_prefix("version = \"")
                .and_then(|v| v.strip_suffix('"'))
                .map(str::to_owned);
        }
    }
    None
}

fn main() {
    let lock = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default())
        .join("../../Cargo.lock");
    println!("cargo:rerun-if-changed={}", lock.display());
    let version = std::fs::read_to_string(&lock)
        .ok()
        .and_then(|t| wgpu_version(&t))
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=HELIA_WGPU_VERSION={version}");
}
