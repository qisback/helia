// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The example plugins in `examples/plugins`, run through the real host: the Rust one built by
//! the workspace and the two Python ones. A missing Python or Rust build is a skip with a
//! message, unless `HELIA_REQUIRE_PLUGIN_EXAMPLES` is set (CI sets it).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::float_cmp,
    clippy::assert_is_empty,
    missing_docs
)]

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use helia_core::frame::Frame;
use helia_plugin::{Approvals, Registry, Session};
use serde_json::json;

fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins")
}

fn required() -> bool {
    std::env::var_os("HELIA_REQUIRE_PLUGIN_EXAMPLES").is_some()
}

fn skip(why: &str) -> bool {
    assert!(!required(), "{why}");
    eprintln!("skipped: {why}");
    true
}

fn have_python() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn ramp(w: usize, h: usize) -> Frame {
    Frame::new(w, h, (0..w * h).map(|i| i as f32).collect()).unwrap()
}

fn session(dir: &Path, name: &str) -> Session {
    let mut reg = Registry::discover(None, &[dir.to_path_buf()], Approvals::in_memory());
    let l = reg.get(name).unwrap().clone();
    reg.approvals.approve(&l).unwrap();
    Session::start(&reg, name).unwrap()
}

fn invert_checks(s: &mut Session) {
    let cancel = AtomicBool::new(false);
    let mut fractions = Vec::new();
    let out = s
        .image_op(
            "invert",
            &json!({}),
            &ramp(8, 6),
            "post-edit",
            &cancel,
            &mut |p| fractions.extend(p.fraction),
        )
        .unwrap();
    assert_eq!((out.width, out.height), (8, 6));
    // max + min - v on a ramp 0..47
    assert_eq!(out.data[0], 47.0);
    assert_eq!(out.data[47], 0.0);
    assert_eq!(out.data[10], 37.0);
    assert!(!fractions.is_empty(), "progress was reported");
}

#[test]
fn python_invert_runs_end_to_end() {
    if !have_python() && skip("python3 is not installed") {
        return;
    }
    let mut s = session(&examples().join("invert-py"), "invert-py");
    invert_checks(&mut s);
    s.close();
}

#[test]
fn python_stats_reports_metrics_and_overlays() {
    if !have_python() && skip("python3 is not installed") {
        return;
    }
    let mut s = session(&examples().join("stats-py"), "stats-py");
    let cancel = AtomicBool::new(false);
    let mut img = Frame::new(32, 32, vec![0.0; 32 * 32]).unwrap();
    for y in 8..24 {
        for x in 4..20 {
            img.data[y * 32 + x] = 100.0;
        }
    }
    let a = s
        .analyse(
            "stats",
            &json!({"threshold": 0.5}),
            &img,
            &cancel,
            &mut |_| {},
        )
        .unwrap();
    assert_eq!(a.metrics["max"], 100.0);
    assert_eq!(a.metrics["min"], 0.0);
    assert!((a.metrics["mean"] - 100.0 * 256.0 / 1024.0).abs() < 1e-3);
    assert!(a.overlays.iter().any(|o| o["kind"] == "rect"));
    assert!(!a.notes.is_empty());
    s.close();
}

#[test]
fn python_stats_refuses_a_threshold_outside_its_range() {
    if !have_python() && skip("python3 is not installed") {
        return;
    }
    let mut s = session(&examples().join("stats-py"), "stats-py");
    let cancel = AtomicBool::new(false);
    let r = s.analyse(
        "stats",
        &json!({"threshold": 7}),
        &ramp(4, 4),
        &cancel,
        &mut |_| {},
    );
    assert!(r.is_err());
    s.close();
}

/// Build the Rust example (a plain `cargo test` does not build another package's program).
fn build_rust_example() -> Option<PathBuf> {
    let cargo = std::env::var_os("CARGO")?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let ok = std::process::Command::new(cargo)
        .args(["build", "-p", "helia-example-invert"])
        .current_dir(&root)
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        return None;
    }
    let name = if cfg!(windows) {
        "helia-example-invert.exe"
    } else {
        "helia-example-invert"
    };
    // .../target/debug/deps/examples-HASH  ->  .../target/debug/helia-example-invert
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.parent()?.join(name)).filter(|p| p.is_file())
}

#[test]
fn rust_invert_runs_end_to_end() {
    let Some(built) = build_rust_example() else {
        skip("the Rust example could not be built");
        return;
    };
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("invert");
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = std::fs::read_to_string(examples().join("invert/plugin.toml")).unwrap();
    let want = if cfg!(windows) {
        "invert.exe"
    } else {
        "invert"
    };
    std::fs::write(
        dir.join("plugin.toml"),
        manifest.replace(
            "executable = \"invert\"",
            &format!("executable = \"{want}\""),
        ),
    )
    .unwrap();
    std::fs::copy(&built, dir.join(want)).unwrap();
    let mut s = session(&dir, "invert");
    invert_checks(&mut s);
    s.close();
}

#[test]
fn every_example_manifest_loads() {
    for d in ["invert-py", "stats-py"] {
        let l = helia_plugin::Loaded::load(&examples().join(d));
        assert!(l.is_ok(), "{d}: {:?}", l.err());
    }
}
