// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The pinned release public keys: the one place to put them.
//!
//! [`PINNED_KEYS`] holds the Helia release key (minisign key id 5725F88D0DF42020).
//! Its secret half is in bao, `claude-automation/release-signing/helia`, and is read
//! only by the signing step of the release workflow. During a key rotation the list
//! holds the current and the next key. The release workflow refuses to publish while
//! [`KEYS_ARE_PLACEHOLDER`] is `true` or the old throwaway key is pinned.

/// True while [`PINNED_KEYS`] is a throwaway placeholder.
pub const KEYS_ARE_PLACEHOLDER: bool = false;

/// One or two pinned minisign public keys (the base64 line of the `.pub` file).
pub const PINNED_KEYS: &[&str] = &[
    // Helia release key, id 5725F88D0DF42020.
    "RWQgIPQNjfglV5U0HSmFdRpTCMtwAuMrkaqgTANRv4KHD+ZRfen3Vw4J",
];
