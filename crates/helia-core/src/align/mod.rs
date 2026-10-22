// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Frame registration.
//!
//! Shift convention used throughout: a frame has shift `s` relative to the
//! reference when `frame(x) = reference(x - s)`. Aligning therefore samples
//! the frame at `x + s`.

pub mod apstats;
pub mod global;
pub mod limb;
pub mod method;
pub mod multipoint;
pub mod refine;

use std::ops::{Add, Neg, Sub};

/// Sub-pixel translation in pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Shift {
    /// Horizontal component (positive = content moved right).
    pub dx: f32,
    /// Vertical component (positive = content moved down).
    pub dy: f32,
}

impl Shift {
    /// Construct from components.
    #[must_use]
    pub fn new(dx: f32, dy: f32) -> Self {
        Self { dx, dy }
    }

    /// Euclidean length.
    #[must_use]
    pub fn length(self) -> f32 {
        self.dx.hypot(self.dy)
    }
}

impl Add for Shift {
    type Output = Self;
    fn add(self, o: Self) -> Self {
        Self::new(self.dx + o.dx, self.dy + o.dy)
    }
}

impl Sub for Shift {
    type Output = Self;
    fn sub(self, o: Self) -> Self {
        Self::new(self.dx - o.dx, self.dy - o.dy)
    }
}

impl Neg for Shift {
    type Output = Self;
    fn neg(self) -> Self {
        Self::new(-self.dx, -self.dy)
    }
}
