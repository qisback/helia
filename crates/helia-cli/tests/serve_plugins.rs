// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Plugin steps and a stack backend through the API: `helia serve` as a real process with a
//! plugin enabled in its own settings folder, a job posted, the summary read back. Skipped with a
//! message when `python3` is missing, unless `HELIA_REQUIRE_PLUGIN_EXAMPLES` is set.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use helia_core::testutil::{
    CaptureConfig, SyntheticCapture, SyntheticConfig, SyntheticSun, ser_bytes,
};
use serde_json::{Value, json};

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
               "capabilities": ["stack-backend", "image-op", "analyse"], "steps": ["plus", "mean"]}
    elif meth == "stack.run":
        a = array.array("f", [float(i % 64) / 64 for i in range(64 * 64)])
        res = {"output": wr(wd, p["output"], 64, 64, 1, a), "note": "kit stacked " + os.path.basename(p["input"])}
    elif meth == "image.process":
        x, y, c, a = rd(wd, p["input"])
        k = float((p.get("params") or {}).get("k", 1))
        a = array.array("f", (v + k for v in a))
        res = {"output": wr(wd, p["output"], x, y, c, a)}
    elif meth == "analyse.run":
        x, y, c, a = rd(wd, p["input"])
        res = {"metrics": {"mean": sum(a) / len(a)}, "overlays": [{"kind": "circle", "x": 3, "y": 4, "r": 5}], "notes": ["kit looked"]}
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
capabilities = ["stack-backend", "image-op", "analyse"]
executable = "kit.py"
interpreter = "python3"

[[steps]]
id = "plus"
capability = "image-op"
[[steps]]
id = "mean"
capability = "analyse"
"#;

fn have_python() -> bool {
    let ok = std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(
        ok || std::env::var_os("HELIA_REQUIRE_PLUGIN_EXAMPLES").is_none(),
        "python3 is required"
    );
    ok
}

struct Serve {
    child: Child,
    addr: String,
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(root: &Path, config: &Path) -> Serve {
    let mut child = Command::new(assert_cmd::cargo::cargo_bin("helia"))
        .arg("serve")
        .args(["--bind", "127.0.0.1:0", "--data-root"])
        .arg(root)
        .env("HELIA_CONFIG_DIR", config)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn helia serve");
    let mut reader = BufReader::new(child.stdout.take().expect("stdout"));
    let mut line = String::new();
    reader.read_line(&mut line).expect("first line");
    let addr = line
        .trim()
        .strip_prefix("listening on http://")
        .unwrap_or_else(|| panic!("unexpected first line {line:?}"))
        .to_owned();
    // Keep draining so the server never meets a closed pipe.
    std::thread::spawn(move || {
        let mut sink = String::new();
        while reader.read_line(&mut sink).is_ok_and(|n| n > 0) {
            sink.clear();
        }
    });
    Serve { child, addr }
}

fn http(addr: &str, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
    let mut s = TcpStream::connect(addr).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(120))).unwrap();
    let body = body.map(Value::to_string).unwrap_or_default();
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf).into_owned();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let json = text
        .split_once("\r\n\r\n")
        .and_then(|(_, b)| serde_json::from_str(b.trim()).ok())
        .unwrap_or(Value::Null);
    (status, json)
}

fn setup() -> (tempfile::TempDir, std::path::PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let config = t.path().join("cfg");
    let kit = config.join("plugins/kit");
    std::fs::create_dir_all(&kit).unwrap();
    std::fs::write(kit.join("kit.py"), KIT).unwrap();
    std::fs::write(kit.join("plugin.toml"), KIT_TOML).unwrap();
    let ok = assert_cmd::Command::cargo_bin("helia")
        .unwrap()
        .env("HELIA_CONFIG_DIR", &config)
        .args(["plugins", "enable", "kit", "--yes"])
        .output()
        .unwrap();
    assert!(ok.status.success(), "{ok:?}");
    let root = t.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let sun = SyntheticSun::new(&SyntheticConfig {
        width: 96,
        height: 80,
        ..SyntheticConfig::default()
    });
    let cap = SyntheticCapture::generate(
        &sun,
        &CaptureConfig {
            frames: 16,
            noise: 0.02,
            ..CaptureConfig::default()
        },
    );
    std::fs::write(root.join("sun.ser"), ser_bytes(&cap.frames)).unwrap();
    (t, config)
}

fn wait(addr: &str, id: u64) -> Value {
    let t0 = Instant::now();
    loop {
        let (_, v) = http(addr, "GET", &format!("/api/v1/jobs/{id}"), None);
        if matches!(
            v["state"].as_str(),
            Some("succeeded" | "failed" | "cancelled")
        ) {
            return v;
        }
        assert!(t0.elapsed() < Duration::from_secs(180), "stuck: {v}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn a_job_runs_plugin_steps_and_reports_what_analyse_found() {
    if !have_python() {
        eprintln!("skipped: python3 is not installed");
        return;
    }
    let (t, config) = setup();
    let srv = start(&t.path().join("root"), &config);
    let (s, v) = http(
        &srv.addr,
        "POST",
        "/api/v1/jobs",
        Some(&json!({
            "input": "sun.ser", "output": "out.fits",
            "config": {"backend": "cpu", "plugin_steps": ["pre:kit/plus:k=2", "kit/mean"]}
        })),
    );
    assert_eq!(s, 202, "{v}");
    let done = wait(&srv.addr, v["id"].as_u64().unwrap());
    assert_eq!(done["state"], "succeeded", "{done}");
    let p = &done["summary"]["plugins"][0];
    assert_eq!(p["plugin"], "kit");
    assert_eq!(p["step"], "mean");
    assert!(p["metrics"]["mean"].as_f64().unwrap() > 1.5, "{p}");
    assert_eq!(p["overlays"][0]["kind"], "circle");
    assert_eq!(p["notes"][0], "kit looked");
}

#[test]
fn a_stack_backend_replaces_the_servers_pipeline() {
    if !have_python() {
        return;
    }
    let (t, config) = setup();
    let srv = start(&t.path().join("root"), &config);
    let (s, v) = http(
        &srv.addr,
        "POST",
        "/api/v1/jobs",
        Some(&json!({
            "input": "sun.ser", "output": "remote.fits",
            "config": {"backend": "cpu", "stack_backend": "kit"}
        })),
    );
    assert_eq!(s, 202, "{v}");
    let done = wait(&srv.addr, v["id"].as_u64().unwrap());
    assert_eq!(done["state"], "succeeded", "{done}");
    assert_eq!(done["summary"]["stacked_by"], "kit");
    assert_eq!(done["summary"]["width"], 64);
}

#[test]
fn a_bad_plugin_request_is_a_422_before_anything_is_queued() {
    if !have_python() {
        return;
    }
    let (t, config) = setup();
    let srv = start(&t.path().join("root"), &config);
    for (cfg, want) in [
        (json!({"plugin_steps": ["nope/step"]}), "nope"),
        (
            json!({"plugin_steps": ["kit/nosuchstep"]}),
            "no step called",
        ),
        (json!({"plugin_steps": ["kit"]}), "plugin/step"),
        (json!({"stack_backend": "missing"}), "missing"),
        (json!({"stack_backend": "kit", "report": true}), "report"),
    ] {
        let (s, v) = http(
            &srv.addr,
            "POST",
            "/api/v1/jobs",
            Some(&json!({"input": "sun.ser", "output": "x.fits", "config": cfg})),
        );
        assert_eq!(s, 422, "{cfg} -> {v}");
        let msg = v["error"]["message"].as_str().unwrap_or_default();
        assert!(msg.contains(want), "{cfg}: {msg}");
    }
}
