#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

# Check the .deb, .rpm and AppImage in DIST without trusting the build.
#   verify-packages.sh VERSION ARCH DIST RUN
# RUN is 1 when this machine can execute ARCH binaries (the x86_64 job), else 0. Needs
# desktop-file-utils and appstream; the RPM checks need rpm, the install test needs a
# passwordless sudo (it is skipped, loudly, without one).
set -euo pipefail
trap 'echo "FAILED at line $LINENO: $BASH_COMMAND" >&2' ERR
ver=$1 arch=$2 dist=$(realpath "$3") run=$4
dver="${ver//-/\~}"
case "$arch" in x86_64) deb=amd64 ;; aarch64) deb=arm64 ;; *) echo "bad arch" >&2; exit 2 ;; esac
want_files=(
  usr/bin/helia usr/bin/helia-gui
  usr/share/applications/helia.desktop
  usr/share/metainfo/casa.discworld.helia.metainfo.xml
  usr/share/mime/packages/casa.discworld.helia.xml
  usr/share/doc/helia/LICENSE usr/share/doc/helia/ADDITIONAL-TERMS.md usr/share/doc/helia/LOGO-LICENCE.md
  usr/share/doc/helia/THIRD-PARTY-LICENSES.txt usr/share/doc/helia/copyright
)
for s in 16 24 32 48 64 128 256 512; do want_files+=("usr/share/icons/hicolor/${s}x${s}/apps/helia.png"); done
first() { sed -n 1p; }   # reads everything: head -n1 would SIGPIPE the writer under pipefail
say() { printf '== %s\n' "$*"; }

check_tree() {  # check_tree ROOT
  local root=$1 f
  for f in "${want_files[@]}"; do
    [ -s "$root/$f" ] || { echo "MISSING $f" >&2; exit 1; }
  done
  desktop-file-validate "$root/usr/share/applications/helia.desktop"
  appstreamcli validate --no-net "$root/usr/share/metainfo/casa.discworld.helia.metainfo.xml"
  xmllint --noout "$root/usr/share/mime/packages/casa.discworld.helia.xml" 2>/dev/null || python3 -c "import sys,xml.dom.minidom as m; m.parse(sys.argv[1])" "$root/usr/share/mime/packages/casa.discworld.helia.xml"
  grep -q "<release version=\"${ver}\"" "$root/usr/share/metainfo/casa.discworld.helia.metainfo.xml"
  if [ "$run" = 1 ]; then
    [ "$("$root/usr/bin/helia" --version | first)" = "helia ${ver}" ]
    [ "$("$root/usr/bin/helia-gui" --version | first)" = "helia-gui ${ver}" ]
  fi
}

# ---- .deb --------------------------------------------------------------------
d="$dist/helia_${dver}_${deb}.deb"
say "deb: $d"
dpkg-deb -I "$d" | sed -n '1,20p'
[ "$(dpkg-deb -f "$d" Version)" = "$dver" ]
[ "$(dpkg-deb -f "$d" Architecture)" = "$deb" ]
t="$(mktemp -d)"
dpkg-deb -x "$d" "$t"
check_tree "$t"
echo "deb tree OK (desktop file, AppStream, mime, icons, licences$([ "$run" = 1 ] && echo ', binaries run'))"
if [ "$run" = 1 ]; then
  if sudo -n true 2>/dev/null; then
    say "deb: install, use, remove"
    sudo -n apt-get install -yq shared-mime-info >/dev/null || true
    sudo -n dpkg -i "$d"
    [ "$(helia --version | first)" = "helia ${ver}" ]
    [ "$(helia-gui --version | first)" = "helia-gui ${ver}" ]
    desktop-file-validate /usr/share/applications/helia.desktop
    grep -q 'video/x-astro-ser' /usr/share/mime/globs2 || { echo "mime database not updated by the package triggers" >&2; exit 1; }
    sudo -n dpkg -r helia
    for f in "${want_files[@]}"; do [ ! -e "/$f" ] || { echo "left behind: /$f" >&2; exit 1; }; done
    ! grep -q 'video/x-astro-ser' /usr/share/mime/globs2 || { echo "mime database still lists SER after removal" >&2; exit 1; }
    echo "deb installs, runs and removes cleanly"
  else
    echo "SKIP: no passwordless sudo here, so the deb was unpacked and checked but not installed"
  fi
fi

# ---- .rpm --------------------------------------------------------------------
r="$(ls "$dist"/helia-*."${arch}".rpm)"
say "rpm: $r"
rpm -qpi "$r" | sed -n '1,14p'
[ "$(rpm -qp --qf '%{VERSION}' "$r")" = "$dver" ]
[ "$(rpm -qp --qf '%{ARCH}' "$r")" = "$arch" ]
rpm -qp --requires "$r"
rpm -qpl "$r" > "$t/rpm-files.txt"
for f in "${want_files[@]}"; do grep -qx "/$f" "$t/rpm-files.txt" || { echo "rpm lacks /$f" >&2; exit 1; }; done
rt="$(mktemp -d)"
(cd "$rt" && rpm2cpio "$r" | cpio -idm --quiet --no-preserve-owner)
check_tree "$rt"
echo "rpm OK (file list, requires, desktop file, AppStream, licences$([ "$run" = 1 ] && echo ', binaries run'))"

# ---- AppImage ----------------------------------------------------------------
a="$dist/Helia-${ver}-${arch}.AppImage"
say "appimage: $a"
[ -x "$a" ]
file "$a"
if [ "$run" = 1 ]; then
  export APPIMAGE_EXTRACT_AND_RUN=1
  [ "$("$a" --version | first)" = "helia-gui ${ver}" ]
  [ "$("$a" --cli --version | first)" = "helia ${ver}" ]
  echo "AppImage runs --version (GUI and --cli)"
fi
ex="$(mktemp -d)"
(cd "$ex" && "$a" --appimage-extract >/dev/null 2>&1 || true)
if [ -d "$ex/squashfs-root" ]; then
  desktop-file-validate "$ex/squashfs-root/helia.desktop"
  [ -s "$ex/squashfs-root/helia.png" ]
  [ -s "$ex/squashfs-root/usr/share/doc/helia/ADDITIONAL-TERMS.md" ]
else
  # A foreign-architecture image cannot be extracted by running it: check the ELF machine instead.
  readelf -h "$a" | grep -q "Machine:.*$([ "$arch" = aarch64 ] && echo AArch64 || echo X86-64)"
fi
echo "AppImage OK"
