// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Records how this build was made (commit, tree state, UTC date, profile,
//! target, compiler) for `about::build_info`. Every value degrades to
//! `unknown` when it cannot be found, for example in a source tarball.

use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn run(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// Days since 1970-01-01 to a civil (year, month, day), proleptic Gregorian.
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");

    let commit =
        run("git", &["rev-parse", "--short=8", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = run("git", &["status", "--porcelain", "--untracked-files=no"]).is_some();
    // A new commit moves HEAD (or the branch ref it names): rebuild to record it.
    if let Some(dir) = run("git", &["rev-parse", "--absolute-git-dir"]) {
        let dir = PathBuf::from(dir);
        println!("cargo:rerun-if-changed={}", dir.join("HEAD").display());
        if let Some(r) = run("git", &["symbolic-ref", "-q", "HEAD"]) {
            let common = run("git", &["rev-parse", "--git-common-dir"]).map(PathBuf::from);
            for base in [Some(dir.clone()), common].into_iter().flatten() {
                println!("cargo:rerun-if-changed={}", base.join(&r).display());
            }
        }
    }

    let secs = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
        });
    let (y, m, d) = civil(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    let built = format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        rem / 3600,
        rem % 3600 / 60
    );

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let rustc_version = run(&rustc, &["--version"]).unwrap_or_else(|| "unknown".into());

    // Pre-release while the major version is 0 (semantic versioning's "anything
    // may change" range). The one place that decides it.
    let pre_release = std::env::var("CARGO_PKG_VERSION_MAJOR").is_ok_and(|m| m == "0")
        || std::env::var("CARGO_PKG_VERSION_PRE").is_ok_and(|p| !p.is_empty());

    for (k, v) in [
        ("HELIA_PRE_RELEASE", pre_release.to_string()),
        ("HELIA_BUILD_COMMIT", commit),
        ("HELIA_BUILD_DIRTY", dirty.to_string()),
        ("HELIA_BUILD_DATE", built),
        (
            "HELIA_BUILD_PROFILE",
            std::env::var("PROFILE").unwrap_or_else(|_| "unknown".into()),
        ),
        (
            "HELIA_BUILD_TARGET",
            std::env::var("TARGET").unwrap_or_else(|_| "unknown".into()),
        ),
        ("HELIA_BUILD_RUSTC", rustc_version),
    ] {
        println!("cargo:rustc-env={k}={v}");
    }
}
