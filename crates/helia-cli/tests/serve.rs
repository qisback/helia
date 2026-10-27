// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `helia serve` as a real process: start on an ephemeral port, call it, stop it.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

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

fn start(args: &[&str]) -> Serve {
    // The child's stderr goes to a file so a server that never prints its address can be
    // reported with what it said (issue #185).
    let err_path = std::env::temp_dir().join(format!(
        "helia-serve-{}-{:?}.err",
        std::process::id(),
        std::thread::current().id()
    ));
    let err = std::fs::File::create(&err_path).expect("stderr file");
    let mut child = Command::new(assert_cmd::cargo::cargo_bin("helia"))
        .arg("serve")
        .args(["--bind", "127.0.0.1:0"])
        .args(args)
        .stdout(Stdio::piped())
        .stderr(err)
        .spawn()
        .expect("spawn helia serve");
    let stdout = child.stdout.take().expect("stdout");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let _ = reader.read_line(&mut line);
        let _ = tx.send(line);
        // Keep reading until the server exits. Closing the pipe after the first line
        // breaks the server's next write (its "data root:" line), and a server that
        // dies there refuses or resets the test's connection: a race that fails under load.
        let mut sink = String::new();
        while reader.read_line(&mut sink).is_ok_and(|n| n > 0) {
            sink.clear();
        }
    });
    let Ok(line) = rx.recv_timeout(Duration::from_secs(60)) else {
        let _ = child.kill();
        let _ = child.wait();
        let said = std::fs::read_to_string(&err_path).unwrap_or_default();
        panic!("helia serve printed no address in 60 s; stderr:\n{said}");
    };
    let _ = std::fs::remove_file(&err_path);
    let addr = line
        .trim()
        .strip_prefix("listening on http://")
        .unwrap_or_else(|| panic!("unexpected first line {line:?}"))
        .to_owned();
    Serve { child, addr }
}

fn http_get(addr: &str, path: &str, auth: Option<&str>) -> String {
    let mut s = TcpStream::connect(addr).expect("connect");
    // `/version` starts the GPU to name it, which takes tens of seconds when the machine is
    // swamped (a loaded CI host): generous, and a timeout is reported as one rather than
    // read as an empty answer (issue #185).
    s.set_read_timeout(Some(Duration::from_secs(180)))
        .expect("timeout");
    let auth = auth.map_or(String::new(), |t| format!("Authorization: Bearer {t}\r\n"));
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\n{auth}Connection: close\r\n\r\n"
    )
    .expect("write");
    // A reset after the response is complete is fine; keep what arrived.
    let mut buf = Vec::new();
    if let Err(e) = s.read_to_end(&mut buf) {
        assert!(
            !matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ),
            "GET {path}: no answer in 180 s"
        );
    }
    String::from_utf8_lossy(&buf).into_owned()
}

#[test]
fn serve_answers_health_and_stops() {
    let dir = tempfile::tempdir().expect("tempdir");
    let srv = start(&["--data-root", dir.path().to_str().expect("utf8")]);
    let res = http_get(&srv.addr, "/api/v1/health", None);
    assert!(res.starts_with("HTTP/1.1 200"), "{res}");
    assert!(res.contains(r#""status":"ok""#), "{res}");
    let res = http_get(&srv.addr, "/api/v1/version", None);
    assert!(res.starts_with("HTTP/1.1 200"), "{res}");
    assert!(res.contains(r#""cpu""#), "{res}");
}

#[test]
fn serve_enforces_the_token_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let tok = dir.path().join("token");
    std::fs::write(&tok, "file-token\n").expect("write");
    let srv = start(&[
        "--data-root",
        dir.path().to_str().expect("utf8"),
        "--token-file",
        tok.to_str().expect("utf8"),
    ]);
    // Each answer is printed when it is not the expected one: a bare `false` says nothing.
    for (path, token, status) in [
        ("/api/v1/health", None, "HTTP/1.1 200"),
        ("/api/v1/version", None, "HTTP/1.1 401"),
        ("/api/v1/version", Some("file-token"), "HTTP/1.1 200"),
    ] {
        let res = http_get(&srv.addr, path, token);
        assert!(res.starts_with(status), "{path} with {token:?}: {res:?}");
    }
}

/// A `helia` that is meant to exit by itself, killed if it does not (issue #185).
fn refusal(args: &[&str]) -> std::process::Output {
    let out = assert_cmd::Command::cargo_bin("helia")
        .expect("helia binary")
        .timeout(Duration::from_secs(120))
        .args(args)
        .env_remove("HELIA_TOKEN")
        .output()
        .expect("run");
    assert!(
        out.status.code().is_some(),
        "helia {args:?} hung and was killed\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

#[test]
fn serve_refuses_a_public_bind_without_a_token() {
    let out = refusal(&["serve", "--bind", "0.0.0.0:0"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("token"));
}

#[test]
fn serve_refuses_a_missing_data_root() {
    let out = refusal(&["serve", "--data-root", "definitely/not/here"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("data root"));
}
