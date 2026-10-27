// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Minisign signature verification against pinned public keys.
//!
//! Format: <https://jedisct1.github.io/minisign/>. A key is Ed25519; the default
//! signature algorithm `ED` signs the BLAKE2b-512 hash of the file, and the
//! trusted comment is covered by a second (global) signature. The check is done
//! by the permissively licensed `minisign-verify` crate (MIT).
//!
//! The signature covers the exact bytes of the manifest file: verify before
//! parsing and never re-serialise.

use minisign_verify::{Error as MsError, PublicKey, Signature};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// Parse a public key given as the contents of a `.pub` file (comment line and
/// base64 line) or as the bare base64 line.
///
/// # Errors
/// [`Error::BadKey`].
pub fn parse_public_key(text: &str) -> Result<PublicKey> {
    let text = text.trim();
    let r = if text.lines().count() > 1 {
        PublicKey::decode(text)
    } else {
        PublicKey::from_base64(text)
    };
    r.map_err(|e| Error::BadKey(e.to_string()))
}

/// Check that `signature_text` (the contents of a `.minisig` file) signs `data`
/// with one of the pinned `keys`. Legacy (unhashed) signatures are refused.
///
/// # Errors
/// [`Error::BadSignatureFormat`], [`Error::BadKey`], [`Error::KeyCount`] or
/// [`Error::BadSignature`].
pub fn verify(data: &[u8], signature_text: &str, keys: &[String]) -> Result<()> {
    if keys.is_empty() || keys.len() > 2 {
        return Err(Error::KeyCount(keys.len()));
    }
    if signature_text.len() > 4096 {
        return Err(Error::BadSignatureFormat("too long".into()));
    }
    let sig =
        Signature::decode(signature_text).map_err(|e| Error::BadSignatureFormat(e.to_string()))?;
    let mut ok = false;
    for k in keys {
        let pk = parse_public_key(k)?;
        match pk.verify(data, &sig, false) {
            Ok(()) => ok = true,
            Err(
                MsError::InvalidSignature | MsError::UnexpectedKeyId | MsError::UnexpectedAlgorithm,
            ) => {}
            Err(e) => return Err(Error::BadSignatureFormat(e.to_string())),
        }
    }
    if ok { Ok(()) } else { Err(Error::BadSignature) }
}

/// Lower-case hex sha256 of `data`.
#[must_use]
pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

/// Lower-case hex of bytes.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    // The key and signature from the minisign-verify crate's own tests (made by
    // the reference minisign tool): a known answer for the real format.
    const KEY: &str = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
    const SIG: &str = "untrusted comment: signature from minisign secret key
RUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=
trusted comment: timestamp:1556193335	file:test
y/rUw2y8/hOUYjZU71eHp/Wo1KZ40fGy2VJEDl34XMJM+TX48Ss/17u3IvIfbVR1FkZZSNCisQbuQY+bHwhEBg==";
    // The same key's legacy (unhashed) signature of "test": refused by policy.
    const LEGACY: &str = "untrusted comment: signature from minisign secret key
RWQf6LRCGA9i59SLOFxz6NxvASXDJeRtuZykwQepbDEGt87ig1BNpWaVWuNrm73YiIiJbq71Wi+dP9eKL8OC351vwIasSSbXxwA=
trusted comment: timestamp:1555779966	file:test
QtKMXWyYcwdpZAlPF7tE2ENJkRd1ujvKjlj1m9RtHTBnZPa5WKU5uWRs5GoP5M/VqE81QFuMKI5k/SfNQUaOAA==";

    #[test]
    fn legacy_signatures_are_refused() {
        assert!(matches!(
            verify(b"test", LEGACY, &[KEY.to_string()]),
            Err(Error::BadSignature)
        ));
    }

    #[test]
    fn reference_minisign_signature_verifies() {
        verify(b"test", SIG, &[KEY.to_string()]).unwrap();
        assert!(matches!(
            verify(b"Test", SIG, &[KEY.to_string()]),
            Err(Error::BadSignature)
        ));
        let full = format!("untrusted comment: minisign public key E7620F1842B4E81F\n{KEY}\n");
        verify(b"test", SIG, &[full]).unwrap();
    }

    #[test]
    fn key_count_is_one_or_two() {
        assert!(matches!(verify(b"test", SIG, &[]), Err(Error::KeyCount(0))));
        let k = KEY.to_string();
        assert!(matches!(
            verify(b"test", SIG, &[k.clone(), k.clone(), k]),
            Err(Error::KeyCount(3))
        ));
    }

    #[test]
    fn garbage_is_refused() {
        assert!(matches!(
            verify(b"test", "nonsense", &[KEY.to_string()]),
            Err(Error::BadSignatureFormat(_))
        ));
        assert!(matches!(
            verify(b"test", SIG, &["not a key".to_string()]),
            Err(Error::BadKey(_))
        ));
    }

    #[test]
    fn sha256_known_answer() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
