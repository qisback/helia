#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

# Stage and pack one Linux archive.
#   package-linux.sh VERSION TARGET BINDIR METADIR OUTDIR [STRIP]
# BINDIR holds the built helia and helia-gui; METADIR holds THIRD-PARTY-LICENSES.txt.
# The archive is reproducible for a given commit: sorted names, fixed owner and
# mtime (the commit time), gzip without a timestamp.
set -euo pipefail
ver=$1 target=$2 bindir=$3 meta=$4 out=$5 strip=${6:-strip}
name="helia-${ver}-${target}"
stage="$(mktemp -d)/${name}"
mkdir -p "$stage" "$out"
for b in helia helia-gui; do
  install -m 0755 "$bindir/$b" "$stage/$b"
  "$strip" "$stage/$b"
done
cp LICENSE ADDITIONAL-TERMS.md LOGO-LICENCE.md README.md CHANGELOG.md "$stage/"
# Desktop entry and icons in the freedesktop hicolor layout (copy share/ into /usr or ~/.local).
install -D -m 0644 packaging/linux/helia.desktop "$stage/share/applications/helia.desktop"
for s in 16 24 32 48 64 128 256 512; do
  install -D -m 0644 "assets/branding/icons/helia-$s.png" "$stage/share/icons/hicolor/${s}x${s}/apps/helia.png"
done
cp "$meta/THIRD-PARTY-LICENSES.txt" "$stage/"
epoch="$(git log -1 --format=%ct)"
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime="@${epoch}" \
  -C "$(dirname "$stage")" -cf - "$name" | gzip -9n > "$out/${name}.tar.gz"
ls -l "$out/${name}.tar.gz"
