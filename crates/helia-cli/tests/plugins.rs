// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `helia plugins` and the plugin options of `helia stack` and `helia edit`, end to end with
//! small Python plugins. Skipped with a message when `python3` is missing, unless
//! `HELIA_REQUIRE_PLUGIN_EXAMPLES` is set (CI sets it).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use helia_core::testutil::{
    CaptureConfig, SyntheticCapture, SyntheticConfig, SyntheticSun, ser_bytes,
};
use predicates::prelude::*;

fn python() -> bool {
    let ok = std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(
        ok || std::env::var_os("HELIA_REQUIRE_PLUGIN_EXAMPLES").is_none(),
        "python3 is required"
    );
    if !ok {
        eprintln!("skipped: python3 is not installed");
    }
    ok
}

fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins")
}

/// `helia` with its own empty plugin settings folder.
fn helia(config: &Path) -> Command {
    let mut c = Command::cargo_bin("helia").expect("helia binary");
    c.env("HELIA_CONFIG_DIR", config);
    c.timeout(std::time::Duration::from_secs(300));
    c
}

fn capture(dir: &Path) -> PathBuf {
    let sun = SyntheticSun::new(&SyntheticConfig {
        width: 128,
        height: 112,
        ..SyntheticConfig::default()
    });
    let cap = SyntheticCapture::generate(
        &sun,
        &CaptureConfig {
            frames: 20,
            noise: 0.02,
            ..CaptureConfig::default()
        },
    );
    let path = dir.join("sun.ser");
    std::fs::write(&path, ser_bytes(&cap.frames)).expect("write ser");
    path
}

/// A plugin offering every capability, in a few lines of Python.
const KIT: &str = r#"
import json, struct, sys, os, array
H = struct.Struct("<4sIIIII")
def rd(w, r):
    raw = open(os.path.join(w, r["path"]), "rb").read()
    _, _, x, y, c, _ = H.unpack_from(raw)
    a = array.array("f"); a.frombytes(raw[24:]); return x, y, c, a
def wr(w, o, x, y, c, a):
    p = os.path.join(w, o["path"])
    open(p, "wb").write(H.pack(b"HLPX", 1, x, y, c, 0) + a.tobytes())
    return {"path": o["path"], "format": "hlpx", "width": x, "height": y, "channels": c}
wd = None
for line in sys.stdin:
    m = json.loads(line)
    if m.get("method") == "exit": break
    if "id" not in m: continue
    p = m.get("params") or {}
    meth = m["method"]
    if meth == "initialize":
        wd = p["workdir"]
        res = {"api_version": 1, "name": "kit", "version": "1",
               "capabilities": ["stack-backend", "frame-op", "image-op", "analyse"],
               "steps": ["half", "plus", "mean"]}
    elif meth == "stack.run":
        a = array.array("f", [float(i % 64) * 100 for i in range(64 * 64)])
        sys.stderr.write("job " + json.dumps(p["job"]) + "\n")
        res = {"output": wr(wd, p["output"], 64, 64, 1, a), "note": "kit stacked " + os.path.basename(p["input"])}
    elif meth in ("frame.process", "image.process"):
        x, y, c, a = rd(wd, p["input"])
        k = float((p.get("params") or {}).get("k", 1))
        if p["step"] == "half": a = array.array("f", (v * 0.5 for v in a))
        else: a = array.array("f", (v + k for v in a))
        res = {"output": wr(wd, p["output"], x, y, c, a)}
    elif meth == "analyse.run":
        x, y, c, a = rd(wd, p["input"])
        res = {"metrics": {"mean": sum(a) / len(a)}, "overlays": [], "notes": ["kit looked"]}
    elif meth == "shutdown":
        res = None
    else:
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": m["id"], "error": {"code": -32601, "message": "no"}}) + "\n"); sys.stdout.flush(); continue
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": m["id"], "result": res}) + "\n"); sys.stdout.flush()
"#;

const KIT_TOML: &str = r#"
name = "kit"
version = "1"
api_version = 1
capabilities = ["stack-backend", "frame-op", "image-op", "analyse"]
executable = "kit.py"
interpreter = "python3"

[[steps]]
id = "half"
capability = "frame-op"
[[steps]]
id = "plus"
capability = "image-op"
params_schema = "plus.json"
[[steps]]
id = "mean"
capability = "analyse"
"#;

fn kit(dir: &Path) -> PathBuf {
    let d = dir.join("kit");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("kit.py"), KIT).unwrap();
    std::fs::write(d.join("plugin.toml"), KIT_TOML).unwrap();
    std::fs::write(
        d.join("plus.json"),
        r#"{"type":"object","properties":{"k":{"type":"number","minimum":0,"maximum":1000,"default":1}}}"#,
    )
    .unwrap();
    d
}

#[test]
fn list_shows_a_found_plugin_as_off_and_enable_needs_a_yes() {
    if !python() {
        return;
    }
    let t = tempfile::tempdir().unwrap();
    let cfg = t.path().join("cfg");
    let kit = kit(t.path());
    helia(&cfg)
        .args(["--plugin"])
        .arg(&kit)
        .args(["plugins", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("kit"))
        .stdout(predicate::str::contains("off"));
    // No terminal and no --yes: it must not enable by itself.
    helia(&cfg)
        .arg("--plugin")
        .arg(&kit)
        .args(["plugins", "enable", "kit"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--yes"));
    helia(&cfg)
        .arg("--plugin")
        .arg(&kit)
        .args(["plugins", "enable", "kit", "--yes"])
        .assert()
        .success()
        .stderr(predicate::str::contains("runs with your rights"))
        .stdout(predicate::str::contains("is on"));
    helia(&cfg)
        .arg("--plugin")
        .arg(&kit)
        .args(["plugins", "test", "kit"])
        .assert()
        .success()
        .stdout(predicate::str::contains("offers no check"));
    helia(&cfg)
        .arg("--plugin")
        .arg(&kit)
        .args(["plugins", "list", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"on\""));
    helia(&cfg)
        .arg("--plugin")
        .arg(&kit)
        .args(["plugins", "disable", "kit"])
        .assert()
        .success();
}

#[test]
fn a_changed_program_is_switched_off_until_approved_again() {
    if !python() {
        return;
    }
    let t = tempfile::tempdir().unwrap();
    let cfg = t.path().join("cfg");
    let kit = kit(t.path());
    let run = |args: &[&str]| {
        let mut c = helia(&cfg);
        c.arg("--plugin").arg(&kit).args(args);
        c
    };
    run(&["plugins", "enable", "kit", "--yes"])
        .assert()
        .success();
    std::fs::write(kit.join("kit.py"), format!("{KIT}\n# edited\n")).unwrap();
    run(&["plugins", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("changed"));
    run(&["plugins", "test", "kit"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("changed since you approved it"));
}

#[test]
fn a_plugin_that_is_off_stops_the_run_before_any_work() {
    if !python() {
        return;
    }
    let t = tempfile::tempdir().unwrap();
    let cfg = t.path().join("cfg");
    let kit = kit(t.path());
    let ser = capture(t.path());
    helia(&cfg)
        .arg("--plugin")
        .arg(&kit)
        .arg("stack")
        .arg(&ser)
        .arg("-o")
        .arg(t.path().join("out.tif"))
        .args(["--plugin-step", "kit/plus", "--quiet"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("is not turned on"))
        .stderr(predicate::str::contains("helia plugins enable"));
    assert!(!t.path().join("out.tif").exists());
}

#[test]
fn stack_runs_frame_image_and_analyse_steps() {
    if !python() {
        return;
    }
    let t = tempfile::tempdir().unwrap();
    let cfg = t.path().join("cfg");
    let kit = kit(t.path());
    let ser = capture(t.path());
    helia(&cfg)
        .arg("--plugin")
        .arg(&kit)
        .args(["plugins", "enable", "kit", "--yes"])
        .assert()
        .success();
    let out = t.path().join("out.tif");
    helia(&cfg)
        .arg("--plugin")
        .arg(&kit)
        .arg("stack")
        .arg(&ser)
        .arg("-o")
        .arg(&out)
        .args([
            "--plugin-step",
            "kit/half",
            "--plugin-step",
            "pre:kit/plus:k=5",
            "--plugin-step",
            "kit/mean",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("kit/half (frame-op"))
        .stderr(predicate::str::contains("kit/mean"));
    assert!(out.is_file());
    let analysis = std::fs::read_to_string(t.path().join("out.analysis.json")).unwrap();
    assert!(analysis.contains("\"mean\""), "{analysis}");
    assert!(analysis.contains("kit looked"), "{analysis}");
}

#[test]
fn a_parameter_out_of_range_is_refused_up_front() {
    if !python() {
        return;
    }
    let t = tempfile::tempdir().unwrap();
    let cfg = t.path().join("cfg");
    let kit = kit(t.path());
    let ser = capture(t.path());
    helia(&cfg)
        .arg("--plugin")
        .arg(&kit)
        .args(["plugins", "enable", "kit", "--yes"])
        .assert()
        .success();
    helia(&cfg)
        .arg("--plugin")
        .arg(&kit)
        .arg("stack")
        .arg(&ser)
        .arg("-o")
        .arg(t.path().join("out.tif"))
        .args(["--plugin-step", "kit/plus:k=99999", "--quiet"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not right"));
}

#[test]
fn stack_backend_replaces_the_local_stack_and_gets_the_job() {
    if !python() {
        return;
    }
    let t = tempfile::tempdir().unwrap();
    let cfg = t.path().join("cfg");
    let kit = kit(t.path());
    let ser = capture(t.path());
    helia(&cfg)
        .arg("--plugin")
        .arg(&kit)
        .args(["plugins", "enable", "kit", "--yes"])
        .assert()
        .success();
    let out = t.path().join("remote.tif");
    helia(&cfg)
        .arg("--plugin")
        .arg(&kit)
        .arg("stack")
        .arg(&ser)
        .arg("-o")
        .arg(&out)
        .args(["--stack-backend", "kit", "--keep", "40%"])
        .assert()
        .success()
        .stderr(predicate::str::contains("kit stacked sun.ser"))
        .stderr(predicate::str::contains("by plugin kit"));
    let (img, _) = helia_core::io::any::read_any(&out).expect("readable output");
    assert_eq!(img.size(), (64, 64));
    // Settings a backend cannot honour are refused in plain words.
    helia(&cfg)
        .arg("--plugin")
        .arg(&kit)
        .arg("stack")
        .arg(&ser)
        .arg("-o")
        .arg(t.path().join("x.tif"))
        .args(["--stack-backend", "kit", "--report"])
        .arg(t.path().join("rep"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("--report"));
}

#[test]
fn edit_runs_an_image_step_and_refuses_a_frame_step() {
    if !python() {
        return;
    }
    let t = tempfile::tempdir().unwrap();
    let cfg = t.path().join("cfg");
    let kit = kit(t.path());
    let ser = capture(t.path());
    helia(&cfg)
        .arg("--plugin")
        .arg(&kit)
        .args(["plugins", "enable", "kit", "--yes"])
        .assert()
        .success();
    let stacked = t.path().join("s.tif");
    helia(&cfg)
        .arg("stack")
        .arg(&ser)
        .arg("-o")
        .arg(&stacked)
        .arg("--quiet")
        .assert()
        .success();
    let out = t.path().join("e.tif");
    helia(&cfg)
        .arg("--plugin")
        .arg(&kit)
        .arg("edit")
        .arg(&stacked)
        .arg("-o")
        .arg(&out)
        .args(["--plugin-step", "kit/plus:k=100", "--quiet"])
        .assert()
        .success();
    assert!(out.is_file());
    helia(&cfg)
        .arg("--plugin")
        .arg(&kit)
        .arg("edit")
        .arg(&stacked)
        .arg("-o")
        .arg(&out)
        .args(["--plugin-step", "kit/half", "--quiet"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("before stacking"));
}

#[test]
fn the_python_examples_list_and_enable() {
    if !python() {
        return;
    }
    let t = tempfile::tempdir().unwrap();
    let cfg = t.path().join("cfg");
    for name in ["invert-py", "stats-py"] {
        helia(&cfg)
            .arg("--plugin")
            .arg(examples().join(name))
            .args(["plugins", "enable", name, "--yes"])
            .assert()
            .success();
        helia(&cfg)
            .arg("--plugin")
            .arg(examples().join(name))
            .args(["plugins", "info", name])
            .assert()
            .success()
            .stdout(predicate::str::contains("sha256:"))
            .stdout(predicate::str::contains("unsigned"));
    }
}

#[test]
fn settings_are_checked_and_secrets_stay_hidden() {
    if !python() {
        return;
    }
    let t = tempfile::tempdir().unwrap();
    let cfg = t.path().join("cfg");
    let kit = kit(t.path());
    // `settings_schema` must sit before the first table; rebuild the manifest in order.
    let manifest = format!(
        "settings_schema = \"settings.json\"\n{}",
        KIT_TOML.trim_start()
    );
    std::fs::write(kit.join("plugin.toml"), manifest).unwrap();
    std::fs::write(
        kit.join("settings.json"),
        r#"{"type":"object","properties":{"server":{"type":"string","default":"x"},"token":{"type":"string","format":"password"},"port":{"type":"integer","minimum":1,"maximum":65535}}}"#,
    )
    .unwrap();
    let run = |args: &[&str]| {
        let mut c = helia(&cfg);
        c.arg("--plugin").arg(&kit).args(args);
        c
    };
    run(&["plugins", "set", "kit", "port=99999"])
        .assert()
        .failure();
    run(&["plugins", "set", "kit", "port=7871", "token=s3cret"])
        .assert()
        .success();
    run(&["plugins", "info", "kit"])
        .assert()
        .success()
        .stdout(predicate::str::contains("7871"))
        .stdout(predicate::str::contains("s3cret").not())
        .stdout(predicate::str::contains("(hidden)"));
}

#[test]
fn the_file_says_which_plugin_made_it() {
    if !python() {
        return;
    }
    let t = tempfile::tempdir().unwrap();
    let cfg = t.path().join("cfg");
    let kit = kit(t.path());
    let ser = capture(t.path());
    helia(&cfg)
        .arg("--plugin")
        .arg(&kit)
        .args(["plugins", "enable", "kit", "--yes"])
        .assert()
        .success();
    let out = t.path().join("out.fits");
    helia(&cfg)
        .arg("--plugin")
        .arg(&kit)
        .arg("stack")
        .arg(&ser)
        .arg("-o")
        .arg(&out)
        .args(["--plugin-step", "pre:kit/plus:k=1", "--quiet"])
        .assert()
        .success();
    let bytes = std::fs::read(&out).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("Derived by plugin kit 1, program SHA-256 "),
        "no provenance in the FITS history"
    );
}
