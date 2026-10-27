#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

# Build the .rpm from a staged tree (needs rpmbuild; `apt install rpm` works on Debian).
#   build-rpm.sh VERSION ARCH STAGE OUTDIR      ARCH is x86_64 or aarch64
set -euo pipefail
ver=$1 arch=$2 stage=$(realpath "$3") out=$4
rver="${ver//-/\~}"
top="$(mktemp -d)"
sed "s/@VERSION@/${rver}/" packaging/linux/helia.spec.in > "$top/helia.spec"
SOURCE_DATE_EPOCH="$(git log -1 --format=%ct)"
export SOURCE_DATE_EPOCH
rpmbuild -bb --target "${arch}-linux" \
  --define "_topdir $top" --define "stage $stage" \
  --define "_buildhost build.invalid" --define "use_source_date_epoch_as_buildtime 1" \
  --define "clamp_mtime_to_source_date_epoch 1" --define "_binary_payload w9.xzdio" \
  "$top/helia.spec"
install -d "$out"
cp "$top/RPMS/${arch}/"*.rpm "$out/"
ls -l "$out"/*.rpm
