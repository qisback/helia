# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
"""Offline tests for package_managers.py and tools/downloads.py: python3 -m unittest discover -s packaging"""
import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent / "tools"))
import downloads as dl  # noqa: E402
import package_managers as pm  # noqa: E402

DATA = HERE / "testdata"
REL = json.loads((DATA / "release-0.7.1.json").read_text("utf8"))


class PackageManagersTest(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        (self.tmp / "flatpak").mkdir()
        shutil.copy(HERE / "flatpak" / "casa.discworld.helia.yml", self.tmp / "flatpak")

    def run_pm(self, rel=None):
        rel_path = DATA / "release-0.7.1.json"
        if rel is not None:
            rel_path = self.tmp / "rel.json"
            rel_path.write_text(json.dumps(rel), "utf8")
        return pm.main(["--release-json", str(rel_path), "--sums", str(DATA / "SHA256SUMS"), "--out", str(self.tmp)])

    def test_winget_manifest_has_the_real_installer_hash(self):
        self.assertEqual(self.run_pm(), 0)
        d = self.tmp / "winget/manifests/l/LaurenceHorrocksBarlow/Helia/0.7.1"
        self.assertEqual(sorted(p.name for p in d.iterdir()), [
            "LaurenceHorrocksBarlow.Helia.installer.yaml", "LaurenceHorrocksBarlow.Helia.locale.en-GB.yaml",
            "LaurenceHorrocksBarlow.Helia.yaml"])
        inst = (d / "LaurenceHorrocksBarlow.Helia.installer.yaml").read_text("utf8")
        self.assertIn("InstallerSha256: B3FB82678EF0E224A89F9D10619FB09E0EA97B0C024CC4E735EC3055BB7D5C0D", inst)
        self.assertIn("InstallerUrl: https://github.com/qisback/helia/releases/download/v0.7.1/helia-0.7.1-x86_64-pc-windows-msvc-setup.exe", inst)
        self.assertIn("InstallerType: nsis", inst)
        self.assertIn("ReleaseDate: 2026-10-22", inst)
        self.assertNotIn("{", inst)
        loc = (d / "LaurenceHorrocksBarlow.Helia.locale.en-GB.yaml").read_text("utf8")
        self.assertIn("Publisher: Laurence Horrocks-Barlow", loc)
        self.assertNotIn("@", loc)                                      # no email in notices

    def test_flatpak_sources_follow_the_release(self):
        self.run_pm()
        y = (self.tmp / "flatpak/casa.discworld.helia.yml").read_text("utf8")
        self.assertIn('sha256: "1dc30fce31004135e41f298014145ebf239a645f55a1bdfa47d999ef071618ba"', y)
        self.assertIn("only-arches: [aarch64]", y)
        self.assertIn('sha256: "0d3580507af880dba7d25207cedfb6600441ebc9489023c3f4e1b577603522ad"', y)
        self.assertEqual(y.count("type: archive"), 2)
        self.assertIn("# END sources", y)

    def test_homebrew_only_with_a_dmg(self):
        self.run_pm()
        self.assertFalse((self.tmp / "homebrew").exists())
        rel = dict(REL, assets=REL["assets"] + [{"name": "Helia-0.7.1-macos.dmg", "size": 1, "browser_download_url": "x"}])
        sums = (DATA / "SHA256SUMS").read_text("utf8") + "ab" * 32 + "  Helia-0.7.1-macos.dmg\n"
        (self.tmp / "sums").write_text(sums, "utf8")
        (self.tmp / "rel.json").write_text(json.dumps(rel), "utf8")
        pm.main(["--release-json", str(self.tmp / "rel.json"), "--sums", str(self.tmp / "sums"), "--out", str(self.tmp)])
        cask = (self.tmp / "homebrew/helia.rb").read_text("utf8")
        self.assertIn('version "0.7.1"', cask)
        self.assertIn('v#{version}/Helia-#{version}-macos.dmg', cask)

    def test_checked_in_flatpak_matches_the_release_data(self):
        text = (HERE / "flatpak/casa.discworld.helia.yml").read_text("utf8")
        self.assertIn("# BEGIN sources", text)
        self.assertNotIn("0000000000000000000000000000000000000000000000000000000000000000", text)


class DownloadsTest(unittest.TestCase):
    def test_every_asset_is_classified_and_buttons_exist(self):
        m = dl.build("helia", REL)
        ids = {o["id"] for o in m["options"]}
        self.assertIn("windows-x64-windows-setup", ids)
        self.assertIn("linux-arm64-appimage", ids)
        self.assertEqual(len(ids), 10)
        self.assertIn("Installs from your software centre", m["fallback"])

    def test_docs_page_reads_the_manifest_beside_the_site(self):
        page = dl.docs_page(dl.build("helia", REL))
        self.assertIn('data-dl-src="../../downloads.json"', page)

    def test_cli_writes_manifest_and_page(self):
        d = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, d, ignore_errors=True)
        r = subprocess.run([sys.executable, str(HERE.parent / "tools" / "downloads.py"), "helia", "--release-json",
                            str(DATA / "release-0.7.1.json"), "--out", str(d / "d.json"), "--docs-page", str(d / "p.md")],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(json.loads((d / "d.json").read_text("utf8"))["version"], "0.7.1")
        self.assertTrue((d / "p.md").read_text("utf8").startswith("---\n"))


if __name__ == "__main__":
    unittest.main()
