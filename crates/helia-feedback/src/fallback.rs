// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! What a person can do when the relay cannot be reached: save the report as a
//! `.zip`, or open the forge's new-issue page already filled in (which works
//! for people who have an account there).

use std::fmt::Write as _;

use crate::protocol::{FORGE_URL, Report};

/// Longest new-issue URL we build; browsers and proxies cut off beyond a few
/// thousand characters.
pub const MAX_URL_LEN: usize = 6000;

/// Percent-encode everything but the unreserved characters.
#[must_use]
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// The issue title as filed: the kind in brackets, then what was typed.
#[must_use]
pub fn issue_title(report: &Report) -> String {
    format!("[{}] {}", report.kind.label(), report.title.trim())
}

/// The issue body as markdown: the description, the system details if any,
/// and the highlighted areas.
#[must_use]
pub fn issue_body(report: &Report, screenshot_note: Option<&str>) -> String {
    let mut s = report.description.trim().to_owned();
    if report.app.is_some() || report.os.is_some() {
        s.push_str("\n\n---\n");
        if let Some(a) = &report.app {
            let _ = write!(
                s,
                "\n- {} {} (commit {})\n- Target: {}",
                a.name, a.version, a.commit, a.target
            );
            if let Some(g) = &a.gpu {
                let _ = write!(s, "\n- GPU: {g}");
            }
        }
        if let Some(os) = &report.os {
            let _ = write!(s, "\n- OS: {os}");
        }
    }
    if !report.highlights.is_empty() {
        s.push_str("\n\nHighlighted areas (x, y, width, height in screenshot pixels):");
        for h in &report.highlights {
            let _ = write!(s, "\n- {}, {}, {}, {}", h.x, h.y, h.w, h.h);
        }
    }
    if let Some(n) = screenshot_note {
        let _ = write!(s, "\n\n{n}");
    }
    s
}

/// The new-issue page of `forge` (a project URL, without a trailing slash)
/// filled in from the report. The body is cut, at a character boundary, so
/// the whole URL stays within [`MAX_URL_LEN`].
#[must_use]
pub fn issue_url_for(forge: &str, report: &Report, screenshot_note: Option<&str>) -> String {
    let forge = forge.trim_end_matches('/');
    let title = percent_encode(&issue_title(report));
    let prefix = format!("{forge}/issues/new?title={title}&body=");
    let body = issue_body(report, screenshot_note);
    let mut keep = String::new();
    let mut used = prefix.len();
    for c in body.chars() {
        let mut enc = [0u8; 4];
        let cost = percent_encode(c.encode_utf8(&mut enc)).len();
        if used + cost > MAX_URL_LEN {
            break;
        }
        used += cost;
        keep.push(c);
    }
    format!("{prefix}{}", percent_encode(&keep))
}

/// [`issue_url_for`] for the Helia project.
#[must_use]
pub fn issue_url(report: &Report, screenshot_note: Option<&str>) -> String {
    issue_url_for(FORGE_URL, report, screenshot_note)
}

/// The note added to a browser-filed issue when a screenshot exists.
pub const ATTACH_NOTE: &str = "Please attach the screenshot from the saved feedback .zip (screenshot.png) to this issue. Check it shows nothing private first.";

/// A file name for a saved report, from a time in seconds since 1970.
#[must_use]
pub fn zip_name(unix_secs: u64) -> String {
    format!("helia-feedback-{unix_secs}.zip")
}

/// Why a zip could not be made.
#[cfg(feature = "client")]
#[derive(Debug, thiserror::Error)]
pub enum ZipError {
    /// The archive writer failed.
    #[error("zip: {0}")]
    Zip(#[from] zip::result::ZipError),
    /// Writing a member failed.
    #[error("zip: {0}")]
    Io(#[from] std::io::Error),
    /// The screenshot could not be encoded.
    #[error(transparent)]
    Shot(#[from] crate::screenshot::ShotError),
}

/// The saved report: `report.json` (the highlights as rectangles, no proof of
/// work), `screenshot.png` (highlights drawn on a copy) and
/// `screenshot-clean.png` (as captured), the last two only with a screenshot.
///
/// # Errors
/// If a member cannot be encoded or written.
#[cfg(feature = "client")]
pub fn build_zip(
    report: &Report,
    screenshot: Option<&crate::screenshot::Screenshot>,
) -> Result<Vec<u8>, ZipError> {
    use std::io::{Cursor, Write as _};

    let opts =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let mut zw = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let mut saved = report.clone();
    saved.pow = None;
    zw.start_file("report.json", opts)?;
    zw.write_all(
        serde_json::to_string_pretty(&saved)
            .map_err(std::io::Error::other)?
            .as_bytes(),
    )?;
    if let Some(shot) = screenshot {
        zw.start_file("screenshot.png", opts)?;
        zw.write_all(&shot.with_highlights(&report.highlights).to_png()?)?;
        zw.start_file("screenshot-clean.png", opts)?;
        zw.write_all(&shot.to_png()?)?;
    }
    Ok(zw.finish()?.into_inner())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use super::*;
    use crate::protocol::{AppInfo, Highlight, Kind};

    fn report() -> Report {
        Report {
            kind: Kind::Bug,
            title: "Stack & save: \"broken\" ü".into(),
            description: "Line one\nLine two #1".into(),
            app: Some(AppInfo {
                name: "helia".into(),
                version: "0.3.0".into(),
                commit: "abc12345".into(),
                target: "x86_64-pc-windows-msvc".into(),
                gpu: Some("Vulkan: Test".into()),
            }),
            os: Some("Windows 11".into()),
            highlights: vec![Highlight {
                x: 1,
                y: 2,
                w: 3,
                h: 4,
            }],
            pow: None,
        }
    }

    #[test]
    fn percent_encoding_covers_reserved_and_utf8() {
        assert_eq!(
            percent_encode("a b&c=d/é~-_."),
            "a%20b%26c%3Dd%2F%C3%A9~-_."
        );
    }

    #[test]
    fn the_issue_url_is_prefilled_and_decodable() {
        let url = issue_url_for("https://forge.example/o/r/", &report(), Some(ATTACH_NOTE));
        assert!(url.starts_with(
            "https://forge.example/o/r/issues/new?title=%5BBug%5D%20Stack%20%26%20save%3A%20%22broken%22%20%C3%BC&body="
        ));
        let body = url.split("&body=").nth(1).unwrap();
        assert!(body.contains("Line%20one%0ALine%20two%20%231"));
        assert!(body.contains("Windows%2011"));
        assert!(body.contains("Please%20attach%20the%20screenshot"));
        assert!(!url.contains(' ') && !url.contains('"') && !url.contains('\n'));
    }

    #[test]
    fn a_long_body_is_cut_to_a_usable_url_on_a_character_boundary() {
        let mut r = report();
        r.description = "é".repeat(5000);
        let url = issue_url_for("https://f/o/r", &r, None);
        assert!(url.len() <= MAX_URL_LEN, "{}", url.len());
        assert!(url.len() > MAX_URL_LEN - 20, "fills what it can");
        let body = url.split("&body=").nth(1).unwrap();
        assert_eq!(body.len() % 6, 0, "whole two-byte characters only");
    }

    #[test]
    fn the_body_lists_details_and_highlights() {
        let b = issue_body(&report(), None);
        assert!(b.starts_with("Line one"));
        assert!(b.contains("- helia 0.3.0 (commit abc12345)"));
        assert!(b.contains("- GPU: Vulkan: Test"));
        assert!(b.contains("- 1, 2, 3, 4"));
        let mut plain = report();
        plain.app = None;
        plain.os = None;
        plain.highlights.clear();
        assert_eq!(issue_body(&plain, None), "Line one\nLine two #1");
    }

    #[cfg(feature = "client")]
    #[test]
    fn the_zip_holds_the_report_and_both_screenshots() {
        use std::io::Read as _;

        use crate::screenshot::{HIGHLIGHT_RGBA, Screenshot};
        let shot = Screenshot::new(40, 40, [9, 9, 9, 255].repeat(1600)).unwrap();
        let mut r = report();
        r.highlights = vec![Highlight {
            x: 5,
            y: 5,
            w: 20,
            h: 20,
        }];
        let bytes = build_zip(&r, Some(&shot)).unwrap();
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut names: Vec<_> = z.file_names().map(|n| n.unwrap().into_owned()).collect();
        names.sort();
        assert_eq!(
            names,
            ["report.json", "screenshot-clean.png", "screenshot.png"]
        );
        let mut json = String::new();
        z.by_name("report.json")
            .unwrap()
            .read_to_string(&mut json)
            .unwrap();
        let back: Report = serde_json::from_str(&json).unwrap();
        assert_eq!(back.highlights, r.highlights);
        let mut burned = Vec::new();
        z.by_name("screenshot.png")
            .unwrap()
            .read_to_end(&mut burned)
            .unwrap();
        let burned = Screenshot::from_png(&burned).unwrap();
        assert_eq!(&burned.rgba[(5 * 40 + 5) * 4..][..4], &HIGHLIGHT_RGBA);
        let mut clean = Vec::new();
        z.by_name("screenshot-clean.png")
            .unwrap()
            .read_to_end(&mut clean)
            .unwrap();
        assert_eq!(Screenshot::from_png(&clean).unwrap(), shot);
        let no_shot = build_zip(&r, None).unwrap();
        let z = zip::ZipArchive::new(std::io::Cursor::new(no_shot)).unwrap();
        assert_eq!(z.len(), 1);
    }
}
