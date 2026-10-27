#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

# Build the .deb from a staged tree.
#   build-deb.sh VERSION ARCH STAGE OUTDIR      ARCH is x86_64 or aarch64
set -euo pipefail
ver=$1 arch=$2 stage=$3 out=$4
case "$arch" in
  x86_64) deb=amd64 ;;
  aarch64) deb=arm64 ;;
  *) echo "unknown arch $arch" >&2; exit 2 ;;
esac
dver="${ver//-/\~}"   # 0.5.0-rc.1 sorts before 0.5.0 as 0.5.0~rc.1
pkg="$(mktemp -d)/pkg"
cp -a "$stage" "$pkg"
install -d "$pkg/DEBIAN"
size="$(du -sk --apparent-size "$pkg/usr" | cut -f1)"
cat > "$pkg/DEBIAN/control" <<EOF
Package: helia
Version: ${dver}
Architecture: ${deb}
Maintainer: Laurence Horrocks-Barlow <9962027+qisback@users.noreply.github.com>
Installed-Size: ${size}
Depends: libc6 (>= 2.35)
Recommends: libvulkan1, mesa-vulkan-drivers, libxkbcommon0
Suggests: ffmpeg
Section: science
Priority: optional
Homepage: https://github.com/qisback/helia
Description: Solar lucky-imaging stacker
 Helia stacks video of the Sun into one sharp image. It scores every frame,
 aligns the best on the whole disc, stacks them, then sharpens and colours the
 result. This package has the desktop app (helia-gui) and the command line
 tool (helia). It reads SER and AVI captures; install ffmpeg for MP4 and MOV.
 .
 Helia is dedicated to the author's daughter, the light of his life.
EOF
install -d "$out"
SOURCE_DATE_EPOCH="$(git log -1 --format=%ct)"
export SOURCE_DATE_EPOCH
dpkg-deb --root-owner-group -Zxz --build "$pkg" "$out/helia_${dver}_${deb}.deb"
ls -l "$out/helia_${dver}_${deb}.deb"
