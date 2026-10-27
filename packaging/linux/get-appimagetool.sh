#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

# Fetch the pinned appimagetool (x86_64 build host) and the aarch64 AppImage runtime into DIR,
# each checked against a pinned SHA-256. Prints nothing but progress.
#   get-appimagetool.sh DIR
set -euo pipefail
dir=$1
# The tool runs on the build host: x86_64 on the x86 runners, aarch64 on the native arm64 runner.
case "$(uname -m)" in
  x86_64)  host=x86_64;  tool_sha=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0 ;;
  aarch64) host=aarch64; tool_sha=f0837e7448a0c1e4e650a93bb3e85802546e60654ef287576f46c71c126a9158 ;;
  *) echo "unsupported build host $(uname -m)" >&2; exit 2 ;;
esac
tool_url=https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-${host}.AppImage
rt_url=https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-aarch64
rt_sha=00cbdfcf917cc6c0ff6d3347d59e0ca1f7f45a6df1a428a0d6d8a78664d87444
mkdir -p "$dir"
fetch() {
  local url=$1 sha=$2 out=$3
  if [ ! -f "$out" ] || [ "$(sha256sum "$out" | cut -d' ' -f1)" != "$sha" ]; then
    curl -fsSL --retry 3 -o "$out" "$url"
  fi
  [ "$(sha256sum "$out" | cut -d' ' -f1)" = "$sha" ] || { echo "checksum mismatch for $out" >&2; exit 1; }
}
fetch "$tool_url" "$tool_sha" "$dir/appimagetool-${host}.AppImage"
fetch "$rt_url" "$rt_sha" "$dir/runtime-aarch64"
chmod +x "$dir/appimagetool-${host}.AppImage"
echo "appimagetool 1.9.1 and runtime 20251108 ready in $dir"
