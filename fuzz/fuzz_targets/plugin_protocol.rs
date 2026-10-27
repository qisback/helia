#![no_main]
// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The host side of the plugin protocol on hostile input: JSON-RPC lines, buffer headers and paths,
//! manifests, schemas and step specifications. The first byte picks the parser.

use std::io::{BufReader, Cursor};
use std::path::PathBuf;
use std::sync::OnceLock;

use helia_plugin::StepSpec;
use helia_plugin::manifest;
use helia_plugin::schema::Schema;
use helia_plugin_sdk::hlpx::{self, DEFAULT_MAX_BYTES, Image};
use helia_plugin_sdk::wire;
use libfuzzer_sys::fuzz_target;

/// A folder holding a program, for the manifest parser to find.
fn folder() -> &'static PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let d = std::env::temp_dir().join(format!("helia-fuzz-plugin-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(d.join("prog"), b"x");
        d
    })
}

fuzz_target!(|data: &[u8]| {
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    match mode % 7 {
        0 => {
            let _ = wire::parse_message(rest);
        }
        1 => {
            let mut r = BufReader::with_capacity(64, Cursor::new(rest));
            while let Ok(Some(line)) = wire::read_line_limited(&mut r, 4096) {
                let _ = wire::parse_message(&line);
            }
        }
        2 => {
            let _ = Image::decode(rest, DEFAULT_MAX_BYTES);
        }
        3 => {
            if let Ok(s) = std::str::from_utf8(rest) {
                let _ = hlpx::check_relative_path(s);
                let _ = hlpx::resolve_in(folder(), s, false);
            }
        }
        4 => {
            if let Ok(s) = std::str::from_utf8(rest) {
                let _ = manifest::parse_in(folder(), s);
            }
        }
        5 => {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(rest) {
                if let Ok(schema) = Schema::from_json(&v) {
                    let _ = schema.validate(&v);
                }
            }
        }
        _ => {
            if let Ok(s) = std::str::from_utf8(rest) {
                let _ = StepSpec::parse(s);
            }
        }
    }
});
