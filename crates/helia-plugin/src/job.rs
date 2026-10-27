// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The job a `stack-backend` plugin receives: the stacking settings of a run, in the vocabulary
//! of the API (the `config` of `POST /api/v1/jobs`), so a plugin that talks to a Helia server needs
//! no second format.
//!
//! Both the command line and the GUI build it from the [`StackConfig`] they resolved, so the two
//! agree. Settings that need files or the observing site on the other side are refused here, in
//! plain words, before any work starts.

use helia_core::Keep;
use helia_core::debayer::{Channel, Method};
use helia_core::frame::BayerPattern;
use helia_core::io::source::{BayerSetting, Endian, OpenOptions};
use helia_core::pipeline::StackConfig;
use helia_core::quality::Metric;
use helia_core::stack::StackMode;
use serde_json::{Value, json};

/// The stacking settings as an API job `config`.
///
/// `filter` is the filter id (for example `ha`) when one is known.
///
/// # Errors
/// A setting that cannot be stacked away from this computer yet: true colour, calibration
/// masters and field rotation.
pub fn stack_job(
    cfg: &StackConfig,
    open: &OpenOptions,
    filter: Option<&str>,
) -> Result<Value, String> {
    if cfg.true_colour.is_some() {
        return Err("True colour cannot be stacked by a plugin yet. What to do: turn true colour off, or stack on this computer.".into());
    }
    if cfg.calibration.is_some() {
        return Err("Calibration masters cannot be used by a stack-backend plugin yet. What to do: clear the dark, flat and bad-pixel files, or stack on this computer.".into());
    }
    if cfg.field_rotation.is_some() {
        return Err("Field rotation needs the observing site, which is not sent to a plugin. What to do: turn it off, or stack on this computer.".into());
    }
    let bayer = match open.bayer {
        BayerSetting::Auto => "auto",
        BayerSetting::Mono => "none",
        BayerSetting::Pattern(BayerPattern::Rggb) => "rggb",
        BayerSetting::Pattern(BayerPattern::Grbg) => "grbg",
        BayerSetting::Pattern(BayerPattern::Gbrg) => "gbrg",
        BayerSetting::Pattern(BayerPattern::Bggr) => "bggr",
    };
    let metric = match cfg.metric {
        Metric::GradientEnergy => "gradient",
        Metric::LaplacianVariance => "laplacian",
        Metric::HelmliScherer => "helmli",
        Metric::Mfgs => "mfgs",
        Metric::DctEnergyRatio => "dct",
        Metric::FourierBand => "fourier",
        Metric::LocalContrast => "contrast",
    };
    let channel = match cfg.debayer.channel {
        Channel::Red => "r",
        Channel::Green => "g",
        Channel::Blue => "b",
        Channel::Luminance => "lum",
    };
    let mut job = json!({
        "ap_auto": cfg.auto_ap.is_some(),
        "ap_residual": cfg.ap_refine.residual,
        "multipoint": cfg.multipoint.is_some(),
        "autocrop": cfg.autocrop.is_some(),
        "bayer": bayer,
        "flip_rows": open.flip_rows,
        "row_order": open.row_order.label(),
        "channel": channel,
        "debayer": match cfg.debayer.method {
            Method::Bilinear => "bilinear",
            Method::Mhc => "mhc",
        },
        "metric": metric,
        "align": cfg.align.id(),
        "exclude": cfg
            .mask
            .exclude
            .iter()
            .map(|r| json!({"x": r.x0, "y": r.y0, "w": r.width(), "h": r.height()}))
            .collect::<Vec<_>>(),
    });
    let m = job.as_object_mut().ok_or("job")?;
    match cfg.keep {
        Keep::Count(n) => m.insert("keep_frames".into(), n.into()),
        Keep::Fraction(f) => m.insert("keep".into(), format!("{}%", f * 100.0).into()),
    };
    if let Some(mp) = &cfg.multipoint
        && cfg.auto_ap.is_none()
    {
        m.insert("ap_size".into(), mp.ap_size.into());
    }
    if let Some(margin) = cfg.autocrop {
        m.insert("autocrop_margin".into(), (margin * 100.0).into());
    }
    match cfg.mode {
        StackMode::Mean => {}
        StackMode::SigmaClip { kappa } => {
            m.insert("robust".into(), "sigma".into());
            m.insert("robust_k".into(), kappa.into());
        }
        StackMode::Huber { k } => {
            m.insert("robust".into(), "huber".into());
            m.insert("robust_k".into(), k.into());
        }
        StackMode::Winsor { k } => {
            m.insert("robust".into(), "winsor".into());
            m.insert("robust_k".into(), k.into());
        }
    }
    if let Some(b) = open.bit_depth {
        m.insert("bit_depth".into(), b.into());
    }
    if let Some(e) = open.endian {
        m.insert(
            "endian".into(),
            match e {
                Endian::Little => "little",
                Endian::Big => "big",
            }
            .into(),
        );
    }
    if let Some(f) = filter {
        m.insert("preset".into(), f.into());
    }
    Ok(job)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_default_run_makes_a_job() {
        let cfg = StackConfig::default();
        let job = stack_job(&cfg, &OpenOptions::default(), Some("ha")).unwrap();
        assert_eq!(job["preset"], "ha");
        assert_eq!(job["align"], "auto");
        assert_eq!(job["metric"], "gradient");
        assert!(job["keep"].is_string());
        assert!(job.get("robust").is_none());
    }

    #[test]
    fn robust_modes_and_counts_come_through() {
        let cfg = StackConfig {
            keep: Keep::Count(40),
            mode: StackMode::Huber { k: 1.5 },
            ..StackConfig::default()
        };
        let job = stack_job(&cfg, &OpenOptions::default(), None).unwrap();
        assert_eq!(job["keep_frames"], 40);
        assert_eq!(job["robust"], "huber");
        assert_eq!(job["robust_k"], 1.5);
    }

    #[test]
    fn what_needs_the_other_side_is_refused() {
        let cfg = StackConfig {
            true_colour: Some(helia_core::truecolour::TrueColour::default()),
            ..StackConfig::default()
        };
        let e = stack_job(&cfg, &OpenOptions::default(), None).unwrap_err();
        assert!(e.contains("True colour") && e.contains("What to do"), "{e}");
    }
}
