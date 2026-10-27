#![no_main]
// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.


use std::io::Cursor;

use helia_core::io::fitsseq::scan_hdus;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // The header walk of a FITS capture: every unit, however the sizes lie.
    let _ = scan_hdus(Cursor::new(data));
});
