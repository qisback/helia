// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Signed update manifests for Helia, and for Valerian, Annora, plugins and add-ons.
//!
//! The crate answers "is there a newer version, and can I trust the answer?" and
//! helps to fetch it safely. It never installs anything: the platform appliers
//! live in the application (see `docs/book/src/developer/updates.md`).
//!
//! * [`manifest`]: the manifest v1 types and the strict parser (64 KiB, depth 8,
//!   https-only URLs, unknown fields ignored, unknown platforms and kinds skipped).
//! * [`verify`]: minisign signature check against up to two pinned public keys.
//! * [`check`]: the policy on top of the two: the manifest names the right app and
//!   channel, has not expired, is not a replay (sequence), is not a downgrade
//!   unless asked, and offers no pre-release on the stable channel.
//! * [`download`]: resumable download into a staging directory, size and sha256
//!   checked, atomic rename.
//! * [`state`]: the small persisted state (last check, highest sequence, staged
//!   file, start-up health flag for rollback).
//! * [`traits`]: [`Clock`], [`Fs`] and [`Http`], so tests need no network, disk or
//!   wall clock. [`mem`] holds in-memory versions of them.
//! * `ureq_http` (feature `client`, on by default): the real [`Http`], rustls only.
//! * `sign` (feature `sign`): signing and key generation for the release pipeline
//!   and for tests. The applications do not enable it.
//!
//! Nothing here sends anything by itself and there is no telemetry. The only
//! requests are `GET` of the manifest, its `.minisig`, and the chosen asset, with
//! the fixed user agent `helia-update/<version>` and no identifiers.
//!
//! # References
//!
//! * Minisign signature format: <https://jedisct1.github.io/minisign/>
//! * The Update Framework (TUF) specification, threat model and the freeze,
//!   rollback and mix-and-match attacks: <https://theupdateframework.github.io/specification/latest/>
//! * Samuel et al., "Survivable Key Compromise in Software Update Systems",
//!   CCS 2010 (the attack classes TUF is built on).
//! * Semantic Versioning 2.0.0: <https://semver.org/>

// The Fs and Http trait methods return plain I/O or HTTP errors; each is described on its type.
#![allow(clippy::missing_errors_doc)]
#![cfg_attr(test, allow(clippy::unwrap_used, reason = "tests"))]

pub mod apply;
pub mod check;
pub mod client;
pub mod download;
pub mod error;
pub mod helia;
pub mod install;
pub mod manifest;
pub mod mem;
pub mod pinned_keys;
pub mod settings;
pub mod source;
pub mod state;
pub mod time;
pub mod traits;
pub mod verify;

#[cfg(test)]
mod client_tests;
#[cfg(feature = "sign")]
pub mod sign;
#[cfg(feature = "client")]
pub mod ureq_http;

pub use check::{CheckOptions, Outcome, Update, Updater, verify_manifest};
pub use download::Staged;
pub use error::{Error, Result};
pub use manifest::{Asset, Kind, Manifest, Platform};
pub use source::UpdateSource;
pub use state::{BootHealth, State, StateStore};
pub use traits::{Clock, Fs, Http, Request, Response, StdFs, SystemClock};

/// The user agent of every request: fixed, and no identifiers.
pub const USER_AGENT: &str = concat!("helia-update/", env!("CARGO_PKG_VERSION"));
