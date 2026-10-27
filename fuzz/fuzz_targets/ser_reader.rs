#![no_main]
// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.


use std::io::Cursor;

use helia_core::io::ser::SerReader;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    for lenient in [false, true] {
        let r = if lenient { SerReader::new_lenient(Cursor::new(data)) } else { SerReader::new(Cursor::new(data)) };
        if let Ok(mut r) = r {
            let _ = r.timestamps();
            // Cap decoded frames; the input is small, so each frame is too.
            for f in r.frames().take(8) {
                let _ = f;
            }
        }
    }
});
