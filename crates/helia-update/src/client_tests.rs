// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The client flow end to end with an in-memory server, disk and clock.
#![allow(
    clippy::single_match_else,
    clippy::assert_is_empty,
    clippy::many_single_char_names,
    reason = "tests"
)]

use std::path::{Path, PathBuf};

use crate::apply::ApplyOutcome;
use crate::apply::fake::{FakeProcess, TestFs};
use crate::client::{CheckReport, Client, Paths};
use crate::error::Error;
use crate::install::fake::FakeProbe;
use crate::install::{HostOs, PackageManager};
use crate::manifest::{AssetDoc, Kind, ManifestDoc, Platform};
use crate::mem::{FixedClock, MemHttp};
use crate::settings::{UpdateMode, UpdateSettings};
use crate::sign::KeyPair;
use crate::source::UpdateSource;
use crate::traits::Clock;
use crate::verify::sha256_hex;

const NOW: u64 = 1_792_864_800;
const BASE: &str = "https://downloads.example.org/helia";
const TARGET: &str = "/home/me/Helia.AppImage";

fn elf(tag: &str) -> Vec<u8> {
    let mut v = vec![0x7f, b'E', b'L', b'F'];
    v.extend_from_slice(tag.as_bytes());
    v
}

struct World {
    http: MemHttp,
    fs: TestFs,
    clock: FixedClock,
    probe: FakeProbe,
    process: FakeProcess,
    paths: Paths,
    key: KeyPair,
}

impl World {
    fn new(kind: &str) -> Self {
        let mut probe = FakeProbe::default();
        match kind {
            "appimage" => {
                probe.env.insert("APPIMAGE".into(), TARGET.into());
                probe.files.insert(PathBuf::from(TARGET));
                probe.exe = Some("/tmp/.mount_x/usr/bin/helia-gui".into());
            }
            _ => {
                probe.exe = Some("/usr/bin/helia-gui".into());
                probe.owner = Some(PackageManager::Dpkg);
            }
        }
        let w = Self {
            http: MemHttp::new(),
            fs: TestFs::default(),
            clock: FixedClock::new(NOW),
            probe,
            process: FakeProcess::default(),
            paths: Paths::under(Path::new("/cfg/helia")),
            key: KeyPair::generate().unwrap(),
        };
        w.fs.mem.put(Path::new(TARGET), &elf("old"));
        w
    }

    /// Serve `version`, signed, valid for a day from the clock's now.
    fn publish(&self, version: &str, sequence: u64) -> Vec<u8> {
        self.publish_at(version, sequence, BASE)
    }

    /// As [`Self::publish`], with the asset files at `asset_base`.
    fn publish_at(&self, version: &str, sequence: u64, asset_base: &str) -> Vec<u8> {
        let now = self.clock.now_unix();
        let body = elf(&format!("new-{version}"));
        let asset = |kind: &str, url: String| AssetDoc {
            platform: "linux-x86_64".into(),
            kind: kind.into(),
            url,
            sha256: sha256_hex(&body),
            size: body.len() as u64,
        };
        let doc = ManifestDoc {
            schema: 1,
            app: "helia".into(),
            channel: "stable".into(),
            version: version.into(),
            date: crate::time::format_utc(now - 60),
            sequence,
            expires: crate::time::format_utc(now + 86_400),
            min_supported: None,
            notes_url: Some("https://downloads.example.org/notes".into()),
            assets: vec![
                asset("appimage", format!("{asset_base}/{version}/Helia.AppImage")),
                asset("deb", format!("{asset_base}/{version}/helia.deb")),
            ],
        };
        let json = serde_json::to_vec_pretty(&doc).unwrap();
        self.http
            .serve(&format!("{BASE}/stable/manifest.json"), &json);
        self.http.serve(
            &format!("{BASE}/stable/manifest.json.minisig"),
            self.key
                .sign(&json, "timestamp:1\tfile:manifest.json")
                .as_bytes(),
        );
        self.http
            .serve(&format!("{asset_base}/{version}/Helia.AppImage"), &body);
        self.http
            .serve(&format!("{asset_base}/{version}/helia.deb"), &body);
        body
    }

    fn source(&self) -> impl Fn(&str) -> UpdateSource + use<> {
        let key = self.key.public_key();
        move |ch: &str| {
            UpdateSource::new("helia", ch, &format!("{BASE}/{ch}/manifest.json"), &[&key])
        }
    }

    fn client<'a>(&'a self, version: &'a str, src: &'a dyn Fn(&str) -> UpdateSource) -> Client<'a> {
        Client {
            http: &self.http,
            fs: &self.fs,
            clock: &self.clock,
            probe: &self.probe,
            process: &self.process,
            paths: &self.paths,
            version,
            os: HostOs::Linux,
            platform: Some(Platform::LinuxX86_64),
            source_for: src,
            loopback: false,
            relaunch_gui: true,
            channel_override: None,
            backend: None,
        }
    }
}

fn set_mode(c: &Client<'_>, mode: UpdateMode) {
    c.save_settings(&UpdateSettings {
        mode,
        ..UpdateSettings::default()
    })
    .unwrap();
}

#[test]
fn off_never_touches_the_network_unless_asked() {
    let w = World::new("appimage");
    w.publish("0.8.0", 1);
    let src = w.source();
    let c = w.client("0.7.1", &src);
    set_mode(&c, UpdateMode::Off);
    assert!(matches!(c.check(false).unwrap(), CheckReport::Disabled));
    assert!(w.http.requests().is_empty());
    assert!(matches!(c.check(true).unwrap(), CheckReport::Available(_)));
    assert_eq!(w.http.requests().len(), 2, "manifest and signature only");
}

#[test]
fn automatic_checks_run_once_a_day() {
    let w = World::new("appimage");
    w.publish("0.8.0", 1);
    let src = w.source();
    let c = w.client("0.7.1", &src);
    assert!(matches!(c.check(false).unwrap(), CheckReport::Available(_)));
    assert!(matches!(c.check(false).unwrap(), CheckReport::NotDue));
    assert!(matches!(c.check(true).unwrap(), CheckReport::Available(_)));
    // A day on, the manifest has expired: the check fails but is still recorded.
    w.clock.set(NOW + 25 * 3600);
    assert!(c.check(false).is_err());
    assert!(matches!(c.check(false).unwrap(), CheckReport::NotDue));
}

#[test]
fn appimage_update_from_check_to_restart() {
    let w = World::new("appimage");
    let body = w.publish("0.8.0", 1);
    let src = w.source();
    let c = w.client("0.7.1", &src);
    let CheckReport::Available(a) = c.check(true).unwrap() else {
        panic!()
    };
    assert!(a.self_applies());
    assert_eq!(a.version, "0.8.0");
    let mut last = (0, 0);
    let staged = c.download(&a, &mut |d, t| last = (d, t)).unwrap();
    assert_eq!(last.0, body.len() as u64);
    let out = c.apply(&staged, a.notes_url.as_deref()).unwrap();
    assert!(matches!(out, ApplyOutcome::Installed { .. }));
    assert_eq!(w.fs.mem.get(Path::new(TARGET)).unwrap(), body);
    assert_eq!(c.status().previous.as_deref(), Some("0.7.1"));
    assert!(c.status().staged.is_none());

    // The new version starts: it is told what happened, once.
    let new = w.client("0.8.0", &src);
    let s = new.startup().unwrap();
    assert!(s.notice.unwrap().contains("updated from 0.7.1 to 0.8.0"));
    assert!(new.startup().unwrap().notice.is_none());
    new.mark_healthy().unwrap();
}

#[test]
fn a_version_that_keeps_failing_is_rolled_back_and_not_offered_again() {
    let w = World::new("appimage");
    let body = w.publish("0.8.0", 1);
    let src = w.source();
    let old = w.client("0.7.1", &src);
    let CheckReport::Available(a) = old.check(true).unwrap() else {
        panic!()
    };
    let staged = old.download(&a, &mut |_, _| {}).unwrap();
    old.apply(&staged, None).unwrap();
    let new = w.client("0.8.0", &src);
    assert!(new.startup().unwrap().rollback.is_none());
    assert!(new.startup().unwrap().rollback.is_none());
    let third = new.startup().unwrap();
    let r = third
        .rollback
        .expect("rolled back on the third failed start");
    assert_eq!(r.to_version, "0.7.1");
    assert!(third.notice.unwrap().contains("did not start properly"));
    assert_eq!(w.fs.mem.get(Path::new(TARGET)).unwrap(), elf("old"));
    assert_ne!(w.fs.mem.get(Path::new(TARGET)).unwrap(), body);
    // A daily check does not push the failed version again.
    w.clock.set(NOW + 25 * 3600);
    w.publish("0.8.0", 1);
    assert!(matches!(
        old.check(false).unwrap(),
        CheckReport::UpToDate { .. }
    ));
}

#[test]
fn deb_install_shows_the_command_and_touches_nothing() {
    let w = World::new("deb");
    w.publish("0.8.0", 1);
    let src = w.source();
    let c = w.client("0.7.1", &src);
    let CheckReport::Available(a) = c.check(true).unwrap() else {
        panic!()
    };
    assert!(!a.self_applies());
    assert_eq!(a.asset.as_ref().unwrap().kind, Kind::Deb);
    let staged = c.download(&a, &mut |_, _| {}).unwrap();
    let ApplyOutcome::Manual { command, .. } = c.apply(&staged, None).unwrap() else {
        panic!()
    };
    assert!(
        command
            .unwrap()
            // On Windows the path has backslashes, so the real command quotes it: compare the bare path.
            .replace('\\', "/")
            .replace('\'', "")
            .starts_with("sudo apt install /cfg/helia/updates/")
    );
    assert_eq!(w.fs.mem.get(Path::new(TARGET)).unwrap(), elf("old"));
    assert!(c.rollback_now().is_err());
    assert!(c.status().staged.is_some());
    // Once the package is installed, the leftover download is cleaned up.
    let later = w.client("0.8.0", &src);
    later.startup().unwrap();
    assert!(later.status().staged.is_none());
}

#[test]
fn settings_from_a_newer_helia_are_kept() {
    let w = World::new("appimage");
    w.fs.mem.put(&w.paths.settings, b"schema=7\nmode=off\n");
    let src = w.source();
    let c = w.client("0.7.1", &src);
    assert_eq!(c.status().newer_settings_schema, Some(7));
    assert!(c.save_settings(&UpdateSettings::default()).is_err());
    assert_eq!(
        w.fs.mem.get(&w.paths.settings).unwrap(),
        b"schema=7\nmode=off\n"
    );
}

// ---- the backend hook: a source build has none, an official build plugs one in ----

const OTHER_DL: &str = "https://gated.example.org/dl/helia";

/// A stand-in for a gated update service: it opens one more host, sends a bearer to it,
/// decides downloads are allowed, leaves a notice and adds a status line.
struct StubBackend {
    allow: bool,
}

impl crate::backend::UpdateBackend for StubBackend {
    fn extend_source(&self, mut s: UpdateSource) -> UpdateSource {
        s.asset_hosts.push("gated.example.org".into());
        s
    }
    fn asset_access(
        &self,
        _cx: &crate::backend::BackendCx<'_>,
    ) -> Option<crate::backend::AssetAccess> {
        Some(crate::backend::AssetAccess {
            bearer: "stub-bearer-0123456789".into(),
            bearer_host: "gated.example.org".into(),
            refusal: Box::new(|status, _| Error::Refused(format!("refused with {status}"))),
        })
    }
    fn refresh(&self, _cx: &crate::backend::BackendCx<'_>) -> crate::backend::Refresh {
        crate::backend::Refresh {
            fresh: true,
            notice: Some("the stub left a notice".into()),
        }
    }
    fn gate(
        &self,
        _cx: &crate::backend::BackendCx<'_>,
        _v: &semver::Version,
        fresh: bool,
    ) -> Option<crate::backend::Gate> {
        Some(crate::backend::Gate {
            auto_download: self.allow && fresh,
            note: (!self.allow).then(|| "the stub says no".to_string()),
            link: None,
        })
    }
    fn status(&self, _cx: &crate::backend::BackendCx<'_>) -> Option<crate::backend::BackendStatus> {
        Some(crate::backend::BackendStatus {
            label: "Stub".into(),
            line: "all good".into(),
            json: serde_json::json!({ "ok": true }),
        })
    }
}

fn with_backend<'a>(
    w: &'a World,
    src: &'a dyn Fn(&str) -> UpdateSource,
    b: &'a StubBackend,
) -> Client<'a> {
    let mut c = w.client("0.8.0", src);
    c.backend = Some(b);
    c
}

#[test]
fn without_a_backend_a_file_off_the_allowed_hosts_is_left_to_the_user() {
    let w = World::new("appimage");
    w.publish_at("0.8.1", 1, OTHER_DL);
    let src = w.source();
    let c = w.client("0.8.0", &src);
    let CheckReport::Available(a) = c.check(true).unwrap() else {
        panic!()
    };
    assert!(!a.auto_download && !a.may_auto_download());
    assert!(
        a.note
            .as_deref()
            .unwrap()
            .contains("does not fetch installers by itself")
    );
    assert_eq!(a.link.as_deref(), Some(crate::helia::PAGE_URL));
    let before = w.http.requests().len();
    let e = c.download(&a, &mut |_, _| {}).unwrap_err();
    assert!(matches!(e, Error::Refused(_)), "{e}");
    assert_eq!(w.http.requests().len(), before, "nothing was fetched");
    assert!(c.backend_panel().is_none() && c.backend_status().is_none());
}

#[test]
fn a_file_on_an_allowed_host_downloads_without_a_backend() {
    let w = World::new("appimage");
    w.publish("0.8.1", 1);
    let src = w.source();
    let c = w.client("0.8.0", &src);
    let CheckReport::Available(a) = c.check(true).unwrap() else {
        panic!()
    };
    assert!(a.auto_download && a.note.is_none());
    c.download(&a, &mut |_, _| {}).unwrap();
    assert!(w.http.bearers().iter().all(|(_, t, _)| t.is_none()));
}

#[test]
fn a_backend_opens_its_host_sends_its_bearer_only_there_and_leaves_a_notice() {
    let w = World::new("appimage");
    w.publish_at("0.8.1", 1, OTHER_DL);
    let src = w.source();
    let b = StubBackend { allow: true };
    let c = with_backend(&w, &src, &b);
    let CheckReport::Available(a) = c.check(true).unwrap() else {
        panic!()
    };
    assert!(a.auto_download && a.may_auto_download());
    c.download(&a, &mut |_, _| {}).unwrap();
    let seen = w.http.bearers();
    let asset: Vec<_> = seen
        .iter()
        .filter(|(u, _, _)| u.starts_with(OTHER_DL))
        .collect();
    assert!(!asset.is_empty());
    for (_, token, host) in &asset {
        assert_eq!(token.as_deref(), Some("stub-bearer-0123456789"));
        assert_eq!(host.as_deref(), Some("gated.example.org"));
    }
    for (u, token, _) in &seen {
        assert!(
            token.is_none() || u.starts_with(OTHER_DL),
            "the bearer went to {u}"
        );
    }
    assert_eq!(
        c.status().backend.map(|s| (s.label, s.line)),
        Some(("Stub".into(), "all good".into()))
    );
    // The notice the backend left is shown once at the next start.
    let n = w.client("0.8.0", &src).startup().unwrap().notice;
    assert_eq!(n.as_deref(), Some("the stub left a notice"));
}

#[test]
fn a_backend_can_say_no_and_the_note_reaches_the_user() {
    let w = World::new("appimage");
    w.publish_at("0.8.1", 1, OTHER_DL);
    let src = w.source();
    let b = StubBackend { allow: false };
    let c = with_backend(&w, &src, &b);
    let CheckReport::Available(a) = c.check(true).unwrap() else {
        panic!()
    };
    assert!(!a.auto_download);
    assert_eq!(a.note.as_deref(), Some("the stub says no"));
    let e = c.download(&a, &mut |_, _| {}).unwrap_err();
    assert!(e.to_string().contains("the stub says no"), "{e}");
}

#[test]
fn a_refused_download_is_explained_by_the_backend() {
    let w = World::new("appimage");
    w.publish_at("0.8.1", 1, OTHER_DL);
    w.http
        .serve_status(&format!("{OTHER_DL}/0.8.1/Helia.AppImage"), 403, b"{}");
    let src = w.source();
    let b = StubBackend { allow: true };
    let c = with_backend(&w, &src, &b);
    let CheckReport::Available(a) = c.check(true).unwrap() else {
        panic!()
    };
    let e = c.download(&a, &mut |_, _| {}).unwrap_err();
    assert_eq!(e.to_string(), "refused with 403");
}
