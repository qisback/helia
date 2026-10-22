// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The proof of work the relay asks for.
//!
//! A nonce solves a challenge when `SHA-256(challenge_bytes || nonce as 8
//! little-endian bytes)` has at least `difficulty` leading zero bits. It costs
//! a client well under a second at the default difficulty and costs a spammer
//! on every request.
//!
//! Test vector (also in the protocol document): challenge `000102...1f` (the
//! bytes 0 to 31). Difficulty 12: the smallest nonce is 1048. Difficulty 16:
//! 14417, digest
//! `000011acd5d18fd8abb6021f278532519f3226a075b84d2cbbcf9281dc06f13f`.

use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};

/// Decode lower or upper case hex; `None` for odd length or other characters.
#[must_use]
pub fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Encode as lower case hex.
#[must_use]
pub fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Leading zero bits of a byte string.
#[must_use]
pub fn leading_zero_bits(hash: &[u8]) -> u32 {
    let mut n = 0;
    for &b in hash {
        if b == 0 {
            n += 8;
        } else {
            return n + b.leading_zeros();
        }
    }
    n
}

/// The digest for a challenge and nonce.
#[must_use]
pub fn digest(challenge: &[u8], nonce: u64) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(challenge);
    h.update(nonce.to_le_bytes());
    h.finalize().into()
}

/// Does `nonce` solve the hex `challenge` at `difficulty`? (The relay's check.)
#[must_use]
pub fn verify(challenge_hex: &str, nonce: u64, difficulty: u32) -> bool {
    decode_hex(challenge_hex).is_some_and(|c| leading_zero_bits(&digest(&c, nonce)) >= difficulty)
}

/// Find the smallest nonce that solves the challenge. Returns `None` when
/// `cancel` is set first or the challenge is not valid hex. Run it off the UI
/// thread.
#[must_use]
pub fn solve(challenge_hex: &str, difficulty: u32, cancel: &AtomicBool) -> Option<u64> {
    let challenge = decode_hex(challenge_hex)?;
    let mut prefix = Sha256::new();
    prefix.update(&challenge);
    for nonce in 0..u64::MAX {
        if nonce % 4096 == 0 && cancel.load(Ordering::Relaxed) {
            return None;
        }
        let mut h = prefix.clone();
        h.update(nonce.to_le_bytes());
        let out: [u8; 32] = h.finalize().into();
        if leading_zero_bits(&out) >= difficulty {
            return Some(nonce);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use super::*;

    fn vector() -> String {
        encode_hex(&(0..32).collect::<Vec<u8>>())
    }

    #[test]
    fn the_shared_test_vector() {
        let c = decode_hex(&vector()).unwrap();
        assert_eq!(
            encode_hex(&digest(&c, 14417)),
            "000011acd5d18fd8abb6021f278532519f3226a075b84d2cbbcf9281dc06f13f"
        );
        let never = AtomicBool::new(false);
        assert_eq!(solve(&vector(), 12, &never), Some(1048));
        assert_eq!(solve(&vector(), 16, &never), Some(14417));
        assert!(verify(&vector(), 14417, 16));
        assert!(!verify(&vector(), 14416, 16));
        assert!(!verify("zz", 0, 1));
    }

    #[test]
    fn counting_zero_bits() {
        assert_eq!(leading_zero_bits(&[0xff]), 0);
        assert_eq!(leading_zero_bits(&[0x00, 0x10]), 11);
        assert_eq!(leading_zero_bits(&[0x00, 0x00]), 16);
        assert_eq!(leading_zero_bits(&[0x01]), 7);
    }

    #[test]
    fn solving_can_be_cancelled() {
        let stop = AtomicBool::new(true);
        // 255 bits cannot be met; cancellation is the only way out.
        assert_eq!(solve(&vector(), 255, &stop), None);
    }

    #[test]
    fn hex_round_trips_and_rejects_rubbish() {
        assert_eq!(decode_hex("00ff10"), Some(vec![0, 255, 16]));
        assert_eq!(decode_hex("abc"), None);
        assert_eq!(decode_hex("0g"), None);
        assert_eq!(encode_hex(&[1, 171]), "01ab");
    }
}
