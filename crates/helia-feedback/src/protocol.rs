// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Wire types of the feedback relay protocol (version 1).
//!
//! See `docs/book/src/reference/feedback-protocol.md`. The relay uses these
//! same types, so a change here is a change to the protocol.

use serde::{Deserialize, Serialize};

/// Where the public relay lives.
pub const DEFAULT_RELAY_URL: &str = "https://feedback.discworld.casa";
/// The project on the forge (the issue list the relay files into, and the
/// browser fallback target).
pub const FORGE_URL: &str = "https://github.com/qisback/helia";
/// What every sender is told before anything leaves their machine. The one
/// place to change when the issue tracker becomes (or stops being) public.
pub const SENT_WARNING: &str = "Your feedback is sent to the Helia developers and filed in the Helia issue tracker. Check the screenshot shows nothing private.";
/// Longest title, in characters.
pub const MAX_TITLE: usize = 120;
/// Longest description, in characters.
pub const MAX_DESCRIPTION: usize = 8000;
/// Largest screenshot, in bytes.
pub const MAX_SCREENSHOT_BYTES: usize = 5 * 1024 * 1024;
/// Most highlight rectangles in a report.
pub const MAX_HIGHLIGHTS: usize = 32;
/// Hardest proof of work a client will attempt (leading zero bits).
pub const MAX_DIFFICULTY: u32 = 28;
/// Path of the challenge endpoint.
pub const CHALLENGE_PATH: &str = "/api/v1/challenge";
/// Path of the report endpoint.
pub const FEEDBACK_PATH: &str = "/api/v1/feedback";

/// What kind of feedback this is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Something is broken or wrong.
    #[default]
    Bug,
    /// A request for something new.
    Feature,
    /// A question about using Helia.
    Question,
    /// Anything else.
    Other,
}

impl Kind {
    /// Every kind, in the order the dialog lists them.
    pub const ALL: [Kind; 4] = [Kind::Bug, Kind::Feature, Kind::Question, Kind::Other];

    /// The wire name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bug => "bug",
            Self::Feature => "feature",
            Self::Question => "question",
            Self::Other => "other",
        }
    }

    /// The name shown to people.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Bug => "Bug",
            Self::Feature => "Feature request",
            Self::Question => "Question",
            Self::Other => "Other",
        }
    }

    /// Parse a wire name.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// A rectangle drawn over the screenshot, in screenshot pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Highlight {
    /// Left edge.
    pub x: u32,
    /// Top edge.
    pub y: u32,
    /// Width.
    pub w: u32,
    /// Height.
    pub h: u32,
}

impl Highlight {
    /// The rectangle spanned by two corners, clamped to a `width` by `height`
    /// image. `None` when it ends up smaller than 4 pixels either way (a click
    /// rather than a drag).
    #[must_use]
    pub fn from_corners(a: (f32, f32), b: (f32, f32), width: u32, height: u32) -> Option<Self> {
        let clamp = |v: f32, max: u32| v.round().clamp(0.0, max as f32) as u32;
        let (x0, x1) = (clamp(a.0.min(b.0), width), clamp(a.0.max(b.0), width));
        let (y0, y1) = (clamp(a.1.min(b.1), height), clamp(a.1.max(b.1), height));
        (x1 - x0 >= 4 && y1 - y0 >= 4).then_some(Self {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        })
    }

    /// The same rectangle for an image shrunk by `factor` (2 halves it).
    #[must_use]
    pub fn shrunk(self, factor: u32) -> Self {
        Self {
            x: self.x / factor,
            y: self.y / factor,
            w: (self.w / factor).max(1),
            h: (self.h / factor).max(1),
        }
    }
}

/// Which build sent the report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppInfo {
    /// Always `helia`.
    pub name: String,
    /// Version, for example `0.3.0`.
    pub version: String,
    /// Short commit hash, with `+dirty` for a modified tree.
    pub commit: String,
    /// Target triple.
    pub target: String,
    /// The GPU adapter or backend, when known.
    pub gpu: Option<String>,
}

/// A solved proof of work.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pow {
    /// The challenge, as the relay sent it (hex).
    pub challenge: String,
    /// The nonce found.
    pub nonce: u64,
}

/// The `report` part of a submission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    /// Bug, feature, question or other.
    pub kind: Kind,
    /// One line, at most [`MAX_TITLE`] characters.
    pub title: String,
    /// At most [`MAX_DESCRIPTION`] characters. May end with a block of system
    /// details (including the open capture's header summary).
    pub description: String,
    /// The build, when the user chose to include system details.
    pub app: Option<AppInfo>,
    /// The operating system, when the user chose to include system details.
    pub os: Option<String>,
    /// Rectangles over the screenshot (empty without a screenshot).
    pub highlights: Vec<Highlight>,
    /// Proof of work. Required by the relay today; a future authentication
    /// method (a client certificate) replaces it, so it may be absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pow: Option<Pow>,
}

/// Why a report is not valid.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Invalid {
    /// The title is empty.
    #[error("the title is empty")]
    EmptyTitle,
    /// The title is too long.
    #[error("the title is longer than {MAX_TITLE} characters")]
    TitleTooLong,
    /// The description is too long.
    #[error("the description is longer than {MAX_DESCRIPTION} characters")]
    DescriptionTooLong,
    /// Too many highlights, or one with no area.
    #[error("the highlights are not valid (at most {MAX_HIGHLIGHTS}, each with an area)")]
    Highlights,
}

impl Report {
    /// Check the limits both sides enforce.
    ///
    /// # Errors
    /// The first limit that is broken.
    pub fn validate(&self) -> Result<(), Invalid> {
        if self.title.trim().is_empty() {
            return Err(Invalid::EmptyTitle);
        }
        if self.title.chars().count() > MAX_TITLE {
            return Err(Invalid::TitleTooLong);
        }
        if self.description.chars().count() > MAX_DESCRIPTION {
            return Err(Invalid::DescriptionTooLong);
        }
        if self.highlights.len() > MAX_HIGHLIGHTS
            || self.highlights.iter().any(|h| h.w == 0 || h.h == 0)
        {
            return Err(Invalid::Highlights);
        }
        Ok(())
    }

    /// The JSON the relay receives, compact.
    ///
    /// # Panics
    /// Never: every field serialises.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("a report always serialises")
    }

    /// The JSON as shown in the preview: indented, and with a note where the
    /// proof of work goes, because it is computed when Send is pressed.
    ///
    /// # Panics
    /// Never: every field serialises.
    #[must_use]
    pub fn preview_json(&self) -> String {
        let mut v = serde_json::to_value(self).expect("a report always serialises");
        if let Some(o) = v.as_object_mut() {
            o.insert(
                "pow".into(),
                serde_json::json!({
                    "challenge": "(fetched from the relay when you press Send)",
                    "nonce": "(found when you press Send)"
                }),
            );
        }
        serde_json::to_string_pretty(&v).expect("a value always serialises")
    }
}

/// Answer of `GET /api/v1/challenge`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Challenge {
    /// 32 random bytes, hex.
    pub challenge: String,
    /// Leading zero bits required of the hash.
    pub difficulty: u32,
    /// RFC 3339 time after which the relay refuses it.
    pub expires: String,
}

/// Answer of a successful `POST /api/v1/feedback` (201).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Created {
    /// The new issue's number.
    pub issue_number: u64,
    /// Its address.
    pub issue_url: String,
}

/// The body of a 400, 413 or 415 answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    /// What went wrong.
    pub error: ErrorDetail,
}

/// Inside [`ErrorBody`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorDetail {
    /// Machine-readable, for example `title_too_long`.
    pub code: String,
    /// For people.
    pub message: String,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use super::*;

    fn report() -> Report {
        Report {
            kind: Kind::Bug,
            title: "Crash on open".into(),
            description: "It closes.".into(),
            app: Some(AppInfo {
                name: "helia".into(),
                version: "0.3.0".into(),
                commit: "abc12345".into(),
                target: "x86_64-unknown-linux-gnu".into(),
                gpu: None,
            }),
            os: Some("Debian 13".into()),
            highlights: vec![Highlight {
                x: 1,
                y: 2,
                w: 30,
                h: 40,
            }],
            pow: Some(Pow {
                challenge: "00".repeat(32),
                nonce: 7,
            }),
        }
    }

    #[test]
    fn json_matches_the_documented_shape_exactly() {
        let expected = format!(
            r#"{{"kind":"bug","title":"Crash on open","description":"It closes.","app":{{"name":"helia","version":"0.3.0","commit":"abc12345","target":"x86_64-unknown-linux-gnu","gpu":null}},"os":"Debian 13","highlights":[{{"x":1,"y":2,"w":30,"h":40}}],"pow":{{"challenge":"{}","nonce":7}}}}"#,
            "00".repeat(32)
        );
        assert_eq!(report().to_json(), expected);
        let back: Report = serde_json::from_str(&expected).unwrap();
        assert_eq!(back, report());
    }

    #[test]
    fn optional_parts_are_null_and_kinds_are_lowercase() {
        let mut r = report();
        r.app = None;
        r.os = None;
        r.highlights.clear();
        r.kind = Kind::Feature;
        r.pow = None;
        assert_eq!(
            r.to_json(),
            r#"{"kind":"feature","title":"Crash on open","description":"It closes.","app":null,"os":null,"highlights":[]}"#
        );
        for k in Kind::ALL {
            assert_eq!(Kind::parse(k.as_str()), Some(k));
        }
    }

    #[test]
    fn limits_are_enforced() {
        let mut r = report();
        assert_eq!(r.validate(), Ok(()));
        r.title = "  ".into();
        assert_eq!(r.validate(), Err(Invalid::EmptyTitle));
        r.title = "x".repeat(MAX_TITLE + 1);
        assert_eq!(r.validate(), Err(Invalid::TitleTooLong));
        r.title = "x".repeat(MAX_TITLE);
        assert_eq!(r.validate(), Ok(()));
        r.description = "é".repeat(MAX_DESCRIPTION + 1);
        assert_eq!(r.validate(), Err(Invalid::DescriptionTooLong));
        r.description.clear();
        r.highlights = vec![Highlight {
            x: 0,
            y: 0,
            w: 0,
            h: 5,
        }];
        assert_eq!(r.validate(), Err(Invalid::Highlights));
    }

    #[test]
    fn highlight_from_corners_clamps_and_ignores_clicks() {
        let h = Highlight::from_corners((50.0, 40.0), (10.0, 79.0), 100, 80).unwrap();
        assert_eq!(
            h,
            Highlight {
                x: 10,
                y: 40,
                w: 40,
                h: 39
            }
        );
        let h = Highlight::from_corners((-20.0, -5.0), (500.0, 500.0), 100, 80).unwrap();
        assert_eq!((h.x, h.y, h.w, h.h), (0, 0, 100, 80));
        assert_eq!(
            Highlight::from_corners((5.0, 5.0), (7.0, 60.0), 100, 80),
            None
        );
    }

    #[test]
    fn preview_marks_the_proof_of_work_as_pending() {
        let p = report().preview_json();
        assert!(p.contains("fetched from the relay"));
        assert!(!p.contains(&"00".repeat(32)));
    }
}
