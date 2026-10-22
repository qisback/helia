#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

"""Release helpers for the Forgejo workflows (stdlib only, Python 3.10+).

  version   check that a tag matches the workspace version, print the version
  notes     build the release notes from the CHANGELOG section for a version
  sbom      set the author and supplier in CycloneDX SBOMs
  probe     check the token can write releases (draft create and delete)
  publish   create the Forgejo release (or reuse it) and upload the assets
  attach    add files to an existing release and merge them into its SHA256SUMS

Nothing here prints the token. The token comes from FORGEJO_TOKEN.
"""
import argparse
import datetime
import hashlib
import json
import os
import pathlib
import re
import sys
import urllib.error
import urllib.parse
import urllib.request

TAG_RE = re.compile(r"^v(\d+)\.(\d+)\.(\d+)(-[0-9A-Za-z.]+)?$")


def die(msg):
    print(f"release: {msg}", file=sys.stderr)
    sys.exit(1)


def workspace_version(cargo_toml):
    text = pathlib.Path(cargo_toml).read_text(encoding="utf-8")
    block = text.split("[workspace.package]", 1)[-1]
    m = re.search(r'^version\s*=\s*"([^"]+)"', block, re.M)
    if not m:
        die("no version in [workspace.package]")
    return m.group(1)


def is_prerelease(version):
    # A pre-release suffix (-rc.1, -beta.2) or any 0.x version.
    return "-" in version or version.startswith("0.")


def cmd_version(a):
    cargo = workspace_version(a.cargo_toml)
    if a.tag:
        m = TAG_RE.match(a.tag)
        if not m:
            die(f"tag {a.tag!r} is not vMAJOR.MINOR.PATCH[-suffix]")
        if a.tag[1:] != cargo:
            die(f"tag {a.tag} does not match the workspace version {cargo}")
    print(cargo)


def changelog_section(path, version):
    return changelog_entry(path, version)[0]


def changelog_entry(path, version):
    """The CHANGELOG body and the date (or None) of the section for a version."""
    lines = pathlib.Path(path).read_text(encoding="utf-8").splitlines()
    head = re.compile(r"^## \[" + re.escape(version) + r"\](?:\s+-\s+(\d{4}-\d{2}-\d{2}))?")
    start = next((i for i, l in enumerate(lines) if head.match(l)), None)
    if start is None:
        die(f"CHANGELOG.md has no '## [{version}]' section")
    date = head.match(lines[start]).group(1)
    end = next((i for i in range(start + 1, len(lines)) if lines[i].startswith("## [")), len(lines))
    body = "\n".join(lines[start + 1:end]).strip()
    if not body:
        die(f"the CHANGELOG.md section for {version} is empty")
    return body, date


AUTHOR = "Laurence Horrocks-Barlow"
COPYRIGHT = f"Copyright (C) 2026 {AUTHOR}. Helia is free software under the GPL-3.0-or-later."


def cmd_sbom(a):
    """Name the author and supplier in CycloneDX SBOMs (cargo-cyclonedx cannot)."""
    for f in a.files:
        path = pathlib.Path(f)
        doc = json.loads(path.read_text(encoding="utf-8"))
        meta = doc.setdefault("metadata", {})
        meta["authors"] = [{"name": AUTHOR}]
        meta["supplier"] = {"name": AUTHOR}
        comp = meta.get("component")
        if isinstance(comp, dict):
            comp["author"] = AUTHOR
            comp["supplier"] = {"name": AUTHOR}
            comp["copyright"] = COPYRIGHT.split(". ")[0]
        path.write_text(json.dumps(doc, indent=2) + "\n", encoding="utf-8")
        print(f"set author and supplier in {f}")


SECTION_ORDER = ["Added", "Changed", "Fixed", "Security"]
MAX_LINE = 160
LINK_RE = re.compile(r"\[([^\]]*)\]\(([^)]*)\)")
ISSUE_RE = re.compile(r"\[#(\d+)\]\([^)]*\)")
SENTENCE_END = re.compile(r"[a-z)`#0-9][.!?](?=\s+[A-Z`(])")


def one_line(bullet):
    """First sentence of a CHANGELOG bullet: the what, not the how."""
    text = " ".join(bullet.split())
    text = ISSUE_RE.sub(r"#\1", text)
    text = LINK_RE.sub(r"\1", text)
    m = re.match(r"\*\*(.+?)\*\*[:.]?\s*(\([^)]*\))?", text)
    if m:
        # A bold lead-in is the summary; keep a parenthesis that follows it directly.
        text = m.group(1).rstrip(".:") + (" " + m.group(2) if m.group(2) else "")
    else:
        m = SENTENCE_END.search(text)
        if m:
            text = text[:m.end()]
        # Prose after a colon or semicolon outside brackets is the how, not the what.
        depth = 0
        for i, ch in enumerate(text):
            depth += ch == "(" or -(ch == ")")
            if depth <= 0 and ch in ":;" and text[i + 1:i + 2] == " " and i >= 25:
                text = text[:i]
                break
    text = text.replace("**", "").rstrip(".").strip()
    if len(text) > MAX_LINE:
        cut = text[:MAX_LINE]
        k = max(cut.rfind(", "), cut.rfind(" ("), cut.rfind("; "))
        text = cut[:k] if k > 60 else cut[:cut.rfind(" ")]
    text = text.rstrip(",;: ")
    if text.count("(") > text.count(")"):
        text = text[:text.rfind("(")].rstrip(",;: ")
    return text


def terse_changes(body):
    """Group the CHANGELOG bullets by heading, one line each: Added, Changed, Fixed, Security first."""
    groups = {}
    cur = None
    for line in body.splitlines():
        m = re.match(r"^###\s+(.+?)\s*$", line)
        if m:
            cur = m.group(1)
            groups.setdefault(cur, [])
        elif re.match(r"^[-*]\s+", line):
            groups.setdefault(cur or "Changed", []).append(re.sub(r"^[-*]\s+", "", line))
        elif cur is not None and line.strip() and groups[cur] and line[0] in " \t":
            groups[cur][-1] += " " + line.strip()
    names = [n for n in SECTION_ORDER if groups.get(n)] + [n for n in groups if n not in SECTION_ORDER and groups[n]]
    out = []
    for n in names:
        out += [f"### {n}", ""] + [f"- {one_line(b)}" for b in groups[n]] + [""]
    return "\n".join(out).strip()


PLATFORMS = [
    ("Linux x86_64", "tar.gz, .deb, .rpm, AppImage"),
    ("Linux aarch64", "tar.gz, .deb, .rpm, AppImage"),
    ("Windows x86_64", "installer (setup.exe), zip"),
]
MACOS_ROW = ("macOS (Apple silicon)", "dmg")
NO_MACOS = "No macOS build for this release."


def build_notes(version, tag, date, body, server, repo, macos=False):
    base = f"{server}/{repo}"
    pre = " (pre-release)" if is_prerelease(version) else ""
    out = [
        f'<img src="{base}/raw/tag/{tag}/assets/branding/helia-logo.png" alt="Helia" width="96">',
        "",
        f"# Helia {version}{pre}",
        "",
        f"Released {date}.",
        "",
        "## What changed",
        "",
        terse_changes(body),
        "",
        "## Downloads",
        "",
        "| Platform | Files |",
        "| --- | --- |",
        *[f"| {p} | {f} |" for p, f in PLATFORMS + ([MACOS_ROW] if macos else [])],
        "",
        *([] if macos else [NO_MACOS, ""]),
        "Linux builds need glibc 2.35 or newer. Verify with `sha256sum -c SHA256SUMS --ignore-missing`. "
        "`helia-*.cdx.json` are the CycloneDX SBOMs.",
        "",
        f"Source: [{tag}.tar.gz]({base}/archive/{tag}.tar.gz), [zip]({base}/archive/{tag}.zip).",
        "",
        "Licence: GPL-3.0-or-later with additional terms under section 7 (see `ADDITIONAL-TERMS.md`).",
        "",
    ]
    return "\n".join(out).replace("\n\n\n", "\n\n")


def cmd_notes(a):
    body, date = changelog_entry(a.changelog, a.version)
    date = date or datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%d")
    pathlib.Path(a.out).write_text(build_notes(a.version, a.tag, date, body, a.server, a.repo, a.macos), encoding="utf-8")
    print(f"wrote {a.out}")


SUMS_BEGIN = "<!-- sha256sums:begin -->"
SUMS_END = "<!-- sha256sums:end -->"
SUM_LINE = re.compile(r"^([0-9a-f]{64})\s+\*?(\S.*)$")


def parse_sums_text(text):
    out = {}
    for line in text.splitlines():
        m = SUM_LINE.match(line.strip())
        if m:
            out[m.group(2)] = m.group(1)
    return out


def sums_in_body(body):
    """The SHA256SUMS lines kept in the release notes (the CI token cannot download assets)."""
    if SUMS_BEGIN in body and SUMS_END in body:
        return parse_sums_text(body.split(SUMS_BEGIN, 1)[1].split(SUMS_END, 1)[0])
    return {}


def body_with_sums(body, sums):
    nl = chr(10)
    block = SUMS_BEGIN + nl + "```text" + nl + "".join(f"{sums[n]}  {n}" + nl for n in sorted(sums)) + "```" + nl + SUMS_END
    if SUMS_BEGIN in body and SUMS_END in body:
        head = body.split(SUMS_BEGIN, 1)[0]
        tail = body.split(SUMS_END, 1)[1]
        return head + block + tail
    return body.rstrip() + nl + nl + "## SHA256SUMS" + nl + nl + block + nl


class Forge:
    def __init__(self, server, repo):
        self.base = f"{server}/api/v1/repos/{repo}"
        self.token = os.environ.get("FORGEJO_TOKEN") or die("FORGEJO_TOKEN is not set")

    def call(self, method, path, body=None, data=None, headers=None, ok404=False):
        h = {"Authorization": "token " + self.token, "User-Agent": "helia-release/1"}
        if headers:
            h.update(headers)
        if body is not None:
            data = json.dumps(body).encode()
            h["Content-Type"] = "application/json"
        req = urllib.request.Request(self.base + path, data=data, method=method, headers=h)
        try:
            with urllib.request.urlopen(req, timeout=300) as r:
                raw = r.read()
                return json.loads(raw) if raw else None
        except urllib.error.HTTPError as e:
            if ok404 and e.code == 404:
                return None
            detail = e.read().decode(errors="replace")[:300]
            die(f"{method} {path} -> HTTP {e.code}: {detail}")


def multipart(name, filename, payload):
    boundary = "----heliarelease7d2f"
    head = (f"--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; "
            f"filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n").encode()
    tail = f"\r\n--{boundary}--\r\n".encode()
    return head + payload + tail, {"Content-Type": f"multipart/form-data; boundary={boundary}"}


def cmd_publish(a):
    if not TAG_RE.match(a.tag):
        die(f"tag {a.tag!r} is not vMAJOR.MINOR.PATCH[-suffix]")
    version = a.tag[1:]
    assets = sorted(p for p in pathlib.Path(a.assets).iterdir() if p.is_file())
    if not assets:
        die(f"no files in {a.assets}")
    forge = Forge(a.server, a.repo)
    quoted = urllib.parse.quote(a.tag, safe="")
    release = forge.call("GET", f"/releases/tags/{quoted}", ok404=True)
    if release is None:
        notes = pathlib.Path(a.notes).read_text(encoding="utf-8")
        sums_file = pathlib.Path(a.assets) / "SHA256SUMS"
        if sums_file.is_file():
            notes = body_with_sums(notes, parse_sums_text(sums_file.read_text(encoding="utf-8")))
        release = forge.call("POST", "/releases", body={
            "tag_name": a.tag,
            "name": f"Helia {version}",
            "body": notes,
            "draft": False,
            "prerelease": is_prerelease(version),
        })
        print(f"created release {a.tag} (prerelease={is_prerelease(version)})")
    else:
        print(f"release {a.tag} already exists (id {release['id']})")
    existing = {x["name"]: x["id"] for x in release.get("assets", [])}
    clash = [p.name for p in assets if p.name in existing]
    if clash and not a.replace:
        die("asset(s) already on the release and replace is false: " + ", ".join(clash))
    for p in assets:
        if p.name in existing:
            forge.call("DELETE", f"/releases/{release['id']}/assets/{existing[p.name]}")
            print(f"replaced {p.name}")
        data, hdr = multipart("attachment", p.name, p.read_bytes())
        forge.call("POST", f"/releases/{release['id']}/assets?name={urllib.parse.quote(p.name)}",
                   data=data, headers=hdr)
        print(f"uploaded {p.name} ({p.stat().st_size} bytes)")


def cmd_attach(a):
    """Upload files to an existing release and add their lines to SHA256SUMS."""
    if not TAG_RE.match(a.tag):
        die(f"tag {a.tag!r} is not vMAJOR.MINOR.PATCH[-suffix]")
    files = sorted(p for p in pathlib.Path(a.assets).iterdir()
                   if p.is_file() and p.name != "SHA256SUMS" and not p.name.endswith(".sha256"))
    if not files:
        die(f"no files in {a.assets}")
    forge = Forge(a.server, a.repo)
    release = forge.call("GET", f"/releases/tags/{urllib.parse.quote(a.tag, safe='')}", ok404=True)
    if release is None:
        die(f"no release for {a.tag}: run release.yml first")
    existing = {x["name"]: x for x in release.get("assets", [])}
    clash = [p.name for p in files if p.name in existing]
    if clash and not a.replace:
        die("asset(s) already on the release and replace is false: " + ", ".join(clash))
    sums = sums_in_body(release.get("body") or "")
    if not sums and "SHA256SUMS" in existing:
        die("release notes hold no SHA256SUMS block, so attaching would drop the existing sums: add the block first")
    def upload(p):
        if p.name in existing:
            forge.call("DELETE", f"/releases/{release['id']}/assets/{existing[p.name]['id']}")
        data, hdr = multipart("attachment", p.name, p.read_bytes())
        forge.call("POST", f"/releases/{release['id']}/assets?name={urllib.parse.quote(p.name)}",
                   data=data, headers=hdr)
        print(f"uploaded {p.name} ({p.stat().st_size} bytes)")
    for p in files:
        sums[p.name] = hashlib.sha256(p.read_bytes()).hexdigest()
        upload(p)
    out = pathlib.Path(a.assets) / "SHA256SUMS"
    out.write_text("".join(f"{sums[n]}  {n}"+chr(10) for n in sorted(sums)), encoding="utf-8")
    upload(out)
    forge.call("PATCH", f"/releases/{release['id']}", body={"body": body_with_sums(release.get("body") or "", sums)})
    print(out.read_text(), end="")


def cmd_probe(a):
    """Create and delete a draft release: proves the token can write releases."""
    forge = Forge(a.server, a.repo)
    rel = forge.call("POST", "/releases", body={
        "tag_name": a.name, "name": "token probe (deleted at once)",
        "body": "", "draft": True, "prerelease": True,
    })
    forge.call("DELETE", f"/releases/{rel['id']}")
    print("token can create and delete releases")


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    sub = ap.add_subparsers(dest="cmd", required=True)
    v = sub.add_parser("version")
    v.add_argument("--cargo-toml", default="Cargo.toml")
    v.add_argument("--tag", default="")
    v.set_defaults(fn=cmd_version)
    n = sub.add_parser("notes")
    n.add_argument("--version", required=True)
    n.add_argument("--tag", required=True)
    n.add_argument("--changelog", default="CHANGELOG.md")
    n.add_argument("--server", required=True)
    n.add_argument("--repo", required=True)
    n.add_argument("--out", required=True)
    n.add_argument("--macos", action="store_true", help="list the macOS dmg (only when one is attached)")
    n.set_defaults(fn=cmd_notes)
    s = sub.add_parser("sbom")
    s.add_argument("files", nargs="+")
    s.set_defaults(fn=cmd_sbom)
    p = sub.add_parser("publish")
    p.add_argument("--tag", required=True)
    p.add_argument("--server", required=True)
    p.add_argument("--repo", required=True)
    p.add_argument("--assets", required=True)
    p.add_argument("--notes", required=True)
    p.add_argument("--replace", action="store_true")
    p.set_defaults(fn=cmd_publish)
    at = sub.add_parser("attach")
    at.add_argument("--tag", required=True)
    at.add_argument("--server", required=True)
    at.add_argument("--repo", required=True)
    at.add_argument("--assets", required=True)
    at.add_argument("--replace", action="store_true")
    at.set_defaults(fn=cmd_attach)
    q = sub.add_parser("probe")
    q.add_argument("--server", required=True)
    q.add_argument("--repo", required=True)
    q.add_argument("--name", required=True)
    q.set_defaults(fn=cmd_probe)
    a = ap.parse_args()
    a.fn(a)


if __name__ == "__main__":
    main()
