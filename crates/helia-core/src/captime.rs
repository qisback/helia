// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! When a capture or a stacked image was taken.
//!
//! A SER file stamps every frame, a FITS file carries `DATE-AVG`. An AVI or an
//! MP4 does not, but capture programs name the file after the start time
//! (`15_32_09.avi`, `2025-09-17-161400-Solar.mp4`). This module reads the time
//! from the first of those that exists, in that order, and says which it was.

use std::path::Path;

use crate::io::solarnet::{CaptureInfo, fits_time, unix_from_iso};

/// What the caller knows that the file does not.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TimeHints {
    /// UTC midnight (unix seconds) of the observing day, for a file name that
    /// holds only the time of day.
    pub date: Option<f64>,
    /// Hours east of UTC of the clock that named the files (`1` for British
    /// Summer Time): the name's time is local time. Default 0, taking the name
    /// to be UTC.
    pub utc_offset_h: f64,
}

/// A time and where it came from.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptureTime {
    /// Unix seconds, UTC.
    pub unix: f64,
    /// `"SER frame timestamps"`, `"FITS header"`, `"file name"` or
    /// `"file name and --date"`.
    pub source: &'static str,
}

/// Time-of-day in seconds, and the date as unix seconds at UTC midnight, found
/// in a file name. Either can be missing.
#[must_use]
pub fn from_name(name: &str) -> (Option<f64>, Option<f64>) {
    let groups: Vec<&str> = name
        .split(|c: char| !c.is_ascii_digit())
        .filter(|g| !g.is_empty())
        .collect();
    let num = |g: Option<&&str>| g.and_then(|x| x.parse::<u32>().ok());
    let clock = |h: u32, m: u32, s: u32| {
        (h < 24 && m < 60 && s < 61).then(|| f64::from(h * 3600 + m * 60 + s))
    };
    let date_of = |y: u32, m: u32, d: u32| {
        ((1990..2200).contains(&y) && (1..=12).contains(&m) && (1..=31).contains(&d))
            .then(|| unix_from_iso(&format!("{y:04}-{m:02}-{d:02}")))
            .flatten()
    };
    let len = |i: usize| groups.get(i).map_or(0, |g| g.len());
    let (mut date, mut time) = (None, None);
    let mut i = 0;
    while i < groups.len() {
        // YYYY-MM-DD
        if date.is_none()
            && len(i) == 4
            && len(i + 1) == 2
            && len(i + 2) == 2
            && let (Some(y), Some(m), Some(d)) = (
                num(groups.get(i)),
                num(groups.get(i + 1)),
                num(groups.get(i + 2)),
            )
            && let Some(u) = date_of(y, m, d)
        {
            date = Some(u);
            i += 3;
            continue;
        }
        // YYYYMMDD
        if date.is_none()
            && len(i) == 8
            && let Some(v) = num(groups.get(i))
            && let Some(u) = date_of(v / 10_000, v / 100 % 100, v % 100)
        {
            date = Some(u);
            i += 1;
            continue;
        }
        // HH MM SS
        if time.is_none()
            && len(i) == 2
            && len(i + 1) == 2
            && len(i + 2) == 2
            && let (Some(h), Some(m), Some(s)) = (
                num(groups.get(i)),
                num(groups.get(i + 1)),
                num(groups.get(i + 2)),
            )
            && let Some(t) = clock(h, m, s)
        {
            time = Some(t);
            i += 3;
            continue;
        }
        // HHMMSS
        if time.is_none()
            && len(i) == 6
            && let Some(v) = num(groups.get(i))
            && let Some(t) = clock(v / 10_000, v / 100 % 100, v % 100)
        {
            time = Some(t);
        }
        i += 1;
    }
    (time, date)
}

/// The time `path` was taken, with its source. SER frame stamps come first,
/// then a FITS header, then the file name.
#[must_use]
pub fn capture_time(path: &Path, hints: &TimeHints) -> Option<CaptureTime> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if ext == "ser"
        && let Some(unix) = CaptureInfo::read(path, None).mid_unix()
    {
        return Some(CaptureTime {
            unix,
            source: "SER frame timestamps",
        });
    }
    if matches!(ext.as_str(), "fits" | "fit" | "fts")
        && let Some(unix) = fits_time(path)
    {
        return Some(CaptureTime {
            unix,
            source: "FITS header",
        });
    }
    let stem = path.file_stem()?.to_string_lossy();
    let (time, date) = from_name(&stem);
    let time = time?;
    let (day, source) = match (date, hints.date) {
        (Some(d), _) => (d, "file name"),
        (None, Some(d)) => (d, "file name and --date"),
        (None, None) => return None,
    };
    Some(CaptureTime {
        unix: day + time - hints.utc_offset_h * 3600.0,
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sharpcap_time_of_day() {
        let (t, d) = from_name("15_32_09");
        assert_eq!(t, Some(15.0 * 3600.0 + 32.0 * 60.0 + 9.0));
        assert_eq!(d, None);
    }

    #[test]
    fn date_and_six_digit_time() {
        let (t, d) = from_name("2025-09-17-161400-Solar");
        assert_eq!(t, Some(16.0 * 3600.0 + 14.0 * 60.0));
        assert_eq!(d, unix_from_iso("2025-09-17"));
    }

    #[test]
    fn compact_date_and_time() {
        let (t, d) = from_name("sun_20260115_093005_ha");
        assert_eq!(t, Some(9.0 * 3600.0 + 30.0 * 60.0 + 5.0));
        assert_eq!(d, unix_from_iso("2026-01-15"));
    }

    #[test]
    fn a_name_without_a_clock_has_none() {
        assert_eq!(from_name("Sun_Test"), (None, None));
        assert_eq!(from_name("frame_99_99_99").0, None);
    }

    #[test]
    fn the_offset_turns_local_time_into_utc() {
        let hints = TimeHints {
            date: unix_from_iso("2026-10-20"),
            utc_offset_h: 1.0,
        };
        let t =
            capture_time(Path::new("/x/15_32_09.avi"), &hints).unwrap_or_else(|| panic!("time"));
        assert_eq!(t.source, "file name and --date");
        assert_eq!(
            t.unix,
            unix_from_iso("2026-10-20T14:32:09").unwrap_or_else(|| panic!("iso"))
        );
        assert!(capture_time(Path::new("15_32_09.avi"), &TimeHints::default()).is_none());
    }
}
