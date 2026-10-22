// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The "Save bug report" bundle: plain text about the installation and the
//! last few log lines, as a `.zip` the user can attach to an issue.
//!
//! It holds no image data and no screenshot. The caller supplies already
//! scrubbed text (file names, never whole paths); nothing is sent anywhere.

use std::io::{Cursor, Write as _};

use crate::fallback::ZipError;

/// What goes into the bundle.
#[derive(Clone, Debug, Default)]
pub struct BugReport {
    /// The version block (`helia_core::about::version_block`).
    pub version: String,
    /// The operating system, as [`crate::sysinfo::os_description`].
    pub os: String,
    /// The GPU adapter in use, or why none is.
    pub gpu: String,
    /// The open capture's header in one line (file name, size, depth, frames).
    pub capture: Option<String>,
    /// The saved settings file, as text.
    pub settings: String,
    /// The end of the log file.
    pub log_tail: String,
}

/// A file name for a saved bug report, from a time in seconds since 1970.
#[must_use]
pub fn zip_name(unix_secs: u64) -> String {
    format!("helia-bug-report-{unix_secs}.zip")
}

/// The members of the bundle, as (name, text).
#[must_use]
pub fn members(r: &BugReport) -> Vec<(&'static str, String)> {
    let mut summary = String::from(
        "Helia bug report\n\nAttach this file to an issue at https://github.com/qisback/helia/issues.\nIt holds text only: no images, no screenshot. Read the files before you send it.\n\n",
    );
    summary.push_str(&r.version);
    summary.push_str("\nos: ");
    summary.push_str(&r.os);
    summary.push_str("\ngpu: ");
    summary.push_str(&r.gpu);
    summary.push('\n');
    summary.push_str("\nWhat I was doing and what went wrong:\n(write it in the issue)\n");
    let mut out = vec![("summary.txt", summary)];
    out.push((
        "capture.txt",
        r.capture
            .clone()
            .unwrap_or_else(|| "no capture open\n".into()),
    ));
    out.push(("settings.txt", r.settings.clone()));
    out.push((
        "log-tail.txt",
        if r.log_tail.is_empty() {
            "the log is empty\n".into()
        } else {
            r.log_tail.clone()
        },
    ));
    out
}

/// Build the zip.
///
/// # Errors
/// If a member cannot be written.
pub fn build_zip(r: &BugReport) -> Result<Vec<u8>, ZipError> {
    let opts =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let mut zw = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, text) in members(r) {
        zw.start_file(name, opts)?;
        zw.write_all(text.as_bytes())?;
    }
    Ok(zw.finish()?.into_inner())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use super::*;
    use std::io::Read as _;

    #[test]
    fn the_zip_holds_four_text_files_and_no_images() {
        let r = BugReport {
            version: "helia-gui 0.3.0".into(),
            os: "Test OS".into(),
            gpu: "Vulkan: Test".into(),
            capture: Some("sun.ser, 384x288, 8-bit, 60 frames".into()),
            settings: "tips_dismissed=true\n".into(),
            log_tail: "2026-10-19T12:00:00Z INFO helia-gui: started\n".into(),
        };
        let bytes = build_zip(&r).unwrap();
        let mut z = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut names: Vec<_> = z.file_names().map(|n| n.unwrap().into_owned()).collect();
        names.sort();
        assert_eq!(
            names,
            ["capture.txt", "log-tail.txt", "settings.txt", "summary.txt"]
        );
        let mut s = String::new();
        z.by_name("summary.txt")
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        assert!(s.contains("helia-gui 0.3.0") && s.contains("Vulkan: Test"));
    }

    #[test]
    fn empty_parts_say_so() {
        let m = members(&BugReport::default());
        assert!(m[1].1.contains("no capture"));
        assert!(m[3].1.contains("empty"));
    }
}
