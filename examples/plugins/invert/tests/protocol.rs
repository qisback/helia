// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Drives the compiled example by hand, the way a host does: initialize, one image.process, shutdown.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use helia_plugin_sdk::Image;
use helia_plugin_sdk::hlpx::DEFAULT_MAX_BYTES;
use serde_json::{Value, json};

#[test]
fn inverts_an_image_through_the_protocol() {
    let work = tempfile::tempdir().expect("tempdir");
    let img = Image::new(4, 2, 1, vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]).expect("image");
    std::fs::write(work.path().join("in.hlpx"), img.encode()).expect("write input");

    let mut child = Command::new(env!("CARGO_BIN_EXE_helia-example-invert"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("start the example");
    let mut stdin = child.stdin.take().expect("stdin");
    let mut lines = BufReader::new(child.stdout.take().expect("stdout")).lines();
    let mut ask = |id: i64, method: &str, params: Value| -> Value {
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(stdin, "{msg}").expect("send");
        loop {
            let line = lines.next().expect("an answer").expect("a line");
            let v: Value = serde_json::from_str(&line).expect("json");
            // Progress notifications come first; the answer carries the id.
            if v.get("id") == Some(&json!(id)) {
                return v;
            }
            assert_eq!(v["method"], "progress", "{line}");
        }
    };

    let init = ask(
        1,
        "initialize",
        json!({"api_version": 1, "workdir": work.path(), "settings": {}}),
    );
    assert_eq!(init["result"]["name"], "invert");
    assert_eq!(init["result"]["capabilities"], json!(["image-op"]));

    let done = ask(
        2,
        "image.process",
        json!({
            "step": "invert", "params": {}, "settings": {},
            "input": {"path": "in.hlpx", "format": "hlpx", "width": 4, "height": 2, "channels": 1},
            "output": {"path": "out.hlpx", "format": "hlpx"},
            "meta": {"stage": "post-edit"}
        }),
    );
    assert_eq!(done["result"]["output"]["width"], 4, "{done}");
    let bytes = std::fs::read(work.path().join("out.hlpx")).expect("output file");
    let out = Image::decode(&bytes, DEFAULT_MAX_BYTES).expect("decode");
    assert_eq!(out.data, vec![7.0, 6.0, 5.0, 4.0, 3.0, 2.0, 1.0, 0.0]);

    let bye = ask(3, "shutdown", json!({}));
    assert!(bye["result"].is_null());
    writeln!(stdin, r#"{{"jsonrpc":"2.0","method":"exit"}}"#).expect("exit");
    assert!(child.wait().expect("wait").success());
}
