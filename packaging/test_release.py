#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

"""Tests for the release-notes generator (run: python3 -m unittest discover -s packaging)."""
import contextlib
import io
import json
import os
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

### Summary

- You can now do the new thing.
- Fixed a crash on empty input.

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

    def test_summary_is_for_the_public_changelog_not_the_forge_notes(self):
        self.assertNotIn("### Summary", self.notes)
        self.assertNotIn("You can now do the new thing", self.notes)

    def test_notes_need_a_summary(self):
        cl = self.cl.parent / "NOSUMMARY.md"
        cl.write_text(CHANGELOG.replace("### Summary\n\n- You can now do the new thing.\n- Fixed a crash on empty input.\n\n", ""),
                      encoding="utf-8")
        args = type("A", (), {"changelog": str(cl), "version": "1.2.3", "tag": "v1.2.3", "server": "https://example.invalid",
                              "repo": "o/r", "out": str(self.cl.parent / "notes.md"), "macos": False})()
        err = io.StringIO()
        with self.assertRaises(SystemExit), contextlib.redirect_stderr(err):
            release.cmd_notes(args)
        self.assertIn("### Summary", err.getvalue())
        args.changelog = str(self.cl)
        with contextlib.redirect_stdout(io.StringIO()):
            release.cmd_notes(args)

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

    def test_contents(self):
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

    def test_attaching_a_dmg_edits_the_notes_into_the_macos_form(self):
        body, _ = release.changelog_entry(self.cl, "1.2.3")
        with_mac = release.build_notes("1.2.3", "v1.2.3", self.date, body, "https://example.invalid", "o/r", macos=True)
        edited = release.notes_with_macos(self.notes)
        self.assertEqual(edited, with_mac)
        self.assertEqual(release.notes_with_macos(edited), edited)  # idempotent
        kept = release.body_with_sums(self.notes, {"a.tar.gz": "0" * 64})
        self.assertIn("a.tar.gz", release.notes_with_macos(kept))  # the sums block survives

    def test_attach_a_dmg_updates_the_release_notes_and_sums(self):
        # Regression: attaching used to stop after the upload with a TypeError (release[id]), leaving
        # the notes without the macOS row and SHA256SUMS without the dmg line.
        body, _ = release.changelog_entry(self.cl, "1.2.3")
        notes = release.body_with_sums(
            release.build_notes("1.2.3", "v1.2.3", self.date, body, "https://example.invalid", "o/r"), {"a.tar.gz": "0" * 64})
        calls = []

        class FakeForge:
            def __init__(self, server, repo):
                pass

            def call(self, method, path, **kw):
                calls.append((method, path, kw))
                if method == "GET":
                    return {"id": 7, "body": notes, "assets": []}
                return {}

        with tempfile.TemporaryDirectory() as d:
            pathlib.Path(d, "Helia-1.2.3-macos.dmg").write_bytes(b"dmg")
            args = type("A", (), {"tag": "v1.2.3", "assets": d, "server": "https://example.invalid", "repo": "o/r", "replace": False})()
            real = release.Forge
            release.Forge = FakeForge
            try:
                with contextlib.redirect_stdout(io.StringIO()):
                    release.cmd_attach(args)
            finally:
                release.Forge = real
        patches = [kw["body"]["body"] for m, _, kw in calls if m == "PATCH"]
        self.assertEqual(len(patches), 2)
        self.assertIn("| macOS (Apple silicon) | dmg |", patches[0])
        self.assertIn("Helia-1.2.3-macos.dmg", patches[1])
        self.assertIn("a.tar.gz", patches[1])
        self.assertIn("| macOS (Apple silicon) | dmg |", patches[1])  # the sums write must keep the edited notes
        self.assertNotIn(release.NO_MACOS, patches[1])

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


V = "0.8.0"
FILES = {
    f"helia-{V}-x86_64-pc-windows-msvc-setup.exe": b"win",
    f"helia-{V}-x86_64-pc-windows-msvc.zip": b"winzip",
    f"Helia-{V}-x86_64.AppImage": b"ai64",
    f"Helia-{V}-aarch64.AppImage": b"aiarm",
    f"helia_{V}_amd64.deb": b"deb64",
    f"helia-{V}-1.aarch64.rpm": b"rpmarm",
    f"helia-{V}-x86_64-unknown-linux-gnu.tar.gz": b"tgz",
    f"helia-{V}-cli.cdx.json": b"sbom",
    "SHA256SUMS": b"",
}


def write_assets(files=FILES):
    import hashlib
    d = pathlib.Path(tempfile.mkdtemp())
    for n, data in files.items():
        (d / n).write_bytes(data)
    sums = "".join(f"{hashlib.sha256(v).hexdigest()}  {n}\n" for n, v in sorted(files.items()) if n != "SHA256SUMS")
    (d / "SHA256SUMS").write_text(sums)
    return d


def manifest_args(**kw):
    import argparse
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd")
    release.add_manifest_parsers(sub)
    out = pathlib.Path(tempfile.mkdtemp())
    argv = ["manifest", "--out", str(out)]
    kw.setdefault("base_url", "https://paid.example.org/dl/helia/{version}/")
    for k, v in kw.items():
        argv += [f"--{k.replace('_', '-')}"] + ([] if v is True else [str(v)])
    return ap.parse_args(argv), out


def argparse_ns(**kw):
    import argparse
    return argparse.Namespace(**kw)


class UpdateManifest(unittest.TestCase):
    def test_channels(self):
        self.assertEqual(release.channels_for("0.8.0"), ["stable", "prerelease"])
        self.assertEqual(release.channels_for("1.0.0"), ["stable", "prerelease"])
        self.assertEqual(release.channels_for("0.9.0-rc.1"), ["prerelease"])
        self.assertEqual(release.channels_for("1.0.0-beta.2"), ["prerelease"])

    def test_classify(self):
        c = release.classify
        self.assertEqual(c(f"helia-{V}-x86_64-pc-windows-msvc-setup.exe", V), ("windows-x86_64", "nsis"))
        self.assertEqual(c(f"Helia-{V}-aarch64.AppImage", V), ("linux-aarch64", "appimage"))
        self.assertEqual(c(f"helia_{V}_arm64.deb", V), ("linux-aarch64", "deb"))
        self.assertEqual(c(f"helia-{V}-1.x86_64.rpm", V), ("linux-x86_64", "rpm"))
        self.assertEqual(c(f"Helia-{V}-macos.dmg", V), ("macos-aarch64", "dmg"))
        self.assertEqual(c("helia_0.9.0~rc.1_amd64.deb", "0.9.0-rc.1"), ("linux-x86_64", "deb"))
        for name in (f"helia-{V}-x86_64-pc-windows-msvc.zip", "SHA256SUMS", f"helia-{V}-cli.cdx.json",
                     "Helia-0.7.9-x86_64.AppImage", f"Helia-{V}-x86_64.AppImage.sig", "../Helia-0.8.0-x86_64.AppImage"):
            self.assertIsNone(c(name, V), name)

    def test_version_order(self):
        k = release.version_key
        self.assertLess(k("0.7.1"), k("0.8.0"))
        self.assertLess(k("0.8.0-rc.1"), k("0.8.0"))
        self.assertLess(k("0.8.0-rc.2"), k("0.8.0-rc.10"))
        self.assertLess(k("0.9.0"), k("0.10.0"))

    def test_builds_the_agreed_shape(self):
        a, out = manifest_args(tag=f"v{V}", assets_dir=write_assets(), date="2026-10-24T18:00:00Z",
                               min_supported="0.7.0", notes_url="https://example.invalid/n")
        release.cmd_manifest(a)
        doc = json.loads((out / "helia" / "stable" / "manifest.json").read_text())
        self.assertEqual(list(doc)[:7], ["schema", "app", "channel", "version", "date", "sequence", "expires"])
        self.assertEqual((doc["schema"], doc["app"], doc["channel"], doc["version"]), (1, "helia", "stable", V))
        self.assertEqual(doc["sequence"], release.parse_stamp("2026-10-24T18:00:00Z"))  # the date as Unix seconds
        self.assertEqual(doc["expires"], "2027-02-21T18:00:00Z")  # 120 days
        self.assertEqual(doc["min_supported"], "0.7.0")
        got = {(x["platform"], x["kind"]) for x in doc["assets"]}
        self.assertEqual(got, {("windows-x86_64", "nsis"), ("linux-x86_64", "appimage"), ("linux-aarch64", "appimage"),
                               ("linux-x86_64", "deb"), ("linux-aarch64", "rpm"), ("linux-x86_64", "targz")})
        win = next(x for x in doc["assets"] if x["kind"] == "nsis")
        self.assertEqual(win["url"], f"https://paid.example.org/dl/helia/{V}/helia-{V}-x86_64-pc-windows-msvc-setup.exe")
        self.assertEqual((win["size"], len(win["sha256"])), (3, 64))
        self.assertTrue((out / "helia" / "prerelease" / "manifest.json").is_file())

    def test_prerelease_version_only_prerelease_channel(self):
        v = "0.9.0-rc.1"
        a, out = manifest_args(version=v, assets_dir=write_assets({f"Helia-{v}-x86_64.AppImage": b"x"}),
                               allow_missing=True, date="2026-10-24T18:00:00Z")
        release.cmd_manifest(a)
        self.assertEqual(sorted(p.parent.name for p in out.glob("helia/*/manifest.json")), ["prerelease"])

    def test_github_asset_base_is_refused_and_a_base_is_required(self):
        for bad in (f"https://github.com/qisback/helia/releases/download/v{V}/",
                    "https://objects.githubusercontent.com/x/", "https://example.org/releases/download/v1/"):
            a, _ = manifest_args(tag=f"v{V}", assets_dir=write_assets(), base_url=bad)
            with self.assertRaises(SystemExit):
                with contextlib.redirect_stderr(io.StringIO()) as err:
                    release.cmd_manifest(a)
            self.assertIn("What:", err.getvalue())
            self.assertIn("Why:", err.getvalue())
            self.assertIn("What to do:", err.getvalue())
        # No base, no manifest: there is no public default.
        a, out = manifest_args(tag=f"v{V}", assets_dir=write_assets(), base_url="")
        with self.assertRaises(SystemExit):
            with contextlib.redirect_stderr(io.StringIO()) as err:
                release.cmd_manifest(a)
        self.assertIn("What to do:", err.getvalue())
        self.assertFalse(list(out.glob("helia/*/manifest.json")))
        # {version} and {tag} are filled in.
        a, out = manifest_args(tag=f"v{V}", assets_dir=write_assets(), base_url="https://shop.example/dl/{version}/{tag}")
        release.cmd_manifest(a)
        doc = json.loads((out / "helia" / "stable" / "manifest.json").read_text())
        self.assertTrue(all(x["url"].startswith(f"https://shop.example/dl/{V}/v{V}/") for x in doc["assets"]))

    def test_missing_required_asset_fails(self):
        files = {k: v for k, v in FILES.items() if "windows-msvc-setup" not in k}
        a, _ = manifest_args(tag=f"v{V}", assets_dir=write_assets(files))
        with self.assertRaises(SystemExit):
            release.cmd_manifest(a)

    def test_bad_sums_file_fails(self):
        d = write_assets()
        (d / "SHA256SUMS").write_text(f"{'0' * 64}  Helia-{V}-x86_64.AppImage\n")
        a, _ = manifest_args(tag=f"v{V}", assets_dir=d)
        with self.assertRaises(SystemExit):
            release.cmd_manifest(a)

    def test_sequence_is_raised_above_the_live_one_and_older_is_refused(self):
        orig = release.live_floor
        url = "https://x.invalid/{app}/{channel}/m.json"
        try:
            release.live_floor = lambda u: (4000000000, "0.7.0")
            a, out = manifest_args(tag=f"v{V}", assets_dir=write_assets(), floor_url=url)
            release.cmd_manifest(a)
            doc = json.loads((out / "helia" / "stable" / "manifest.json").read_text())
            self.assertEqual(doc["sequence"], 4000000001)
            release.live_floor = lambda u: (5, "0.9.0")
            a, _ = manifest_args(tag=f"v{V}", assets_dir=write_assets(), floor_url=url)
            with self.assertRaises(SystemExit):
                release.cmd_manifest(a)
            a.allow_older = True
            release.cmd_manifest(a)
        finally:
            release.live_floor = orig

    def test_release_source_uses_the_notes_block_and_sizes(self):
        class Fake:
            def call(self, method, path, **kw):
                body = release.body_with_sums("notes", {n: "ab" * 32 for n in FILES if n != "SHA256SUMS"})
                return {"body": body, "assets": [{"name": n, "size": 7} for n in FILES] + [{"name": "x.bin", "size": 1}]}
        facts = release.facts_from_release(Fake(), f"v{V}")
        self.assertEqual({f["name"] for f in facts}, set(FILES) - {"SHA256SUMS"})
        self.assertTrue(all(f["size"] == 7 and f["sha256"] == "ab" * 32 for f in facts))

    def test_pinned_keys_file(self):
        src = pathlib.Path(tempfile.mkdtemp()) / "pinned_keys.rs"
        key = "RWRU3O0qD7N8OGqGTd2ih2sihZojl1yd3AKzDHtOA20G7VbJ5jf86kMH"
        src.write_text('pub const KEYS_ARE_PLACEHOLDER: bool = true;\npub const PINNED_KEYS: &[&str] = &[\n    // note "x"\n    "%s",\n];\n' % key)
        self.assertEqual(release.pinned_keys(src), ([key], True))
        src.write_text(src.read_text().replace("= true", "= false"))
        self.assertEqual(release.pinned_keys(src), ([key], False))
        src.write_text(src.read_text().replace("&[\n", '&[\n    "%s",\n    "%s",\n' % (key, key)))
        with self.assertRaises(SystemExit):  # three keys
            release.pinned_keys(src)

    def test_placeholder_blocks_a_real_release(self):
        src = pathlib.Path(tempfile.mkdtemp()) / "pinned_keys.rs"
        src.write_text('pub const KEYS_ARE_PLACEHOLDER: bool = true;\npub const PINNED_KEYS: &[&str] = &["%s"];\n' % ("A" * 56))
        with self.assertRaises(SystemExit):
            release.cmd_pinned_keys(argparse_ns(file=str(src), require_real=True))

    def test_fetch_key_needs_https_and_writes_0600(self):
        import io
        from unittest import mock
        out = pathlib.Path(tempfile.mkdtemp()) / "k"
        a = argparse_ns(path="claude-automation/release-signing/helia", field="secret_key", kv_version=2,
                        out=str(out), pub_out="")
        with mock.patch.dict(os.environ, {"BAO_TOKEN": "t", "BAO_ADDR": "http://bao.invalid"}):
            with self.assertRaises(SystemExit):
                release.cmd_fetch_key(a)
        reply = json.dumps({"data": {"data": {"secret_key": "untrusted comment: minisign secret key\nABC\n"}}}).encode()

        class Resp(io.BytesIO):
            def __enter__(self):
                return self

            def __exit__(self, *x):
                return False
        seen = {}

        def fake(req, timeout=0):
            seen["url"] = req.full_url
            return Resp(reply)
        with mock.patch.dict(os.environ, {"BAO_TOKEN": "tok", "BAO_ADDR": "https://bao.invalid"}), \
                mock.patch.object(release.urllib.request, "urlopen", fake):
            release.cmd_fetch_key(a)
        self.assertEqual(seen["url"], "https://bao.invalid/v1/claude-automation/data/release-signing/helia")
        self.assertEqual(out.read_text().splitlines()[0], "untrusted comment: minisign secret key")
        if os.name == "posix":
            self.assertEqual(out.stat().st_mode & 0o777, 0o600)

    def test_fetch_key_logs_in_with_an_approle(self):
        import io
        from unittest import mock
        out = pathlib.Path(tempfile.mkdtemp()) / "k"
        a = argparse_ns(path="claude-automation/release-signing/helia", field="secret_key", kv_version=2,
                        out=str(out), pub_out="")
        calls = []

        class Resp(io.BytesIO):
            def __enter__(self):
                return self

            def __exit__(self, *x):
                return False

        def fake(req, timeout=0):
            calls.append((req.full_url, req.get_header("X-vault-token"), req.data))
            if req.full_url.endswith("/auth/approle/login"):
                return Resp(json.dumps({"auth": {"client_token": "short-lived"}}).encode())
            return Resp(json.dumps({"data": {"data": {"secret_key": "untrusted comment: k\nABC\n"}}}).encode())
        with mock.patch.dict(os.environ, {"BAO_TOKEN": "role:secret", "BAO_ADDR": "https://bao.invalid"}), \
                mock.patch.object(release.urllib.request, "urlopen", fake):
            release.cmd_fetch_key(a)
        self.assertEqual(json.loads(calls[0][2]), {"role_id": "role", "secret_id": "secret"})
        self.assertEqual(calls[1][1], "short-lived")
        self.assertTrue(out.is_file())

    def test_fetch_key_refuses_a_field_that_is_not_a_key(self):
        import io
        from unittest import mock
        out = pathlib.Path(tempfile.mkdtemp()) / "k"
        a = argparse_ns(path="m/p", field="secret_key", kv_version=2, out=str(out), pub_out="")

        class Resp(io.BytesIO):
            def __enter__(self):
                return self

            def __exit__(self, *x):
                return False
        with mock.patch.dict(os.environ, {"BAO_TOKEN": "tok", "BAO_ADDR": "https://bao.invalid"}), \
                mock.patch.object(release.urllib.request, "urlopen",
                                  lambda req, timeout=0: Resp(json.dumps({"data": {"data": {"secret_key": "nope"}}}).encode())):
            with self.assertRaises(SystemExit):
                release.cmd_fetch_key(a)
        self.assertFalse(out.exists())

    def test_waits_for_the_windows_installer(self):
        from unittest import mock
        orig_facts = release.facts_from_release
        n = {"calls": 0}
        full = [{"name": k, "sha256": "ab" * 32, "size": 3} for k in FILES if k != "SHA256SUMS"]
        without_win = [f for f in full if "windows-msvc-setup" not in f["name"]]

        def fake(forge, tag):
            n["calls"] += 1
            return without_win if n["calls"] < 3 else full
        try:
            release.facts_from_release = fake
            a, out = manifest_args(tag=f"v{V}", server="https://s.invalid", repo="o/r", wait=600, date="2026-10-24T18:00:00Z")
            with mock.patch.object(release, "Forge", lambda s, r: None), mock.patch.object(release.time, "sleep", lambda s: None):
                release.cmd_manifest(a)
        finally:
            release.facts_from_release = orig_facts
        self.assertEqual(n["calls"], 3)
        self.assertTrue((out / "helia" / "stable" / "manifest.json").is_file())

    def test_check_keys_refuses_placeholder_override_and_foreign_binaries(self):
        from unittest import mock
        d = pathlib.Path(tempfile.mkdtemp())
        real = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3"
        src = d / "pinned_keys.rs"

        def write(flag, key):
            src.write_text(f'pub const KEYS_ARE_PLACEHOLDER: bool = {flag};'+chr(10)+f'pub const PINNED_KEYS: &[&str] = &["{key}"];'+chr(10))
        ns = lambda **kw: argparse_ns(file=str(src), binary=kw.get("binary", []))  # noqa: E731
        write("true", real)
        with self.assertRaises(SystemExit):
            release.cmd_check_keys(ns())
        write("false", release.PLACEHOLDER_KEY)
        with self.assertRaises(SystemExit):
            release.cmd_check_keys(ns())
        write("false", real)
        release.cmd_check_keys(ns())
        with mock.patch.dict(os.environ, {"HELIA_UPDATE_PINNED_KEYS": "x"}):
            with self.assertRaises(SystemExit):
                release.cmd_check_keys(ns())
        good, other, ph = d / "good", d / "other", d / "ph"
        good.write_bytes(b"xx" + real.encode())
        other.write_bytes(b"xx")
        ph.write_bytes(real.encode() + release.PLACEHOLDER_KEY.encode())
        release.cmd_check_keys(ns(binary=[str(good)]))
        for bad in (other, ph):
            with self.assertRaises(SystemExit):
                release.cmd_check_keys(ns(binary=[str(bad)]))

    def test_verify_with_no_manifest_fails(self):
        with self.assertRaises(SystemExit):
            release.cmd_verify_manifests(argparse_ns(dir=tempfile.mkdtemp(), tool="/bin/true", key=["k"], tamper_check=""))

    @unittest.skipUnless(os.environ.get("HELIA_MANIFEST_BIN"), "set HELIA_MANIFEST_BIN to the helia-manifest binary")
    def test_matches_the_rust_tool_signs_and_verifies(self):
        import subprocess
        tool = os.environ["HELIA_MANIFEST_BIN"]

        def sh(*args):
            subprocess.run([tool, *args], check=True, capture_output=True)
        files = write_assets()
        a, out = manifest_args(tag=f"v{V}", assets_dir=files, date="2026-10-24T18:00:00Z", sequence=42,
                               notes_url="https://example.invalid/n", min_supported="0.7.0")
        release.cmd_manifest(a)
        stable = out / "helia" / "stable" / "manifest.json"
        specs = []
        for f in (x for x in FILES if release.classify(x, V)):
            plat, kind = release.classify(f, V)
            specs += ["--asset", f"{plat}:{kind}:{files / f}"]
        ref = out / "ref.json"
        sh("build", "--app", "helia", "--channel", "stable", "--version", V, "--sequence", "42",
           "--date", "2026-10-24T18:00:00Z", "--expires-days", "120", "--min-supported", "0.7.0",
           "--notes-url", "https://example.invalid/n", "--base-url", f"https://paid.example.org/dl/helia/{V}/",
           "--out", str(ref), *specs)
        theirs = json.loads(ref.read_text())
        theirs["assets"].sort(key=lambda x: (x["platform"], x["kind"]))  # mine are sorted, the tool keeps the given order
        self.assertEqual(json.loads(stable.read_bytes()), theirs)
        keys = pathlib.Path(tempfile.mkdtemp())
        sh("keygen-for-tests", "--out-dir", str(keys))
        for ch in ("stable", "prerelease"):
            sh("sign", "--manifest", str(out / "helia" / ch / "manifest.json"), "--secret-key-file", str(keys / "test.key"))
        gate = argparse_ns(dir=str(out), tool=tool, key=[str(keys / "test.pub")], tamper_check=tempfile.mkdtemp())
        release.cmd_verify_manifests(gate)  # verifies, and a changed byte is refused
        other = pathlib.Path(tempfile.mkdtemp())
        sh("keygen-for-tests", "--out-dir", str(other))
        with self.assertRaises(SystemExit):  # a key that is not pinned
            release.cmd_verify_manifests(argparse_ns(dir=str(out), tool=tool, key=[str(other / "test.pub")], tamper_check=""))
        (out / "helia" / "prerelease" / "manifest.json.minisig").unlink()
        with self.assertRaises(SystemExit):  # a missing signature
            release.cmd_verify_manifests(gate)


if __name__ == "__main__":
    unittest.main()
