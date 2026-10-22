// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `helia feedback`: the same client as the GUI's Feedback page, for people
//! without a display.

use std::io::Read as _;
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result, bail};
use helia_core::about;
use helia_feedback::client::{FeedbackError, ProofOfWork, Relay, submit};
use helia_feedback::fallback;
use helia_feedback::protocol::{AppInfo, Highlight, Kind, Report, SENT_WARNING};
use helia_feedback::screenshot::Screenshot;
use helia_feedback::sysinfo;

use crate::{FeedbackArgs, FeedbackKind};

impl From<FeedbackKind> for Kind {
    fn from(k: FeedbackKind) -> Self {
        match k {
            FeedbackKind::Bug => Self::Bug,
            FeedbackKind::Feature => Self::Feature,
            FeedbackKind::Question => Self::Question,
            FeedbackKind::Other => Self::Other,
        }
    }
}

/// Parse `x,y,w,h`.
///
/// # Errors
/// If it is not four whole numbers, or has no area.
#[allow(
    clippy::many_single_char_names,
    reason = "x, y, w, h are the protocol's names"
)]
pub fn parse_highlight(text: &str) -> Result<Highlight> {
    let nums: Vec<u32> = text
        .split(',')
        .map(|v| v.trim().parse::<u32>())
        .collect::<Result<_, _>>()
        .with_context(|| format!("--highlight {text:?} is not x,y,w,h"))?;
    let [x, y, w, h] = nums[..] else {
        bail!("--highlight {text:?} is not x,y,w,h");
    };
    if w == 0 || h == 0 {
        bail!("--highlight {text:?} has no area");
    }
    Ok(Highlight { x, y, w, h })
}

/// The system details of this build and machine.
#[must_use]
pub fn app_info() -> AppInfo {
    let b = about::build_info();
    AppInfo {
        name: "helia".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        commit: b.commit_label(),
        target: b.target.into(),
        gpu: None,
    }
}

/// Build the report and the screenshot from the arguments.
///
/// # Errors
/// If the description, screenshot or a highlight cannot be read, or the
/// report breaks a limit.
pub fn build(args: &FeedbackArgs) -> Result<(Report, Option<Screenshot>)> {
    let description = if args.description == "-" {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .context("reading the description from standard input")?;
        s
    } else {
        args.description.clone()
    };
    let shot = match &args.screenshot {
        Some(p) => {
            let bytes = std::fs::read(p).with_context(|| format!("reading {}", p.display()))?;
            Some(
                Screenshot::from_png(&bytes)
                    .with_context(|| format!("{} is not a PNG", p.display()))?,
            )
        }
        None => None,
    };
    let highlights = args
        .highlight
        .iter()
        .map(|h| parse_highlight(h))
        .collect::<Result<Vec<_>>>()?;
    let report = Report {
        kind: args.kind.into(),
        title: args.title.trim().to_owned(),
        description: description.trim().to_owned(),
        app: args.system_info.then(app_info),
        os: args.system_info.then(sysinfo::os_description),
        highlights,
        pow: None,
    };
    report.validate().context("the report is not valid")?;
    Ok((report, shot))
}

/// Run the command.
///
/// # Errors
/// If the report is not valid or could not be sent. On a failure the saved
/// `.zip` (with `--save`) and a pre-filled new-issue address are printed first.
pub fn run(args: &FeedbackArgs) -> Result<()> {
    let (report, shot) = build(args)?;
    eprintln!("{SENT_WARNING}");
    if args.dry_run {
        println!("{}", report.preview_json());
        println!(
            "\nScreenshot: {}\nNothing was sent (--dry-run).",
            match &shot {
                Some(s) => format!("{}x{} PNG", s.width, s.height),
                None => "none".into(),
            }
        );
        return Ok(());
    }
    // The flag beats the environment variable, which beats the default.
    let relay = match &args.url {
        Some(url) => Relay::new(url)?,
        None => Relay::configured(None)?,
    };
    let cancel = AtomicBool::new(false);
    match submit(
        &relay,
        &ProofOfWork,
        report.clone(),
        shot.as_ref(),
        &cancel,
        &mut |_| {},
    ) {
        Ok(created) => {
            println!(
                "Thank you. Filed as issue #{}: {}",
                created.issue_number, created.issue_url
            );
            Ok(())
        }
        Err(e) => {
            if e.offers_fallback() {
                fallbacks(args, &report, shot.as_ref(), &e);
            }
            Err(anyhow::Error::new(e))
        }
    }
}

fn fallbacks(args: &FeedbackArgs, report: &Report, shot: Option<&Screenshot>, e: &FeedbackError) {
    eprintln!("helia: {e}");
    if let Some(path) = &args.save {
        match fallback::build_zip(report, shot)
            .map_err(|e| e.to_string())
            .and_then(|b| std::fs::write(path, b).map_err(|e| e.to_string()))
        {
            Ok(()) => eprintln!("Saved the report to {}.", path.display()),
            Err(err) => eprintln!("Could not save the report: {err}"),
        }
    } else {
        eprintln!("Run again with --save FILE.zip to keep the report and the screenshot.");
    }
    let note = shot.map(|_| fallback::ATTACH_NOTE);
    eprintln!(
        "Or, if you have an account on github.com, open this address to file it yourself:\n{}",
        fallback::issue_url(report, note)
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use super::*;

    #[test]
    fn highlights_parse_strictly() {
        assert_eq!(
            parse_highlight("10, 20,30,40").unwrap(),
            Highlight {
                x: 10,
                y: 20,
                w: 30,
                h: 40
            }
        );
        for bad in ["1,2,3", "1,2,3,4,5", "a,b,c,d", "1,2,0,4", "-1,2,3,4", ""] {
            assert!(parse_highlight(bad).is_err(), "{bad}");
        }
    }
}
