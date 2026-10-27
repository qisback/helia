#![no_main]
// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Update manifest parser and signature path.
//!
//! The input is `[u16 BE signature length][signature text][manifest bytes]`. The
//! manifest bytes go through the strict parser on their own (the part an attacker
//! reaches only with a valid signature, so a fuzzer must reach it directly) and
//! through the full verify-and-policy function with a pinned key. Neither may
//! panic or run away; the signature of random input must never verify.

use helia_update::manifest::{self, ParseOptions};
use helia_update::{UpdateSource, verify_manifest};
use libfuzzer_sys::fuzz_target;

const KEY: &str = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";

fuzz_target!(|data: &[u8]| {
    let (sig_len, rest) = match data {
        [a, b, rest @ ..] => (usize::from(u16::from_be_bytes([*a, *b])), rest),
        _ => return,
    };
    let sig_len = sig_len.min(rest.len());
    let (sig, body) = rest.split_at(sig_len);

    for allow in [false, true] {
        let _ = manifest::parse(body, ParseOptions { allow_loopback_http: allow });
    }
    let _ = manifest::check_shape(body);
    let _ = manifest::peek_schema(body);
    if let Ok(text) = std::str::from_utf8(body) {
        let _ = manifest::check_url(text, true);
        let _ = helia_update::time::parse_utc(text);
    }

    let source = UpdateSource::new("helia", "stable", "https://downloads.12hobbies.studio/helia/stable/manifest.json", &[KEY]);
    let sig = String::from_utf8_lossy(sig);
    let r = verify_manifest(&source, body, &sig, 1_792_864_800, 0, false);
    assert!(r.is_err(), "random input verified");
});
