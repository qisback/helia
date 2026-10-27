// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! FITS cards for a product a plugin helped make.
//!
//! A plugin may return `meta.provenance`, a free-form object. The host copies **only** the ten keys
//! below, each checked for type and size, into `HIERARCH HELIA ML ...` cards; everything else is
//! ignored, so a plugin cannot put arbitrary cards in a file. Whatever a plugin returns, the host
//! also sets `HIERARCH HELIA PLUGIN` and `HIERARCH HELIA PLUGIN VERSION` itself.

use helia_core::io::fits::HeaderCard;
use serde_json::Value;

/// The fixed text of the not-measurement-grade card.
pub const NOT_MEASUREMENT_GRADE: &str = "NOT MEASUREMENT GRADE";

fn word(v: &Value, max: usize) -> Option<String> {
    let s = v.as_str()?;
    let ok = !s.is_empty()
        && s.len() <= max
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._:/+- ".contains(&c));
    ok.then(|| s.to_owned())
}

/// The cards `meta` (a result's `meta` object) is allowed to add, in a fixed order.
#[must_use]
pub fn ml_cards(meta: &Value) -> Vec<HeaderCard> {
    let Some(p) = meta.get("provenance").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // Nothing is written unless the plugin says a neural network changed the pixels.
    if p.get("HELIA ML").and_then(Value::as_bool) != Some(true) {
        return out;
    }
    out.push(HeaderCard::boolean(
        "HIERARCH HELIA ML",
        true,
        "Pixels changed by a neural network",
    ));
    let text = |key: &str, max: usize, comment: &str, out: &mut Vec<HeaderCard>| {
        if let Some(s) = p.get(key).and_then(|v| word(v, max)) {
            out.push(HeaderCard::string(&format!("HIERARCH {key}"), s, comment));
        }
    };
    if let Some(t) = p
        .get("HELIA ML TASK")
        .and_then(Value::as_str)
        .filter(|t| matches!(*t, "denoise" | "sharpen"))
    {
        out.push(HeaderCard::string(
            "HIERARCH HELIA ML TASK",
            t,
            "denoise or sharpen",
        ));
    }
    text("HELIA ML MODEL", 64, "Model id", &mut out);
    text("HELIA ML VERSION", 32, "Model version", &mut out);
    for (key, comment) in [
        ("HELIA ML SHA1", "Model SHA-256, first 32 hex digits"),
        ("HELIA ML SHA2", "Model SHA-256, last 32 hex digits"),
    ] {
        if let Some(h) = p
            .get(key)
            .and_then(Value::as_str)
            .filter(|h| h.len() == 32 && h.bytes().all(|c| c.is_ascii_hexdigit()))
        {
            out.push(HeaderCard::string(
                &format!("HIERARCH {key}"),
                h.to_ascii_lowercase(),
                comment,
            ));
        }
    }
    text(
        "HELIA ML PROVIDER",
        30,
        "cpu, cuda, directml or remote:NAME",
        &mut out,
    );
    if let Some(s) = p
        .get("HELIA ML STRENGTH")
        .and_then(Value::as_f64)
        .filter(|s| s.is_finite() && (0.0..=1.0).contains(s))
    {
        out.push(HeaderCard::float(
            "HIERARCH HELIA ML STRENGTH",
            s,
            "Blend, 0 = input, 1 = model output",
        ));
    }
    if let Some(t) = p
        .get("HELIA ML TILE")
        .and_then(Value::as_i64)
        .filter(|t| (1..=65_536).contains(t))
    {
        out.push(HeaderCard::int(
            "HIERARCH HELIA ML TILE",
            t,
            "Tile edge in pixels",
        ));
    }
    if p.get("HELIA ML NOTE").and_then(Value::as_str) == Some(NOT_MEASUREMENT_GRADE) {
        out.push(HeaderCard::string(
            "HIERARCH HELIA ML NOTE",
            NOT_MEASUREMENT_GRADE,
            "Not for measurement",
        ));
    }
    out
}

/// The cards the host sets for every plugin that took part: `(name, version)` pairs, in order.
#[must_use]
pub fn plugin_cards(plugins: &[(String, String)]) -> Vec<HeaderCard> {
    if plugins.is_empty() {
        return Vec::new();
    }
    let names: Vec<&str> = plugins.iter().map(|(n, _)| n.as_str()).collect();
    let versions: Vec<String> = plugins.iter().map(|(n, v)| format!("{n} {v}")).collect();
    let clip = |s: String| s.chars().take(60).collect::<String>();
    vec![
        HeaderCard::string(
            "HIERARCH HELIA PLUGIN",
            clip(names.join(", ")),
            "Plugin that helped make this",
        ),
        HeaderCard::string(
            "HIERARCH HELIA PLUGIN VERSION",
            clip(versions.join(", ")),
            "Its version",
        ),
    ]
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::assert_is_empty)]
mod tests {
    use super::*;
    use serde_json::json;

    fn key_list(c: &[HeaderCard]) -> Vec<&str> {
        c.iter().map(|c| c.key.as_str()).collect()
    }

    #[test]
    fn the_ten_keys_pass_when_valid() {
        let sha = "0123456789abcdef0123456789ABCDEF";
        let meta = json!({"provenance": {
            "HELIA ML": true, "HELIA ML TASK": "denoise", "HELIA ML MODEL": "helia-denoise-v1",
            "HELIA ML VERSION": "1.0.0", "HELIA ML SHA1": sha, "HELIA ML SHA2": sha,
            "HELIA ML PROVIDER": "remote:hex", "HELIA ML STRENGTH": 0.5, "HELIA ML TILE": 512,
            "HELIA ML NOTE": "NOT MEASUREMENT GRADE"}});
        let c = ml_cards(&meta);
        assert_eq!(c.len(), 10, "{:?}", key_list(&c));
        assert!(c.iter().all(|c| c.key.starts_with("HIERARCH HELIA ML")));
    }

    #[test]
    fn everything_else_is_ignored_or_dropped() {
        let meta = json!({"provenance": {
            "HELIA ML": true, "SIMPLE": "F", "HELIA ML EVIL": "x", "HELIA ML TASK": "paint",
            "HELIA ML MODEL": "bad'quote", "HELIA ML SHA1": "short", "HELIA ML STRENGTH": 7,
            "HELIA ML TILE": 0, "HELIA ML NOTE": "fine really", "HELIA ML PROVIDER": "x".repeat(31)}});
        let c = ml_cards(&meta);
        assert_eq!(key_list(&c), ["HIERARCH HELIA ML"]);
    }

    #[test]
    fn no_flag_no_cards() {
        assert!(ml_cards(&json!({"provenance": {"HELIA ML MODEL": "x"}})).is_empty());
        assert!(ml_cards(&json!({"provenance": {"HELIA ML": "yes"}})).is_empty());
        assert!(ml_cards(&json!(null)).is_empty());
    }

    #[test]
    fn the_host_names_the_plugin_itself() {
        let c = plugin_cards(&[("kit".into(), "1".into())]);
        assert_eq!(
            key_list(&c),
            ["HIERARCH HELIA PLUGIN", "HIERARCH HELIA PLUGIN VERSION"]
        );
        assert!(plugin_cards(&[]).is_empty());
    }
}
