// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The flatten edit: divide out the disc's limb darkening.
//!
//! The arithmetic is in [`crate::limbdark`]. The edit measures the image it is
//! given, so it has to come first in a list (before anything changes the
//! levels or the pixel grid); [`super::EditList::resolve`] fits the disc and
//! fixes the gain table, and the CPU and the GPU then apply the same table.

use serde::{Deserialize, Serialize};

use crate::limbdark::{FlattenPlan, Law, Source};

/// What has been worked out for the edit so far. Not part of the JSON form: a
/// saved list says what to do, and the image says how much.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum State {
    /// Not yet fitted.
    #[default]
    Unresolved,
    /// Fitted: the gain to apply.
    Ready(FlattenPlan),
    /// There was no clear disc to fit, so the image passes unchanged.
    NoDisc,
}

/// Divide the disc by its limb-darkening profile.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Flatten {
    /// Divide by the fitted law or by the measured profile.
    pub source: Source,
    /// The law fitted (the model divides by it; the profile only reports it).
    pub law: Law,
    /// The result of fitting, once resolved.
    #[serde(skip)]
    pub state: State,
}

impl Flatten {
    /// A flatten by `source` with `law`.
    #[must_use]
    pub fn new(source: Source, law: Law) -> Self {
        Self {
            source,
            law,
            state: State::Unresolved,
        }
    }

    /// One line saying what the edit does, for the file history.
    #[must_use]
    pub fn describe(&self) -> String {
        match self.source {
            Source::Model => format!(
                "flatten the limb darkening by the fitted {} law, on the disc only",
                self.law.id()
            ),
            Source::Profile => {
                "flatten the limb darkening by the measured radial profile (azimuthal median), \
                 on the disc only"
                    .to_owned()
            }
        }
    }

    /// The same edit with the disc fitted on `frame`.
    #[must_use]
    pub fn resolved(&self, frame: &crate::frame::Frame) -> Self {
        Self {
            state: crate::limbdark::plan(frame, self.source, self.law)
                .map_or(State::NoDisc, |(p, _)| State::Ready(p)),
            ..self.clone()
        }
    }

    /// Apply to `frame`, fitting first when the edit is not resolved.
    #[must_use]
    pub fn apply(&self, frame: &crate::frame::Frame) -> crate::frame::Frame {
        match &self.state {
            State::Ready(p) => p.apply(frame),
            State::NoDisc => frame.clone(),
            State::Unresolved => match crate::limbdark::plan(frame, self.source, self.law) {
                Some((p, _)) => p.apply(frame),
                None => frame.clone(),
            },
        }
    }
}
