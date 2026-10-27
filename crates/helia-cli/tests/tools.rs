// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
#![allow(
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::many_single_char_names,
    reason = "tests"
)]

//! `helia tools` against a local server on 127.0.0.1: the licence is shown and must be
//! accepted before anything is fetched, a changed licence asks again, scripts accept by hash,
//! and remove, status and the package manager route behave.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Cursor, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use helia_cli::tools::{Ctx, MethodArg, Pager, ToolsCommand, run_with};
use helia_external::manifest::{
    DownloadDoc, ItemDoc, LicenceDoc, ManifestDoc, MemberDoc, PackageDoc,
};
use helia_external::{Client, CommandRunner, Dirs, ExternalSource};
use helia_update::Platform;
use helia_update::mem::FixedClock;
use helia_update::sign::KeyPair;
use helia_update::time::format_utc;
use helia_update::traits::StdFs;
use helia_update::ureq_http::UreqHttp;
use helia_update::verify::sha256_hex;

const NOW: u64 = 1_792_864_800;
const LICENCE: &str =
    "GNU LESSER GENERAL PUBLIC LICENSE\nVersion 3, 29 June 2007\nTHE FULL TEXT IS HERE\n";

struct Server {
    port: u16,
    routes: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn start() -> Self {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let routes: Arc<Mutex<HashMap<String, Vec<u8>>>> = Arc::default();
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

    fn downloads_seen(&self) -> usize {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.starts_with("/dl/"))
            .count()
    }
}

fn serve(mut s: TcpStream, routes: &Mutex<HashMap<String, Vec<u8>>>, seen: &Mutex<Vec<String>>) {
    let mut rd = BufReader::new(s.try_clone().unwrap());
    let mut first = String::new();
    loop {
        let mut line = String::new();
        if rd.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
        if first.is_empty() {
            first = line;
        }
    }
    let path = first.split_whitespace().nth(1).unwrap_or("/").to_string();
    seen.lock().unwrap().push(path.clone());
    let body = routes.lock().unwrap().get(&path).cloned();
    let resp = match body {
        None => {
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
        }
        Some(b) => {
            let mut h = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                b.len()
            )
            .into_bytes();
            h.extend_from_slice(&b);
            h
        }
    };
    let _ = s.write_all(&resp);
    let _ = s.flush();
    let mut sink = [0u8; 64];
    let _ = s.set_read_timeout(Some(std::time::Duration::from_millis(50)));
    let _ = s.read(&mut sink);
}

fn zip_bytes() -> Vec<u8> {
    let mut buf = Cursor::new(Vec::new());
    {
        let mut w = zip::ZipWriter::new(&mut buf);
        let o = zip::write::SimpleFileOptions::default();
        w.start_file("pkg/bin/ffmpeg", o).unwrap();
        w.write_all(b"ffmpeg-bytes").unwrap();
        w.start_file("pkg/bin/ffprobe", o).unwrap();
        w.write_all(b"ffprobe-bytes").unwrap();
        w.finish().unwrap();
    }
    buf.into_inner()
}

struct Fixture {
    srv: Server,
    key: KeyPair,
    dirs: Dirs,
    _tmp: tempfile::TempDir,
    archive: Vec<u8>,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let f = Self {
            srv: Server::start(),
            key: KeyPair::generate().unwrap(),
            dirs: Dirs::under(tmp.path()),
            _tmp: tmp,
            archive: zip_bytes(),
        };
        f.publish(1, LICENCE);
        f
    }

    fn publish(&self, sequence: u64, licence: &str) {
        let d = ManifestDoc {
            schema: 1,
            app: "helia".into(),
            date: format_utc(NOW - 100),
            sequence,
            expires: format_utc(NOW + 60 * 86_400),
            licences: vec![LicenceDoc {
                id: "lgpl-3.0".into(),
                name: "GNU Lesser General Public License, version 3".into(),
                text: licence.into(),
                source_url: "https://www.gnu.org/licenses/lgpl-3.0.html".into(),
                attribution: Some("FFmpeg is a trademark of Fabrice Bellard.".into()),
                show_attribution: true,
                spdx: None,
            }],
            items: vec![ItemDoc {
                id: "ffmpeg".into(),
                name: "FFmpeg".into(),
                version: "9.0.2".into(),
                provider: "Test provider".into(),
                provider_url: None,
                description: "Reads video.".into(),
                required: false,
                licence: "lgpl-3.0".into(),
                downloads: vec![DownloadDoc {
                    platform: "linux-x86_64".into(),
                    url: self.srv.url("/dl/ffmpeg.zip"),
                    sha256: sha256_hex(&self.archive),
                    size: self.archive.len() as u64,
                    archive: "zip".into(),
                    members: ["ffmpeg", "ffprobe"]
                        .map(|n| MemberDoc {
                            find: format!("bin/{n}"),
                            save_as: n.into(),
                            provides: Some(n.into()),
                            executable: true,
                        })
                        .to_vec(),
                    licence: None,
                    provider: Some("Download provider".into()),
                }],
                packages: vec![PackageDoc {
                    platform: "linux-x86_64".into(),
                    manager: "apt".into(),
                    argv: ["sudo", "apt", "install", "ffmpeg"]
                        .map(String::from)
                        .to_vec(),
                    licence: None,
                    provider: None,
                }],
            }],
        };
        let bytes = serde_json::to_vec_pretty(&d).unwrap();
        let mut r = self.srv.routes.lock().unwrap();
        r.insert("/m/external.json".into(), bytes.clone());
        r.insert(
            "/m/external.json.minisig".into(),
            self.key.sign(&bytes, "t").into_bytes(),
        );
        r.insert("/dl/ffmpeg.zip".into(), self.archive.clone());
    }

    fn source(&self) -> ExternalSource {
        ExternalSource::new(
            "helia",
            &self.srv.url("/m/external.json"),
            &[&self.key.public_key()],
        )
        .with_manifest_hosts(&["127.0.0.1"])
        .with_asset_hosts(&["127.0.0.1"])
        .with_loopback_http(true)
    }
}

struct PlainPager;

impl Pager for PlainPager {
    fn show(&self, text: &str, out: &mut dyn Write) -> std::io::Result<()> {
        out.write_all(text.as_bytes())
    }
}

struct Runner(Mutex<Vec<Vec<String>>>);

impl CommandRunner for Runner {
    fn run(&self, argv: &[String]) -> std::io::Result<i32> {
        self.0.lock().unwrap().push(argv.to_vec());
        Ok(0)
    }
}

struct Result3 {
    code: i32,
    out: String,
    err: String,
}

/// Run one `helia tools` command with scripted input.
fn tools(
    f: &Fixture,
    cmd: &ToolsCommand,
    input: &str,
    interactive: bool,
    programs: &[&str],
    runner: &Runner,
) -> Result3 {
    let (http, clock, source) = (UreqHttp::new(true), FixedClock::new(NOW), f.source());
    let client = Client {
        http: &http,
        fs: &StdFs,
        clock: &clock,
        source: &source,
        dirs: &f.dirs,
    };
    let has = |p: &str| programs.contains(&p);
    let found = || None;
    let ctx = Ctx {
        client: &client,
        platform: Some(Platform::LinuxX86_64),
        interactive,
        has_program: &has,
        runner,
        pager: &PlainPager,
        found_ffmpeg: &found,
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run_with(
        &ctx,
        cmd,
        &mut Cursor::new(input.as_bytes().to_vec()),
        &mut out,
        &mut err,
    )
    .unwrap();
    Result3 {
        code,
        out: String::from_utf8(out).unwrap(),
        err: String::from_utf8(err).unwrap(),
    }
}

fn install(method: MethodArg, accept: Option<&str>) -> ToolsCommand {
    ToolsCommand::Install {
        item: "ffmpeg".into(),
        method,
        accept_licence: accept.map(String::from),
    }
}

fn no_runner() -> Runner {
    Runner(Mutex::default())
}

#[test]
fn status_with_nothing_installed_uses_no_network() {
    let f = Fixture::new();
    let r = tools(
        &f,
        &ToolsCommand::Status { json: false },
        "",
        false,
        &[],
        &no_runner(),
    );
    assert_eq!(r.code, 0);
    assert!(r.out.contains("Nothing installed with Helia"), "{}", r.out);
    assert!(r.out.contains("cannot find ffmpeg"));
    assert!(f.srv.seen.lock().unwrap().is_empty());
    let j = tools(
        &f,
        &ToolsCommand::Status { json: true },
        "",
        false,
        &[],
        &no_runner(),
    );
    let v: serde_json::Value = serde_json::from_str(&j.out).unwrap();
    assert_eq!(v["installed"].as_array().unwrap().len(), 0);
}

#[test]
fn the_licence_is_shown_and_yes_installs() {
    let f = Fixture::new();
    let r = tools(
        &f,
        &install(MethodArg::Download, None),
        "yes\n",
        true,
        &[],
        &no_runner(),
    );
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    // Everything the owner asked to see before the question.
    assert!(r.out.contains("THE FULL TEXT IS HERE"));
    assert!(r.out.contains("Provided by: Download provider"));
    assert!(r.out.contains("lgpl-3.0.html"));
    assert!(r.out.contains("Type yes to accept"));
    assert!(r.out.find("THE FULL TEXT IS HERE") < r.out.find("Type yes"));
    assert!(r.out.contains("Installed FFmpeg 9.0.2"));
    assert!(r.out.contains("trademark of Fabrice Bellard"));
    assert_eq!(
        std::fs::read(helia_external::installed_program(&f.dirs, "ffmpeg", "ffprobe").unwrap())
            .unwrap(),
        b"ffprobe-bytes"
    );
    let acc = std::fs::read_to_string(&f.dirs.accepted).unwrap();
    assert!(
        acc.contains("\"via\": \"cli\"") && acc.contains("licence_sha256"),
        "{acc}"
    );

    // Status now lists it, with the licence.
    let s = tools(
        &f,
        &ToolsCommand::Status { json: false },
        "",
        false,
        &[],
        &no_runner(),
    );
    assert!(s.out.contains("FFmpeg 9.0.2 (download)"), "{}", s.out);
    assert!(s.out.contains("Licence: GNU Lesser General Public License"));
}

#[test]
fn anything_but_yes_downloads_nothing() {
    let f = Fixture::new();
    for answer in ["no\n", "\n", "y\n", "yess\n"] {
        let r = tools(
            &f,
            &install(MethodArg::Download, None),
            answer,
            true,
            &[],
            &no_runner(),
        );
        assert_eq!(r.code, 3, "{answer:?}");
        assert!(r.out.contains("Nothing was downloaded"));
    }
    assert_eq!(f.srv.downloads_seen(), 0);
    assert!(!f.dirs.accepted.exists());
    assert!(!f.dirs.tools.exists());
}

#[test]
fn without_a_terminal_it_refuses_and_says_how_to_accept() {
    let f = Fixture::new();
    let r = tools(
        &f,
        &install(MethodArg::Download, None),
        "yes\n",
        false,
        &[],
        &no_runner(),
    );
    assert_eq!(r.code, 3);
    assert!(r.err.contains("--accept-licence"), "{}", r.err);
    assert_eq!(f.srv.downloads_seen(), 0);
}

#[test]
fn a_script_accepts_with_the_licence_hash_and_a_wrong_hash_is_refused() {
    let f = Fixture::new();
    let hash = sha256_hex(LICENCE.as_bytes());
    let bad = tools(
        &f,
        &install(MethodArg::Download, Some("deadbeefdead")),
        "",
        false,
        &[],
        &no_runner(),
    );
    assert_eq!(bad.code, 3);
    assert!(bad.err.contains("does not match"));
    let short = tools(
        &f,
        &install(MethodArg::Download, Some(&hash[..6])),
        "",
        false,
        &[],
        &no_runner(),
    );
    assert_eq!(short.code, 3);
    assert_eq!(f.srv.downloads_seen(), 0);

    let ok = tools(
        &f,
        &install(MethodArg::Download, Some(&hash[..12])),
        "",
        false,
        &[],
        &no_runner(),
    );
    assert_eq!(ok.code, 0, "{}{}", ok.out, ok.err);
    assert!(f.dirs.item_dir("ffmpeg").join("ffmpeg").is_file());
    assert!(
        std::fs::read_to_string(&f.dirs.accepted)
            .unwrap()
            .contains("cli-flag")
    );
}

#[test]
fn an_unchanged_licence_is_not_asked_again_but_a_changed_one_is() {
    let f = Fixture::new();
    assert_eq!(
        tools(
            &f,
            &install(MethodArg::Download, None),
            "yes\n",
            true,
            &[],
            &no_runner()
        )
        .code,
        0
    );

    // Same text: no question, so an empty input is fine.
    let again = tools(
        &f,
        &install(MethodArg::Download, None),
        "",
        true,
        &[],
        &no_runner(),
    );
    assert_eq!(again.code, 0, "{}{}", again.out, again.err);
    assert!(again.out.contains("it has not changed"));
    assert!(!again.out.contains("Type yes"));

    // The provider amends the licence.
    f.publish(2, &format!("{LICENCE}A new clause.\n"));
    let changed = tools(
        &f,
        &install(MethodArg::Download, None),
        "no\n",
        true,
        &[],
        &no_runner(),
    );
    assert_eq!(changed.code, 3);
    assert!(changed.out.contains("A new clause."));
    assert!(changed.out.contains("Type yes"));
}

#[test]
fn remove_asks_or_needs_yes_and_can_forget_the_licence() {
    let f = Fixture::new();
    tools(
        &f,
        &install(MethodArg::Download, None),
        "yes\n",
        true,
        &[],
        &no_runner(),
    );
    let rm = |yes, forget| ToolsCommand::Remove {
        item: "ffmpeg".into(),
        forget_licence: forget,
        yes,
    };
    let declined = tools(&f, &rm(false, false), "no\n", true, &[], &no_runner());
    assert_eq!(declined.code, 3);
    assert!(f.dirs.item_dir("ffmpeg").exists());
    let done = tools(&f, &rm(true, true), "", false, &[], &no_runner());
    assert_eq!(done.code, 0);
    assert!(!f.dirs.item_dir("ffmpeg").exists());
    assert!(done.out.contains("Forgot 1 licence acceptance"));
    // Installing again asks again.
    let again = tools(
        &f,
        &install(MethodArg::Download, None),
        "no\n",
        true,
        &[],
        &no_runner(),
    );
    assert!(again.out.contains("Type yes"));
}

#[test]
fn the_package_route_asks_twice_and_runs_the_command_once() {
    let f = Fixture::new();
    let runner = no_runner();
    // Auto picks the package manager when sudo and apt exist.
    let r = tools(
        &f,
        &install(MethodArg::Auto, None),
        "yes\nyes\n",
        true,
        &["sudo", "apt"],
        &runner,
    );
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    assert!(r.out.contains("sudo apt install ffmpeg"));
    assert_eq!(runner.0.lock().unwrap().len(), 1);
    assert_eq!(f.srv.downloads_seen(), 0);
    assert!(r.out.contains("with your package manager"));

    // Without a package manager, auto falls back to the download.
    let f2 = Fixture::new();
    let r2 = tools(
        &f2,
        &install(MethodArg::Auto, None),
        "yes\n",
        true,
        &[],
        &no_runner(),
    );
    assert_eq!(r2.code, 0, "{}{}", r2.out, r2.err);
    assert_eq!(f2.srv.downloads_seen(), 1);

    // --method package with none available says so.
    let f3 = Fixture::new();
    let (http, clock, source) = (UreqHttp::new(true), FixedClock::new(NOW), f3.source());
    let client = Client {
        http: &http,
        fs: &StdFs,
        clock: &clock,
        source: &source,
        dirs: &f3.dirs,
    };
    let has = |_: &str| false;
    let found = || None;
    let rn = no_runner();
    let ctx = Ctx {
        client: &client,
        platform: Some(Platform::LinuxX86_64),
        interactive: true,
        has_program: &has,
        runner: &rn,
        pager: &PlainPager,
        found_ffmpeg: &found,
    };
    let e = run_with(
        &ctx,
        &install(MethodArg::Package, None),
        &mut Cursor::new(Vec::new()),
        &mut Vec::new(),
        &mut Vec::new(),
    )
    .unwrap_err();
    assert!(
        e.to_string().contains("no supported package manager"),
        "{e}"
    );
}

#[test]
fn licence_prints_the_text_the_hash_and_the_source() {
    let f = Fixture::new();
    let r = tools(
        &f,
        &ToolsCommand::Licence {
            item: "ffmpeg".into(),
        },
        "",
        false,
        &["sudo", "apt"],
        &no_runner(),
    );
    assert_eq!(r.code, 0);
    assert!(r.out.contains("THE FULL TEXT IS HERE"));
    assert!(r.out.contains(&sha256_hex(LICENCE.as_bytes())));
    assert!(r.out.contains("lgpl-3.0.html"));
    assert_eq!(f.srv.downloads_seen(), 0);
}
