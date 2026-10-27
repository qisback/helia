// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Helia feedback: the client side of the feedback relay.
//!
//! The relay (`https://feedback.discworld.casa`) files a report as an
//! issue on the Helia forge, because forge registration is closed. This crate
//! holds everything both sides share, and the client:
//!
//! * [`protocol`] and [`pow`]: the wire types and the proof of work. These need
//!   only `serde` and `sha2`, so the relay can depend on this crate with
//!   `default-features = false` and reuse them. The protocol is documented in
//!   `docs/book/src/reference/feedback-protocol.md`.
//! * [`screenshot`]: PNG encoding, highlight burn-in and downscaling.
//! * [`fallback`]: the report as a `.zip` and the pre-filled new-issue URL, for
//!   when the relay cannot be reached.
//! * `bugreport` (feature `client`): the "Save bug report" zip of version, log
//!   tail, capture summary, settings and GPU adapter, built with the same zip
//!   writer as [`fallback`].
//! * [`sysinfo`]: the operating system, described for a report.
//! * `client` (feature `client`, on by default): fetches the challenge, solves
//!   it and posts the report. Authentication sits behind `client::RelayAuth`.
//!
//! Nothing here sends anything by itself: the caller decides when to call
//! `client::submit`, and there is no telemetry.

#[cfg(feature = "client")]
pub mod bugreport;
pub mod fallback;
pub mod pow;
pub mod protocol;
pub mod screenshot;
pub mod sysinfo;

#[cfg(feature = "client")]
pub mod client;

pub use protocol::{AppInfo, Challenge, Created, Highlight, Kind, Pow, Report};
