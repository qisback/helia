// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
#![allow(
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::many_single_char_names,
    reason = "tests"
)]

//! The whole flow against a local test server on 127.0.0.1: fetch and verify the
//! manifest, refuse without consent, hash mismatch, a host that is not on the allow-list,
//! an interrupted download that resumes, a changed licence that asks again, a tampered
//! manifest, a replayed one, cancel, and the package manager route with a fake runner.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use helia_external::manifest::{
    DownloadDoc, ItemDoc, LicenceDoc, ManifestDoc, MemberDoc, PackageDoc,
};
use helia_external::{
    Acceptances, Client, CommandRunner, Dirs, Error, ExternalSource, Hooks, Manifest, Phase,
    Progress, Route, installed_program,
};
use helia_update::Platform;
use helia_update::mem::FixedClock;
use helia_update::sign::KeyPair;
use helia_update::time::format_utc;
use helia_update::traits::StdFs;
use helia_update::ureq_http::UreqHttp;
use helia_update::verify::sha256_hex;

const NOW: u64 = 1_792_864_800;

#[derive(Clone)]
enum Route2 {
    Body(Vec<u8>),
    /// Declare the full length but close after this many bytes (first request only).
    CutOnce(Vec<u8>, usize),
}

struct Server {
    port: u16,
    routes: Arc<Mutex<HashMap<String, Route2>>>,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn start() -> Self {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let routes: Arc<Mutex<HashMap<String, Route2>>> = Arc::default();
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

    fn set(&self, path: &str, r: Route2) {
        self.routes.lock().unwrap().insert(path.into(), r);
    }

    fn paths_seen(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|h| h.split_whitespace().nth(1).unwrap_or("").to_string())
            .collect()
    }

    fn heads_for(&self, path: &str) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|h| h.split_whitespace().nth(1) == Some(path))
            .cloned()
            .collect()
    }
}

fn serve(mut s: TcpStream, routes: &Mutex<HashMap<String, Route2>>, seen: &Mutex<Vec<String>>) {
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
        Some(Route2::Body(b)) => reply(&b, range, None),
        Some(Route2::CutOnce(b, n)) => {
            cut_once = Some(path.clone());
            reply(&b, range, if range.is_none() { Some(n) } else { None })
        }
    };
    if let Some(p) = cut_once {
        let mut g = routes.lock().unwrap();
        if let Some(Route2::CutOnce(b, _)) = g.get(&p).cloned() {
            g.insert(p, Route2::Body(b));
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
            "Content-Range: bytes {start}-{}/{}\r\n",
            body.len() - 1,
            body.len()
        );
    }
    h.push_str("\r\n");
    let mut out = h.into_bytes();
    out.extend_from_slice(&part[..cut.unwrap_or(part.len()).min(part.len())]);
    out
}

// ---- fixtures ------------------------------------------------------------------

const LICENCE_TEXT: &str = "GNU LESSER GENERAL PUBLIC LICENSE\nVersion 3, 29 June 2007\n";

fn zip_bytes() -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut w = zip::ZipWriter::new(&mut buf);
        let o = zip::write::SimpleFileOptions::default();
        w.start_file("pkg/bin/ffmpeg", o).unwrap();
        // Incompressible-ish so a cut lands inside the body.
        let mut x = 12345u32;
        let body: Vec<u8> = (0..300_000)
            .map(|_| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (x >> 24) as u8
            })
            .collect();
        w.write_all(&body).unwrap();
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

fn doc(
    f: &Fixture,
    sequence: u64,
    licence_text: &str,
    archive_sha: &str,
    url: &str,
) -> ManifestDoc {
    ManifestDoc {
        schema: 1,
        app: "app".into(),
        date: format_utc(NOW - 100),
        sequence,
        expires: format_utc(NOW + 60 * 86_400),
        licences: vec![LicenceDoc {
            id: "lgpl-3.0".into(),
            name: "GNU Lesser General Public License, version 3".into(),
            spdx: Some("LGPL-3.0-or-later".into()),
            text: licence_text.into(),
            source_url: "https://www.gnu.org/licenses/lgpl-3.0.html".into(),
            attribution: Some("FFmpeg is a trademark of Fabrice Bellard.".into()),
            show_attribution: true,
        }],
        items: vec![ItemDoc {
            id: "ffmpeg".into(),
            name: "FFmpeg".into(),
            version: "9.0.2".into(),
            provider: "Test provider".into(),
            provider_url: Some("https://example.org/".into()),
            description: "Reads video.".into(),
            required: false,
            licence: "lgpl-3.0".into(),
            downloads: vec![DownloadDoc {
                platform: Platform::current()
                    .map_or("linux-x86_64", Platform::as_str)
                    .into(),
                url: url.into(),
                sha256: archive_sha.into(),
                size: f.archive.len() as u64,
                archive: "zip".into(),
                members: vec![
                    MemberDoc {
                        find: "bin/ffmpeg".into(),
                        save_as: "ffmpeg".into(),
                        provides: Some("ffmpeg".into()),
                        executable: true,
                    },
                    MemberDoc {
                        find: "bin/ffprobe".into(),
                        save_as: "ffprobe".into(),
                        provides: Some("ffprobe".into()),
                        executable: true,
                    },
                ],
                licence: None,
                provider: None,
            }],
            packages: vec![PackageDoc {
                platform: Platform::current()
                    .map_or("linux-x86_64", Platform::as_str)
                    .into(),
                manager: "apt".into(),
                argv: ["sudo", "apt", "install", "ffmpeg"]
                    .map(String::from)
                    .to_vec(),
                licence: None,
                provider: None,
            }],
        }],
    }
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
        f.publish(1, LICENCE_TEXT);
        f
    }

    fn asset_url(&self) -> String {
        self.srv.url("/dl/ffmpeg.zip")
    }

    /// Publish a signed manifest with this sequence and licence text; serve the archive.
    fn publish(&self, sequence: u64, licence_text: &str) {
        let d = doc(
            self,
            sequence,
            licence_text,
            &sha256_hex(&self.archive),
            &self.asset_url(),
        );
        self.publish_doc(&d);
        self.srv
            .set("/dl/ffmpeg.zip", Route2::Body(self.archive.clone()));
    }

    fn publish_doc(&self, d: &ManifestDoc) {
        let bytes = serde_json::to_vec_pretty(d).unwrap();
        self.srv
            .set("/m/external.json", Route2::Body(bytes.clone()));
        self.srv.set(
            "/m/external.json.minisig",
            Route2::Body(self.key.sign(&bytes, "test").into_bytes()),
        );
    }

    fn source(&self) -> ExternalSource {
        ExternalSource::new(
            "app",
            &self.srv.url("/m/external.json"),
            &[&self.key.public_key()],
        )
        .with_manifest_hosts(&["127.0.0.1"])
        .with_asset_hosts(&["127.0.0.1"])
        .with_loopback_http(true)
    }

    fn fetch(&self) -> Result<Manifest, Error> {
        let (http, clock, src) = (UreqHttp::new(true), FixedClock::new(NOW), self.source());
        client(&http, &clock, &src, &self.dirs).fetch_manifest()
    }
}

fn client<'a>(
    http: &'a UreqHttp,
    clock: &'a FixedClock,
    source: &'a ExternalSource,
    dirs: &'a Dirs,
) -> Client<'a> {
    Client {
        http,
        fs: &StdFs,
        clock,
        source,
        dirs,
    }
}

fn platform() -> Option<Platform> {
    Platform::current().or(Some(Platform::LinuxX86_64))
}

/// Run `body` with a client.
fn with_client<R>(f: &Fixture, body: impl FnOnce(&Client<'_>) -> R) -> R {
    let (http, clock, src) = (UreqHttp::new(true), FixedClock::new(NOW), f.source());
    body(&client(&http, &clock, &src, &f.dirs))
}

fn accepted_for(m: &Manifest, route_download: bool) -> Acceptances {
    let offers = m.offers("ffmpeg", platform()).unwrap();
    let offer = offers
        .iter()
        .find(|o| matches!(o.route, Route::Download(_)) == route_download)
        .unwrap();
    let mut a = Acceptances::default();
    a.accept(offer, NOW, "test");
    a
}

fn download_offer(m: &Manifest) -> helia_external::Offer<'_> {
    m.offers("ffmpeg", platform())
        .unwrap()
        .into_iter()
        .find(|o| matches!(o.route, Route::Download(_)))
        .unwrap()
}

// ---- tests ---------------------------------------------------------------------

#[test]
fn install_status_and_remove_end_to_end() {
    let f = Fixture::new();
    let m = f.fetch().unwrap();
    let offer = download_offer(&m);
    let acc = accepted_for(&m, true);

    let mut phases = Vec::new();
    let mut progress = |p: &Progress| phases.push(p.phase);
    let cancelled = || false;
    let rec = with_client(&f, |c| {
        c.install_with(
            &offer,
            &acc,
            &mut Hooks {
                progress: &mut progress,
                cancelled: &cancelled,
            },
        )
    })
    .unwrap();
    assert_eq!(rec.version, "9.0.2");
    assert!(rec.show_attribution);
    assert!(phases.contains(&Phase::Downloading));
    assert!(phases.contains(&Phase::Verifying));
    assert!(phases.contains(&Phase::Unpacking));

    let ffprobe = installed_program(&f.dirs, "ffmpeg", "ffprobe").unwrap();
    assert_eq!(std::fs::read(ffprobe).unwrap(), b"ffprobe-bytes");
    assert!(installed_program(&f.dirs, "ffmpeg", "ffplay").is_none());
    let dir = f.dirs.item_dir("ffmpeg");
    assert_eq!(
        std::fs::read_to_string(dir.join("LICENCE.txt")).unwrap(),
        LICENCE_TEXT
    );
    assert!(
        std::fs::read_to_string(dir.join("SOURCE.txt"))
            .unwrap()
            .contains("Test provider")
    );
    // The finished download is not kept.
    let staged = std::fs::read_dir(&f.dirs.staging).map_or(0, Iterator::count);
    assert_eq!(staged, 0);

    with_client(&f, |c| {
        let all = c.list(true).unwrap();
        assert_eq!(all.len(), 1);
        assert!(all[0].ok());
        // A changed file is reported.
        std::fs::write(dir.join("ffprobe"), b"tampered-bytes").unwrap();
        assert!(!c.check("ffmpeg", false).unwrap().unwrap().ok());
        let r = c.remove("ffmpeg").unwrap();
        assert!(r.note.is_none());
        assert_eq!(c.list(false).unwrap().len(), 0);
        assert!(matches!(c.remove("ffmpeg"), Err(Error::NotInstalled(_))));
    });
    assert!(installed_program(&f.dirs, "ffmpeg", "ffmpeg").is_none());
}

#[test]
fn nothing_is_requested_without_consent() {
    let f = Fixture::new();
    let m = f.fetch().unwrap();
    let offer = download_offer(&m);
    let r = with_client(&f, |c| c.install(&offer, &Acceptances::default()));
    assert!(matches!(r, Err(Error::NotAccepted(_))), "{r:?}");
    assert!(!f.srv.paths_seen().iter().any(|p| p.starts_with("/dl/")));
    assert!(!f.dirs.tools.exists());
}

#[test]
fn a_changed_licence_asks_again() {
    let f = Fixture::new();
    let m = f.fetch().unwrap();
    let acc = accepted_for(&m, true);

    // The provider amends the licence; the new manifest has a higher sequence.
    f.publish(2, &format!("{LICENCE_TEXT}Section 99 added.\n"));
    let m2 = f.fetch().unwrap();
    let offer2 = download_offer(&m2);
    assert!(!acc.is_accepted(&offer2));
    let r = with_client(&f, |c| c.install(&offer2, &acc));
    assert!(matches!(r, Err(Error::NotAccepted(_))));
    assert!(!f.srv.paths_seen().iter().any(|p| p.starts_with("/dl/")));
}

#[test]
fn a_wrong_checksum_is_discarded() {
    let f = Fixture::new();
    // Same size, different content: the declared hash is of other bytes.
    let d = doc(
        &f,
        1,
        LICENCE_TEXT,
        &sha256_hex(b"something else"),
        &f.asset_url(),
    );
    f.publish_doc(&d);
    let m = f.fetch().unwrap();
    let offer = download_offer(&m);
    let acc = accepted_for(&m, true);
    let r = with_client(&f, |c| c.install(&offer, &acc));
    assert!(
        matches!(r, Err(Error::Transport(helia_update::Error::HashMismatch))),
        "{r:?}"
    );
    let left: Vec<_> = std::fs::read_dir(&f.dirs.staging)
        .unwrap()
        .flatten()
        .collect();
    assert!(left.is_empty(), "the bad file must be deleted: {left:?}");
    assert!(installed_program(&f.dirs, "ffmpeg", "ffmpeg").is_none());
}

#[test]
fn a_host_off_the_allow_list_is_refused_before_any_request() {
    let f = Fixture::new();
    let d = doc(
        &f,
        1,
        LICENCE_TEXT,
        &sha256_hex(&f.archive),
        "https://evil.example/ffmpeg.zip",
    );
    f.publish_doc(&d);
    let m = f.fetch().unwrap();
    let offer = download_offer(&m);
    let acc = accepted_for(&m, true);
    let before = f.srv.paths_seen().len();
    let r = with_client(&f, |c| c.install(&offer, &acc));
    assert!(
        matches!(
            r,
            Err(Error::Transport(helia_update::Error::ForeignHost(_)))
        ),
        "{r:?}"
    );
    assert_eq!(f.srv.paths_seen().len(), before, "no request may be made");
}

#[test]
fn an_interrupted_download_resumes_with_a_range_request() {
    let f = Fixture::new();
    f.srv.set(
        "/dl/ffmpeg.zip",
        Route2::CutOnce(f.archive.clone(), 120_000),
    );
    let m = f.fetch().unwrap();
    let offer = download_offer(&m);
    let acc = accepted_for(&m, true);
    let r = with_client(&f, |c| c.install(&offer, &acc));
    assert!(
        matches!(
            r,
            Err(Error::Transport(helia_update::Error::Truncated {
                got: 120_000,
                ..
            }))
        ),
        "{r:?}"
    );
    let rec = with_client(&f, |c| c.install(&offer, &acc)).unwrap();
    assert_eq!(rec.files.len(), 2);
    assert!(
        f.srv
            .heads_for("/dl/ffmpeg.zip")
            .iter()
            .any(|h| h.to_ascii_lowercase().contains("range: bytes=120000-"))
    );
}

#[test]
fn cancel_keeps_the_partial_download() {
    let f = Fixture::new();
    let m = f.fetch().unwrap();
    let offer = download_offer(&m);
    let acc = accepted_for(&m, true);
    let stop = AtomicBool::new(false);
    let mut progress = |p: &Progress| {
        if p.done > 50_000 {
            stop.store(true, Ordering::SeqCst);
        }
    };
    let cancelled = || stop.load(Ordering::SeqCst);
    let r = with_client(&f, |c| {
        c.install_with(
            &offer,
            &acc,
            &mut Hooks {
                progress: &mut progress,
                cancelled: &cancelled,
            },
        )
    });
    assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    let parts: Vec<_> = std::fs::read_dir(&f.dirs.staging)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
        .collect();
    assert_eq!(parts.len(), 1);
    // Carrying on finishes the job.
    with_client(&f, |c| c.install(&offer, &acc)).unwrap();
}

#[test]
fn a_tampered_or_wrongly_signed_manifest_is_refused() {
    let f = Fixture::new();
    // One byte changed after signing.
    let d = doc(&f, 1, LICENCE_TEXT, &sha256_hex(&f.archive), &f.asset_url());
    let bytes = serde_json::to_vec_pretty(&d).unwrap();
    let sig = f.key.sign(&bytes, "test");
    let mut bad = bytes.clone();
    let at = bad.iter().position(|&b| b == b'9').unwrap();
    bad[at] = b'8';
    f.srv.set("/m/external.json", Route2::Body(bad));
    f.srv
        .set("/m/external.json.minisig", Route2::Body(sig.into_bytes()));
    assert!(matches!(
        f.fetch(),
        Err(Error::Transport(helia_update::Error::BadSignature))
    ));

    // Signed by a key the app does not trust.
    let other = KeyPair::generate().unwrap();
    f.srv.set("/m/external.json", Route2::Body(bytes.clone()));
    f.srv.set(
        "/m/external.json.minisig",
        Route2::Body(other.sign(&bytes, "test").into_bytes()),
    );
    assert!(matches!(
        f.fetch(),
        Err(Error::Transport(helia_update::Error::BadSignature))
    ));

    // Nothing was remembered from the refused manifests.
    assert!(!f.dirs.state.exists());
}

#[test]
fn an_older_manifest_is_a_replay() {
    let f = Fixture::new();
    f.publish(5, LICENCE_TEXT);
    f.fetch().unwrap();
    f.publish(4, LICENCE_TEXT);
    assert!(matches!(
        f.fetch(),
        Err(Error::Replay {
            seen: 4,
            highest: 5
        })
    ));
    f.publish(5, LICENCE_TEXT);
    f.fetch().unwrap();
}

#[test]
fn a_missing_signature_is_an_error() {
    let f = Fixture::new();
    f.srv
        .routes
        .lock()
        .unwrap()
        .remove("/m/external.json.minisig");
    assert!(matches!(
        f.fetch(),
        Err(Error::Transport(helia_update::Error::HttpStatus(404)))
    ));
}

struct FakeRunner {
    code: i32,
    ran: Mutex<Vec<Vec<String>>>,
}

impl CommandRunner for FakeRunner {
    fn run(&self, argv: &[String]) -> std::io::Result<i32> {
        self.ran.lock().unwrap().push(argv.to_vec());
        Ok(self.code)
    }
}

#[test]
fn the_package_route_runs_only_with_consent_and_records_the_result() {
    let f = Fixture::new();
    let m = f.fetch().unwrap();
    let offers = m.offers("ffmpeg", platform()).unwrap();
    let pkg = offers
        .iter()
        .find(|o| matches!(o.route, Route::Package(_)))
        .unwrap();
    let runner = FakeRunner {
        code: 0,
        ran: Mutex::new(Vec::new()),
    };

    let r = with_client(&f, |c| c.run_package(pkg, &Acceptances::default(), &runner));
    assert!(matches!(r, Err(Error::NotAccepted(_))));
    assert!(runner.ran.lock().unwrap().is_empty());

    // The download route refuses a package offer, and the other way round.
    let acc = accepted_for(&m, false);
    let dl = download_offer(&m);
    assert!(matches!(
        with_client(&f, |c| c.install(pkg, &acc)),
        Err(Error::WrongRoute(_))
    ));
    assert!(matches!(
        with_client(&f, |c| c.run_package(&dl, &acc, &runner)),
        Err(Error::NotAccepted(_) | Error::WrongRoute(_))
    ));

    let rec = with_client(&f, |c| c.run_package(pkg, &acc, &runner)).unwrap();
    assert_eq!(rec.route, "package");
    assert_eq!(
        runner.ran.lock().unwrap()[0],
        ["sudo", "apt", "install", "ffmpeg"]
            .map(String::from)
            .to_vec()
    );
    let removal = with_client(&f, |c| c.remove("ffmpeg")).unwrap();
    assert!(removal.note.unwrap().contains("package manager"));

    let failing = FakeRunner {
        code: 100,
        ran: Mutex::new(Vec::new()),
    };
    let r = with_client(&f, |c| c.run_package(pkg, &acc, &failing));
    assert!(matches!(r, Err(Error::CommandFailed(_))));
    assert!(!f.dirs.item_dir("ffmpeg").exists());
}
