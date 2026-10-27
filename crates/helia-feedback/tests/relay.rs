// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The client against a fake relay on a loopback port: the 201, 429, 413, 503
//! and unreachable paths, the proof of work as the relay checks it, and the
//! swap of the authentication method.
#![allow(clippy::unwrap_used, reason = "tests")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread;

use helia_feedback::client::{
    AuthProof, FeedbackError, Phase, ProofOfWork, Relay, RelayAuth, default_agent, submit,
};
use helia_feedback::pow;
use helia_feedback::protocol::{Highlight, Kind, Report};
use helia_feedback::screenshot::Screenshot;

#[derive(Clone, Copy)]
enum Mode {
    Created,
    RateLimited,
    TooBig,
    Degraded,
    ChallengeDown,
    TooHard,
}

#[derive(Default)]
struct Seen {
    report: Option<String>,
    screenshot_len: Option<usize>,
    pow_ok: Option<bool>,
}

const DIFFICULTY: u32 = 10;
const CHALLENGE: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

fn read_request(s: &mut TcpStream) -> (String, String, Vec<u8>) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let header_end = loop {
        let n = s.read(&mut tmp).unwrap();
        assert!(n > 0, "connection closed early");
        buf.extend_from_slice(&tmp[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let len = head
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    while buf.len() < header_end + len {
        let n = s.read(&mut tmp).unwrap();
        assert!(n > 0, "body cut short");
        buf.extend_from_slice(&tmp[..n]);
    }
    let first = head.lines().next().unwrap().to_owned();
    (first, head, buf[header_end..header_end + len].to_vec())
}

fn respond(s: &mut TcpStream, status: &str, extra: &str, body: &str) {
    let r = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}",
        body.len()
    );
    s.write_all(r.as_bytes()).unwrap();
}

/// Split the multipart body at its boundary into (name, content) parts.
fn parts(head: &str, body: &[u8]) -> Vec<(String, Vec<u8>)> {
    let ct = head
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-type:"))
        .unwrap();
    let boundary = format!("--{}", ct.split("boundary=").nth(1).unwrap().trim());
    let text = body;
    let mut out = Vec::new();
    let b = boundary.as_bytes();
    let mut starts = Vec::new();
    let mut i = 0;
    while i + b.len() <= text.len() {
        if &text[i..i + b.len()] == b {
            starts.push(i);
            i += b.len();
        } else {
            i += 1;
        }
    }
    for w in starts.windows(2) {
        let seg = &text[w[0] + b.len() + 2..w[1] - 2];
        let split = seg.windows(4).position(|x| x == b"\r\n\r\n").unwrap();
        let hdr = String::from_utf8_lossy(&seg[..split]).to_string();
        let name = hdr
            .split("name=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        out.push((name.to_owned(), seg[split + 4..].to_vec()));
    }
    out
}

fn fake_relay(mode: Mode) -> (Relay, Arc<Mutex<Seen>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let seen2 = seen.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { break };
            let (first, head, body) = read_request(&mut s);
            if first.starts_with("GET /api/v1/challenge ") {
                match mode {
                    Mode::ChallengeDown => respond(&mut s, "503 Service Unavailable", "", "{}"),
                    Mode::TooHard => respond(
                        &mut s,
                        "200 OK",
                        "",
                        &format!(
                            r#"{{"challenge":"{CHALLENGE}","difficulty":60,"expires":"2030-01-01T00:00:00Z"}}"#
                        ),
                    ),
                    _ => respond(
                        &mut s,
                        "200 OK",
                        "",
                        &format!(
                            r#"{{"challenge":"{CHALLENGE}","difficulty":{DIFFICULTY},"expires":"2030-01-01T00:00:00Z"}}"#
                        ),
                    ),
                }
            } else if first.starts_with("POST /api/v1/feedback ") {
                let ps = parts(&head, &body);
                let report = String::from_utf8(ps[0].1.clone()).unwrap();
                let v: serde_json::Value = serde_json::from_str(&report).unwrap();
                let nonce = v["pow"]["nonce"].as_u64().unwrap_or(0);
                let ok =
                    v["pow"]["challenge"] == CHALLENGE && pow::verify(CHALLENGE, nonce, DIFFICULTY);
                {
                    let mut g = seen2.lock().unwrap();
                    g.report = Some(report);
                    g.screenshot_len = ps.get(1).map(|p| p.1.len());
                    g.pow_ok = Some(ok);
                }
                match mode {
                    Mode::Created if ok => respond(
                        &mut s,
                        "201 Created",
                        "",
                        r#"{"issue_number":81,"issue_url":"https://github.com/qisback/helia/issues/81"}"#,
                    ),
                    Mode::RateLimited => respond(
                        &mut s,
                        "429 Too Many Requests",
                        "Retry-After: 120\r\n",
                        "{}",
                    ),
                    Mode::TooBig => respond(
                        &mut s,
                        "413 Payload Too Large",
                        "",
                        r#"{"error":{"code":"screenshot_too_large","message":"Screenshot over 5 MiB"}}"#,
                    ),
                    Mode::Degraded => respond(&mut s, "503 Service Unavailable", "", "{}"),
                    _ => respond(
                        &mut s,
                        "400 Bad Request",
                        "",
                        r#"{"error":{"code":"bad_pow","message":"proof of work failed"}}"#,
                    ),
                }
            }
        }
    });
    (
        Relay::new(&format!("http://127.0.0.1:{port}")).unwrap(),
        seen,
    )
}

fn report() -> Report {
    Report {
        kind: Kind::Feature,
        title: "Dark mode".into(),
        description: "Please add one.".into(),
        app: None,
        os: Some("Test OS".into()),
        highlights: vec![Highlight {
            x: 10,
            y: 10,
            w: 20,
            h: 20,
        }],
        pow: None,
    }
}

fn shot() -> Screenshot {
    Screenshot::new(64, 48, [30, 60, 90, 255].repeat(64 * 48)).unwrap()
}

fn run(
    relay: &Relay,
    auth: &dyn RelayAuth,
    with_shot: bool,
) -> (Result<helia_feedback::Created, FeedbackError>, Vec<Phase>) {
    let mut phases = Vec::new();
    let s = shot();
    let r = submit(
        relay,
        auth,
        report(),
        with_shot.then_some(&s),
        &AtomicBool::new(false),
        &mut |p| phases.push(p),
    );
    (r, phases)
}

#[test]
fn a_report_is_filed_and_the_issue_returned() {
    let (relay, seen) = fake_relay(Mode::Created);
    let (r, phases) = run(&relay, &ProofOfWork, true);
    let created = r.unwrap();
    assert_eq!(created.issue_number, 81);
    assert_eq!(
        created.issue_url,
        "https://github.com/qisback/helia/issues/81"
    );
    assert_eq!(phases, [Phase::Preparing, Phase::Sending]);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.pow_ok, Some(true), "the relay's check of the proof");
    assert!(seen.screenshot_len.unwrap() > 50, "a PNG came with it");
    let v: serde_json::Value = serde_json::from_str(seen.report.as_ref().unwrap()).unwrap();
    assert_eq!(v["kind"], "feature");
    assert_eq!(v["highlights"][0]["w"], 20);
    assert!(v["app"].is_null());
}

#[test]
fn without_a_screenshot_there_is_no_part_and_no_highlights() {
    let (relay, seen) = fake_relay(Mode::Created);
    run(&relay, &ProofOfWork, false).0.unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.screenshot_len, None);
    let v: serde_json::Value = serde_json::from_str(seen.report.as_ref().unwrap()).unwrap();
    assert_eq!(v["highlights"].as_array().unwrap().len(), 0);
}

#[test]
fn a_busy_relay_says_when_to_retry_and_fallbacks_are_offered() {
    let (relay, _) = fake_relay(Mode::RateLimited);
    let e = run(&relay, &ProofOfWork, true).0.unwrap_err();
    assert!(matches!(
        e,
        FeedbackError::RateLimited {
            retry_after: Some(120)
        }
    ));
    assert!(e.offers_fallback());
    assert!(e.to_string().contains("120 seconds"));
}

#[test]
fn a_refusal_carries_the_relays_message() {
    let (relay, _) = fake_relay(Mode::TooBig);
    match run(&relay, &ProofOfWork, true).0.unwrap_err() {
        FeedbackError::Rejected {
            status,
            code,
            message,
        } => {
            assert_eq!(status, 413);
            assert_eq!(code, "screenshot_too_large");
            assert!(message.contains("5 MiB"));
        }
        e => panic!("{e:?}"),
    }
}

#[test]
fn a_degraded_relay_is_reported_and_offers_fallbacks() {
    for mode in [Mode::Degraded, Mode::ChallengeDown] {
        let (relay, _) = fake_relay(mode);
        let e = run(&relay, &ProofOfWork, true).0.unwrap_err();
        assert!(matches!(e, FeedbackError::Degraded), "{e:?}");
        assert!(e.offers_fallback());
    }
}

#[test]
fn an_unreachable_relay_offers_the_fallbacks() {
    // Bind and drop to find a port nothing listens on.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let relay = Relay::new(&format!("http://127.0.0.1:{port}")).unwrap();
    let e = run(&relay, &ProofOfWork, true).0.unwrap_err();
    assert!(matches!(e, FeedbackError::Unreachable(_)), "{e:?}");
    assert!(e.offers_fallback());
}

#[test]
fn an_unreasonable_challenge_is_not_attempted() {
    let (relay, _) = fake_relay(Mode::TooHard);
    let e = run(&relay, &ProofOfWork, true).0.unwrap_err();
    assert!(matches!(e, FeedbackError::ChallengeTooHard(60)), "{e:?}");
}

#[test]
fn cancelling_before_the_proof_stops_the_send() {
    let (relay, seen) = fake_relay(Mode::Created);
    let cancel = AtomicBool::new(true);
    let s = shot();
    let e = submit(
        &relay,
        &ProofOfWork,
        report(),
        Some(&s),
        &cancel,
        &mut |_| {},
    )
    .unwrap_err();
    assert!(matches!(e, FeedbackError::Cancelled));
    assert!(!e.offers_fallback());
    assert!(seen.lock().unwrap().report.is_none(), "nothing was posted");
}

#[test]
fn an_invalid_report_never_leaves_the_machine() {
    let (relay, seen) = fake_relay(Mode::Created);
    let mut bad = report();
    bad.title = String::new();
    let e = submit(
        &relay,
        &ProofOfWork,
        bad,
        None,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap_err();
    assert!(matches!(e, FeedbackError::Invalid(_)));
    assert!(seen.lock().unwrap().report.is_none());
}

/// A stand-in for the planned client-certificate method: it adds no proof of
/// work, and the rest of the flow is untouched.
struct NoProof;

impl RelayAuth for NoProof {
    fn name(&self) -> &'static str {
        "test-no-proof"
    }
    fn prepare(
        &self,
        _: &Relay,
        _: &ureq::Agent,
        _: &AtomicBool,
    ) -> Result<AuthProof, FeedbackError> {
        let _ = default_agent();
        Ok(AuthProof::default())
    }
}

#[test]
fn the_authentication_method_is_swappable() {
    let (relay, seen) = fake_relay(Mode::Created);
    // The fake relay insists on a proof, so it refuses this one; what matters
    // is that the report went out without a `pow` member.
    let e = run(&relay, &NoProof, false).0.unwrap_err();
    assert!(
        matches!(e, FeedbackError::Rejected { status: 400, .. }),
        "{e:?}"
    );
    let seen = seen.lock().unwrap();
    let v: serde_json::Value = serde_json::from_str(seen.report.as_ref().unwrap()).unwrap();
    assert!(v.get("pow").is_none());
}
