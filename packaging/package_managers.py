#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
"""Prepare the package-manager manifests (winget, Flathub, Homebrew) from a GitHub release.

    python3 packaging/package_managers.py                    # latest release, writes under packaging/
    python3 packaging/package_managers.py --release-json r.json --sums SHA256SUMS

Nothing is submitted anywhere: this only writes files. Submitting each one is an outward
action that needs the owner's yes; packaging/SUBMITTING.md says how. Standard library only.

    winget    packaging/winget/manifests/l/LaurenceHorrocksBarlow/Helia/<version>/   (three files)
    flatpak   the `sources` block of packaging/flatpak/casa.discworld.helia.yml
    homebrew  packaging/homebrew/helia.rb, only when the release has a .dmg
"""
import argparse
import json
import os
import re
import sys
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = "qisback/helia"
WINGET_ID = "LaurenceHorrocksBarlow.Helia"
HOME = "https://12hobbies.studio/projects/helia/"
SHORT = "Sharp pictures of the Sun from a video."
LONG = ("Helia turns a video of the Sun into one sharp picture. It finds the clearest moments in thousands of "
        "blurry frames, lines them up and combines them, then sharpens and colours the result.")


def fetch(url):
    req = urllib.request.Request(url, headers={"User-Agent": "helia-packaging/1", "Accept": "application/vnd.github+json"})
    if os.environ.get("GITHUB_TOKEN") and "api.github.com" in url:
        req.add_header("Authorization", "Bearer " + os.environ["GITHUB_TOKEN"])
    with urllib.request.urlopen(req, timeout=30) as r:
        return r.read()


def parse_sums(text):
    out = {}
    for line in text.splitlines():
        m = re.match(r"^([0-9a-f]{64})\s+\*?(\S.*)$", line)
        if m:
            out[m.group(2)] = m.group(1)
    return out


def find(names, pattern):
    hits = [n for n in names if re.search(pattern, n)]
    return hits[0] if hits else None


def winget(version, base, sums, names):
    exe = find(names, r"-x86_64-pc-windows-msvc-setup\.exe$")
    if not exe or exe not in sums:
        return None
    d = f"""# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
# yaml-language-server: $schema=https://aka.ms/winget-manifest.{{kind}}.1.6.0.schema.json
"""
    ident = f"PackageIdentifier: {WINGET_ID}\nPackageVersion: {version}\n"
    files = {}
    files[f"{WINGET_ID}.yaml"] = (d.format(kind="version") + ident +
                                  "DefaultLocale: en-GB\nManifestType: version\nManifestVersion: 1.6.0\n")
    files[f"{WINGET_ID}.installer.yaml"] = (d.format(kind="installer") + ident + f"""InstallerLocale: en-GB
InstallerType: nsis
Scope: user
InstallModes:
- interactive
- silent
- silentWithProgress
InstallerSwitches:
  Silent: /S
  SilentWithProgress: /S
UpgradeBehavior: install
ReleaseDate: {{date}}
Installers:
- Architecture: x64
  InstallerUrl: {base}/{exe}
  InstallerSha256: {sums[exe].upper()}
ManifestType: installer
ManifestVersion: 1.6.0
""")
    files[f"{WINGET_ID}.locale.en-GB.yaml"] = (d.format(kind="defaultLocale") + ident + f"""PackageLocale: en-GB
Publisher: Laurence Horrocks-Barlow
PublisherUrl: https://12hobbies.studio
PackageName: Helia
PackageUrl: {HOME}
License: GPL-3.0-or-later
LicenseUrl: https://github.com/{REPO}/blob/main/LICENSE
Copyright: Copyright (C) 2026 Laurence Horrocks-Barlow
ShortDescription: {SHORT}
Description: {LONG}
Moniker: helia
Tags:
- astronomy
- astrophotography
- image-processing
- solar
ReleaseNotesUrl: https://github.com/{REPO}/releases/tag/v{version}
ManifestType: defaultLocale
ManifestVersion: 1.6.0
""")
    return files


def flatpak_sources(version, base, sums, names):
    out = ["    sources:"]
    for arch, pat in (("x86_64", r"-x86_64-unknown-linux-gnu\.tar\.gz$"), ("aarch64", r"-aarch64-unknown-linux-gnu\.tar\.gz$")):
        n = find(names, pat)
        if not n or n not in sums:
            continue
        out += ["      - type: archive", f"        only-arches: [{arch}]", f"        url: {base}/{n}", f'        sha256: "{sums[n]}"']
    return "\n".join(out) + "\n"


def patch_flatpak(path, sources):
    text = path.read_text(encoding="utf8")
    new = re.sub(r"(?s)(\n    # BEGIN sources[^\n]*\n).*?(    # END sources)", lambda m: m.group(1) + sources + m.group(2), text)
    if new == text and "# BEGIN sources" not in text:
        raise SystemExit(f"{path} has no BEGIN/END sources markers")
    path.write_text(new, encoding="utf8", newline="\n")


def homebrew(version, base, sums, names):
    dmg = find(names, r"\.dmg$")
    if not dmg or dmg not in sums:
        return None
    name = dmg.replace(version, "#" + "{version}")
    url = f"https://github.com/{REPO}/releases/download/v#" + "{version}/" + name
    return f"""# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
cask "helia" do
  version "{version}"
  sha256 "{sums[dmg]}"

  url "{url}"
  name "Helia"
  desc "{SHORT}"
  homepage "{HOME}"

  depends_on arch: :arm64

  app "Helia.app"

  zap trash: [
    "~/Library/Application Support/Helia",
    "~/Library/Preferences/studio.12hobbies.helia.plist",
  ]
end
"""


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--release-json", help="a saved GitHub release (otherwise the latest is fetched)")
    ap.add_argument("--sums", help="a saved SHA256SUMS (otherwise the release's own is fetched)")
    ap.add_argument("--out", default=str(HERE), help="the packaging directory to write under")
    a = ap.parse_args(argv)
    rel = json.loads(Path(a.release_json).read_text("utf8")) if a.release_json else json.loads(fetch(f"https://api.github.com/repos/{REPO}/releases/latest"))
    version = rel["tag_name"].lstrip("v")
    names = [x["name"] for x in rel["assets"]]
    base = f"https://github.com/{REPO}/releases/download/v{version}"
    if a.sums:
        sums = parse_sums(Path(a.sums).read_text("utf8"))
    else:
        sums = parse_sums(fetch(f"{base}/SHA256SUMS").decode())
    out = Path(a.out)
    date = (rel.get("published_at") or "")[:10]

    w = winget(version, base, sums, names)
    if w:
        d = out / "winget" / "manifests" / "l" / "LaurenceHorrocksBarlow" / "Helia" / version
        d.mkdir(parents=True, exist_ok=True)
        for n, t in w.items():
            (d / n).write_text(t.replace("{date}", date), encoding="utf8", newline="\n")
        print(f"winget: wrote {d}")
    else:
        print("winget: no Windows installer in this release; nothing written")
    src = flatpak_sources(version, base, sums, names)
    patch_flatpak(out / "flatpak" / "casa.discworld.helia.yml", src)
    print("flatpak: sources updated")
    cask = homebrew(version, base, sums, names)
    if cask:
        (out / "homebrew").mkdir(exist_ok=True)
        (out / "homebrew" / "helia.rb").write_text(cask, encoding="utf8", newline="\n")
        print("homebrew: wrote helia.rb")
    else:
        print("homebrew: this release has no .dmg; the cask waits for a macOS build")
    return 0


if __name__ == "__main__":
    sys.exit(main())
