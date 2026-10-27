// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
#![allow(
    clippy::unwrap_used,
    clippy::many_single_char_names,
    clippy::too_many_lines,
    reason = "tests"
)]

//! The real HTTP client against a local test server on 127.0.0.1: headers sent,
//! a man in the middle, redirects, plain http, and a resumed download.
#![cfg(feature = "client")]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;

use helia_update::check::{CheckOptions, Outcome, Updater};
use helia_update::manifest::{AssetDoc, ManifestDoc, Platform};
use helia_update::mem::{FixedClock, MemFs};
use helia_update::sign::KeyPair;
use helia_update::state::State;
use helia_update::ureq_http::UreqHttp;
use helia_update::verify::sha256_hex;
use helia_update::{Error, Kind, UpdateSource};

const NOW: u64 = 1_792_864_800;

#[derive(Clone)]
enum Route {
    Body(Vec<u8>),
    /// Declare the full length but close after this many bytes (first request only).
    CutOnce(Vec<u8>, usize),
    Redirect(String),
}

struct Server {
    port: u16,
    routes: Arc<Mutex<HashMap<String, Route>>>,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn start() -> Self {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let routes: Arc<Mutex<HashMap<String, Route>>> = Arc::default();
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let (r2, s2) = (routes.clone(), seen.clone());
        thread::spawn(move || {
            for s in l.incoming().flatten() {
                let (r, sn) = (r2.clone(), s2.clone());
                thread::spawn(move || serve(s, &r, &sn));
            }
        });
        Self { port, routes, seen }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    fn set(&self, path: &str, r: Route) {
        self.routes.lock().unwrap().insert(path.into(), r);
    }
}

fn serve(mut s: TcpStream, routes: &Mutex<HashMap<String, Route>>, seen: &Mutex<Vec<String>>) {
    let mut rd = BufReader::new(s.try_clone().unwrap());
    let mut head = String::new();
    loop {
        let mut line = String::new();
        if rd.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
        head.push_str(&line);
    }
    seen.lock().unwrap().push(head.clone());
    let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
    let range = head.lines().find_map(|l| {
        l.to_ascii_lowercase()
            .strip_prefix("range: bytes=")
            .map(|v| v.trim_end_matches('-').parse::<usize>().unwrap_or(0))
    });
    let route = routes.lock().unwrap().get(&path).cloned();
    let mut cut_once = None;
    let resp: Vec<u8> = match route {
        None => {
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
        }
        Some(Route::Redirect(to)) => format!(
            "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .into_bytes(),
        Some(Route::Body(b)) => reply(&b, range, None),
        Some(Route::CutOnce(b, n)) => {
            cut_once = Some(path.clone());
            reply(&b, range, if range.is_none() { Some(n) } else { None })
        }
    };
    if let Some(p) = cut_once {
        // Only the first request is cut.
        let mut g = routes.lock().unwrap();
        if let Some(Route::CutOnce(b, _)) = g.get(&p).cloned() {
            g.insert(p, Route::Body(b));
        }
    }
    let _ = s.write_all(&resp);
    let _ = s.flush();
    let mut sink = [0u8; 64];
    let _ = s.set_read_timeout(Some(std::time::Duration::from_millis(50)));
    let _ = s.read(&mut sink);
}

fn reply(body: &[u8], range: Option<usize>, cut: Option<usize>) -> Vec<u8> {
    let (status, start) = match range {
        Some(n) if n < body.len() => ("206 Partial Content", n),
        _ => ("200 OK", 0),
    };
    let part = &body[start..];
    let mut h = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
        part.len()
    );
    if start > 0 {
        use std::fmt::Write as _;
        let _ = write!(
            h,
            "Content-Range: bytes {start}-{}/{}",
            body.len() - 1,
            body.len()
        );
        h.push_str("\r\n");
    }
    h.push_str("\r\n");
    let mut out = h.into_bytes();
    out.extend_from_slice(&part[..cut.unwrap_or(part.len()).min(part.len())]);
    out
}

fn asset() -> Vec<u8> {
    (0..300_000u32).map(|i| (i * 7 % 253) as u8).collect()
}

fn manifest(srv: &Server, version: &str) -> Vec<u8> {
    let a = asset();
    let d = ManifestDoc {
        schema: 1,
        app: "app".into(),
        channel: "stable".into(),
        version: version.into(),
        date: helia_update::time::format_utc(NOW - 100),
        sequence: 3,
        expires: helia_update::time::format_utc(NOW + 100_000),
        min_supported: None,
        notes_url: None,
        assets: vec![AssetDoc {
            platform: "linux-x86_64".into(),
            kind: "appimage".into(),
            url: srv.url("/app/0.8.0/app.AppImage"),
            sha256: sha256_hex(&a),
            size: a.len() as u64,
        }],
    };
    serde_json::to_vec(&d).unwrap()
}

fn check(srv: &Server, k: &KeyPair, http: &UreqHttp) -> Result<Outcome, Error> {
    let fs = MemFs::new();
    let clock = FixedClock::new(NOW);
    let src = UpdateSource::new(
        "app",
        "stable",
        &srv.url("/app/stable/manifest.json"),
        &[&k.public_key()],
    );
    Updater {
        http,
        fs: &fs,
        clock: &clock,
        access: None,
    }
    .check(
        &src,
        "0.7.1",
        &mut State::new("app", "stable"),
        CheckOptions {
            allow_loopback_http: true,
            ..CheckOptions::default()
        },
    )
}

#[test]
fn only_the_documented_headers_are_sent() {
    let (srv, k) = (Server::start(), KeyPair::generate().unwrap());
    let m = manifest(&srv, "0.8.0");
    srv.set("/app/stable/manifest.json", Route::Body(m.clone()));
    srv.set(
        "/app/stable/manifest.json.minisig",
        Route::Body(k.sign(&m, "t").into_bytes()),
    );
    assert!(matches!(
        check(&srv, &k, &UreqHttp::new(true)).unwrap(),
        Outcome::Available(_)
    ));
    let seen = srv.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2, "exactly the manifest and its signature");
    for head in &seen {
        let mut names: Vec<String> = head
            .lines()
            .skip(1)
            .filter_map(|l| l.split(':').next())
            .map(str::to_ascii_lowercase)
            .collect();
        names.sort();
        assert_eq!(names, ["accept", "host", "user-agent"], "{head}");
        assert!(
            head.contains(&format!("user-agent: {}\r\n", helia_update::USER_AGENT)),
            "{head}"
        );
    }
    assert!(helia_update::USER_AGENT.starts_with("helia-update/"));
}

#[test]
fn man_in_the_middle_with_the_old_signature_is_rejected() {
    let (srv, k) = (Server::start(), KeyPair::generate().unwrap());
    let genuine = manifest(&srv, "0.8.0");
    let sig = k.sign(&genuine, "t");
    // The attacker swaps in a manifest offering a malicious version, keeps the old signature.
    let evil = String::from_utf8(manifest(&srv, "0.8.0"))
        .unwrap()
        .replace("0.8.0", "6.6.6")
        .into_bytes();
    srv.set("/app/stable/manifest.json", Route::Body(evil));
    srv.set(
        "/app/stable/manifest.json.minisig",
        Route::Body(sig.into_bytes()),
    );
    assert!(matches!(
        check(&srv, &k, &UreqHttp::new(true)),
        Err(Error::BadSignature)
    ));
}

#[test]
fn plain_http_is_refused_unless_loopback_is_allowed() {
    let (srv, k) = (Server::start(), KeyPair::generate().unwrap());
    let m = manifest(&srv, "0.8.0");
    srv.set("/app/stable/manifest.json", Route::Body(m.clone()));
    srv.set(
        "/app/stable/manifest.json.minisig",
        Route::Body(k.sign(&m, "t").into_bytes()),
    );
    // The production client (loopback not allowed) will not even try http.
    let fs = MemFs::new();
    let clock = FixedClock::new(NOW);
    let src = UpdateSource::new(
        "app",
        "stable",
        &srv.url("/app/stable/manifest.json"),
        &[&k.public_key()],
    );
    let r = Updater {
        http: &UreqHttp::new(false),
        fs: &fs,
        clock: &clock,
        access: None,
    }
    .check(
        &src,
        "0.7.1",
        &mut State::new("app", "stable"),
        CheckOptions::default(),
    );
    assert!(matches!(r, Err(Error::InsecureUrl(_))), "{r:?}");
    assert!(srv.seen.lock().unwrap().is_empty(), "no request was made");
}

#[test]
fn redirect_to_another_host_is_refused_by_the_real_client() {
    let (srv, k) = (Server::start(), KeyPair::generate().unwrap());
    srv.set(
        "/app/stable/manifest.json",
        Route::Redirect("https://evil.example.net/manifest.json".into()),
    );
    let r = check(&srv, &k, &UreqHttp::new(true));
    assert!(matches!(r, Err(Error::ForeignHost(_))), "{r:?}");
    // And one to another port is not a different host, but to localhost (another name) is.
    srv.set(
        "/app/stable/manifest.json",
        Route::Redirect(format!("http://localhost:{}/x", srv.port)),
    );
    assert!(matches!(
        check(&srv, &k, &UreqHttp::new(true)),
        Err(Error::InsecureUrl(_) | Error::ForeignHost(_))
    ));
}

#[test]
fn same_host_redirect_is_followed() {
    let (srv, k) = (Server::start(), KeyPair::generate().unwrap());
    let m = manifest(&srv, "0.8.0");
    srv.set(
        "/app/stable/manifest.json",
        Route::Redirect("/real/manifest.json".into()),
    );
    srv.set("/real/manifest.json", Route::Body(m.clone()));
    srv.set(
        "/app/stable/manifest.json.minisig",
        Route::Body(k.sign(&m, "t").into_bytes()),
    );
    assert!(matches!(
        check(&srv, &k, &UreqHttp::new(true)).unwrap(),
        Outcome::Available(_)
    ));
}

#[test]
fn real_download_is_cut_then_resumed() {
    let (srv, k) = (Server::start(), KeyPair::generate().unwrap());
    let m = manifest(&srv, "0.8.0");
    srv.set("/app/stable/manifest.json", Route::Body(m.clone()));
    srv.set(
        "/app/stable/manifest.json.minisig",
        Route::Body(k.sign(&m, "t").into_bytes()),
    );
    srv.set("/app/0.8.0/app.AppImage", Route::CutOnce(asset(), 120_000));
    let http = UreqHttp::new(true);
    let Outcome::Available(u) = check(&srv, &k, &http).unwrap() else {
        panic!()
    };
    let fs = MemFs::new();
    let clock = FixedClock::new(NOW);
    let up = Updater {
        http: &http,
        fs: &fs,
        clock: &clock,
        access: None,
    };
    let src = UpdateSource::new(
        "app",
        "stable",
        &srv.url("/app/stable/manifest.json"),
        &[&k.public_key()],
    );
    let a = u
        .asset(Platform::LinuxX86_64, &[Kind::Appimage])
        .unwrap()
        .clone();
    let r = up.download(&src, &u, &a, Path::new("/stage"), true, &mut |_, _| {});
    assert!(matches!(r, Err(Error::Truncated { .. })), "{r:?}");
    let s = up
        .download(&src, &u, &a, Path::new("/stage"), true, &mut |_, _| {})
        .unwrap();
    assert_eq!(fs.get(&s.path).unwrap(), asset());
    assert!(
        srv.seen
            .lock()
            .unwrap()
            .iter()
            .any(|h| h.to_ascii_lowercase().contains("range: bytes=120000-"))
    );
}
