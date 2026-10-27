// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Malicious-input suite for the host side of the plugin protocol: what a hostile or broken
//! plugin (or manifest) can send must be refused without a panic, a hang or a large allocation.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::assert_is_empty,
    clippy::format_push_string,
    clippy::many_single_char_names,
    missing_docs
)]

use std::io::{BufRead, BufReader, Read};

use helia_plugin::manifest::{self, Loaded};
use helia_plugin::schema::Schema;
use helia_plugin::{Registry, StepSpec};
use helia_plugin_sdk::hlpx::{self, DEFAULT_MAX_BYTES, HEADER_LEN, HlpxError, Image, MAGIC};
use helia_plugin_sdk::wire::{self, MAX_DEPTH, MAX_LINE, Message, WireError};
use serde_json::json;

/// An endless stream of one byte: reading it all would never finish.
struct Endless(u8);

impl Read for Endless {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        buf.fill(self.0);
        Ok(buf.len())
    }
}

fn header(w: u32, h: u32, c: u32) -> Vec<u8> {
    let mut v = MAGIC.to_vec();
    for x in [1u32, w, h, c, 0] {
        v.extend_from_slice(&x.to_le_bytes());
    }
    v
}

// ---- frames -------------------------------------------------------------------------------

#[test]
fn an_endless_line_stops_at_the_limit() {
    let mut r = BufReader::with_capacity(8192, Endless(b'x'));
    assert_eq!(
        wire::read_line_limited(&mut r, MAX_LINE),
        Err(WireError::LineTooLong)
    );
}

#[test]
fn a_huge_line_with_a_newline_far_away_is_refused() {
    let mut data = vec![b'a'; MAX_LINE + 10];
    data.push(b'\n');
    let mut r = BufReader::new(std::io::Cursor::new(data));
    assert_eq!(
        wire::read_line_limited(&mut r, MAX_LINE),
        Err(WireError::LineTooLong)
    );
}

#[test]
fn a_line_exactly_at_the_limit_is_accepted() {
    let mut data = vec![b'a'; MAX_LINE];
    data.push(b'\n');
    let mut r = BufReader::new(std::io::Cursor::new(data));
    assert_eq!(
        wire::read_line_limited(&mut r, MAX_LINE)
            .unwrap()
            .unwrap()
            .len(),
        MAX_LINE
    );
}

#[test]
fn deeply_nested_json_is_refused_before_parsing() {
    for depth in [MAX_DEPTH + 1, 1000, 1_000_000] {
        let s = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
        assert_eq!(
            wire::parse_message(s.as_bytes()),
            Err(WireError::TooDeep),
            "depth {depth}"
        );
        let o = format!("{}1{}", "{\"a\":".repeat(depth), "}".repeat(depth));
        // Past 4 MiB the length limit answers first.
        let want = if o.len() > MAX_LINE {
            WireError::LineTooLong
        } else {
            WireError::TooDeep
        };
        assert_eq!(
            wire::parse_message(o.as_bytes()),
            Err(want),
            "object depth {depth}"
        );
    }
}

#[test]
fn truncated_frames_are_json_errors_not_panics() {
    let good = br#"{"jsonrpc":"2.0","id":1,"result":{"output":{"path":"a.hlpx"}}}"#;
    for cut in 0..good.len() - 1 {
        let r = wire::parse_message(&good[..cut]);
        assert!(r.is_err(), "cut at {cut} should fail");
    }
    assert!(wire::parse_message(good).is_ok());
}

#[test]
fn bad_utf8_is_refused() {
    for bytes in [
        &[0xffu8][..],
        b"{\"jsonrpc\":\"2.0\",\"method\":\"\xc3\x28\"}",
        b"\xed\xa0\x80",
        b"\xf8\x88\x80\x80\x80",
    ] {
        assert_eq!(wire::parse_message(bytes), Err(WireError::BadUtf8));
    }
}

#[test]
fn json_that_is_not_one_object_is_refused() {
    for s in [
        "[]",
        "[{}]",
        "1",
        "\"x\"",
        "null",
        "true",
        "{}{}",
        "{} {}",
        "{\"jsonrpc\":\"2.0\"}",
        "{\"jsonrpc\":\"1.0\",\"method\":\"m\"}",
    ] {
        assert!(wire::parse_message(s.as_bytes()).is_err(), "{s}");
    }
}

#[test]
fn odd_ids_and_params_are_refused() {
    for s in [
        r#"{"jsonrpc":"2.0","id":[1],"method":"m"}"#,
        r#"{"jsonrpc":"2.0","id":{"a":1},"method":"m"}"#,
        r#"{"jsonrpc":"2.0","id":1,"method":"m","params":5}"#,
        r#"{"jsonrpc":"2.0","id":1,"method":7}"#,
        r#"{"jsonrpc":"2.0","id":1,"result":1,"error":{"code":1}}"#,
        r#"{"jsonrpc":"2.0","id":1}"#,
        r#"{"jsonrpc":"2.0","id":1,"error":"text"}"#,
    ] {
        assert!(wire::parse_message(s.as_bytes()).is_err(), "{s}");
    }
}

#[test]
fn a_long_method_name_is_refused() {
    let s = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"{}"}}"#,
        "m".repeat(5000)
    );
    assert!(wire::parse_message(s.as_bytes()).is_err());
}

#[test]
fn error_text_from_a_plugin_is_clipped() {
    let long = "é".repeat(100_000);
    let s = format!(
        r#"{{"jsonrpc":"2.0","id":1,"error":{{"code":-1,"message":"{long}","data":{{"what":"{long}"}}}}}}"#
    );
    let Message::Response { result: Err(e), .. } = wire::parse_message(s.as_bytes()).unwrap()
    else {
        panic!("expected an error response");
    };
    assert!(e.message.len() <= 2000 && e.what.unwrap().len() <= 2000);
}

#[test]
fn a_flood_of_blank_and_crlf_lines_is_cheap() {
    let mut r = BufReader::new(std::io::Cursor::new(b"\r\n".repeat(100_000)));
    let mut n = 0;
    while wire::read_line_limited(&mut r, MAX_LINE).unwrap().is_some() {
        n += 1;
    }
    assert_eq!(n, 100_000);
}

// ---- shared-file paths ---------------------------------------------------------------------

#[test]
fn path_traversal_in_buffer_paths_is_refused() {
    for p in [
        "../x",
        "..",
        "a/../../x",
        "/etc/passwd",
        "\\windows\\system32",
        "C:\\x",
        "C:/x",
        "c:x",
        "a\0b",
        "",
        ".",
        "a/.",
        "a//b",
        "a/b/",
        "~/x",
        "//server/share",
        "%2e%2e/x",
        "..\\x",
        "a:b",
        "con:",
        &"a/".repeat(100),
    ] {
        // "~/x" and "%2e%2e/x" are ordinary file names on every platform we support: they are
        // allowed because they resolve inside the working folder, not because the text is odd.
        let r = hlpx::check_relative_path(p);
        if p == "~/x" || p == "%2e%2e/x" {
            assert!(r.is_ok(), "{p:?}");
        } else {
            assert!(r.is_err(), "{p:?} should be refused");
        }
    }
}

#[cfg(unix)]
#[test]
fn a_link_that_leaves_the_working_folder_is_refused() {
    let work = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret"), b"x").unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret"), work.path().join("in.hlpx")).unwrap();
    std::os::unix::fs::symlink(outside.path(), work.path().join("dir")).unwrap();
    assert!(matches!(
        hlpx::resolve_in(work.path(), "in.hlpx", true),
        Err(HlpxError::BadPath(_))
    ));
    assert!(matches!(
        hlpx::resolve_in(work.path(), "dir/secret", true),
        Err(HlpxError::BadPath(_))
    ));
    assert!(matches!(
        hlpx::resolve_in(work.path(), "dir/new.hlpx", false),
        Err(HlpxError::BadPath(_))
    ));
    // A link already sitting at an output name would redirect the write.
    std::os::unix::fs::symlink(outside.path().join("target"), work.path().join("out.hlpx"))
        .unwrap();
    assert!(hlpx::resolve_in(work.path(), "out.hlpx", false).is_err());
    assert!(hlpx::resolve_in(work.path(), "fine.hlpx", false).is_ok());
}

// ---- size lies -----------------------------------------------------------------------------

#[test]
fn a_header_that_claims_gigabytes_is_refused_before_reading() {
    let work = tempfile::tempdir().unwrap();
    let p = work.path().join("big.hlpx");
    // 65536 x 65536 x 4 channels is 64 GiB of samples, claimed on a 24-byte file.
    std::fs::write(&p, header(65_536, 65_536, 4)).unwrap();
    assert!(matches!(
        Image::read_file(&p, DEFAULT_MAX_BYTES),
        Err(HlpxError::TooLarge(_))
    ));
    // Over the dimension limit altogether.
    std::fs::write(&p, header(65_537, 1, 1)).unwrap();
    assert!(matches!(
        Image::read_file(&p, DEFAULT_MAX_BYTES),
        Err(HlpxError::BadFile(_))
    ));
    // u32::MAX in every field must not overflow.
    std::fs::write(&p, header(u32::MAX, u32::MAX, u32::MAX)).unwrap();
    assert!(Image::read_file(&p, DEFAULT_MAX_BYTES).is_err());
    assert!(hlpx::file_len(u32::MAX, u32::MAX, u32::MAX).is_none());
}

#[test]
fn files_shorter_or_longer_than_the_header_says_are_refused() {
    let work = tempfile::tempdir().unwrap();
    let p = work.path().join("a.hlpx");
    let img = Image::new(4, 4, 1, vec![1.0; 16]).unwrap();
    let good = img.encode();
    for len in [
        0,
        3,
        23,
        HEADER_LEN,
        good.len() - 1,
        good.len() + 1,
        good.len() + 4096,
    ] {
        let mut b = good.clone();
        b.resize(len, 0);
        std::fs::write(&p, &b).unwrap();
        assert!(
            Image::read_file(&p, DEFAULT_MAX_BYTES).is_err(),
            "length {len}"
        );
        assert!(
            Image::decode(&b, DEFAULT_MAX_BYTES).is_err(),
            "decode length {len}"
        );
    }
    std::fs::write(&p, &good).unwrap();
    assert!(Image::read_file(&p, DEFAULT_MAX_BYTES).is_ok());
    // The limit applies even to an honest file.
    assert!(matches!(
        Image::read_file(&p, 40),
        Err(HlpxError::TooLarge(_))
    ));
}

#[test]
fn wrong_magic_version_and_zero_dimensions_are_refused() {
    let mut b = Image::new(2, 2, 1, vec![0.0; 4]).unwrap().encode();
    let mut bad_magic = b.clone();
    bad_magic[0] = b'X';
    let mut bad_version = b.clone();
    bad_version[4] = 9;
    for (name, bytes) in [("magic", bad_magic), ("version", bad_version)] {
        assert!(Image::decode(&bytes, DEFAULT_MAX_BYTES).is_err(), "{name}");
    }
    for (w, h, c) in [(0, 2, 1), (2, 0, 1), (2, 2, 0), (2, 2, 5)] {
        b.splice(0..HEADER_LEN, header(w, h, c));
        assert!(Image::decode(&b, DEFAULT_MAX_BYTES).is_err(), "{w}x{h}x{c}");
    }
}

#[test]
fn a_directory_or_device_is_not_a_buffer() {
    let work = tempfile::tempdir().unwrap();
    assert!(Image::read_file(work.path(), DEFAULT_MAX_BYTES).is_err());
    #[cfg(unix)]
    assert!(Image::read_file(std::path::Path::new("/dev/zero"), DEFAULT_MAX_BYTES).is_err());
}

// ---- manifests ----------------------------------------------------------------------------

fn folder(manifest: &str) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("prog"), b"x").unwrap();
    std::fs::write(d.path().join("plugin.toml"), manifest).unwrap();
    d
}

const BASE: &str =
    "name = \"p\"\napi_version = 1\ncapabilities = [\"stack-backend\"]\nexecutable = \"prog\"\n";

#[test]
fn hostile_manifests_are_refused() {
    let cases: Vec<(&str, String)> = vec![
        ("name with a path", BASE.replace("\"p\"", "\"../p\"")),
        ("name with a space", BASE.replace("\"p\"", "\"a b\"")),
        (
            "name of 33 characters",
            BASE.replace("\"p\"", &format!("\"{}\"", "a".repeat(33))),
        ),
        (
            "traversing executable",
            BASE.replace("\"prog\"", "\"../../bin/sh\""),
        ),
        (
            "absolute executable",
            BASE.replace("\"prog\"", "\"/bin/sh\""),
        ),
        (
            "drive letter",
            BASE.replace("\"prog\"", "\"C:\\\\Windows\\\\cmd.exe\""),
        ),
        (
            "interpreter with a path",
            format!("interpreter = \"/usr/bin/python3\"\n{BASE}"),
        ),
        (
            "interpreter with arguments",
            format!("interpreter = \"sh -c\"\n{BASE}"),
        ),
        (
            "huge args",
            format!("args = [{}]\n{BASE}", vec!["\"a\""; 100].join(",")),
        ),
        ("limits of zero", format!("{BASE}[limits]\ntimeout_s = 0\n")),
        (
            "limits too large",
            format!("{BASE}[limits]\nmax_total_s = 99999999\n"),
        ),
        (
            "schema outside",
            format!("settings_schema = \"../x.json\"\n{BASE}"),
        ),
        (
            "missing schema",
            format!("settings_schema = \"nope.json\"\n{BASE}"),
        ),
        (
            "duplicate step",
            format!(
                "{BASE}[[steps]]\nid=\"a\"\ncapability=\"image-op\"\n[[steps]]\nid=\"a\"\ncapability=\"image-op\"\n"
            ),
        ),
        (
            "sha256 of wrong length",
            format!("sha256 = \"abc\"\n{BASE}"),
        ),
        (
            "wrong types",
            "name = 5\napi_version = \"one\"\n".to_owned(),
        ),
        (
            "api from the future",
            BASE.replace("api_version = 1", "api_version = 2"),
        ),
    ];
    for (name, text) in cases {
        let d = folder(&text);
        assert!(Loaded::load(d.path()).is_err(), "{name} should be refused");
    }
}

#[cfg(unix)]
#[test]
fn an_executable_that_is_a_link_outside_the_folder_is_refused() {
    let d = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    std::fs::write(out.path().join("real"), b"x").unwrap();
    std::os::unix::fs::symlink(out.path().join("real"), d.path().join("prog")).unwrap();
    std::fs::write(d.path().join("plugin.toml"), BASE).unwrap();
    assert!(Loaded::load(d.path()).is_err());
}

#[test]
fn a_schema_file_that_is_a_bomb_is_refused() {
    let d = folder(&format!("settings_schema = \"s.json\"\n{BASE}"));
    let deep = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
    std::fs::write(d.path().join("s.json"), deep).unwrap();
    assert!(Loaded::load(d.path()).is_err());
    std::fs::write(d.path().join("s.json"), vec![b' '; 300 * 1024]).unwrap();
    assert!(Loaded::load(d.path()).is_err());
    std::fs::write(d.path().join("s.json"), "[1,2").unwrap();
    assert!(Loaded::load(d.path()).is_err());
}

#[test]
fn schemas_with_odd_content_do_not_panic() {
    for v in [
        json!(null),
        json!([]),
        json!({"properties": {"a": {"type": "string", "minLength": u64::MAX, "maxLength": 0}}}),
        json!({"properties": {"a": {"enum": []}}}),
        json!({"properties": {"a": {"type": "number", "minimum": "x"}}}),
        json!({"properties": {"": {"type": "string"}}}),
        json!({"required": 5, "properties": {"a": {"type": "boolean"}}}),
    ] {
        let _ = Schema::from_json(&v).map(|s| s.validate(&json!({"a": "x"})));
    }
}

#[test]
fn step_specs_from_the_command_line_do_not_panic() {
    for s in [
        "",
        ":",
        "/",
        "a/b:{",
        "a/b:{}}",
        "a/b:\u{0}",
        "pre:",
        "pre:a/b:k=v=w",
        &"a/b:".repeat(1000),
        "a/b:{\"x\":[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[1]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]}",
    ] {
        let _ = StepSpec::parse(s);
    }
}

#[test]
fn an_approvals_file_that_is_huge_or_wrong_is_ignored() {
    let t = tempfile::tempdir().unwrap();
    let f = t.path().join("plugins.toml");
    std::fs::write(&f, vec![b'a'; 2 * 1024 * 1024]).unwrap();
    assert!(helia_plugin::Approvals::load(&f).entries.is_empty());
    std::fs::write(&f, "[[plugin]]\nname = 5\n").unwrap();
    assert!(helia_plugin::Approvals::load(&f).entries.is_empty());
    // A registry over an empty folder is empty, not an error.
    assert!(
        Registry::discover(Some(t.path()), &[], helia_plugin::Approvals::in_memory())
            .found
            .is_empty()
    );
}

#[test]
fn manifest_parse_in_handles_arbitrary_text() {
    let d = folder(BASE);
    for text in [
        "",
        "\u{0}",
        "name",
        "[[steps]]",
        "a = ",
        &"[".repeat(10_000),
        &"x = 1\n".repeat(20_000),
    ] {
        let _ = manifest::parse_in(d.path(), text);
    }
}

#[test]
fn bufread_is_still_line_based() {
    // Sanity check of the assumption the reader thread relies on.
    let mut r = BufReader::new(std::io::Cursor::new(b"a\nb\n".to_vec()));
    let mut s = String::new();
    r.read_line(&mut s).unwrap();
    assert_eq!(s, "a\n");
}
