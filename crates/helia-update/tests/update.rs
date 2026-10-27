// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
#![allow(
    clippy::unwrap_used,
    clippy::many_single_char_names,
    clippy::too_many_lines,
    reason = "tests"
)]

//! Attack and policy tests with throwaway keys, an in-memory server and clock.

use std::path::Path;

use helia_update::check::{CheckOptions, Outcome, Updater};
use helia_update::manifest::{AssetDoc, ManifestDoc, Platform};
use helia_update::mem::{FixedClock, MemFs, MemHttp};
use helia_update::sign::KeyPair;
use helia_update::state::{State, StateStore};
use helia_update::verify::sha256_hex;
use helia_update::{Error, Kind, UpdateSource, verify_manifest};

const NOW: u64 = 1_792_864_800; // 2026-10-24T18:00:00Z
const HOST: &str = "downloads.example.org";
const MANIFEST_URL: &str = "https://downloads.example.org/app/stable/manifest.json";
const SIG_URL: &str = "https://downloads.example.org/app/stable/manifest.json.minisig";
const ASSET_URL: &str = "https://downloads.example.org/app/0.8.0/app.AppImage";

fn asset_bytes() -> Vec<u8> {
    (0..200_000u32).map(|i| (i * 31 % 251) as u8).collect()
}

fn doc(version: &str, sequence: u64, expires_in: i64) -> ManifestDoc {
    let a = asset_bytes();
    ManifestDoc {
        schema: 1,
        app: "app".into(),
        channel: "stable".into(),
        version: version.into(),
        date: helia_update::time::format_utc(NOW - 3600),
        sequence,
        expires: helia_update::time::format_utc(NOW.saturating_add_signed(expires_in)),
        min_supported: Some("0.7.0".into()),
        notes_url: Some("https://downloads.example.org/notes".into()),
        assets: vec![AssetDoc {
            platform: "linux-x86_64".into(),
            kind: "appimage".into(),
            url: ASSET_URL.into(),
            sha256: sha256_hex(&a),
            size: a.len() as u64,
        }],
    }
}

fn json(d: &ManifestDoc) -> Vec<u8> {
    serde_json::to_vec_pretty(d).unwrap()
}

fn source(keys: &[&KeyPair]) -> UpdateSource {
    let pubs: Vec<String> = keys.iter().map(|k| k.public_key()).collect();
    let refs: Vec<&str> = pubs.iter().map(String::as_str).collect();
    UpdateSource::new("app", "stable", MANIFEST_URL, &refs)
}

struct World {
    http: MemHttp,
    fs: MemFs,
    clock: FixedClock,
}

impl World {
    fn new() -> Self {
        Self {
            http: MemHttp::new(),
            fs: MemFs::new(),
            clock: FixedClock::new(NOW),
        }
    }

    fn publish(&self, bytes: &[u8], sig: &str) {
        self.http.serve(MANIFEST_URL, bytes);
        self.http.serve(SIG_URL, sig.as_bytes());
        self.http.serve(ASSET_URL, &asset_bytes());
    }

    fn updater(&self) -> Updater<'_> {
        Updater {
            http: &self.http,
            fs: &self.fs,
            clock: &self.clock,
            access: None,
        }
    }
}

fn sign(k: &KeyPair, bytes: &[u8]) -> String {
    k.sign(bytes, "timestamp:1\tfile:manifest.json")
}

fn check(
    w: &World,
    src: &UpdateSource,
    current: &str,
    st: &mut State,
    opts: CheckOptions,
) -> Result<Outcome, Error> {
    w.updater().check(src, current, st, opts)
}

#[test]
fn good_manifest_offers_an_update() {
    let (k, w) = (KeyPair::generate().unwrap(), World::new());
    let b = json(&doc("0.8.0", 5, 86_400));
    w.publish(&b, &sign(&k, &b));
    let mut st = State::new("app", "stable");
    let Outcome::Available(u) = check(
        &w,
        &source(&[&k]),
        "0.7.1",
        &mut st,
        CheckOptions::default(),
    )
    .unwrap() else {
        panic!("expected an update")
    };
    assert!(!u.downgrade && !u.required);
    assert_eq!(st.highest_sequence, 5);
    assert_eq!(st.last_check, NOW);
    let a = u
        .asset(Platform::LinuxX86_64, &[Kind::Deb, Kind::Appimage])
        .unwrap();
    assert_eq!(a.kind, Kind::Appimage);
    // Below min_supported flags the update as required.
    let Outcome::Available(u) = check(
        &w,
        &source(&[&k]),
        "0.6.0",
        &mut st,
        CheckOptions::default(),
    )
    .unwrap() else {
        panic!()
    };
    assert!(u.required);
    // Same version: up to date.
    assert!(matches!(
        check(
            &w,
            &source(&[&k]),
            "0.8.0",
            &mut st,
            CheckOptions::default()
        )
        .unwrap(),
        Outcome::UpToDate { .. }
    ));
}

#[test]
fn tampered_manifest_is_rejected() {
    let (k, w) = (KeyPair::generate().unwrap(), World::new());
    let b = json(&doc("0.8.0", 5, 86_400));
    let sig = sign(&k, &b);
    let evil = String::from_utf8(b)
        .unwrap()
        .replace("0.8.0", "9.9.9")
        .into_bytes();
    w.publish(&evil, &sig);
    let r = check(
        &w,
        &source(&[&k]),
        "0.7.1",
        &mut State::new("app", "stable"),
        CheckOptions::default(),
    );
    assert!(matches!(r, Err(Error::BadSignature)), "{r:?}");
    // One flipped bit anywhere is caught.
    let good = json(&doc("0.8.0", 5, 86_400));
    let sig = sign(&k, &good);
    for i in (0..good.len()).step_by(37) {
        let mut t = good.clone();
        t[i] ^= 1;
        assert!(
            verify_manifest(&source(&[&k]), &t, &sig, NOW, 0, false).is_err(),
            "byte {i}"
        );
    }
}

#[test]
fn bad_and_missing_signature_formats() {
    let (k, w) = (KeyPair::generate().unwrap(), World::new());
    let b = json(&doc("0.8.0", 5, 86_400));
    w.publish(&b, "this is not a signature");
    let r = check(
        &w,
        &source(&[&k]),
        "0.7.1",
        &mut State::new("app", "stable"),
        CheckOptions::default(),
    );
    assert!(matches!(r, Err(Error::BadSignatureFormat(_))), "{r:?}");
    let w2 = World::new(); // nothing served: 404
    let r = check(
        &w2,
        &source(&[&k]),
        "0.7.1",
        &mut State::new("app", "stable"),
        CheckOptions::default(),
    );
    assert!(matches!(r, Err(Error::HttpStatus(404))), "{r:?}");
}

#[test]
fn signature_by_an_unpinned_key_is_rejected() {
    let (pinned, attacker, w) = (
        KeyPair::generate().unwrap(),
        KeyPair::generate().unwrap(),
        World::new(),
    );
    let b = json(&doc("0.8.0", 5, 86_400));
    w.publish(&b, &sign(&attacker, &b));
    let r = check(
        &w,
        &source(&[&pinned]),
        "0.7.1",
        &mut State::new("app", "stable"),
        CheckOptions::default(),
    );
    assert!(matches!(r, Err(Error::BadSignature)), "{r:?}");
}

#[test]
fn rotation_accepts_either_pinned_key() {
    let (old, new, w) = (
        KeyPair::generate().unwrap(),
        KeyPair::generate().unwrap(),
        World::new(),
    );
    let src = source(&[&old, &new]);
    let b = json(&doc("0.8.0", 5, 86_400));
    for k in [&old, &new] {
        w.publish(&b, &sign(k, &b));
        let r = check(
            &w,
            &src,
            "0.7.1",
            &mut State::new("app", "stable"),
            CheckOptions::default(),
        );
        assert!(matches!(r, Ok(Outcome::Available(_))), "{r:?}");
    }
    // A third key is still refused, and three pinned keys are not a valid source.
    let third = KeyPair::generate().unwrap();
    w.publish(&b, &sign(&third, &b));
    assert!(
        check(
            &w,
            &src,
            "0.7.1",
            &mut State::new("app", "stable"),
            CheckOptions::default()
        )
        .is_err()
    );
    let mut three = src.clone();
    three.pinned_keys.push(third.public_key());
    assert!(matches!(three.validate(false), Err(Error::KeyCount(3))));
}

#[test]
fn downgrade_is_refused_unless_explicit() {
    let (k, w) = (KeyPair::generate().unwrap(), World::new());
    let b = json(&doc("0.6.0", 9, 86_400));
    w.publish(&b, &sign(&k, &b));
    let src = source(&[&k]);
    let r = check(
        &w,
        &src,
        "0.7.1",
        &mut State::new("app", "stable"),
        CheckOptions::default(),
    )
    .unwrap();
    assert!(matches!(r, Outcome::OlderOnServer { .. }), "{r:?}");
    let opts = CheckOptions {
        allow_downgrade: true,
        ..CheckOptions::default()
    };
    let Outcome::Available(u) =
        check(&w, &src, "0.7.1", &mut State::new("app", "stable"), opts).unwrap()
    else {
        panic!()
    };
    assert!(u.downgrade);
}

#[test]
fn replayed_old_sequence_is_refused() {
    let (k, w) = (KeyPair::generate().unwrap(), World::new());
    let src = source(&[&k]);
    let mut st = State::new("app", "stable");
    let new = json(&doc("0.8.0", 10, 86_400));
    w.publish(&new, &sign(&k, &new));
    check(&w, &src, "0.7.1", &mut st, CheckOptions::default()).unwrap();
    assert_eq!(st.highest_sequence, 10);
    // An attacker replays the older, validly signed manifest.
    let old = json(&doc("0.7.5", 9, 86_400));
    w.publish(&old, &sign(&k, &old));
    let r = check(&w, &src, "0.7.1", &mut st, CheckOptions::default());
    assert!(
        matches!(
            r,
            Err(Error::Replay {
                seen: 9,
                highest: 10
            })
        ),
        "{r:?}"
    );
    assert_eq!(st.highest_sequence, 10);
    // Even with allow_downgrade the replay is refused: only a higher sequence rolls back.
    let opts = CheckOptions {
        allow_downgrade: true,
        ..CheckOptions::default()
    };
    assert!(check(&w, &src, "0.7.1", &mut st, opts).is_err());
    // The same sequence again (re-fetch) is fine.
    w.publish(&new, &sign(&k, &new));
    assert!(check(&w, &src, "0.7.1", &mut st, CheckOptions::default()).is_ok());
}

#[test]
fn state_persists_the_replay_memory() {
    let (k, w) = (KeyPair::generate().unwrap(), World::new());
    let src = source(&[&k]);
    let store = StateStore::new(&w.fs, Path::new("/cfg/update.json"));
    let new = json(&doc("0.8.0", 10, 86_400));
    w.publish(&new, &sign(&k, &new));
    w.updater()
        .check_stored(&src, "0.7.1", &store, CheckOptions::default())
        .unwrap();
    let old = json(&doc("0.7.5", 3, 86_400));
    w.publish(&old, &sign(&k, &old));
    let r = w
        .updater()
        .check_stored(&src, "0.7.1", &store, CheckOptions::default());
    assert!(matches!(r, Err(Error::Replay { .. })));
    let st = store.load("app", "stable").unwrap();
    assert_eq!((st.highest_sequence, st.last_check), (10, NOW));
}

#[test]
fn expired_manifest_is_refused() {
    let (k, w) = (KeyPair::generate().unwrap(), World::new());
    let b = json(&doc("0.8.0", 5, 600));
    w.publish(&b, &sign(&k, &b));
    let src = source(&[&k]);
    assert!(
        check(
            &w,
            &src,
            "0.7.1",
            &mut State::new("app", "stable"),
            CheckOptions::default()
        )
        .is_ok()
    );
    w.clock.set(NOW + 601); // a freeze attack: the old manifest is replayed after it lapsed
    let r = check(
        &w,
        &src,
        "0.7.1",
        &mut State::new("app", "stable"),
        CheckOptions::default(),
    );
    assert!(matches!(r, Err(Error::Expired { .. })), "{r:?}");
}

#[test]
fn mix_and_match_wrong_app_or_channel_is_refused() {
    let (k, w) = (KeyPair::generate().unwrap(), World::new());
    let src = source(&[&k]);
    for (app, channel) in [("other", "stable"), ("app", "prerelease")] {
        let mut d = doc("0.8.0", 5, 86_400);
        d.app = app.into();
        d.channel = channel.into();
        let b = json(&d);
        w.publish(&b, &sign(&k, &b));
        let r = check(
            &w,
            &src,
            "0.7.1",
            &mut State::new("app", "stable"),
            CheckOptions::default(),
        );
        assert!(
            matches!(r, Err(Error::WrongTarget { .. })),
            "{app}/{channel}: {r:?}"
        );
    }
}

#[test]
fn stable_channel_refuses_a_prerelease_but_prerelease_channel_takes_it() {
    let (k, w) = (KeyPair::generate().unwrap(), World::new());
    let b = json(&doc("0.9.0-rc.1", 5, 86_400));
    w.publish(&b, &sign(&k, &b));
    let r = check(
        &w,
        &source(&[&k]),
        "0.8.0",
        &mut State::new("app", "stable"),
        CheckOptions::default(),
    );
    assert!(matches!(r, Err(Error::PrereleaseOnStable(_))), "{r:?}");
    let mut d = doc("0.9.0-rc.1", 5, 86_400);
    d.channel = "prerelease".into();
    let b = json(&d);
    let url = "https://downloads.example.org/app/prerelease/manifest.json";
    w.http.serve(url, &b);
    w.http
        .serve(&format!("{url}.minisig"), sign(&k, &b).as_bytes());
    let mut src = source(&[&k]);
    src.channel = "prerelease".into();
    src.manifest_url = url.into();
    // rc.1 is newer than 0.8.0 and older than the final 0.9.0.
    assert!(matches!(
        check(
            &w,
            &src,
            "0.8.0",
            &mut State::new("app", "prerelease"),
            CheckOptions::default()
        ),
        Ok(Outcome::Available(_))
    ));
    assert!(matches!(
        check(
            &w,
            &src,
            "0.9.0",
            &mut State::new("app", "prerelease"),
            CheckOptions::default()
        ),
        Ok(Outcome::OlderOnServer { .. })
    ));
}

#[test]
fn newer_schema_asks_for_a_manual_update() {
    let (k, w) = (KeyPair::generate().unwrap(), World::new());
    let b = String::from_utf8(json(&doc("0.8.0", 5, 86_400)))
        .unwrap()
        .replace("\"schema\": 1", "\"schema\": 2")
        .into_bytes();
    w.publish(&b, &sign(&k, &b));
    let r = check(
        &w,
        &source(&[&k]),
        "0.7.1",
        &mut State::new("app", "stable"),
        CheckOptions::default(),
    );
    assert!(
        matches!(r, Ok(Outcome::SchemaTooNew { schema: 2 })),
        "{r:?}"
    );
}

#[test]
fn oversized_and_deeply_nested_input_is_rejected_before_parsing() {
    let (k, w) = (KeyPair::generate().unwrap(), World::new());
    let src = source(&[&k]);
    let big = vec![b' '; 70_000];
    let r = verify_manifest(&src, &big, &sign(&k, &big), NOW, 0, false);
    assert!(matches!(r, Err(Error::TooLarge { .. })), "{r:?}");
    // Signed but absurdly deep: refused by the depth limit, not by a stack overflow.
    let deep = format!("{}{}", "[".repeat(5000), "]".repeat(5000)).into_bytes();
    let r = verify_manifest(&src, &deep, &sign(&k, &deep), NOW, 0, false);
    assert!(matches!(r, Err(Error::TooDeep)), "{r:?}");
    // The fetch itself stops reading at the limit.
    w.http.serve(MANIFEST_URL, &vec![b'x'; 5_000_000]);
    w.http.serve(SIG_URL, b"x");
    let r = check(
        &w,
        &src,
        "0.7.1",
        &mut State::new("app", "stable"),
        CheckOptions::default(),
    );
    assert!(matches!(r, Err(Error::TooLarge { .. })), "{r:?}");
}

#[test]
fn unknown_fields_kinds_and_platforms_are_tolerated() {
    let (k, w) = (KeyPair::generate().unwrap(), World::new());
    let mut v: serde_json::Value = serde_json::from_slice(&json(&doc("0.8.0", 5, 86_400))).unwrap();
    v["future_field"] = serde_json::json!({"x": [1, 2, 3]});
    v["assets"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"platform": "plan9-mips", "kind": "tgz"}));
    v["assets"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"platform": "linux-x86_64", "kind": "snap", "url": "x"}));
    let b = serde_json::to_vec(&v).unwrap();
    w.publish(&b, &sign(&k, &b));
    let Outcome::Available(u) = check(
        &w,
        &source(&[&k]),
        "0.7.1",
        &mut State::new("app", "stable"),
        CheckOptions::default(),
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(u.manifest.assets.len(), 1);
    assert_eq!(u.manifest.skipped_assets, 2);
}

#[test]
fn insecure_manifest_urls_are_refused() {
    let k = KeyPair::generate().unwrap();
    let w = World::new();
    let mut src = source(&[&k]);
    src.manifest_url = "http://downloads.example.org/app/stable/manifest.json".into();
    let r = check(
        &w,
        &src,
        "0.7.1",
        &mut State::new("app", "stable"),
        CheckOptions::default(),
    );
    assert!(matches!(r, Err(Error::InsecureUrl(_))), "{r:?}");
    // An asset URL that is plain http is refused when the manifest is parsed.
    let mut d = doc("0.8.0", 5, 86_400);
    d.assets[0].url = "http://downloads.example.org/app.AppImage".into();
    let b = json(&d);
    assert!(verify_manifest(&source(&[&k]), &b, &sign(&k, &b), NOW, 0, false).is_err());
}

#[test]
fn asset_on_another_host_is_listed_but_never_downloaded() {
    let (k, w) = (KeyPair::generate().unwrap(), World::new());
    let mut d = doc("0.8.0", 5, 86_400);
    d.assets[0].url = "https://evil.example.net/app.AppImage".into();
    let b = json(&d);
    w.publish(&b, &sign(&k, &b));
    let src = source(&[&k]);
    let r = check(
        &w,
        &src,
        "0.7.1",
        &mut State::new("app", "stable"),
        CheckOptions::default(),
    )
    .expect("the signed manifest is accepted");
    let Outcome::Available(u) = r else {
        panic!("{r:?}")
    };
    let a = u
        .asset(Platform::LinuxX86_64, &[Kind::Appimage])
        .unwrap()
        .clone();
    let before = w.http.requests().len();
    assert!(matches!(
        w.updater()
            .download(&src, &u, &a, Path::new("/s"), false, &mut |_, _| {}),
        Err(Error::ForeignHost(_))
    ));
    assert_eq!(
        w.http.requests().len(),
        before,
        "the host was never contacted"
    );
    // Look-alike hosts do not match: exact host only.
    for u in [
        "https://downloads.example.org.evil.net/a",
        "https://evildownloads.example.org/a",
        "https://downloads.example.org@evil.net/a",
    ] {
        assert!(!source(&[&k]).asset_host_allowed(u), "{u}");
    }
}

#[test]
fn redirect_to_another_host_is_refused() {
    let (k, w) = (KeyPair::generate().unwrap(), World::new());
    let b = json(&doc("0.8.0", 5, 86_400));
    w.publish(&b, &sign(&k, &b));
    w.http
        .redirect(MANIFEST_URL, "https://evil.example.net/manifest.json");
    let r = check(
        &w,
        &source(&[&k]),
        "0.7.1",
        &mut State::new("app", "stable"),
        CheckOptions::default(),
    );
    assert!(matches!(r, Err(Error::ForeignHost(_))), "{r:?}");
}

fn offer(w: &World, k: &KeyPair) -> (UpdateSource, helia_update::Update) {
    let b = json(&doc("0.8.0", 5, 86_400));
    w.publish(&b, &sign(k, &b));
    let src = source(&[k]);
    let Outcome::Available(u) = check(
        w,
        &src,
        "0.7.1",
        &mut State::new("app", "stable"),
        CheckOptions::default(),
    )
    .unwrap() else {
        panic!()
    };
    (src, u)
}

#[test]
fn download_verifies_and_renames_atomically() {
    let (k, w) = (KeyPair::generate().unwrap(), World::new());
    let (src, u) = offer(&w, &k);
    let a = u
        .asset(Platform::LinuxX86_64, &[Kind::Appimage])
        .unwrap()
        .clone();
    let mut last = (0, 0);
    let s = w
        .updater()
        .download(&src, &u, &a, Path::new("/stage"), false, &mut |d, t| {
            last = (d, t);
        })
        .unwrap();
    assert_eq!(last, (200_000, 200_000));
    assert_eq!(w.fs.get(&s.path).unwrap(), asset_bytes());
    assert!(
        w.fs.paths()
            .iter()
            .all(|p| !p.to_string_lossy().ends_with(".part"))
    );
    // A second call finds the finished file and does not fetch again.
    let n = w.http.requests().len();
    w.updater()
        .download(&src, &u, &a, Path::new("/stage"), false, &mut |_, _| {})
        .unwrap();
    assert_eq!(w.http.requests().len(), n);
}

#[test]
fn truncated_download_is_detected_and_resumed() {
    let (k, w) = (KeyPair::generate().unwrap(), World::new());
    let (src, u) = offer(&w, &k);
    let a = u
        .asset(Platform::LinuxX86_64, &[Kind::Appimage])
        .unwrap()
        .clone();
    w.http.tweak(ASSET_URL, |r| r.cut_next_at = Some(70_000));
    let r = w
        .updater()
        .download(&src, &u, &a, Path::new("/stage"), false, &mut |_, _| {});
    assert!(
        matches!(
            r,
            Err(Error::Truncated {
                got: 70_000,
                expected: 200_000
            })
        ),
        "{r:?}"
    );
    assert!(
        w.fs.paths()
            .iter()
            .any(|p| p.to_string_lossy().ends_with(".part")),
        "partial file kept"
    );
    let s = w
        .updater()
        .download(&src, &u, &a, Path::new("/stage"), false, &mut |_, _| {})
        .unwrap();
    assert_eq!(w.fs.get(&s.path).unwrap(), asset_bytes());
    let reqs = w.http.requests();
    assert_eq!(
        reqs.last().unwrap(),
        &(ASSET_URL.to_string(), Some(70_000)),
        "resumed with a Range request"
    );
}

#[test]
fn server_that_ignores_range_restarts_cleanly() {
    let (k, w) = (KeyPair::generate().unwrap(), World::new());
    let (src, u) = offer(&w, &k);
    let a = u
        .asset(Platform::LinuxX86_64, &[Kind::Appimage])
        .unwrap()
        .clone();
    w.http.tweak(ASSET_URL, |r| r.cut_next_at = Some(1000));
    assert!(
        w.updater()
            .download(&src, &u, &a, Path::new("/stage"), false, &mut |_, _| {})
            .is_err()
    );
    w.http.tweak(ASSET_URL, |r| r.ignore_range = true);
    let s = w
        .updater()
        .download(&src, &u, &a, Path::new("/stage"), false, &mut |_, _| {})
        .unwrap();
    assert_eq!(w.fs.get(&s.path).unwrap(), asset_bytes());
}

#[test]
fn sha256_mismatch_is_rejected_and_discarded() {
    let (k, w) = (KeyPair::generate().unwrap(), World::new());
    let (src, u) = offer(&w, &k);
    let a = u
        .asset(Platform::LinuxX86_64, &[Kind::Appimage])
        .unwrap()
        .clone();
    // The attacker serves a same-size, different file at the asset URL.
    let mut evil = asset_bytes();
    evil[100] ^= 0xff;
    w.http.serve(ASSET_URL, &evil);
    let r = w
        .updater()
        .download(&src, &u, &a, Path::new("/stage"), false, &mut |_, _| {});
    assert!(matches!(r, Err(Error::HashMismatch)), "{r:?}");
    assert!(
        w.fs.paths().is_empty(),
        "nothing left behind: {:?}",
        w.fs.paths()
    );
}

#[test]
fn oversize_download_is_rejected() {
    let (k, w) = (KeyPair::generate().unwrap(), World::new());
    let (src, u) = offer(&w, &k);
    let a = u
        .asset(Platform::LinuxX86_64, &[Kind::Appimage])
        .unwrap()
        .clone();
    let mut big = asset_bytes();
    big.extend_from_slice(b"extra");
    w.http.serve(ASSET_URL, &big);
    let r = w
        .updater()
        .download(&src, &u, &a, Path::new("/stage"), false, &mut |_, _| {});
    assert!(matches!(r, Err(Error::TooMuchData)), "{r:?}");
    assert_eq!(w.fs.paths(), Vec::<std::path::PathBuf>::new());
}

#[test]
fn download_refuses_foreign_host_and_plain_http() {
    let (k, w) = (KeyPair::generate().unwrap(), World::new());
    let (src, u) = offer(&w, &k);
    let mut a = u
        .asset(Platform::LinuxX86_64, &[Kind::Appimage])
        .unwrap()
        .clone();
    a.url = "https://evil.example.net/x".into();
    assert!(matches!(
        w.updater()
            .download(&src, &u, &a, Path::new("/s"), false, &mut |_, _| {}),
        Err(Error::ForeignHost(_))
    ));
    a.url = format!("http://{HOST}/x");
    assert!(matches!(
        w.updater()
            .download(&src, &u, &a, Path::new("/s"), false, &mut |_, _| {}),
        Err(Error::InsecureUrl(_))
    ));
}

#[test]
fn the_pinned_release_key_is_valid_and_rejects_other_signers() {
    let src = helia_update::helia::source("stable");
    src.validate(false).unwrap();
    const { assert!(!helia_update::helia::KEYS_ARE_PLACEHOLDER) };
    assert_eq!(
        helia_update::helia::PINNED_KEYS,
        ["RWQgIPQNjfglV5U0HSmFdRpTCMtwAuMrkaqgTANRv4KHD+ZRfen3Vw4J"]
    );
    // A key that is not the release key verifies nothing.
    let k = KeyPair::generate().unwrap();
    let b = json(&doc("0.8.0", 5, 86_400));
    assert!(verify_manifest(&src, &b, &sign(&k, &b), NOW, 0, false).is_err());
}

#[test]
fn manifest_and_asset_hosts_are_separate_lists() {
    let k = KeyPair::generate().unwrap();
    // The fallback layout: manifest on 12hobbies.studio, files on GitHub.
    let s = helia_update::helia::source_from(
        "https://12hobbies.studio/updates",
        Some("12hobbies.studio"),
        Some("github.com, objects.githubusercontent.com"),
        "stable",
    );
    assert_eq!(
        s.manifest_url,
        "https://12hobbies.studio/updates/helia/stable/manifest.json"
    );
    assert!(s.manifest_host_allowed("https://12hobbies.studio/x"));
    assert!(
        !s.manifest_host_allowed("https://github.com/x"),
        "a file host may not serve the manifest"
    );
    assert!(s.asset_host_allowed("https://github.com/qisback/helia/releases/download/v1/a"));
    assert!(s.asset_host_allowed("https://objects.githubusercontent.com/a"));
    assert!(
        !s.asset_host_allowed("https://12hobbies.studio/a"),
        "the manifest host may not serve files"
    );
    // Defaults come from the base URL.
    let d = helia_update::helia::source_from(
        "https://downloads.12hobbies.studio",
        None,
        Some(""),
        "stable",
    );
    assert_eq!(d.manifest_hosts, ["downloads.12hobbies.studio"]);
    assert_eq!(d.asset_hosts, ["downloads.12hobbies.studio"]);
    // The loopback test setup validates only when plain http is allowed.
    let t = helia_update::helia::source_from(
        "http://127.0.0.1:8123",
        Some("127.0.0.1"),
        Some("127.0.0.1"),
        "stable",
    );
    assert!(t.validate(false).is_err());
    t.validate(true).unwrap();
    // An asset on a manifest-only host is accepted with the manifest; the download refuses it.
    let mut src = source(&[&k]).with_hosts(&[HOST], &["files.example.org"]);
    let b = json(&doc("0.8.0", 5, 86_400));
    assert!(verify_manifest(&src, &b, &sign(&k, &b), NOW, 0, false).is_ok());
    assert!(!src.asset_host_allowed(&format!("https://{HOST}/x")));
    src.asset_hosts = vec![HOST.into()];
    assert!(verify_manifest(&src, &b, &sign(&k, &b), NOW, 0, false).is_ok());
}
