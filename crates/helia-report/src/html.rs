// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The self-contained HTML report: figures inlined as base64 PNG, the tables,
//! and the settings the stack ran with. One file to keep next to the stack.

use std::fmt::Write as _;

use crate::bundle::{Bundle, Section};

/// Standard base64 (RFC 4648) with padding.
#[must_use]
pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(c.get(1).copied().unwrap_or(0)) << 8)
            | u32::from(c.get(2).copied().unwrap_or(0));
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Escape text for HTML.
#[must_use]
pub fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            c => o.push(c),
        }
    }
    o
}

/// What the report says about the run it describes.
#[derive(Clone, Debug, Default)]
pub struct Meta {
    /// Heading, usually the capture's file name.
    pub title: String,
    /// The capture and the stacked output, one line each.
    pub inputs: Vec<(String, String)>,
    /// The settings the stack ran with, name and value.
    pub settings: Vec<(String, String)>,
    /// When the report was made, ISO 8601.
    pub generated: String,
    /// Version of the program.
    pub version: String,
}

const STYLE: &str = "body{font-family:system-ui,Segoe UI,sans-serif;max-width:1040px;margin:0 auto;padding:16px;color:#1d1d1d;background:#fff;line-height:1.5}\
h1{font-size:1.6rem;margin:.2em 0}h2{font-size:1.3rem;border-bottom:2px solid #ddd;padding-bottom:.2em;margin-top:2em}h3{font-size:1.05rem;margin:1.4em 0 .3em}\
figure{margin:1em 0}figure img{max-width:100%;height:auto;border:1px solid #ddd}figcaption{font-size:.92rem;color:#333}\
table{border-collapse:collapse;margin:.6em 0;font-size:.92rem}th,td{border:1px solid #ccc;padding:.25em .6em;text-align:left}th{background:#f1f1f1}\
.data{font-size:.85rem;color:#555}.note{background:#fff7e0;border-left:4px solid #e6a100;padding:.4em .8em;margin:.5em 0;font-size:.92rem}\
nav a{margin-right:1em}code{background:#f1f1f1;padding:0 .25em}footer{margin-top:3em;font-size:.85rem;color:#555;border-top:1px solid #ddd;padding-top:.6em}\
@media(prefers-color-scheme:dark){body{background:#17181a;color:#e6e6e6}h2{border-color:#444}figure img{border-color:#444;background:#fff}figcaption{color:#cfcfcf}th{background:#2a2c2f}th,td{border-color:#555}.note{background:#3a3220}.data,footer{color:#aaa}code{background:#2a2c2f}}";

fn table(out: &mut String, t: &crate::bundle::Table) {
    let _ = write!(out, "<h3>{}</h3><table><tr>", escape(&t.title));
    for h in &t.header {
        let _ = write!(out, "<th>{}</th>", escape(h));
    }
    out.push_str("</tr>");
    for r in &t.rows {
        out.push_str("<tr>");
        for c in r {
            let _ = write!(out, "<td>{}</td>", escape(c));
        }
        out.push_str("</tr>");
    }
    out.push_str("</table>");
}

fn section(out: &mut String, bundle: &Bundle, s: &Section) {
    let _ = write!(
        out,
        "<section id=\"{}\"><h2>{}</h2><p>{}</p>",
        escape(&s.id),
        escape(&s.title),
        escape(&s.intro)
    );
    for f in &s.figures {
        let Some(png) = bundle.file(&f.png) else {
            continue;
        };
        let _ = write!(
            out,
            "<figure><h3>{t}</h3><img alt=\"{t}\" src=\"data:image/png;base64,{b}\"><figcaption>{c}<div class=\"data\">Data: {d}</div></figcaption></figure>",
            t = escape(&f.title),
            b = base64(&png.bytes),
            c = escape(&f.caption),
            d = f
                .data
                .iter()
                .map(|d| format!("<code>{}</code>", escape(d)))
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    for t in &s.tables {
        table(out, t);
    }
    for n in &s.notes {
        let _ = write!(out, "<div class=\"note\">{}</div>", escape(n));
    }
    out.push_str("</section>");
}

/// The report page for the sections already in `bundle`.
#[must_use]
pub fn page(bundle: &Bundle, meta: &Meta) -> String {
    let mut o = String::new();
    let _ = write!(
        o,
        "<!doctype html><html lang=\"en-GB\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Helia science report: {t}</title><style>{STYLE}</style></head><body><h1>Science report: {t}</h1>",
        t = escape(&meta.title)
    );
    o.push_str("<nav>");
    for s in &bundle.sections {
        let _ = write!(o, "<a href=\"#{}\">{}</a>", escape(&s.id), escape(&s.title));
    }
    o.push_str("<a href=\"#settings\">Settings</a></nav>");
    for s in &bundle.sections {
        section(&mut o, bundle, s);
    }
    o.push_str("<section id=\"settings\"><h2>Settings used</h2>");
    if !meta.inputs.is_empty() {
        o.push_str("<table>");
        for (k, v) in &meta.inputs {
            let _ = write!(o, "<tr><th>{}</th><td>{}</td></tr>", escape(k), escape(v));
        }
        o.push_str("</table>");
    }
    o.push_str("<table><tr><th>setting</th><th>value</th></tr>");
    for (k, v) in &meta.settings {
        let _ = write!(o, "<tr><td>{}</td><td>{}</td></tr>", escape(k), escape(v));
    }
    o.push_str("</table>");
    if let Some(h) = bundle.file("fits-header.txt") {
        let _ = write!(
            o,
            "<h3>FITS header of the stack</h3><pre>{}</pre>",
            escape(&String::from_utf8_lossy(&h.bytes))
        );
    }
    o.push_str("</section>");
    let _ = write!(
        o,
        "<footer>Made by Helia {} on {}. Every figure has its numbers beside this file as CSV and JSON; redraw them with <code>helia plot</code> or in Python.</footer></body></html>",
        escape(&meta.version),
        escape(&meta.generated)
    );
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_rfc_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn text_is_escaped() {
        assert_eq!(
            escape("<a href=\"x\">&"),
            "&lt;a href=&quot;x&quot;&gt;&amp;"
        );
    }
}
