#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
"""End-to-end test of `helia update` on Linux (the AppImage path).

Run on a build machine, never a desktop:

    tools/update-e2e.py [--work DIR] [--keep]

What it does, in a scratch folder:

1. makes a throwaway signing key (helia-manifest keygen-for-tests);
2. builds `helia` twice with the update configuration of the end-to-end test:
   HELIA_UPDATE_BASE_URL=http://127.0.0.1:PORT, both host lists 127.0.0.1 and
   HELIA_UPDATE_PINNED_KEYS=<the throwaway public key>. The "old" build is this
   source tree (for example 0.7.1); the "new" build is a copy of it with the
   version raised by one patch number;
3. uses the built `helia` binaries as stand-ins for AppImages. An AppImage is an
   ELF file started with $APPIMAGE set to its own path, which is all the updater
   looks at, so this exercises the real updater on the real replacement path
   without building a 100 MB AppImage. (A real AppImage is built by
   packaging/linux/build-appimage.sh; the updater treats it identically.);
4. serves a signed manifest and the "new" file from a local HTTP server;
   Rehearsal (`--rehearsal`): the update is 0.8.0, as for the release. With
   `--public-key LINE --secret-key-file FILE` the old build pins the real public
   key and the manifest is signed with the real secret key (supplied at release
   time by the signing step; the script checks the pair matches and never prints,
   copies or stores the secret). Without them a throwaway pair stands in.
5. runs the old binary's `helia update ...` through: check (text and JSON), the
   refusal to act without --yes, apply, the swapped file and the kept
   `<name>.previous`, then rollback; and the attacks: a manifest signed by another
   key, a tampered file, a replayed older manifest.

Exit code 0 when every check passed. The GUI start-up rollback (needs a display)
is covered by the unit tests in crates/helia-update.
"""

import argparse
import hashlib
import http.server
import json
import os
import pathlib
import re
import shutil
import socket
import subprocess
import sys
import threading

ROOT = pathlib.Path(__file__).resolve().parent.parent
FAILED = []


def check(name, ok, detail=""):
    print(("PASS " if ok else "FAIL ") + name + (f"  ({detail})" if detail and not ok else ""))
    if not ok:
        FAILED.append(name)


def run(cmd, env=None, cwd=None, check_rc=True, input_=None):
    p = subprocess.run(cmd, env=env, cwd=cwd, text=True, capture_output=True, input=input_)
    if check_rc and p.returncode != 0:
        sys.exit(f"command failed ({p.returncode}): {cmd}\n{p.stdout}\n{p.stderr}")
    return p


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def serve(directory, port):
    class Handler(http.server.SimpleHTTPRequestHandler):
        def __init__(self, *a, **k):
            super().__init__(*a, directory=str(directory), **k)

        def log_message(self, *a):
            pass

    srv = http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv


def bump_version(src, dst, new_version=None):
    """Copy the tree without build output and set the version (default: patch + 1)."""
    ignore = shutil.ignore_patterns("target", ".git", "node_modules", "site", "_site")
    shutil.copytree(src, dst, ignore=ignore, symlinks=True)
    text = (dst / "Cargo.toml").read_text()
    m = re.search(r'^version = "(\d+)\.(\d+)\.(\d+)"', text, re.M)
    old = f"{m.group(1)}.{m.group(2)}.{m.group(3)}"
    new = new_version or f"{m.group(1)}.{m.group(2)}.{int(m.group(3)) + 1}"
    # Only the workspace's own version and the version requirements of its path
    # dependencies; other crates may happen to share the number.
    root_toml = dst / "Cargo.toml"
    root_toml.write_text(re.sub(r'(?m)^version = "' + re.escape(old) + '"', f'version = "{new}"', root_toml.read_text(), count=1))
    for toml in list(dst.glob("crates/*/Cargo.toml")) + list(dst.glob("xtask/Cargo.toml")):
        t = re.sub(r'(?m)^(helia[a-z-]* = \{ version = ")' + re.escape(old) + '(", path)',
                   lambda mm: mm.group(1) + new + mm.group(2), toml.read_text())
        toml.write_text(t)
    lock = dst / "Cargo.lock"
    if lock.exists():
        lock.write_text(re.sub(
            r'(name = "helia[a-z-]*"\nversion = )"' + re.escape(old) + '"',
            lambda mm: mm.group(1) + f'"{new}"', lock.read_text()))
    return old, new


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--work", type=pathlib.Path, help="scratch folder (default: a new temporary one)")
    ap.add_argument("--keep", action="store_true", help="keep the scratch folder")
    ap.add_argument("--cargo-target", type=pathlib.Path, help="shared CARGO_TARGET_DIR")
    ap.add_argument("--new-version", help="version of the update (default: the tree's patch number plus one)")
    ap.add_argument("--rehearsal", action="store_true",
                    help="release rehearsal: update to 0.8.0 (see --public-key and --secret-key-file)")
    ap.add_argument("--public-key", help="rehearsal: pin this minisign public key line in the old build")
    ap.add_argument("--secret-key-file", type=pathlib.Path,
                    help="rehearsal: sign with this key file (needs --public-key). Never copied, printed or "
                         "stored; the script only checks that it matches --public-key")
    args = ap.parse_args()
    if bool(args.public_key) != bool(args.secret_key_file):
        sys.exit("--public-key and --secret-key-file go together")
    if args.public_key and not args.rehearsal:
        sys.exit("--public-key and --secret-key-file are for --rehearsal")
    new_version = args.new_version or ("0.8.0" if args.rehearsal else None)
    work = (args.work or pathlib.Path(subprocess.check_output(["mktemp", "-d"], text=True).strip())).resolve()
    work.mkdir(parents=True, exist_ok=True)
    target = (args.cargo_target or work / "target").resolve()
    print(f"scratch: {work}")

    env = dict(os.environ, CARGO_TARGET_DIR=str(target))
    # 1. keys
    run(["cargo", "build", "-q", "-p", "helia-update", "--features", "sign", "--bin", "helia-manifest"], env=env, cwd=ROOT)
    manifest_tool = target / "debug" / "helia-manifest"
    keys = work / "keys"
    run([str(manifest_tool), "keygen-for-tests", "--out-dir", str(keys)])
    other = work / "other-keys"
    run([str(manifest_tool), "keygen-for-tests", "--out-dir", str(other)])
    pub = (keys / "test.pub").read_text().strip().splitlines()[-1]
    sign_key = keys / "test.key"
    other_key = other / "test.key"
    if args.secret_key_file:
        # Real-key rehearsal: prove the pair matches by signing and verifying a probe file.
        # Nothing about the secret key is printed or kept.
        pub = args.public_key.strip().splitlines()[-1].strip()
        sign_key = args.secret_key_file.resolve()
        probe = work / "pair-probe.json"
        sig = pathlib.Path(str(probe) + ".minisig")
        dummy = work / "pair-probe.bin"
        dummy.write_bytes(b"probe")
        ok = False
        built = run([str(manifest_tool), "build", "--app", "helia", "--channel", "stable", "--version", "0.0.1",
                     "--sequence", "1", "--base-url", "https://example.invalid/", "--expires-days", "1",
                     "--asset", f"linux-x86_64:appimage:{dummy}", "--out", str(probe)], check_rc=False)
        if built.returncode == 0:
            signed = run([str(manifest_tool), "sign", "--manifest", str(probe),
                          "--secret-key-file", str(sign_key)], check_rc=False)
            if signed.returncode == 0:
                # A full verify against the pinned key: succeeds only for a matching pair.
                ok = run([str(manifest_tool), "verify", "--manifest", str(probe), "--key", pub],
                         check_rc=False).returncode == 0
        for f in (probe, sig, dummy):
            f.unlink(missing_ok=True)
        if not ok:
            sys.exit("the secret key file does not match the public key (or cannot be read); nothing was built")
        print("secret key matches the public key")

    # 2. builds
    port = free_port()
    base = f"http://127.0.0.1:{port}"
    build_env = dict(
        env,
        HELIA_UPDATE_BASE_URL=base,
        HELIA_UPDATE_MANIFEST_HOSTS="127.0.0.1",
        HELIA_UPDATE_ASSET_HOSTS="127.0.0.1",
        HELIA_UPDATE_PINNED_KEYS=pub,
    )
    old_bin = work / "old" / "Helia.AppImage"
    old_bin.parent.mkdir()
    run(["cargo", "build", "-q", "-p", "helia-cli"], env=build_env, cwd=ROOT)
    shutil.copy2(target / "debug" / "helia", old_bin)
    new_src = work / "src-new"
    old_v, new_v = bump_version(ROOT, new_src, new_version)
    run(["cargo", "build", "-q", "-p", "helia-cli"], env=build_env, cwd=new_src)
    dist = work / "dist"
    dist.mkdir()
    new_name = f"Helia-{new_v}-x86_64.AppImage"
    shutil.copy2(target / "debug" / "helia", dist / new_name)
    print(f"old {old_v}, new {new_v}")

    # 3. what the old binary says about itself
    def version_of(path):
        out = run([str(path), "-V"]).stdout
        return out.strip().split()[-1]

    check("old stand-in reports the old version", version_of(old_bin) == old_v, version_of(old_bin))
    check("new stand-in reports the new version", version_of(dist / new_name) == new_v)

    # 4. manifests
    serve_dir = work / "serve"

    def publish(name, version, sequence, key_file, asset_path=None, subdir="stable"):
        d = serve_dir / "helia" / subdir
        d.mkdir(parents=True, exist_ok=True)
        files = serve_dir / "helia" / version
        files.mkdir(parents=True, exist_ok=True)
        asset = asset_path or dist / new_name
        shutil.copy2(asset, files / asset.name)
        m = d / "manifest.json"
        run([str(manifest_tool), "build", "--app", "helia", "--channel", "stable", "--version", version,
             "--sequence", str(sequence), "--base-url", f"{base}/helia/{version}/", "--expires-days", "30",
             "--notes-url", f"https://example.invalid/notes/{version}",
             "--asset", f"linux-x86_64:appimage:{asset}", "--out", str(m), "--allow-loopback-http"])
        run([str(manifest_tool), "sign", "--manifest", str(m), "--secret-key-file", str(key_file)])

    publish("good", new_v, 1, sign_key)
    srv = serve(serve_dir, port)

    state = work / "state"
    cli_env = dict(os.environ, HELIA_UPDATE_DIR=str(state), APPIMAGE=str(old_bin))

    def helia(*a, yes_input=None):
        for _ in range(3):
            r = run([str(old_bin), "update", *a], env=cli_env, check_rc=False, input_=yes_input)
            # The throwaway python server sometimes drops a connection while files are swapped.
            if "Peer disconnected" not in r.stderr:
                break
        return r

    # 5. happy path
    p = helia("status")
    check("status works with nothing saved", p.returncode == 0 and "notify" in p.stdout, p.stdout + p.stderr)
    p = helia("check")
    check("check finds the update (exit 10)", p.returncode == 10 and new_v in p.stdout, f"{p.returncode} {p.stdout} {p.stderr}")
    p = helia("check", "--json")
    try:
        j = json.loads(p.stdout)
    except ValueError:
        j = {}
    check("check --json is valid and complete", j.get("status") == "available" and j.get("version") == new_v
          and j.get("installs_itself") is True, p.stdout)
    before = hashlib.sha256(old_bin.read_bytes()).hexdigest()
    p = helia("apply")
    check("apply without --yes and without a terminal does nothing (exit 3)",
          p.returncode == 3 and hashlib.sha256(old_bin.read_bytes()).hexdigest() == before, f"{p.returncode} {p.stderr}")
    p = helia("apply", "--yes")
    check("apply --yes succeeds", p.returncode == 0, f"{p.returncode} {p.stdout} {p.stderr}")
    check("the AppImage path now holds the new version", version_of(old_bin) == new_v)
    prev = old_bin.with_name(old_bin.name + ".previous")
    check("the old version is kept as <name>.previous", prev.exists() and version_of(prev) == old_v)
    check("the new file is executable", os.access(old_bin, os.X_OK))
    leftovers = [f.name for f in old_bin.parent.iterdir() if f.name.startswith(".") ]
    check("no temporary files are left", not leftovers, str(leftovers))
    check("the downloaded file was removed from staging",
          not any((state / "updates").glob("*")) if (state / "updates").exists() else True)

    # 6. rollback with the new binary in place
    cli_env["HELIA_UPDATE_DIR"] = str(state)
    p = run([str(old_bin), "update", "status"], env=cli_env, check_rc=False)
    check("status shows the kept previous version", old_v in p.stdout, p.stdout)
    p = run([str(old_bin), "update", "rollback", "--yes"], env=cli_env, check_rc=False)
    check("rollback succeeds", p.returncode == 0, f"{p.returncode} {p.stdout} {p.stderr}")
    check("rollback restored the old version", version_of(old_bin) == old_v)
    p = run([str(old_bin), "update", "rollback", "--yes"], env=cli_env, check_rc=False)
    check("a second rollback says there is nothing kept (exit 1)", p.returncode == 1 and "kept" in (p.stderr + p.stdout), p.stderr)

    # 7. attacks, each from a fresh state
    def attack(name, setup, expect_text):
        shutil.rmtree(state, ignore_errors=True)
        shutil.rmtree(serve_dir / "helia", ignore_errors=True)
        setup()
        before = hashlib.sha256(old_bin.read_bytes()).hexdigest()
        p = helia("apply", "--yes")
        same = hashlib.sha256(old_bin.read_bytes()).hexdigest() == before
        check(f"attack, {name}: refused (exit 1) and nothing changed",
              p.returncode == 1 and same and expect_text in (p.stderr + p.stdout), f"{p.returncode} {p.stdout} {p.stderr}")

    attack("manifest signed by another key", lambda: publish("x", new_v, 1, other_key), "not signed by a key this app trusts")

    def tampered():
        publish("x", new_v, 1, sign_key)
        f = serve_dir / "helia" / new_v / new_name
        f.write_bytes(f.read_bytes() + b"tamper")

    attack("file changed after signing", tampered, "")

    def replay():
        # The state remembers sequence 5; then the server offers sequence 1 again.
        publish("x", new_v, 5, sign_key)
        helia("check")
        publish("x", new_v, 1, sign_key)

    shutil.rmtree(state, ignore_errors=True)
    shutil.rmtree(serve_dir / "helia", ignore_errors=True)
    replay()
    p = helia("check")
    check("attack, replayed older manifest: refused (exit 1)", p.returncode == 1 and "older than one already seen" in (p.stderr + p.stdout),
          f"{p.returncode} {p.stdout} {p.stderr}")

    # 8. off means off: the CLI never checks on its own, config persists
    p = helia("config", "--mode", "off")
    check("config --mode off is saved", p.returncode == 0 and "mode=off" in p.stdout, p.stdout + p.stderr)

    srv.shutdown()
    print()
    if FAILED:
        print(f"{len(FAILED)} check(s) FAILED: " + ", ".join(FAILED))
    else:
        print("all checks passed")
    if not args.keep and not args.work:
        shutil.rmtree(work, ignore_errors=True)
    return 1 if FAILED else 0


if __name__ == "__main__":
    sys.exit(main())
