#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

# Assemble Helia.app and the disk image. Needs macOS (hdiutil, plutil). Not yet run on a
# Mac: there is no Mac runner (issue #75).
#   build-app.sh VERSION BINDIR METADIR OUTDIR
# BINDIR holds helia and helia-gui (a universal or aarch64 build); METADIR holds
# THIRD-PARTY-LICENSES.txt. Writes OUTDIR/Helia.app and OUTDIR/Helia-VERSION-macos.dmg.
set -euo pipefail
ver=$1 bindir=$2 meta=$3 out=$4
app="$out/Helia.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
install -m 0755 "$bindir/helia-gui" "$app/Contents/MacOS/helia-gui"
# The command line tool rides along inside the bundle so the app is the whole product.
install -m 0755 "$bindir/helia" "$app/Contents/MacOS/helia"
strip -x "$app/Contents/MacOS/helia-gui" "$app/Contents/MacOS/helia"
install -m 0644 assets/branding/helia.icns "$app/Contents/Resources/helia.icns"
sed "s/@VERSION@/${ver}/g" packaging/macos/Info.plist.in > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"
res="$app/Contents/Resources"
install -m 0644 LICENSE ADDITIONAL-TERMS.md LOGO-LICENCE.md README.md CHANGELOG.md "$res/"
install -m 0644 "$meta/THIRD-PARTY-LICENSES.txt" "$res/"

# Disk image: the app and a link to /Applications, plus the licences beside them.
dmgsrc="$(mktemp -d)"
cp -R "$app" "$dmgsrc/"
ln -s /Applications "$dmgsrc/Applications"
install -m 0644 LICENSE "$dmgsrc/LICENSE.txt"
install -m 0644 ADDITIONAL-TERMS.md LOGO-LICENCE.md "$dmgsrc/"
dmg="$out/Helia-${ver}-macos.dmg"
rm -f "$dmg"
hdiutil create -volname "Helia ${ver}" -srcfolder "$dmgsrc" -fs HFS+ -format UDZO -ov "$dmg"
hdiutil verify "$dmg"
ls -l "$dmg"
