#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

# Build the AppImage from a staged tree.
#   build-appimage.sh VERSION ARCH STAGE OUTDIR APPIMAGETOOL [RUNTIME]
# APPIMAGETOOL is a path to appimagetool (itself an AppImage: it is run with
# APPIMAGE_EXTRACT_AND_RUN, so FUSE is not needed). For a foreign ARCH pass the
# matching runtime file; the image is then assembled without running target code.
set -euo pipefail
ver=$1 arch=$2 stage=$3 out=$4 tool=$5 runtime=${6:-}
app="$(mktemp -d)/Helia.AppDir"
cp -a "$stage" "$app"
cat > "$app/AppRun" <<'EOF'
#!/bin/sh
# helia-gui by default; "AppRun --cli ARGS" runs the command line tool instead.
here="$(dirname "$(readlink -f "$0")")"
export PATH="$here/usr/bin:$PATH"
if [ "${1:-}" = "--cli" ]; then shift; exec "$here/usr/bin/helia" "$@"; fi
exec "$here/usr/bin/helia-gui" "$@"
EOF
chmod 0755 "$app/AppRun"
cp "$app/usr/share/applications/helia.desktop" "$app/helia.desktop"
cp "$app/usr/share/icons/hicolor/256x256/apps/helia.png" "$app/helia.png"
ln -s helia.png "$app/.DirIcon"
# The runtime in front of the image statically links libfuse (LGPL-2.1): ship its notice, the
# LGPL text and the source offer inside the image, next to the other third-party licences.
here=$(cd "$(dirname "$0")" && pwd)
doc="$app/usr/share/doc/helia"
install -d "$doc"
{ [ -f "$doc/THIRD-PARTY-LICENSES.txt" ] && cat "$doc/THIRD-PARTY-LICENSES.txt"; printf '
'; cat "$here/appimage-runtime-notice.txt"; } > "$doc/THIRD-PARTY-LICENSES.new"
mv "$doc/THIRD-PARTY-LICENSES.new" "$doc/THIRD-PARTY-LICENSES.txt"
install -m 0644 "$here/licences/LGPL-2.1.txt" "$doc/LGPL-2.1.txt"
install -d "$out"
name="Helia-${ver}-${arch}.AppImage"
args=()
if [ -n "$runtime" ]; then args+=(--runtime-file "$runtime"); fi
ARCH="$arch" APPIMAGE_EXTRACT_AND_RUN=1 "$tool" "${args[@]}" "$app" "$out/$name"
chmod 0755 "$out/$name"
ls -l "$out/$name"
