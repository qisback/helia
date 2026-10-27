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
  manifest  write the signed-update manifests (unsigned) for the channels a version belongs to
  pinned-keys, fetch-key, verify-manifests, upload-manifests
            the signing steps around it

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
import time
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


def short_changes(body):
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
    groups.pop("Summary", None)  # the public Summary is not part of the forge notes
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
        short_changes(body),
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


def notes_with_macos(body):
    """Release notes that list the macOS dmg: add its table row, drop the "no macOS build" line.

    Everything else (changes, sums block, links) is left as written, so it is safe to run twice.
    """
    row = "| %s | %s |" % MACOS_ROW
    if row not in body:
        lines = body.split(chr(10))
        at = max((i for i, l in enumerate(lines) if l.startswith("| ") and "Platform" not in l and "---" not in l), default=None)
        if at is not None:
            lines.insert(at + 1, row)
        body = chr(10).join(lines)
    body = body.replace(NO_MACOS + chr(10) + chr(10), "").replace(NO_MACOS + chr(10), "").replace(NO_MACOS, "")
    return body


def cmd_notes(a):
    body, date = changelog_entry(a.changelog, a.version)
    if not re.search(r"(?m)^###\s+Summary\s*$", body):
        die(f"CHANGELOG.md section {a.version} has no '### Summary' block: write 3 to 6 plain-English bullets for users "
            "(what is new, what is fixed, anything they must do) before releasing; it becomes the public changelog entry")
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
    if any(p.name.endswith(".dmg") for p in files):
        body = release.get("body") or ""
        listed = notes_with_macos(body)
        if listed != body:
            forge.call("PATCH", f"/releases/{release['id']}", body={"body": listed})
            release["body"] = listed
            print("release notes now list the macOS dmg")
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


# ---------------------------------------------------------------------------------------------
# Signed update manifests 
#
# Hashes and sizes come from the release itself (the SHA256SUMS block in the release notes and
# the asset list), because the CI token cannot download release assets. The Rust tool
# `helia-manifest` (crates/helia-update, feature `sign`) signs and verifies; the bytes written
# here are the bytes that are signed and uploaded.
# ---------------------------------------------------------------------------------------------

UPDATE_APP = "helia"
# Installers are sold on 12hobbies.studio; the GitHub copy is source-only. The manifest's asset URLs
# therefore point at the paid-download location, which the release workflow passes with --base-url
# (repository variable UPDATE_ASSET_BASE, or the address its store tooling works out). There is no default.
# An asset base on one of these hosts would hand out installers publicly: the release refuses it.
PUBLIC_ASSET_HOST_MARKERS = ("github.com", "githubusercontent.com", "gitlab.com", "bitbucket.org", "sourceforge.net")
UPDATE_FLOOR_URL = "https://12hobbies.studio/updates/{app}/{channel}/manifest.json"
DEFAULT_EXPIRES_DAYS = 120
# Platforms and kinds a manifest must carry unless --allow-missing is given.
DEFAULT_REQUIRE = "windows-x86_64:nsis,linux-x86_64:appimage,linux-aarch64:appimage"
PLATFORMS_KINDS = {
    "platform": {"windows-x86_64", "linux-x86_64", "linux-aarch64", "macos-aarch64", "macos-x86_64"},
    "kind": {"nsis", "appimage", "deb", "rpm", "targz", "zip", "dmg"},
}
# File name rules; {v} is the version (a deb writes a pre-release dash as a tilde).
ASSET_RULES = [
    (r"helia-{v}-x86_64-pc-windows-msvc-setup\.exe", "windows-x86_64", "nsis"),
    (r"Helia-{v}-x86_64\.AppImage", "linux-x86_64", "appimage"),
    (r"Helia-{v}-aarch64\.AppImage", "linux-aarch64", "appimage"),
    (r"helia_{v}_amd64\.deb", "linux-x86_64", "deb"),
    (r"helia_{v}_arm64\.deb", "linux-aarch64", "deb"),
    (r"helia-{v}-[0-9A-Za-z.]+\.x86_64\.rpm", "linux-x86_64", "rpm"),
    (r"helia-{v}-[0-9A-Za-z.]+\.aarch64\.rpm", "linux-aarch64", "rpm"),
    (r"helia-{v}-x86_64-unknown-linux-gnu\.tar\.gz", "linux-x86_64", "targz"),
    (r"helia-{v}-aarch64-unknown-linux-gnu\.tar\.gz", "linux-aarch64", "targz"),
    (r"Helia-{v}-macos\.dmg", "macos-aarch64", "dmg"),
]
SEMVER_RE = re.compile(r"^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?$")


def channels_for(version):
    """A plain version goes to stable and prerelease; a -rc/-beta version to prerelease only.

    0.x versions are plain versions here: the owner publishes them as Latest, so they are stable
    for updates (the forge flag in cmd_publish is a separate, older rule). The prerelease channel
    carries the newest of both, so someone on it is not stuck on an old -rc.
    """
    return ["prerelease"] if "-" in version else ["stable", "prerelease"]


def version_key(v):
    m = SEMVER_RE.match(v)
    if not m:
        die(f"{v!r} is not a semantic version")
    pre = m.group(4)
    ids = tuple((0, int(p), "") if p.isdigit() else (1, 0, p) for p in pre.split(".")) if pre else ()
    # a version without a suffix sorts after the same version with one
    return (int(m.group(1)), int(m.group(2)), int(m.group(3)), 0 if pre else 1, ids)


def classify(name, version):
    """(platform, kind) of a release file name, or None when it is not an updatable asset."""
    v = "(?:" + "|".join(sorted({re.escape(version), re.escape(version.replace("-", "~"))})) + ")"
    for pat, plat, kind in ASSET_RULES:
        if re.fullmatch(pat.replace("{v}", v), name):
            return plat, kind
    return None


def utc_stamp(epoch):
    return datetime.datetime.fromtimestamp(epoch, datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def parse_stamp(text):
    try:
        t = datetime.datetime.strptime(text, "%Y-%m-%dT%H:%M:%SZ")
    except ValueError:
        die(f"{text!r} is not YYYY-MM-DDTHH:MM:SSZ")
    return int(t.replace(tzinfo=datetime.timezone.utc).timestamp())


def facts_from_dir(path):
    """name, sha256, size of every file in a directory (a local build or a dry run)."""
    out = []
    for p in sorted(pathlib.Path(path).iterdir()):
        if p.is_file():
            data = p.read_bytes()
            out.append({"name": p.name, "sha256": hashlib.sha256(data).hexdigest(), "size": len(data)})
    sums = pathlib.Path(path) / "SHA256SUMS"
    if sums.is_file():
        listed = parse_sums_text(sums.read_text(encoding="utf-8"))
        for f in out:
            if f["name"] in listed and listed[f["name"]] != f["sha256"]:
                die(f"{f['name']} does not match SHA256SUMS")
    return out


def facts_from_release(forge, tag):
    """name, sha256, size of the release assets that the SHA256SUMS block in the notes lists."""
    rel = forge.call("GET", f"/releases/tags/{urllib.parse.quote(tag, safe='')}", ok404=True)
    if rel is None:
        die(f"no release for {tag}")
    sums = sums_in_body(rel.get("body") or "")
    if not sums:
        die("the release notes hold no SHA256SUMS block")
    return [{"name": a["name"], "sha256": sums[a["name"]], "size": a["size"]}
            for a in rel.get("assets", []) if a["name"] in sums]


def refuse_public_asset_base(base):
    """Installers are paid for: never publish a manifest that points at a public code host."""
    host = (urllib.parse.urlsplit(base).hostname or "").lower()
    if any(host == m or host.endswith("." + m) for m in PUBLIC_ASSET_HOST_MARKERS) or "/releases/download/" in base:
        die(f"the asset base {base} is a public code host.\n"
            "  What: the manifest would point updates at installers on a public release page.\n"
            "  Why: installers are sold on 12hobbies.studio and the GitHub copy of a release is source-only, "
            "so nothing here may link to an installer there.\n"
            "  What to do: give the address of the paid-download location with --base-url "
            "(the release workflow passes the repository variable UPDATE_ASSET_BASE).")


def build_manifest(app, channel, version, date, sequence, expires, min_supported, notes_url, base_url, facts):
    """The manifest dict, field order as helia-manifest writes it. Unknown files are left out."""
    assets = []
    for f in facts:
        pk = classify(f["name"], version)
        if pk is None:
            continue
        if not re.fullmatch(r"[0-9a-f]{64}", f["sha256"]) or f["size"] < 1:
            die(f"bad hash or size for {f['name']}")
        assets.append({"platform": pk[0], "kind": pk[1], "url": base_url + urllib.parse.quote(f["name"], safe="~._-"),
                       "sha256": f["sha256"], "size": f["size"]})
    assets.sort(key=lambda a: (a["platform"], a["kind"]))
    pairs = [(a["platform"], a["kind"]) for a in assets]
    if len(pairs) != len(set(pairs)):
        die("two files map to the same platform and kind")
    doc = {"schema": 1, "app": app, "channel": channel, "version": version, "date": date,
           "sequence": sequence, "expires": expires}
    if min_supported:
        doc["min_supported"] = min_supported
    if notes_url:
        doc["notes_url"] = notes_url
    doc["assets"] = assets
    return doc


def manifest_text(doc):
    return json.dumps(doc, indent=2) + "\n"


def missing_in(facts, version, require):
    have = {classify(f["name"], version) for f in facts}
    return [r for r in require.split(",") if r and tuple(r.split(":", 1)) not in have]


def missing_required(doc, require):
    have = {(a["platform"], a["kind"]) for a in doc["assets"]}
    want = [tuple(r.split(":", 1)) for r in require.split(",") if r]
    return [":".join(w) for w in want if w not in have]


def live_floor(url):
    """(sequence, version) of the manifest now live at url, or None when there is none yet."""
    req = urllib.request.Request(url, headers={"User-Agent": "helia-release/1"})
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            doc = json.loads(r.read(70000))
    except urllib.error.HTTPError as e:
        if e.code in (403, 404):
            return None
        die(f"cannot read the live manifest {url}: HTTP {e.code}")
    except urllib.error.URLError as e:
        if "Name or service not known" in str(e.reason) or "getaddrinfo" in str(e.reason):
            return None
        die(f"cannot read the live manifest {url}: {e.reason}")
    except ValueError:
        die(f"the live manifest {url} is not JSON")
    return int(doc.get("sequence", 0)), str(doc.get("version", "0.0.0"))


def cmd_manifest(a):
    """Write dist/update/<app>/<channel>/manifest.json for every channel the version belongs to."""
    version = a.version or (a.tag[1:] if a.tag else "")
    if not SEMVER_RE.match(version):
        die("give --version or --tag vMAJOR.MINOR.PATCH[-suffix]")
    require = "" if a.allow_missing else a.require
    if a.assets_dir:
        facts = facts_from_dir(a.assets_dir)
    else:
        if not (a.server and a.repo and a.tag):
            die("release mode needs --server, --repo and --tag (or give --assets-dir)")
        forge = Forge(a.server, a.repo)
        # The Windows installer is attached by another workflow after the release exists: wait for it.
        deadline = time.time() + a.wait
        while True:
            facts = facts_from_release(forge, a.tag)
            gone = missing_in(facts, version, require)
            if not gone or time.time() >= deadline:
                break
            print(f"waiting for {', '.join(gone)} on the release")
            time.sleep(60)
    now = parse_stamp(a.date) if a.date else int(datetime.datetime.now(datetime.timezone.utc).timestamp())
    channels = a.channels.split(",") if a.channels else channels_for(version)
    if not a.base_url:
        die("no asset base was given.\n"
            "  What: the manifest needs the address its installer files are served from.\n"
            "  Why: installers are sold on 12hobbies.studio, so there is no public default to fall back on.\n"
            "  What to do: pass --base-url (the release workflow passes the repository variable UPDATE_ASSET_BASE).")
    base = a.base_url.replace("{version}", version).replace("{tag}", a.tag or f"v{version}").rstrip("/") + "/"
    refuse_public_asset_base(base)
    notes_url = a.notes_url if a.notes_url is not None else None
    for channel in channels:
        seq = a.sequence if a.sequence else now
        if a.floor_url:
            floor = live_floor(a.floor_url.format(app=a.app, channel=channel))
            if floor:
                if version_key(floor[1]) > version_key(version) and not a.allow_older:
                    die(f"the live {channel} manifest offers {floor[1]}, newer than {version}: clients would refuse "
                        "the older one. Use --allow-older to publish it anyway (an owner decision)")
                if not a.sequence:
                    seq = max(seq, floor[0] + 1)
                elif seq <= floor[0]:
                    die(f"sequence {seq} is not above the live {floor[0]}")
        doc = build_manifest(a.app, channel, version, utc_stamp(now), seq,
                             utc_stamp(now + a.expires_days * 86400), a.min_supported or None, notes_url, base, facts)
        gone = missing_required(doc, require)
        if gone:
            die(f"the {channel} manifest lacks: {', '.join(gone)} (the assets are not on the release yet?)")
        d = pathlib.Path(a.out) / a.app / channel
        d.mkdir(parents=True, exist_ok=True)
        (d / "manifest.json").write_bytes(manifest_text(doc).encode("utf-8"))
        print(f"wrote {d / 'manifest.json'}: {version} sequence {seq}, {len(doc['assets'])} asset(s), expires {doc['expires']}")


def pinned_keys(path):
    text = pathlib.Path(path).read_text(encoding="utf-8")
    block = re.search(r"PINNED_KEYS\s*:\s*&\[&str\]\s*=\s*&\[(.*?)\];", text, re.S)
    if not block:
        die(f"no PINNED_KEYS list in {path}")
    code = re.sub(r"//[^\n]*", "", block.group(1))
    keys = re.findall(r'"([A-Za-z0-9+/=]{20,})"', code)
    if not 1 <= len(keys) <= 2:
        die(f"{path} pins {len(keys)} key(s); one or two are allowed")
    placeholder = re.search(r"KEYS_ARE_PLACEHOLDER\s*:\s*bool\s*=\s*(true|false)", text)
    if not placeholder:
        die(f"no KEYS_ARE_PLACEHOLDER in {path}")
    return keys, placeholder.group(1) == "true"


PLACEHOLDER_KEY = "RWRU3O0qD7N8OGqGTd2ih2sihZojl1yd3AKzDHtOA20G7VbJ5jf86kMH"


def key_overrides():
    """Build-time environment variables that would replace the pinned keys (any HELIA_UPDATE_*KEY*)."""
    return sorted(k for k in os.environ if re.fullmatch(r"HELIA_UPDATE_\w*KEYS?\w*", k) and os.environ[k])


def cmd_check_keys(a):
    """Release gate: real pinned keys, no key override in the environment, and the binaries carry them."""
    keys, placeholder = pinned_keys(a.file)
    if placeholder or PLACEHOLDER_KEY in keys:
        die("the pinned keys are still the placeholder (KEYS_ARE_PLACEHOLDER): a release would accept no update")
    over = key_overrides()
    if over:
        die("the build environment overrides the pinned keys: " + ", ".join(over))
    for b in a.binary:
        data = pathlib.Path(b).read_bytes()
        if PLACEHOLDER_KEY.encode() in data:
            die(f"{b} contains the placeholder key")
        for k in keys:
            if k.encode() not in data:
                die(f"{b} does not contain the pinned key {k[:8]}...: it was built with other keys")
    print(f"{len(keys)} real pinned key(s), no override" + (f", {len(a.binary)} binary(ies) checked" if a.binary else ""))


def cmd_pinned_keys(a):
    keys, placeholder = pinned_keys(a.file)
    if placeholder and a.require_real:
        die("the pinned keys are still the placeholder: put the real public key in "
            "crates/helia-update/src/pinned_keys.rs and set KEYS_ARE_PLACEHOLDER to false")
    for k in keys:
        print(k)


def bao_json(url, token=None, payload=None, what="the secret store"):
    headers = {"User-Agent": "helia-release/1"}
    if token:
        headers["X-Vault-Token"] = token
    data = None
    if payload is not None:
        data = json.dumps(payload).encode()
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=data, headers=headers, method="POST" if data else "GET")
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return json.loads(r.read())
    except urllib.error.HTTPError as e:
        die(f"{what} answered HTTP {e.code}")
    except (urllib.error.URLError, ValueError) as e:
        die(f"cannot reach {what}: {type(e).__name__}")


def cmd_fetch_key(a):
    """Read the signing key from the secret store into a 0600 file.

    BAO_ADDR and BAO_TOKEN come from the environment. BAO_TOKEN is either a token, or `role_id:secret_id` of an
    AppRole (a token never contains a colon): the login then yields a token that lives for minutes.
    """
    token, addr = os.environ.get("BAO_TOKEN"), os.environ.get("BAO_ADDR")
    if not token or not addr:
        die("BAO_TOKEN and BAO_ADDR must be set (Forgejo secret BAO_UPDATE_SIGNING_TOKEN, variable UPDATE_BAO_ADDR)")
    if not addr.startswith("https://"):
        die("BAO_ADDR must be https")
    base = addr.rstrip("/")
    if ":" in token:
        role, _, secret_id = token.partition(":")
        login = bao_json(f"{base}/v1/auth/approle/login", payload={"role_id": role, "secret_id": secret_id},
                         what="the secret store login")
        token = (login.get("auth") or {}).get("client_token")
        if not token:
            die("the secret store login returned no token")
    mount, _, rest = a.path.partition("/")
    url = f"{base}/v1/{mount}/data/{rest}" if a.kv_version == 2 else f"{base}/v1/{a.path}"
    body = bao_json(url, token=token, what=f"the secret {a.path}")
    data = body.get("data", {})
    data = data.get("data", data) if a.kv_version == 2 else data
    secret = data.get(a.field)
    if not secret or "untrusted comment" not in secret:
        die(f"field {a.field} of {a.path} is missing or is not a minisign secret key")
    fd = os.open(a.out, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        f.write(secret if secret.endswith("\n") else secret + "\n")
    os.chmod(a.out, 0o600)
    pub = data.get("public_key")
    if pub and a.pub_out:
        pathlib.Path(a.pub_out).write_text(pub if pub.endswith("\n") else pub + "\n", encoding="utf-8")
    print(f"signing key written to {a.out} (mode 0600); its value is not printed")


def run_tool(tool, *args):
    import subprocess
    p = subprocess.run([tool, *args], capture_output=True, text=True)
    return p.returncode, (p.stdout + p.stderr).strip()


def manifest_files(root):
    return sorted(pathlib.Path(root).glob("*/*/manifest.json"))


def cmd_verify_manifests(a):
    """Gate: every manifest has a signature that verifies against the given keys; none means failure."""
    files = manifest_files(a.dir)
    if not files:
        die(f"no manifest.json under {a.dir}: a release without a signed update manifest fails")
    for m in files:
        sig = m.with_name("manifest.json.minisig")
        if not sig.is_file():
            die(f"{m} has no signature")
        keys = [x for k in a.key for x in ("--key", k)]
        rc, out = run_tool(a.tool, "verify", "--manifest", str(m), *keys)
        if rc != 0:
            die(f"{m} does not verify: {out}")
        print(f"{m.parent.name}: {out}")
        if a.tamper_check:
            bad = pathlib.Path(a.tamper_check) / m.parent.name
            bad.mkdir(parents=True, exist_ok=True)
            raw = bytearray(m.read_bytes())
            raw[len(raw) // 2] ^= 1
            (bad / "manifest.json").write_bytes(bytes(raw))
            (bad / "manifest.json.minisig").write_bytes(sig.read_bytes())
            rc, _ = run_tool(a.tool, "verify", "--manifest", str(bad / "manifest.json"), *keys)
            if rc == 0:
                die("a tampered manifest verified: the verifier is broken")
            print(f"{m.parent.name}: a changed byte is refused")


def cmd_upload_manifests(a):
    """Attach manifest and signature to the forge release as update-<app>-<channel>.json[.minisig].

    Always replaces: a re-run signs a higher sequence. The files are not added to SHA256SUMS.
    r2_publish.py (ci-cd) puts them at <app>/<channel>/manifest.json on the downloads host.
    """
    if not TAG_RE.match(a.tag):
        die(f"tag {a.tag!r} is not vMAJOR.MINOR.PATCH[-suffix]")
    files = manifest_files(a.dir)
    if not files:
        die(f"no manifest.json under {a.dir}")
    forge = Forge(a.server, a.repo)
    rel = forge.call("GET", f"/releases/tags/{urllib.parse.quote(a.tag, safe='')}", ok404=True)
    if rel is None:
        die(f"no release for {a.tag}")
    existing = {x["name"]: x["id"] for x in rel.get("assets", [])}
    for m in files:
        for src, suffix in ((m, ".json"), (m.with_name("manifest.json.minisig"), ".json.minisig")):
            if not src.is_file():
                die(f"{src} is missing: sign first")
            name = f"update-{m.parent.parent.name}-{m.parent.name}{suffix}"
            if name in existing:
                forge.call("DELETE", f"/releases/{rel['id']}/assets/{existing[name]}")
            data, hdr = multipart("attachment", name, src.read_bytes())
            forge.call("POST", f"/releases/{rel['id']}/assets?name={urllib.parse.quote(name)}", data=data, headers=hdr)
            print(f"uploaded {name} ({src.stat().st_size} bytes)")


def add_manifest_parsers(sub):
    m = sub.add_parser("manifest", help="write the update manifest for each channel the version belongs to")
    m.add_argument("--app", default=UPDATE_APP)
    m.add_argument("--tag", default="")
    m.add_argument("--version", default="")
    m.add_argument("--server", default="")
    m.add_argument("--repo", default="")
    m.add_argument("--assets-dir", default="", help="hash the files in this directory instead of reading the release")
    m.add_argument("--out", default="dist/update")
    m.add_argument("--base-url", default="", help="URL prefix of the asset files, required; may contain {version} and {tag}; a GitHub address is refused")
    m.add_argument("--notes-url", default=None)
    m.add_argument("--channels", default="", help="comma list; default from the version")
    m.add_argument("--expires-days", type=int, default=DEFAULT_EXPIRES_DAYS)
    m.add_argument("--min-supported", default="")
    m.add_argument("--date", default="", help="UTC YYYY-MM-DDTHH:MM:SSZ; default now")
    m.add_argument("--sequence", type=int, default=0, help="default: the date as Unix seconds, raised above the live one")
    m.add_argument("--floor-url", default="", help="live manifest URL with {app} and {channel}; sets the sequence floor")
    m.add_argument("--allow-older", action="store_true")
    m.add_argument("--require", default=DEFAULT_REQUIRE)
    m.add_argument("--wait", type=int, default=0, help="seconds to wait for the required assets to appear on the release")
    m.add_argument("--allow-missing", action="store_true", help="do not insist on --require (dry runs)")
    m.set_defaults(fn=cmd_manifest)
    k = sub.add_parser("pinned-keys", help="print the public keys pinned in pinned_keys.rs")
    k.add_argument("--file", default="crates/helia-update/src/pinned_keys.rs")
    k.add_argument("--require-real", action="store_true", help="fail while the placeholder key is pinned")
    k.set_defaults(fn=cmd_pinned_keys)
    c = sub.add_parser("check-keys", help="refuse a release built with the placeholder or overridden keys")
    c.add_argument("--file", default="crates/helia-update/src/pinned_keys.rs")
    c.add_argument("--binary", action="append", default=[], help="a built binary that must carry the pinned keys")
    c.set_defaults(fn=cmd_check_keys)
    f = sub.add_parser("fetch-key", help="read the signing key from the secret store into a 0600 file")
    f.add_argument("--path", default="claude-automation/release-signing/helia")
    f.add_argument("--field", default="secret_key")
    f.add_argument("--kv-version", type=int, choices=(1, 2), default=2)
    f.add_argument("--out", required=True)
    f.add_argument("--pub-out", default="")
    f.set_defaults(fn=cmd_fetch_key)
    v = sub.add_parser("verify-manifests", help="fail unless every manifest verifies against the keys")
    v.add_argument("--dir", default="dist/update")
    v.add_argument("--tool", required=True, help="path of helia-manifest")
    v.add_argument("--key", action="append", required=True, help="public key line or file, repeatable")
    v.add_argument("--tamper-check", default="", help="directory for a changed copy that must be refused")
    v.set_defaults(fn=cmd_verify_manifests)
    u = sub.add_parser("upload-manifests", help="attach the manifests and signatures to the forge release")
    u.add_argument("--tag", required=True)
    u.add_argument("--server", required=True)
    u.add_argument("--repo", required=True)
    u.add_argument("--dir", default="dist/update")
    u.set_defaults(fn=cmd_upload_manifests)


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
    add_manifest_parsers(sub)
    a = ap.parse_args()
    a.fn(a)


if __name__ == "__main__":
    main()
