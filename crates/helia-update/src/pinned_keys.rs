// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The pinned release public keys: the one place to put them.
//!
//! PLACEHOLDER KEY. [`PINNED_KEYS`] holds a throwaway public key whose secret key
//! was destroyed when it was made. Nothing signed by anyone verifies against it, so
//! a build with the placeholder never accepts an update. To install the real key
//! (from bao, `claude-automation/release-signing/helia`, field `public_key`),
//! replace the string and set [`KEYS_ARE_PLACEHOLDER`] to `false`; the release
//! workflow refuses to publish while it is `true`. During a key rotation the list
//! holds the current and the next key.

/// True while [`PINNED_KEYS`] is the throwaway placeholder.
pub const KEYS_ARE_PLACEHOLDER: bool = true;

/// One or two pinned minisign public keys (the base64 line of the `.pub` file).
pub const PINNED_KEYS: &[&str] = &[
    // PLACEHOLDER, no secret key exists. Replace with the release key.
    "RWRU3O0qD7N8OGqGTd2ih2sihZojl1yd3AKzDHtOA20G7VbJ5jf86kMH",
];
