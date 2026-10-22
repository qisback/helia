#![no_main]
// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

// The video path hands the file to ffmpeg; what Helia parses itself is ffprobe's
// JSON and the capture sidecar. Both come from outside, so both are fuzzed.
use helia_core::debayer::Channel;
use helia_core::io::ffmpeg::{parse_probe, plan};
use helia_core::io::sidecar::CameraSettings;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if let Ok(info) = parse_probe(text) {
        let _ = info.output_size();
        let _ = info.quality_note();
        let _ = plan(&info, Channel::Luminance);
    }
    let _ = CameraSettings::parse(text);
});
