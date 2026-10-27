// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Opt-in installer for external programs and data packs, shared by Helia, Valerian and Annora.
//!
//! Some features need something the app must not ship: Helia's video support needs
//! `ffmpeg`, Valerian's catalogue needs the Gaia DR3 data pack. This crate lets an app
//! offer to fetch such an item from its provider, safely and only with the user's say-so:
//!
//! * [`manifest`]: the signed list of items, per platform URL and sha256, licence text,
//!   source link and attribution. One manifest per app.
//! * [`source`]: where the manifest comes from, the pinned keys (the same release keys
//!   `helia-update` uses) and the hosts the app is willing to contact.
//! * [`store`]: the folders, the anti-replay record and the per-user licence acceptances.
//! * [`client`]: [`Client::fetch_manifest`], [`Client::install`] (acceptance checked
//!   before any request), [`Client::run_package`], [`Client::list`], [`Client::remove`]
//!   and [`installed_program`].
//! * [`archive`]: unpack only the named files from a zip or tar.xz.
//!
//! Signatures, key handling, address checks and the HTTP client are the ones in
//! `helia-update`; nothing here invents cryptography. Nothing is sent except `GET` requests
//! for the manifest, its `.minisig` and the chosen download, with the fixed user agent of
//! `helia-update` and no identifiers. The app never contains the item: it downloads from
//! the provider, after the user has read and accepted the provider's licence.
//!
//! The adoption guide is `docs/book/src/developer/external-tools.md`.
//!
//! # References
//!
//! * Minisign signature format: <https://jedisct1.github.io/minisign/>
//! * The Update Framework (TUF): <https://theupdateframework.github.io/specification/latest/>
//!   (freeze, rollback and mix-and-match attacks, which the expiry, sequence and app name
//!   checks answer).
//! * GNU Lesser General Public License v3 and GNU General Public License v3, section 4 to 6
//!   (conveying and source offers): <https://www.gnu.org/licenses/>

// The Fs and Http trait methods return plain I/O or HTTP errors; each is described on its type.
#![allow(clippy::missing_errors_doc)]
#![cfg_attr(test, allow(clippy::unwrap_used, reason = "tests"))]

pub mod archive;
pub mod client;
pub mod error;
pub mod helia;
pub mod manifest;
pub mod source;
pub mod store;

pub use client::{
    Client, CommandRunner, Health, Hooks, Installed, InstalledFile, Phase, Progress, Removal,
    SystemRunner, installed_program,
};
pub use error::{Error, Result};
pub use manifest::{
    ArchiveKind, Download, Item, Licence, Manifest, Member, Offer, PackageCommand, Route,
};
pub use source::ExternalSource;
pub use store::{Acceptance, Acceptances, Dirs, State};
