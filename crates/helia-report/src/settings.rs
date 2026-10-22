// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The settings a stack ran with, as name and value for the report, and a
//! summary of a FITS header.

use helia_core::pipeline::{StackConfig, StackOutput};

/// The stack settings that decide what the figures show.
#[must_use]
pub fn from_config(cfg: &StackConfig, out: &StackOutput) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = Vec::new();
    let mut add = |k: &str, val: String| v.push((k.to_string(), val));
    add(
        "frames kept",
        format!("{:?} ({} frames)", cfg.keep, out.kept.len()),
    );
    add("quality metric", format!("{:?}", cfg.metric));
    add("candidate factor", format!("{}", cfg.candidate_factor));
    add("reference frames", cfg.reference_frames.to_string());
    add("robust ranking", cfg.robust_ranking.to_string());
    match &cfg.multipoint {
        Some(m) => add(
            "alignment points",
            format!(
                "window {} px, step {} px, search radius {} px, minimum correlation {}",
                m.ap_size, m.step, m.search_radius, m.min_correlation
            ),
        ),
        None => add("alignment points", "off (global alignment only)".into()),
    }
    add(
        "alignment point passes",
        format!(
            "{}; smoothing {}; residual {}",
            cfg.ap_refine.passes,
            if cfg.ap_refine.smooth.is_some() {
                "on"
            } else {
                "off"
            },
            if cfg.ap_refine.residual {
                "measured"
            } else {
                "not measured"
            }
        ),
    );
    add("global alignment", out.alignment.summary());
    add("stack mode", format!("{:?}", cfg.mode));
    add("resampling kernel", format!("{:?}", cfg.kernel));
    add("debayer", format!("{:?}", cfg.debayer));
    add("colour plane stacked", format!("{:?}", out.channel));
    add("normalised", cfg.normalise.to_string());
    add("backend", out.backend.clone());
    if let Some(n) = &out.channel_note {
        add("colour plane note", n.clone());
    }
    for n in &out.calibration_notes {
        add("calibration", n.clone());
    }
    v
}

/// The cards of a FITS file's primary header, one per line, or `None` when
/// the file is not FITS. Reads at most 100 blocks.
#[must_use]
pub fn fits_header_summary(path: &std::path::Path) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut text = String::new();
    let mut block = [0u8; 2880];
    for i in 0..100 {
        f.read_exact(&mut block).ok()?;
        if i == 0 && !block.starts_with(b"SIMPLE") {
            return None;
        }
        for card in block.chunks(80) {
            let line = String::from_utf8_lossy(card);
            let line = line.trim_end();
            if line.starts_with("END") && line.trim() == "END" {
                return Some(text);
            }
            if !line.is_empty() {
                text.push_str(line);
                text.push('\n');
            }
        }
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fits_header_is_read_to_the_end_card() {
        let mut block = vec![b' '; 2880];
        let cards = [
            "SIMPLE  =                    T",
            "BITPIX  =                  -32",
            "OBJECT  = 'Sun     '",
            "END",
        ];
        for (i, c) in cards.iter().enumerate() {
            block[i * 80..i * 80 + c.len()].copy_from_slice(c.as_bytes());
        }
        let dir = std::env::temp_dir().join(format!("helia-report-fits-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let p = dir.join("a.fits");
        std::fs::write(&p, &block).expect("write");
        let s = fits_header_summary(&p).expect("header");
        assert_eq!(s.lines().count(), 3);
        assert!(s.contains("OBJECT"));
        std::fs::write(&p, vec![0u8; 2880]).expect("write");
        assert!(fits_header_summary(&p).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
