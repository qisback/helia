// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The client flow end to end with an in-memory server, disk and clock.
#![allow(clippy::single_match_else, clippy::assert_is_empty, reason = "tests")]

use std::path::{Path, PathBuf};

use crate::apply::ApplyOutcome;
use crate::apply::fake::{FakeProcess, TestFs};
use crate::client::{CheckReport, Client, Paths};
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
                asset("appimage", format!("{BASE}/{version}/Helia.AppImage")),
                asset("deb", format!("{BASE}/{version}/helia.deb")),
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
            .serve(&format!("{BASE}/{version}/Helia.AppImage"), &body);
        self.http
            .serve(&format!("{BASE}/{version}/helia.deb"), &body);
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
            .replace('\\', "/")
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
