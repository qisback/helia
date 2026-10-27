// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The whole protocol end to end: the host starts the testbed plugin and talks to it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::assert_is_empty,
    clippy::format_push_string,
    clippy::many_single_char_names,
    missing_docs
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use helia_core::frame::Frame;
use helia_plugin::{Approvals, HostError, Kind, Loaded, Progress, Registry, Session, Status};
use serde_json::json;

fn testbed() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_helia-plugin-testbed"))
}

const STEPS: &[(&str, &str)] = &[
    ("invert", "image-op"),
    ("grow", "image-op"),
    ("slow", "image-op"),
    ("quiet", "image-op"),
    ("hang", "image-op"),
    ("crash", "image-op"),
    ("garbage", "image-op"),
    ("bigline", "image-op"),
    ("badsize", "image-op"),
    ("traverse", "image-op"),
    ("fail", "image-op"),
    ("addn", "frame-op"),
    ("stats", "analyse"),
];

fn make(root: &Path, timeout_s: u64) -> PathBuf {
    let d = root.join("testbed");
    std::fs::create_dir_all(&d).unwrap();
    let exe = d.join(if cfg!(windows) {
        "testbed.exe"
    } else {
        "testbed"
    });
    std::fs::copy(testbed(), &exe).unwrap();
    let mut m = format!(
        "name = \"testbed\"\nversion = \"0.1.0\"\napi_version = 1\ncapabilities = [\"stack-backend\", \"frame-op\", \"image-op\", \"analyse\"]\nexecutable = \"{}\"\nsettings_schema = \"settings.json\"\n[limits]\ntimeout_s = {timeout_s}\n[resources]\nmemory_mb = 64\n",
        exe.file_name().unwrap().to_str().unwrap()
    );
    for (id, cap) in STEPS {
        m.push_str(&format!(
            "[[steps]]\nid = \"{id}\"\ncapability = \"{cap}\"\n"
        ));
        if *id == "addn" {
            m.push_str("params_schema = \"addn.json\"\n");
        }
    }
    std::fs::write(d.join("plugin.toml"), m).unwrap();
    std::fs::write(
        d.join("settings.json"),
        r#"{"type":"object","properties":{"who":{"type":"string","default":"world"}}}"#,
    )
    .unwrap();
    std::fs::write(
        d.join("addn.json"),
        r#"{"type":"object","properties":{"add":{"type":"number","minimum":-100,"maximum":100,"default":1}}}"#,
    )
    .unwrap();
    d
}

fn registry(root: &Path, enable: bool) -> Registry {
    let mut r = Registry::discover(Some(root), &[], Approvals::in_memory());
    if enable {
        let l = r.get("testbed").unwrap().clone();
        r.approvals.approve(&l).unwrap();
    }
    r
}

fn ramp(w: usize, h: usize) -> Frame {
    Frame::new(w, h, (0..w * h).map(|i| i as f32).collect()).unwrap()
}

fn never() -> AtomicBool {
    AtomicBool::new(false)
}

fn start(root: &Path) -> Session {
    Session::start(&registry(root, true), "testbed").unwrap()
}

fn image_op(s: &mut Session, step: &str, cancel: &AtomicBool) -> Result<Frame, HostError> {
    s.image_op(
        step,
        &json!({}),
        &ramp(4, 3),
        "post-edit",
        cancel,
        &mut |_| {},
    )
}

#[test]
fn handshake_and_capabilities() {
    let t = tempfile::tempdir().unwrap();
    make(t.path(), 30);
    let s = start(t.path());
    assert_eq!(s.info.name, "testbed");
    assert_eq!(s.info.api_version, 1);
    assert_eq!(s.info.capabilities.len(), 4);
}

#[test]
fn image_op_inverts_and_reports_progress() {
    let t = tempfile::tempdir().unwrap();
    make(t.path(), 30);
    let mut s = start(t.path());
    let mut seen: Vec<Progress> = Vec::new();
    let out = s
        .image_op(
            "invert",
            &json!({}),
            &ramp(4, 3),
            "post-edit",
            &never(),
            &mut |p| seen.push(p),
        )
        .unwrap();
    assert_eq!(out.data[0], 11.0);
    assert_eq!(out.data[11], 0.0);
    assert!(seen.len() >= 5, "progress was forwarded: {}", seen.len());
    assert!(
        seen.iter()
            .all(|p| p.fraction.is_some_and(|f| (0.0..=1.0).contains(&f)))
    );
    // Working files are cleaned up.
    let left = std::fs::read_dir(s.workdir()).unwrap().count();
    assert_eq!(left, 0);
}

#[test]
fn image_op_can_change_size() {
    let t = tempfile::tempdir().unwrap();
    make(t.path(), 30);
    let mut s = start(t.path());
    let out = image_op(&mut s, "grow", &never()).unwrap();
    assert_eq!((out.width, out.height), (8, 6));
}

#[test]
fn frame_op_keeps_size_and_checks_params() {
    let t = tempfile::tempdir().unwrap();
    make(t.path(), 30);
    let mut s = start(t.path());
    let out = s
        .frame_op("addn", &json!({"add": 5}), &ramp(4, 3), 0, 10, &never())
        .unwrap();
    assert_eq!(out.data[0], 5.0);
    let e = s
        .frame_op("addn", &json!({"add": 1000}), &ramp(4, 3), 0, 10, &never())
        .unwrap_err();
    assert_eq!(e.kind, Kind::Refused);
    assert!(e.to_string().contains("at most 100"), "{e}");
}

#[test]
fn analyse_returns_metrics_and_overlays() {
    let t = tempfile::tempdir().unwrap();
    make(t.path(), 30);
    let mut s = start(t.path());
    let a = s
        .analyse("stats", &json!({}), &ramp(4, 3), &never(), &mut |_| {})
        .unwrap();
    assert_eq!(a.metrics["max"], 11.0);
    assert_eq!(a.overlays.len(), 1);
    assert_eq!(a.notes, vec!["testbed"]);
}

#[test]
fn stack_backend_returns_a_frame_and_a_note() {
    let t = tempfile::tempdir().unwrap();
    make(t.path(), 30);
    let mut s = start(t.path());
    let mut fractions = Vec::new();
    let r = s
        .stack(
            Path::new("/captures/sun.ser"),
            &json!({"keep": "25%"}),
            &never(),
            &mut |p| fractions.push(p.fraction),
        )
        .unwrap();
    assert_eq!((r.frame.width, r.frame.height), (8, 8));
    assert_eq!(r.note.as_deref(), Some("stacked /captures/sun.ser"));
    assert!(!fractions.is_empty());
}

#[test]
fn test_button_and_settings() {
    let t = tempfile::tempdir().unwrap();
    make(t.path(), 30);
    let mut r = registry(t.path(), true);
    r.approvals
        .set_settings("testbed", &json!({"who": "Helia"}));
    let mut s = Session::start(&r, "testbed").unwrap();
    let res = s.test(&never()).unwrap();
    assert!(res.ok);
    assert_eq!(res.message, "Hello Helia");
}

#[test]
fn bad_settings_are_refused_before_starting() {
    let t = tempfile::tempdir().unwrap();
    make(t.path(), 30);
    let mut r = registry(t.path(), true);
    r.approvals.set_settings("testbed", &json!({"who": 5}));
    let e = Session::start(&r, "testbed").err().unwrap();
    assert_eq!(e.kind, Kind::Refused);
}

#[test]
fn plugin_error_keeps_its_plain_english() {
    let t = tempfile::tempdir().unwrap();
    make(t.path(), 30);
    let mut s = start(t.path());
    let e = image_op(&mut s, "fail", &never()).unwrap_err();
    assert_eq!(
        e.kind,
        Kind::Plugin(helia_plugin_sdk::ErrorKind::Unavailable)
    );
    assert_eq!(e.what, "I could not reach the stacking server.");
    assert_eq!(e.todo, "Check that the tunnel is open, then try again.");
    // The session is still usable afterwards.
    assert!(image_op(&mut s, "invert", &never()).is_ok());
}

#[test]
fn cancel_stops_a_cooperative_plugin_quickly() {
    let t = tempfile::tempdir().unwrap();
    make(t.path(), 30);
    let mut s = start(t.path());
    let flag = Arc::new(AtomicBool::new(false));
    let f2 = Arc::clone(&flag);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        f2.store(true, Ordering::Relaxed);
    });
    let t0 = Instant::now();
    let e = image_op(&mut s, "slow", &flag).unwrap_err();
    assert!(e.is_cancelled(), "{e}");
    // Generous: a loaded CI host can stall for seconds; a hang would be minutes.
    assert!(t0.elapsed() < Duration::from_secs(20), "{:?}", t0.elapsed());
    // The plugin stayed alive and can work again.
    assert!(image_op(&mut s, "invert", &never()).is_ok());
}

#[test]
fn cancel_kills_a_plugin_that_ignores_it() {
    let t = tempfile::tempdir().unwrap();
    make(t.path(), 30);
    let mut s = start(t.path());
    let flag = Arc::new(AtomicBool::new(false));
    let f2 = Arc::clone(&flag);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        f2.store(true, Ordering::Relaxed);
    });
    let t0 = Instant::now();
    let e = image_op(&mut s, "hang", &flag).unwrap_err();
    assert!(e.is_cancelled(), "{e}");
    let took = t0.elapsed();
    assert!(
        took >= Duration::from_secs(5) && took < Duration::from_secs(25),
        "{took:?}"
    );
    assert!(s.is_dead());
}

#[test]
fn a_crash_is_reported_in_plain_english() {
    let t = tempfile::tempdir().unwrap();
    make(t.path(), 30);
    let mut s = start(t.path());
    let e = image_op(&mut s, "crash", &never()).unwrap_err();
    assert_eq!(e.kind, Kind::Crashed);
    let text = e.to_string();
    assert!(text.contains("stopped unexpectedly"), "{text}");
    assert!(text.contains("code 3"), "{text}");
    assert!(text.contains("boom"), "the log tail is quoted: {text}");
    // A later call says so instead of hanging.
    assert_eq!(
        image_op(&mut s, "invert", &never()).unwrap_err().kind,
        Kind::Crashed
    );
}

#[test]
fn a_silent_plugin_times_out() {
    let t = tempfile::tempdir().unwrap();
    make(t.path(), 1);
    let mut s = start(t.path());
    let t0 = Instant::now();
    let e = image_op(&mut s, "hang", &never()).unwrap_err();
    assert_eq!(e.kind, Kind::Timeout, "{e}");
    assert!(e.to_string().contains("1 seconds"));
    assert!(t0.elapsed() < Duration::from_secs(30));
}

#[test]
fn non_protocol_output_is_a_protocol_error() {
    let t = tempfile::tempdir().unwrap();
    make(t.path(), 30);
    let mut s = start(t.path());
    let e = image_op(&mut s, "garbage", &never()).unwrap_err();
    assert_eq!(e.kind, Kind::Protocol, "{e}");
}

#[test]
fn an_overlong_line_is_refused_without_buffering_it() {
    let t = tempfile::tempdir().unwrap();
    make(t.path(), 30);
    let mut s = start(t.path());
    let e = image_op(&mut s, "bigline", &never()).unwrap_err();
    assert_eq!(e.kind, Kind::Protocol, "{e}");
    assert!(e.to_string().contains("longer than"), "{e}");
}

#[test]
fn a_result_whose_size_lies_is_refused() {
    let t = tempfile::tempdir().unwrap();
    make(t.path(), 30);
    let mut s = start(t.path());
    let e = image_op(&mut s, "badsize", &never()).unwrap_err();
    assert_eq!(e.kind, Kind::BadResult, "{e}");
}

#[test]
fn a_result_path_outside_the_working_folder_is_refused() {
    let t = tempfile::tempdir().unwrap();
    make(t.path(), 30);
    let mut s = start(t.path());
    let e = image_op(&mut s, "traverse", &never()).unwrap_err();
    assert_eq!(e.kind, Kind::Protocol, "{e}");
}

#[test]
fn a_plugin_that_is_not_enabled_never_starts() {
    let t = tempfile::tempdir().unwrap();
    make(t.path(), 30);
    let r = registry(t.path(), false);
    assert_eq!(r.status("testbed").unwrap(), Status::NotEnabled);
    let e = Session::start(&r, "testbed").err().unwrap();
    assert_eq!(e.kind, Kind::NotEnabled);
    assert!(e.to_string().contains("helia plugins enable testbed"));
}

#[test]
fn a_changed_program_is_disabled_until_approved_again() {
    let t = tempfile::tempdir().unwrap();
    let d = make(t.path(), 30);
    let r = registry(t.path(), true);
    assert!(Session::start(&r, "testbed").is_ok());
    // Append a byte: the program no longer matches what was approved.
    let exe = d.join(if cfg!(windows) {
        "testbed.exe"
    } else {
        "testbed"
    });
    let mut bytes = std::fs::read(&exe).unwrap();
    bytes.push(0);
    std::fs::write(&exe, bytes).unwrap();
    let r2 = Registry::discover(Some(t.path()), &[], r.approvals.clone());
    assert!(matches!(r2.status("testbed").unwrap(), Status::Changed(_)));
    let e = Session::start(&r2, "testbed").err().unwrap();
    assert_eq!(e.kind, Kind::Changed);
    assert!(e.to_string().contains("enable it again"));
}

#[test]
fn a_changed_manifest_is_disabled_too() {
    let t = tempfile::tempdir().unwrap();
    let d = make(t.path(), 30);
    let r = registry(t.path(), true);
    let mut m = std::fs::read_to_string(d.join("plugin.toml")).unwrap();
    m.push_str("\n# edited\n");
    std::fs::write(d.join("plugin.toml"), m).unwrap();
    let r2 = Registry::discover(Some(t.path()), &[], r.approvals.clone());
    assert_eq!(
        r2.status("testbed").unwrap(),
        Status::Changed("manifest".into())
    );
}

#[test]
fn a_hash_that_disagrees_with_the_manifest_is_damaged() {
    let t = tempfile::tempdir().unwrap();
    let d = make(t.path(), 30);
    let mut m = std::fs::read_to_string(d.join("plugin.toml")).unwrap();
    m = m.replace(
        "api_version = 1\n",
        &format!("api_version = 1\nsha256 = \"{}\"\n", "0".repeat(64)),
    );
    std::fs::write(d.join("plugin.toml"), m).unwrap();
    let mut r = Registry::discover(Some(t.path()), &[], Approvals::in_memory());
    let l: Loaded = r.get("testbed").unwrap().clone();
    let e = r.approvals.approve(&l).unwrap_err();
    assert_eq!(e.kind, Kind::Damaged);
}

#[test]
fn disabling_stops_it_running() {
    let t = tempfile::tempdir().unwrap();
    make(t.path(), 30);
    let mut r = registry(t.path(), true);
    assert!(r.approvals.disable("testbed"));
    assert_eq!(
        Session::start(&r, "testbed").err().unwrap().kind,
        Kind::NotEnabled
    );
}

#[test]
fn close_ends_the_process() {
    let t = tempfile::tempdir().unwrap();
    make(t.path(), 30);
    let mut s = start(t.path());
    s.close();
    assert!(s.is_dead());
    assert_eq!(
        image_op(&mut s, "invert", &never()).unwrap_err().kind,
        Kind::Crashed
    );
}

#[test]
fn provenance_names_the_plugin_its_checksum_and_what_it_said() {
    let t = tempfile::tempdir().unwrap();
    make(t.path(), 30);
    let mut s = start(t.path());
    assert!(s.provenance().is_empty(), "nothing ran yet");
    s.stack(
        Path::new("/captures/sun.ser"),
        &json!({}),
        &never(),
        &mut |_| {},
    )
    .unwrap();
    image_op(&mut s, "invert", &never()).unwrap();
    let lines = s.provenance();
    assert!(
        lines[0].starts_with("Derived by plugin testbed 0.1.0, program SHA-256 "),
        "{lines:?}"
    );
    assert!(lines[0].ends_with("stack, invert"), "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("Plugin testbed stack:") && l.contains("\"model\":\"testbed\"")),
        "{lines:?}"
    );
}

#[test]
fn a_crash_always_quotes_the_last_words_even_when_stdout_closes_first() {
    // The end of stdout and the end of stderr arrive on different threads; the crash error must
    // wait for the second (bounded), not race it. Repeated, because the race is a matter of luck.
    let t = tempfile::tempdir().unwrap();
    make(t.path(), 30);
    for i in 0..30 {
        let mut s = start(t.path());
        let e = image_op(&mut s, "crash", &never()).unwrap_err();
        assert_eq!(e.kind, Kind::Crashed, "round {i}: {e}");
        assert!(e.to_string().contains("boom"), "round {i}: {e}");
    }
}

#[test]
fn many_fresh_copies_start_in_parallel_without_text_file_busy() {
    // Each thread copies the program and starts it at once, while the others fork: the race that
    // made `exec` fail with ETXTBSY. The host retries, so every start must succeed.
    let handles: Vec<_> = (0..24)
        .map(|_| {
            std::thread::spawn(|| {
                let t = tempfile::tempdir().unwrap();
                make(t.path(), 30);
                let mut s = start(t.path());
                s.close();
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
}
