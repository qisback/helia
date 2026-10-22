#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

# Check a built Linux binary before it is packed.
#   check-binary.sh KIND VERSION BIN [OBJDUMP]
# KIND is x86_64 (run it) or aarch64 (run it natively on arm64, else under qemu-user if present, else
# check the ELF header only and say so). Always checks the glibc baseline: the
# newest GLIBC_x.y symbol the binary needs must be at most 2.35.
set -euo pipefail
kind=$1 ver=$2 bin=$3 objdump=${4:-objdump}
want="$(basename "$bin") ${ver}"
file "$bin"
need="$("$objdump" -T "$bin" | grep -o 'GLIBC_[0-9][0-9.]*' | sed 's/GLIBC_//' | sort -uV | tail -1)"
echo "newest glibc symbol: ${need}"
newest="$(printf '%s\n2.35\n' "$need" | sort -V | tail -1)"
[ "$newest" = "2.35" ] || { echo "needs glibc ${need}, above the 2.35 baseline" >&2; exit 1; }
case "$kind" in
  x86_64)
    readelf -h "$bin" | grep -q 'Machine:.*X86-64'
    got="$("$bin" --version)"
    ;;
  aarch64)
    readelf -h "$bin" | grep -q 'Machine:.*AArch64'
    if [ "$(uname -m)" = aarch64 ]; then
      got="$("$bin" --version)"
      echo "ran natively"
    elif command -v qemu-aarch64 >/dev/null; then
      got="$(qemu-aarch64 -L /usr/aarch64-linux-gnu "$bin" --version)"
      echo "ran under qemu-user"
    else
      echo "NOTE: qemu-user not available; architecture checked from the ELF header only, binary not run"
      exit 0
    fi
    ;;
  *) echo "unknown kind $kind" >&2; exit 2 ;;
esac
got="${got%%$'\n'*}"  # --version prints the build record and notices after line one
echo "${got}"
[ "$got" = "$want" ] || { echo "expected '${want}', got '${got}'" >&2; exit 1; }
