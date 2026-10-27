# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
"""Checks on the pinned external items: shape, https, hashes, licences present."""
import datetime as dt
import json
import pathlib
import re
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).parent / "external"))
import build_manifest  # noqa: E402

ITEMS = pathlib.Path(__file__).parent / "external" / "helia-items.json"
WHEN = dt.datetime(2026, 10, 26, 12, 0, 0, tzinfo=dt.timezone.utc)


class ExternalManifest(unittest.TestCase):
    def setUp(self):
        self.doc = build_manifest.build(ITEMS, 1, WHEN, 90)

    def test_dates_and_schema(self):
        self.assertEqual(self.doc["schema"], 1)
        self.assertEqual(self.doc["date"], "2026-10-26T12:00:00Z")
        self.assertEqual(self.doc["expires"], "2027-01-24T12:00:00Z")

    def test_every_licence_has_its_full_text(self):
        for lic in self.doc["licences"]:
            self.assertGreater(len(lic["text"]), 1000, lic["id"])
            self.assertTrue(lic["source_url"].startswith("https://"))
        ids = {lic["id"] for lic in self.doc["licences"]}
        for item in self.doc["items"]:
            self.assertIn(item["licence"], ids)
            for d in item["downloads"] + item["packages"]:
                self.assertIn(d.get("licence", item["licence"]), ids)

    def test_downloads_are_https_with_real_looking_hashes(self):
        for item in self.doc["items"]:
            for d in item["downloads"]:
                self.assertTrue(d["url"].startswith("https://"), d["url"])
                self.assertRegex(d["sha256"], r"^[0-9a-f]{64}$")
                self.assertGreater(d["size"], 1_000_000)

    def test_package_commands_are_plain_words(self):
        for item in self.doc["items"]:
            for p in item["packages"]:
                for w in p["argv"]:
                    self.assertRegex(w, r"^[A-Za-z0-9._+:=@-]+$")

    def test_json_round_trip_is_stable(self):
        text = json.dumps(self.doc, indent=2, ensure_ascii=False)
        self.assertEqual(json.loads(text), self.doc)
        self.assertLess(len(text.encode()), 512 * 1024)


if __name__ == "__main__":
    unittest.main()
