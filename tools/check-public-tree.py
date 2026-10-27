#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
"""Fail if the public tree names anything that belongs to the closed store add-on.

    python3 tools/check-public-tree.py [--root DIR]

Public Helia keeps the signed update check, verification and apply, and a small generic backend
hook. The store sign-in, licence keys, entitlement and paid-download code live in a separate
private crate that only the official builds link in. This guard keeps the public tree from
drifting back: it scans every tracked file for the names of the closed crate, its endpoints and
its vocabulary. The forge-only workflows (`.forgejo/`), the changelog (history, and the public
copy has Summary bullets only) and this file are exempt. A user-facing "buy or subscribe at
12hobbies.studio" line does not match any rule.
"""
import argparse
import re
import subprocess
import sys
from pathlib import Path

RULES = [
    (r"helia[-_]store", "the name of the closed store crate"),
    (r"/store/(?:dl|api)\b", "a store endpoint"),
    (r"api/v\d+/entitlement|entitlement_url|Entitlement::", "the store entitlement endpoint or code"),
    (r"\bstore-token\b", "the store token file"),
    (r"UPDATE_PORTAL_URL", "the store portal build setting"),
    (r"\blicen[cs]e[- ]keys?\b", "licence keys"),
    (r"(?<!secret )\bstore[- ]token\b", "the store token"),
    (r"\bpresigned\b", "presigned download addresses"),
]
EXEMPT_PREFIXES = (".forgejo/",)
EXEMPT_FILES = {"CHANGELOG.md", "tools/check-public-tree.py"}
SKIP_SUFFIXES = (".png", ".jpg", ".jpeg", ".gif", ".ico", ".icns", ".ttf", ".otf", ".woff", ".woff2", ".zip",
                 ".gz", ".xz", ".ser", ".fits", ".fit", ".tif", ".tiff", ".svg", ".lock", ".pdf")


def tracked(root: Path):
    out = subprocess.run(["git", "-C", str(root), "ls-files", "-z"], check=True, capture_output=True).stdout
    return [p for p in out.decode("utf-8", "replace").split("\0") if p]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--root", default=".")
    root = Path(ap.parse_args().root).resolve()
    rules = [(re.compile(rx, re.IGNORECASE), why) for rx, why in RULES]
    hits = []
    for rel in tracked(root):
        if rel in EXEMPT_FILES or rel.startswith(EXEMPT_PREFIXES) or rel.lower().endswith(SKIP_SUFFIXES):
            continue
        path = root / rel
        if not path.is_file():
            continue
        try:
            text = path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            continue
        for n, line in enumerate(text.splitlines(), 1):
            for rx, why in rules:
                if rx.search(line):
                    hits.append(f"{rel}:{n}: {why}: {line.strip()[:100]}")
    if hits:
        print("The public tree names the closed store add-on:", file=sys.stderr)
        print("\n".join(hits), file=sys.stderr)
        print(
            "\nWhat: these lines belong to the store add-on, which is not part of public Helia.\n"
            "Why: the store client, licence keys and paid downloads are closed source and are linked in only by "
            "official builds.\n"
            "What to do: move the text or code to the store add-on, or reword it to a plain "
            "\"buy or subscribe at 12hobbies.studio\".",
            file=sys.stderr,
        )
        return 1
    print(f"public tree check: no store add-on names in {len(tracked(root))} tracked files")
    return 0


if __name__ == "__main__":
    sys.exit(main())
