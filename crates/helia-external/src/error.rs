// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The error type. Each message says what happened, why, and what to do, in plain words.

/// Result with [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Everything that can go wrong while listing, accepting, fetching or installing an external item.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A signature, address, host, checksum or network problem from the shared update layer.
    #[error("{}", transport_text(.0))]
    Transport(#[from] helia_update::Error),
    /// The manifest is bigger than the limit.
    #[error(
        "the list of optional tools is {size} bytes, over the {max} byte limit, so it was ignored"
    )]
    TooLarge {
        /// Size seen (or more than this, when the read was cut off).
        size: usize,
        /// The limit.
        max: usize,
    },
    /// The JSON nests deeper than the limit.
    #[error("the list of optional tools is nested too deeply, so it was ignored")]
    TooDeep,
    /// The manifest is not valid.
    #[error("the list of optional tools is not valid: {0}")]
    Malformed(String),
    /// The manifest wants a newer app than this one to read it.
    #[error(
        "the list of optional tools uses format {0}, which this version cannot read; update the app by hand"
    )]
    SchemaTooNew(u32),
    /// The manifest is for another app (mix-and-match).
    #[error("the list of optional tools is for {found}, not {expected}, so it was ignored")]
    WrongApp {
        /// The app this source serves.
        expected: String,
        /// What the manifest says.
        found: String,
    },
    /// The manifest is past its expiry (a freeze attack, or an old copy).
    #[error("the list of optional tools expired (expiry {expires}, now {now}), so it was ignored")]
    Expired {
        /// Expiry, seconds since the Unix epoch.
        expires: u64,
        /// Now, seconds since the Unix epoch.
        now: u64,
    },
    /// The sequence is lower than one already seen (a replayed old manifest).
    #[error(
        "the list of optional tools is older than one already seen (sequence {seen}, highest {highest}), so it was ignored"
    )]
    Replay {
        /// The manifest's sequence.
        seen: u64,
        /// The highest this client has seen.
        highest: u64,
    },
    /// The source configuration is not acceptable.
    #[error("the tool source is not valid: {0}")]
    BadSource(String),
    /// No such item in the manifest.
    #[error("there is no optional tool called {0}")]
    UnknownItem(String),
    /// The item has no download or package command for this system.
    #[error(
        "{item} is not offered for this system ({platform}); install it yourself and point the app at it"
    )]
    NotOffered {
        /// Item id.
        item: String,
        /// Platform spelling, or `unknown`.
        platform: String,
    },
    /// The licence was not accepted (or has changed since it was); nothing was downloaded.
    #[error(
        "the licence for {0} has not been accepted, so nothing was downloaded; read it and accept it first"
    )]
    NotAccepted(String),
    /// The user cancelled.
    #[error("cancelled; the partial download is kept so the next try can carry on")]
    Cancelled,
    /// The archive cannot be unpacked.
    #[error("the downloaded archive could not be unpacked: {0}")]
    Archive(String),
    /// A file the manifest names is not in the archive.
    #[error("the downloaded archive does not contain {0}, so nothing was installed")]
    Member(String),
    /// The offer is a package manager command, not a download (or the other way round).
    #[error("this way of getting {0} cannot be used by that call")]
    WrongRoute(String),
    /// A package manager command is not acceptable.
    #[error("the package manager command is not acceptable: {0}")]
    BadCommand(String),
    /// A package manager command ran and failed.
    #[error("the package manager command failed ({0}); nothing else was changed")]
    CommandFailed(String),
    /// The item is not installed.
    #[error("{0} is not installed")]
    NotInstalled(String),
    /// A state file cannot be read or written.
    #[error("a settings file for optional tools cannot be used: {0}")]
    State(String),
    /// A file operation failed.
    #[error("file error: {0}")]
    Io(#[from] std::io::Error),
}

/// The update layer talks about "updates"; for a tool the same problem is worded for tools.
fn transport_text(e: &helia_update::Error) -> String {
    use helia_update::Error as E;
    match e {
        E::BadSignature => {
            "the list of optional tools is not signed by a key this app trusts, so it was ignored"
                .into()
        }
        E::BadSignatureFormat(why) => format!(
            "the signature of the list of optional tools is missing or not in the expected format: {why}"
        ),
        E::Http(why) => format!("could not reach the server: {why}"),
        E::HttpStatus(code) => format!("the server answered with status {code}"),
        E::ForeignHost(url) => format!(
            "the address {url} is not on a server this app downloads tools from, so it was not used"
        ),
        E::TooMuchData => {
            "the server sent more data than the list of optional tools declared, so the download was discarded"
                .into()
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_problems_are_worded_for_tools() {
        let say = |e: helia_update::Error| Error::Transport(e).to_string();
        assert!(say(helia_update::Error::BadSignature).contains("optional tools"));
        assert!(
            say(helia_update::Error::Http("down".into())).starts_with("could not reach the server")
        );
        assert!(
            say(helia_update::Error::ForeignHost(
                "https://x.example/".into()
            ))
            .contains("downloads tools from")
        );
        assert!(say(helia_update::Error::HashMismatch).contains("checksum"));
        for e in [
            helia_update::Error::BadSignature,
            helia_update::Error::HttpStatus(404),
            helia_update::Error::TooMuchData,
        ] {
            assert!(
                !say(e).contains("update"),
                "tool errors must not talk about updates"
            );
        }
    }
}
