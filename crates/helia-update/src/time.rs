// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! RFC 3339 UTC timestamps (`2026-10-24T18:00:00Z`) without a date library.
//!
//! Only the `Z` form is accepted: the manifest is machine written. The civil-date
//! arithmetic is Howard Hinnant's public-domain `days_from_civil` and
//! `civil_from_days` algorithms (<https://howardhinnant.github.io/date_algorithms.html>).

/// Seconds since the Unix epoch for `YYYY-MM-DDTHH:MM:SSZ`, or `None`.
#[must_use]
#[allow(clippy::many_single_char_names)]
pub fn parse_utc(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let t = &s[r];
        if t.bytes().all(|c| c.is_ascii_digit()) {
            t.parse().ok()
        } else {
            None
        }
    };
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, se) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1970..=9999).contains(&y) || !(1..=12).contains(&mo) || h > 23 || mi > 59 || se > 59 {
        return None;
    }
    if d < 1 || d > days_in_month(y, mo) {
        return None;
    }
    let days = days_from_civil(y, mo, d);
    u64::try_from(days * 86_400 + h * 3600 + mi * 60 + se).ok()
}

/// `YYYY-MM-DDTHH:MM:SSZ` for seconds since the Unix epoch.
#[must_use]
pub fn format_utc(secs: u64) -> String {
    let secs = i64::try_from(secs).unwrap_or(i64::MAX / 2);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if is_leap(y) => 29,
        _ => 28,
    }
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_instants() {
        assert_eq!(parse_utc("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_utc("2000-02-29T12:00:00Z"), Some(951_825_600));
        assert_eq!(parse_utc("2026-10-24T18:00:00Z"), Some(1_792_864_800));
    }

    #[test]
    fn round_trip() {
        for t in [0, 86_399, 951_825_600, 1_792_864_800, 4_102_444_799] {
            assert_eq!(parse_utc(&format_utc(t)), Some(t));
        }
    }

    #[test]
    fn rejects_bad_dates() {
        for s in [
            "2026-13-01T00:00:00Z",
            "2026-02-30T00:00:00Z",
            "2025-02-29T00:00:00Z",
            "2026-10-24 18:00:00Z",
            "2026-10-24T18:00:00+00:00",
            "2026-10-24T24:00:00Z",
            "2026-10-24T18:00:00",
            "",
            "+026-10-24T18:00:00Z",
        ] {
            assert_eq!(parse_utc(s), None, "{s}");
        }
    }
}
