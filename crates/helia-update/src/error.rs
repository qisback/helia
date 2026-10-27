// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The error type. Each message says what happened and why in plain words.

/// Result with [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Everything that can go wrong while checking for or fetching an update.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The manifest is bigger than the limit.
    #[error("the update information is {size} bytes, over the {max} byte limit, so it was ignored")]
    TooLarge {
        /// Size seen (or more than this, when the read was cut off).
        size: usize,
        /// The limit.
        max: usize,
    },
    /// The JSON nests deeper than the limit.
    #[error("the update information is nested too deeply, so it was ignored")]
    TooDeep,
    /// The manifest is not valid manifest v1 JSON.
    #[error("the update information is not valid: {0}")]
    Malformed(String),
    /// The signature file cannot be read as a minisign signature.
    #[error("the update signature is missing or not in the expected format: {0}")]
    BadSignatureFormat(String),
    /// No pinned key made this signature: tampered data, or signed by another key.
    #[error("the update information is not signed by a key this app trusts, so it was ignored")]
    BadSignature,
    /// A pinned public key cannot be read.
    #[error("a pinned update key is not a valid minisign public key: {0}")]
    BadKey(String),
    /// Zero or more than two pinned keys.
    #[error("an update source needs one or two pinned keys, not {0}")]
    KeyCount(usize),
    /// The source's app, channel or URL is not acceptable.
    #[error("the update source is not valid: {0}")]
    BadSource(String),
    /// The manifest is for another app or channel (mix-and-match).
    #[error("the update information is for {found}, not {expected}, so it was ignored")]
    WrongTarget {
        /// What the source asked for, `app/channel`.
        expected: String,
        /// What the manifest says.
        found: String,
    },
    /// The manifest is past its expiry (a freeze attack, or an old copy).
    #[error("the update information expired (expiry {expires}, now {now}), so it was ignored")]
    Expired {
        /// Expiry, seconds since the Unix epoch.
        expires: u64,
        /// Now, seconds since the Unix epoch.
        now: u64,
    },
    /// The sequence is lower than one already seen (a replayed old manifest).
    #[error(
        "the update information is older than one already seen (sequence {seen}, highest {highest}), so it was ignored"
    )]
    Replay {
        /// The manifest's sequence.
        seen: u64,
        /// The highest this client has seen.
        highest: u64,
    },
    /// A pre-release version on the stable channel.
    #[error("the stable channel offered the pre-release {0}, so it was ignored")]
    PrereleaseOnStable(String),
    /// A URL that is not https (or not an allowed loopback http URL).
    #[error("the address {0} is not a secure (https) address, so it was not used")]
    InsecureUrl(String),
    /// An address (or a redirect) on a host this app does not fetch updates from.
    #[error("the address {0} is not on a server this app takes updates from, so it was not used")]
    ForeignHost(String),
    /// The network request failed.
    #[error("could not reach the update server: {0}")]
    Http(String),
    /// The server answered with an unexpected status.
    #[error("the update server answered with status {0}")]
    HttpStatus(u16),
    /// The download stopped early; the partial file is kept and the next try resumes.
    #[error("the download stopped early at {got} of {expected} bytes; it will resume next time")]
    Truncated {
        /// Bytes on disk.
        got: u64,
        /// Bytes expected.
        expected: u64,
    },
    /// The server sent more than the manifest declared.
    #[error(
        "the server sent more data than the update information declared, so the download was discarded"
    )]
    TooMuchData,
    /// The finished file does not match the manifest's sha256.
    #[error("the downloaded file does not match its checksum, so it was discarded")]
    HashMismatch,
    /// No asset fits this platform and install type.
    #[error("no download for this system is listed in the update information")]
    NoAsset,
    /// The manifest wants a newer app than this one to read it.
    #[error(
        "these update details use format {0}, which this version cannot read; update the app by hand"
    )]
    SchemaTooNew(u32),
    /// The download server refused the download, or a backend declined it. The text
    /// says what happened, why and what to do.
    #[error("{0}")]
    Refused(String),
    /// A backend could not do what it was asked. The text says what and why.
    #[error("{0}")]
    Backend(String),
    /// Installing the update failed; the text says what and what was left alone.
    #[error("could not install the update: {0}")]
    Apply(String),
    /// The update settings cannot be saved.
    #[error("the update settings were not saved: {0}")]
    Settings(String),
    /// A file operation failed.
    #[error("file error: {0}")]
    Io(#[from] std::io::Error),
    /// The state file cannot be read.
    #[error("the update state file cannot be read: {0}")]
    State(String),
}
