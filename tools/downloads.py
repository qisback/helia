#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
"""Turn a GitHub release into a plain "which file do I need" download manifest.

    python tools/downloads.py helia                       # fetch the latest release, print the manifest
    python tools/downloads.py helia --out downloads.json  # write it
    python tools/downloads.py helia --release-json r.json # offline, from a saved release
    python tools/downloads.py helia --docs-page PAGE.md   # also write the docs-site download page

The manifest drives the download widget (download-widget.js): a split button that picks the
visitor's system, with a menu of installers and portable versions. It is used on the
12hobbies.studio product page and on the app's documentation site. Both sites carry the same
copy of this file and of the widget; keep them identical (12hobbies-studio tools/ is where
changes are made first, then copied to this repository's tools/).

Nothing is invented: an option exists only when a matching file is in the release. Standard
library only. The GitHub API is read at build or sync time, never by the visitor's browser.
"""
import argparse
import html
import json
import os
import re
import sys
import urllib.error
import urllib.request
from datetime import datetime

PRODUCTS = {
    "helia": {
        "name": "Helia",
        "repo": "qisback/helia",
        "docs": "https://qisback.github.io/helia/",
        "page": "https://12hobbies.studio/projects/helia/#download",
        # Set to "https://downloads.12hobbies.studio" once that site serves <app>/<version>/<file>;
        # until then the files are linked from the GitHub release.
        "files_base": None,
    },
}
UA = "12hobbies-downloads/1.0"
SCHEMA = 2

ARCH_WORDS = {
    "x64": "64-bit Intel or AMD",
    "arm64": "ARM64 (for example Raspberry Pi 4 and 5)",
}
# kind -> (installer or portable, menu title, one-line description, preference rank within the system)
KINDS = {
    "windows-setup": ("installer", "Installer (.exe)", "Installs Helia like any other program. The best choice for most people.", 0),
    "windows-zip": ("portable", "Portable (.zip)", "Portable: runs without installing, good for USB sticks.", 1),
    "deb": ("installer", "Ubuntu, Debian and Raspberry Pi OS (.deb)", "Installs from your software centre.", 1),
    "rpm": ("installer", "Fedora, Red Hat and openSUSE (.rpm)", "Installs from your software centre.", 2),
    "appimage": ("portable", "AppImage", "Portable: one file that runs without installing. Works on most Linux systems.", 0),
    "tar": ("portable", "Archive (.tar.gz)", "Portable: unpack it and run it. For people who like doing it by hand.", 3),
    "dmg": ("installer", "Installer (.dmg)", "Installs Helia like any other Mac app.", 0),
}
KIND_ORDER = ["windows-setup", "windows-zip", "appimage", "deb", "rpm", "tar", "dmg"]
BUTTON_WORDS = {
    "windows-setup": "Windows (installer)",
    "windows-zip": "Windows (portable)",
    "appimage": "Linux (portable AppImage)",
    "deb": "Ubuntu or Debian (installer)",
    "rpm": "Fedora or openSUSE (installer)",
    "tar": "Linux (portable archive)",
    "dmg": "Mac (installer)",
}
OS_TITLES = {"windows": "Windows", "linux": "Linux", "macos": "Mac"}
# Shown greyed out in the menu while a system has no build.
COMING_SOON = {"macos": ("Installer (.dmg)", "Installs Helia like any other Mac app. Coming soon.")}

# (regex on the file name, kind, os, arch)
RULES = [
    (r"-x86_64-pc-windows-msvc-setup\.exe$", "windows-setup", "windows", "x64"),
    (r"-x86_64-pc-windows-msvc\.zip$", "windows-zip", "windows", "x64"),
    (r"\.AppImage$", "appimage", "linux", None),
    (r"\.deb$", "deb", "linux", None),
    (r"\.rpm$", "rpm", "linux", None),
    (r"-unknown-linux-gnu\.tar\.gz$", "tar", "linux", None),
    (r"\.dmg$", "dmg", "macos", "arm64"),
]
ADVANCED = [
    (r"^SHA256SUMS$", "Checksums for every file (SHA256SUMS)"),
    (r"\.sha256$", None),
    (r"-cli\.cdx\.json$", "Software bill of materials, command line tool"),
    (r"-gui\.cdx\.json$", "Software bill of materials, desktop app"),
    (r"\.cdx\.json$", "Software bill of materials"),
    (r"\.(sig|asc|minisig)$", None),
]

# Three steps for each kind. {p} is the product name.
STEPS = {
    "windows-setup": [
        "Download the installer. It is the file ending in setup.exe.",
        "Open it. If Windows says \"Windows protected your PC\", choose More info, then Run anyway. "
        "That happens because {p} is new and the installer is not signed yet.",
        "Follow the steps on screen, then open {p} from the Start menu.",
    ],
    "windows-zip": [
        "Download the zip file.",
        "Right-click it and choose Extract All.",
        "Open the new folder and double-click helia-gui to start {p}. Nothing is installed.",
    ],
    "appimage": [
        "Download the file. It ends in .AppImage.",
        "Right-click it, choose Properties, then Permissions, and tick the box that lets it run as a program. "
        "(In a terminal you can type chmod +x followed by the file name.)",
        "Double-click it to start {p}. There is nothing to install.",
    ],
    "deb": [
        "Download the .deb file.",
        "Double-click it and choose Install in your software centre.",
        "Open {p} from your applications menu.",
    ],
    "rpm": [
        "Download the .rpm file.",
        "Double-click it and choose Install in your software centre.",
        "Open {p} from your applications menu.",
    ],
    "tar": [
        "Download the .tar.gz file.",
        "Right-click it and choose Extract Here.",
        "Open the new folder and double-click helia-gui to start {p}. Nothing is installed.",
    ],
    "dmg": [
        "Download the .dmg file.",
        "Open it and drag {p} into your Applications folder.",
        "The first time, right-click {p} and choose Open, because it is not signed yet.",
    ],
}
NOTES = {"dmg": "For Macs with an Apple chip (M1 or newer)."}
NO_BUILD = {
    "windows": "The Windows version of this release is still being built. Please check again in a little while.",
    "linux": "There is no Linux version of this release yet.",
}


def human_size(n):
    return f"{n / 1_000_000:.1f} MB" if n >= 1_000_000 else f"{max(1, round(n / 1000))} kB"


def human_date(iso):
    d = datetime.strptime(iso[:10], "%Y-%m-%d")
    return f"{d.day} {d:%B %Y}"


def arch_of(name, hint):
    if hint:
        return hint
    low = name.lower()
    if re.search(r"aarch64|arm64", low):
        return "arm64"
    if re.search(r"x86_64|amd64|x64", low):
        return "x64"
    return None


def classify(name):
    """(kind, os, arch) for a release file, or (None, None, None) when it is not a program."""
    for rx, kind, os_, arch in RULES:
        if re.search(rx, name):
            return kind, os_, arch_of(name, arch)
    return None, None, None


def advanced_label(name):
    for rx, label in ADVANCED:
        if re.search(rx, name):
            return label or f"Checksum or signature: {name}"
    return None


def is_per_file_check(name):
    """A .sha256 or signature beside one file; SHA256SUMS already covers it."""
    return any(re.search(rx, name) for rx, label in ADVANCED if label is None)


def fetch_latest(repo):
    headers = {"User-Agent": UA, "Accept": "application/vnd.github+json"}
    if os.environ.get("GITHUB_TOKEN"):      # set by GitHub Actions: lifts the anonymous rate limit
        headers["Authorization"] = "Bearer " + os.environ["GITHUB_TOKEN"]
    req = urllib.request.Request(f"https://api.github.com/repos/{repo}/releases/latest", headers=headers)
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.loads(r.read())


def asset_url(cfg, ver, name, github_url):
    """Where a file is served from: the downloads site when the product has one, else GitHub."""
    base = cfg.get("files_base")
    return f"{base.rstrip('/')}/{cfg['app']}/{ver}/{name}" if base else github_url


def build(product, rel, page=None):
    """Manifest dict (schema 2) for a release of a PRODUCTS entry."""
    cfg = dict(PRODUCTS[product], app=product)
    name, repo = cfg["name"], cfg["repo"]
    base = f"https://github.com/{repo}/releases/"
    ver = rel["tag_name"].lstrip("v")
    if not rel["html_url"].startswith(base):
        raise ValueError(f"release URL is not on {base}: {rel['html_url']}")
    files = []
    for a in rel.get("assets", []):
        url = a.get("browser_download_url", "")
        if not url.startswith(base + "download/"):
            continue
        kind, os_, arch = classify(a["name"])
        files.append({"file": a["name"], "url": asset_url(cfg, ver, a["name"], url), "size": human_size(a["size"]),
                      "kind": kind, "os": os_, "arch": arch, "adv": advanced_label(a["name"])})

    options = []
    for kind in KIND_ORDER:
        for f in sorted((f for f in files if f["kind"] == kind), key=lambda f: f["arch"] != "x64"):
            typ, title, desc, rank = KINDS[kind]
            o = {"id": f"{f['os']}-{f['arch']}-{kind}", "os": f["os"], "arch": f["arch"], "kind": kind, "type": typ,
                 "rank": rank, "title": title, "desc": desc,
                 "arch_label": ARCH_WORDS[f["arch"]] if f["os"] == "linux" else "",
                 "button": f"Download {name} {ver} for {BUTTON_WORDS[kind]}",
                 "file": f["file"], "url": f["url"], "size": f["size"],
                 "steps": [s.format(p=name) for s in STEPS[kind]]}
            if kind in NOTES:
                o["note"] = NOTES[kind]
            options.append(o)
    have_os = {o["os"] for o in options}
    coming = [{"os": o, "os_title": OS_TITLES[o], "title": t, "desc": d}
              for o, (t, d) in COMING_SOON.items() if o not in have_os]
    no_build = {o: t for o, t in NO_BUILD.items() if o not in have_os}
    elsewhere = [OS_TITLES[x] for x in ("windows", "linux") if x in have_os]
    for c in coming:
        text = f"{name} is not available for the Mac yet."
        if elsewhere:
            text += " You can use the " + " or ".join(elsewhere) + " version on another computer."
        no_build[c["os"]] = text

    have_sums = any(f["file"] == "SHA256SUMS" for f in files)
    advanced = [{"label": f["adv"], "file": f["file"], "url": f["url"]} for f in files
                if f["adv"] and f["kind"] is None and not (have_sums and is_per_file_check(f["file"]))]
    advanced.sort(key=lambda a: (not a["file"] == "SHA256SUMS", a["label"], a["file"]))
    advanced.append({"label": "All files for this version on GitHub", "file": "", "url": rel["html_url"]})

    m = {
        "schema": SCHEMA, "product": name, "version": ver,
        "date": human_date(rel.get("published_at") or rel["created_at"]),
        "release_url": rel["html_url"], "docs_url": cfg["docs"], "page_url": page or cfg["page"],
        "os_titles": OS_TITLES, "options": options, "coming_soon": coming, "no_build": no_build, "advanced": advanced,
    }
    m["fallback"] = fallback_html(m)   # the plain list for pages without scripts
    return m


def fallback_html(m):
    """The plain grouped list shown without scripts (and inside the widget until it starts)."""
    out = [f"<p>{html.escape(m['product'])} {html.escape(m['version'])}, released on {html.escape(m['date'])}. "
           "Choose the one for your computer.</p>"]
    for os_ in ("windows", "linux", "macos"):
        opts = [o for o in m["options"] if o["os"] == os_]
        soon = [c for c in m["coming_soon"] if c["os"] == os_]
        if not opts and not soon:
            continue
        out.append(f"<h4>{html.escape(m['os_titles'][os_])}</h4><ul>")
        for o in opts:
            extra = f", {html.escape(o['arch_label'])}" if o["arch_label"] else ""
            out.append(f"<li><a href=\"{html.escape(o['url'], quote=True)}\">{html.escape(o['title'])}{extra}</a> "
                       f"({html.escape(o['size'])}). {html.escape(o['desc'])}</li>")
        for c in soon:
            out.append(f"<li>{html.escape(c['title'])}: coming soon.</li>")
        out.append("</ul>")
    out.append("<p>For advanced users: " + ", ".join(
        f"<a href=\"{html.escape(a['url'], quote=True)}\">{html.escape(a['label'])}</a>" for a in m["advanced"]) + ".</p>")
    return "\n".join(out)


def docs_page(m, src="../../downloads.json"):
    """Markdown for the docs-site page: the widget (reads src) with the plain list as its fallback."""
    name = m["product"]
    return (f"---\ndescription: \"Download {name} for Windows, Linux or Mac: one button picks the right file for your computer.\"\n---\n\n"
            f"# Download {name}\n\n"
            "<!-- Generated by tools/downloads.py at docs build time. Do not edit. -->\n"
            f"<div class=\"dl\" data-dl-src=\"{src}\">\n{fallback_html(m)}\n</div>\n\n"
            "The page works out your system from your browser; nothing is sent anywhere. "
            "Once it is installed, [First launch](first-launch.md) shows what you will see, and "
            "[Download and verify](download-and-verify.md) explains how to check a file against its checksum.\n")


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("product", choices=sorted(PRODUCTS))
    ap.add_argument("--out", help="write the manifest here instead of printing it")
    ap.add_argument("--docs-page", help="also write the docs-site download page (markdown) here")
    ap.add_argument("--release-json", help="read a saved release (one GitHub release object) instead of the API")
    ap.add_argument("--files-base", help="serve the files from BASE/<app>/<version>/<file> (the downloads site)")
    a = ap.parse_args(argv)
    if a.files_base:
        PRODUCTS[a.product]["files_base"] = a.files_base
    try:
        if a.release_json:
            with open(a.release_json, encoding="utf8") as f:
                rel = json.load(f)
        else:
            rel = fetch_latest(PRODUCTS[a.product]["repo"])
    except (urllib.error.URLError, OSError, ValueError) as e:
        print(f"cannot read the release: {e}", file=sys.stderr)
        return 3
    m = build(a.product, rel)
    text = json.dumps(m, indent=2, ensure_ascii=False) + "\n"
    if a.out:
        with open(a.out, "w", encoding="utf8", newline="\n") as f:
            f.write(text)
        print(f"wrote {a.out}")
    else:
        sys.stdout.write(text)
    if a.docs_page:
        with open(a.docs_page, "w", encoding="utf8", newline="\n") as f:
            f.write(docs_page(m))
        print(f"wrote {a.docs_page}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
