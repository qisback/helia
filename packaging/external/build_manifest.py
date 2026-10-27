#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
"""Build the external-items manifest (schema 1) from packaging/external/<app>-items.json.

The items file lists licences by file; this script inlines each licence text and adds the
schema, app, date, sequence and expiry. Sign the output with `helia-manifest sign` (the
same key as the update manifests); the app verifies the exact bytes written here.

    build_manifest.py --items packaging/external/helia-items.json --sequence 12 --out external.json
"""
import argparse
import datetime as dt
import json
import pathlib
import sys

SCHEMA = 1


def utc(t: dt.datetime) -> str:
    return t.astimezone(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def build(items_path: pathlib.Path, sequence: int, date: dt.datetime, expires_days: int) -> dict:
    src = json.loads(items_path.read_text(encoding="utf-8"))
    base = items_path.parent
    licences = []
    for lic in src["licences"]:
        entry = {k: v for k, v in lic.items() if k not in ("file", "spdx") or v is not None}
        entry["text"] = (base / entry.pop("file")).read_text(encoding="utf-8")
        licences.append(entry)
    return {
        "schema": SCHEMA,
        "app": src["app"],
        "date": utc(date),
        "sequence": sequence,
        "expires": utc(date + dt.timedelta(days=expires_days)),
        "licences": licences,
        "items": src["items"],
    }


def main(argv: list[str]) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--items", required=True, type=pathlib.Path)
    p.add_argument("--sequence", required=True, type=int)
    p.add_argument("--date", help="YYYY-MM-DDTHH:MM:SSZ; default now")
    p.add_argument("--expires-days", type=int, default=90)
    p.add_argument("--out", required=True, type=pathlib.Path)
    a = p.parse_args(argv)
    date = (
        dt.datetime.strptime(a.date, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=dt.timezone.utc)
        if a.date
        else dt.datetime.now(dt.timezone.utc)
    )
    doc = build(a.items, a.sequence, date, a.expires_days)
    a.out.write_text(json.dumps(doc, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"wrote {a.out} ({a.out.stat().st_size} bytes, sequence {a.sequence})")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
