#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

"""Tests for the release-notes generator (run: python3 -m unittest discover -s packaging)."""
import pathlib
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import release  # noqa: E402

CHANGELOG = """# Changelog

## [Unreleased]

### Added

- Not released yet.

## [1.2.3] - 2026-10-21

### Fixed

- A crash on empty input
  ([#12](https://example.invalid/issues/12)). Cause: a missing check, found by
  bisecting; measured 3 of 3 runs fixed.
- Slow start-up: the cache is now warmed on a thread.

### Added

- New `--flag` option (#3). It does a thing in a particular way.

### Security

- Reject paths outside the project folder.

### Documentation

- A tutorial.

## [1.2.2] - 2026-10-01

### Changed

- Older.
"""

SUMS = {"a.tar.gz": "a" * 64, "b.zip": "b" * 64}


class Notes(unittest.TestCase):
    def setUp(self):
        d = tempfile.mkdtemp()
        self.cl = pathlib.Path(d) / "CHANGELOG.md"
        self.cl.write_text(CHANGELOG, encoding="utf-8")
        body, date = release.changelog_entry(self.cl, "1.2.3")
        self.date = date
        self.notes = release.build_notes("1.2.3", "v1.2.3", date, body, "https://example.invalid", "o/r")

    def test_date_from_changelog(self):
        self.assertEqual(self.date, "2026-10-21")
        self.assertIn("Released 2026-10-21.", self.notes)

    def test_one_line_per_change_in_order(self):
        order = [self.notes.index(f"### {h}") for h in ("Added", "Fixed", "Security", "Documentation")]
        self.assertEqual(order, sorted(order))
        self.assertLess(self.notes.index("### Added"), self.notes.index("### Fixed"))
        self.assertIn("- A crash on empty input (#12)\n", self.notes)
        self.assertIn("- Slow start-up: the cache is now warmed on a thread\n", self.notes)
        self.assertIn("- New `--flag` option (#3)\n", self.notes)
        self.assertNotIn("bisecting", self.notes)
        self.assertNotIn("Not released yet", self.notes)
        self.assertNotIn("Older", self.notes)

    def test_impersonal(self):
        low = self.notes.lower()
        for word in ("dedicat", "daughter", "12hobbies", "made by", "interested"):
            self.assertNotIn(word, low)
        self.assertNotIn("![", self.notes)
        first = self.notes.splitlines()[0]
        self.assertTrue(first.startswith("<img ") and 'width="96"' in first and "helia-logo.png" in first)
        self.assertIn("GPL-3.0-or-later", self.notes)

    def test_downloads_and_source(self):
        for p in ("Linux x86_64", "Linux aarch64", "Windows x86_64"):
            self.assertIn(p, self.notes)
        self.assertNotIn("macOS (Apple silicon)", self.notes)
        self.assertIn(release.NO_MACOS, self.notes)
        with_mac = release.build_notes("1.2.3", "v1.2.3", self.date, "", "https://example.invalid", "o/r", macos=True)
        self.assertIn("| macOS (Apple silicon) | dmg |", with_mac)
        self.assertNotIn(release.NO_MACOS, with_mac)
        self.assertIn("https://example.invalid/o/r/archive/v1.2.3.tar.gz", self.notes)

    def test_checksum_block_round_trip(self):
        # The release workflows (release.yml, release-windows.yml, release-macos.yml) parse these markers.
        body = release.body_with_sums(self.notes, SUMS)
        self.assertIn(release.SUMS_BEGIN, body)
        self.assertIn(release.SUMS_END, body)
        self.assertEqual(release.sums_in_body(body), SUMS)
        more = dict(SUMS, **{"c.dmg": "c" * 64})
        body2 = release.body_with_sums(body, more)
        self.assertEqual(release.sums_in_body(body2), more)
        self.assertEqual(body2.count(release.SUMS_BEGIN), 1)

    def test_one_line_trims_long_and_balances(self):
        long = "Word " * 60 + "(open paren that never closes " + "x " * 40
        line = release.one_line(long)
        self.assertLessEqual(len(line), release.MAX_LINE)
        self.assertEqual(line.count("("), line.count(")"))


if __name__ == "__main__":
    unittest.main()
