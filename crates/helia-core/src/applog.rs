// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! A small rotating log file shared by the desktop app, the command line and
//! the server.
//!
//! One line per event: `2026-10-19T12:00:00Z INFO helia-gui: message`. The
//! file is `helia.log` in the OS log directory (see [`log_dir`]), rotated at
//! [`MAX_BYTES`] into `helia.log.1` and `helia.log.2`, so it never grows
//! without bound. The log holds events, file names and error text only: never
//! pixels, never whole paths ([`scrub_paths`] shortens any it is given).
//! Nothing is sent anywhere; it leaves the machine only if the user attaches
//! the "Save bug report" zip to an issue.
//!
//! Logging is best effort. A log that cannot be opened or written never stops
//! the program.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Size at which the log rotates.
pub const MAX_BYTES: u64 = 512 * 1024;
/// Rotated files kept besides the live one.
pub const KEEP: usize = 2;
/// Name of the live log file.
pub const FILE_NAME: &str = "helia.log";

/// Severity of a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// Normal events.
    Info,
    /// Something unexpected that was handled.
    Warn,
    /// A failure.
    Error,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }
}

/// The OS log directory: `$HELIA_LOG_DIR`, else `%LOCALAPPDATA%\helia\logs`,
/// else `$XDG_STATE_HOME/helia`, else `~/.local/state/helia`.
#[must_use]
pub fn log_dir() -> Option<PathBuf> {
    let var = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if let Some(p) = var("HELIA_LOG_DIR") {
        return Some(p);
    }
    if let Some(p) = var("LOCALAPPDATA") {
        return Some(p.join("helia").join("logs"));
    }
    if let Some(p) = var("XDG_STATE_HOME") {
        return Some(p.join("helia"));
    }
    var("HOME").map(|h| h.join(".local").join("state").join("helia"))
}

/// Shorten every path-like word in `text` to its last component, so a log
/// line names a file but not the folders (or user name) above it.
#[must_use]
pub fn scrub_paths(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if word.is_empty() {
            return;
        }
        let core = word.trim_matches(|c: char| "\"'`()[]<>,;".contains(c));
        if core.len() > 3 && (core.contains('/') || core.contains('\\')) {
            let last = core
                .trim_end_matches(['/', '\\'])
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or("");
            out.push_str(&word.replace(core, last));
        } else {
            out.push_str(word);
        }
        word.clear();
    };
    for c in text.chars() {
        if c.is_whitespace() {
            flush(&mut word, &mut out);
            out.push(c);
        } else {
            word.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

/// `YYYY-MM-DDTHH:MM:SSZ` for a time in seconds since 1970 (UTC).
#[must_use]
pub fn format_utc(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// The open log file and the rules for rotating it.
struct Sink {
    dir: PathBuf,
    program: String,
    file: Option<File>,
    size: u64,
    max: u64,
}

impl Sink {
    fn open(dir: &Path, program: &str, max: u64) -> Self {
        let _ = fs::create_dir_all(dir);
        let mut s = Self {
            dir: dir.to_owned(),
            program: program.to_owned(),
            file: None,
            size: 0,
            max,
        };
        s.reopen();
        s
    }

    fn path(&self, n: usize) -> PathBuf {
        if n == 0 {
            self.dir.join(FILE_NAME)
        } else {
            self.dir.join(format!("{FILE_NAME}.{n}"))
        }
    }

    fn reopen(&mut self) {
        self.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path(0))
            .ok();
        self.size = self
            .file
            .as_ref()
            .map_or(0, |f| f.metadata().map_or(0, |m| m.len()));
    }

    fn rotate(&mut self) {
        self.file = None;
        for n in (1..=KEEP).rev() {
            let from = self.path(n - 1);
            let to = self.path(n);
            let _ = fs::remove_file(&to);
            let _ = fs::rename(&from, &to);
        }
        self.reopen();
    }

    fn write(&mut self, level: Level, secs: u64, msg: &str) {
        let flat = scrub_paths(msg).replace(['\r', '\n'], " ");
        let line = format!(
            "{} {} {}: {}\n",
            format_utc(secs),
            level.label(),
            self.program,
            flat
        );
        if self.size + line.len() as u64 > self.max && self.size > 0 {
            self.rotate();
        }
        if let Some(f) = &mut self.file
            && f.write_all(line.as_bytes()).is_ok()
        {
            self.size += line.len() as u64;
        }
    }
}

static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();

/// Start logging for `program` (for example `helia-gui`). Safe to call more
/// than once; the first call wins. Also records panics. Returns false when
/// there is no log directory.
pub fn init(program: &str) -> bool {
    let Some(dir) = log_dir() else { return false };
    let _ = SINK.set(Mutex::new(Sink::open(&dir, program, MAX_BYTES)));
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        error(&format!("panic: {info}"));
        prev(info);
    }));
    true
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Write a line. Does nothing before [`init`].
pub fn log(level: Level, msg: &str) {
    if let Some(m) = SINK.get()
        && let Ok(mut s) = m.lock()
    {
        s.write(level, now(), msg);
    }
}

/// Log an event.
pub fn info(msg: &str) {
    log(Level::Info, msg);
}

/// Log something unexpected that was handled.
pub fn warn(msg: &str) {
    log(Level::Warn, msg);
}

/// Log a failure.
pub fn error(msg: &str) {
    log(Level::Error, msg);
}

/// The last `max_lines` lines of the log in `dir`, reaching into the rotated
/// file when the live one is short. Empty when there is no log.
#[must_use]
pub fn tail_in(dir: &Path, max_lines: usize) -> String {
    let mut lines: Vec<String> = Vec::new();
    for n in 0..=KEEP {
        let path = if n == 0 {
            dir.join(FILE_NAME)
        } else {
            dir.join(format!("{FILE_NAME}.{n}"))
        };
        let mut part = read_tail(&path, MAX_BYTES);
        part.append(&mut lines);
        lines = part;
        if lines.len() >= max_lines {
            break;
        }
    }
    let skip = lines.len().saturating_sub(max_lines);
    let mut out = lines[skip..].join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// [`tail_in`] for the default log directory.
#[must_use]
pub fn tail(max_lines: usize) -> String {
    log_dir().map_or_else(String::new, |d| tail_in(&d, max_lines))
}

fn read_tail(path: &Path, max_bytes: u64) -> Vec<String> {
    let Ok(mut f) = File::open(path) else {
        return Vec::new();
    };
    let len = f.metadata().map_or(0, |m| m.len());
    let from = len.saturating_sub(max_bytes);
    let mut buf = Vec::new();
    if f.seek(SeekFrom::Start(from)).is_err() || f.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&buf);
    let mut it = text.lines();
    if from > 0 {
        it.next(); // a cut first line
    }
    it.map(str::to_owned).collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("helia-applog-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn the_clock_formats_known_instants() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_utc(1_792_411_245), "2026-10-19T12:00:45Z");
    }

    #[test]
    fn paths_are_cut_to_file_names() {
        assert_eq!(
            scrub_paths(r#"cannot open "C:\Users\ann\sun\Sun_Test.ser": denied"#),
            r#"cannot open "Sun_Test.ser": denied"#
        );
        assert_eq!(
            scrub_paths("read /home/ann/caps/a.avi failed at 3/4"),
            "read a.avi failed at 3/4"
        );
        assert_eq!(scrub_paths("opened sun.ser"), "opened sun.ser");
    }

    #[test]
    fn lines_are_stamped_flat_and_scrubbed() {
        let d = scratch("lines");
        let mut s = Sink::open(&d, "helia-test", MAX_BYTES);
        s.write(Level::Warn, 0, "one\ntwo /a/b/file.ser");
        let text = fs::read_to_string(d.join(FILE_NAME)).unwrap();
        assert_eq!(
            text,
            "1970-01-01T00:00:00Z WARN helia-test: one two file.ser\n"
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn the_log_rotates_and_keeps_a_bounded_number_of_files() {
        let d = scratch("rotate");
        let mut s = Sink::open(&d, "p", 200);
        for i in 0..60 {
            s.write(Level::Info, 0, &format!("line number {i}"));
        }
        let names: Vec<String> = fs::read_dir(&d)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), KEEP + 1, "{names:?}");
        for n in 0..=KEEP {
            let len = fs::metadata(s.path(n)).unwrap().len();
            assert!(len <= 200, "{n}: {len}");
        }
        let tail = tail_in(&d, 3);
        assert_eq!(tail.lines().count(), 3);
        assert!(tail.trim_end().ends_with("line number 59"), "{tail}");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_missing_log_has_an_empty_tail() {
        assert_eq!(tail_in(&scratch("none"), 10), "");
    }
}
