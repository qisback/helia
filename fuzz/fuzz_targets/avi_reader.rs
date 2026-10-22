#![no_main]
// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.


use std::io::Cursor;

use helia_core::io::avi::AviReader;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(mut r) = AviReader::new(Cursor::new(data)) {
        for i in 0..r.frame_count().min(8) {
            let _ = r.read_frame(i);
        }
    }
});
