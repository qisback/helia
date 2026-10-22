#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

# Install the pinned Rust toolchain for a release build. rustup-init is
# downloaded from a versioned URL and checked against a pinned SHA-256; nothing
# is piped into a shell. Usage: setup-rust.sh [extra rustup target ...]
set -euo pipefail

RUST_VERSION="${RUST_VERSION:?RUST_VERSION is not set}"
RUSTUP_VERSION=1.29.1
case "$(uname -m)" in
  x86_64)  HOST_TRIPLE=x86_64-unknown-linux-gnu;  RUSTUP_INIT_SHA256=dda7234360b7f578ca8b0ddcb80145646fa61a67c1720a5abc7051b35c9fcb71 ;;
  aarch64) HOST_TRIPLE=aarch64-unknown-linux-gnu; RUSTUP_INIT_SHA256=15f6e4ce9f583b929c996c91562bad6d4454f3281de858b02cdfdef615fac433 ;;
  *) echo "unsupported build host $(uname -m)" >&2; exit 2 ;;
esac

export PATH="$HOME/.cargo/bin:$PATH"
if ! command -v rustup >/dev/null; then
  tmp="${RUNNER_TEMP:-/tmp}/rustup-init"
  curl -sSfL -o "$tmp" \
    "https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/${HOST_TRIPLE}/rustup-init"
  echo "${RUSTUP_INIT_SHA256}  ${tmp}" | sha256sum -c -
  chmod +x "$tmp"
  "$tmp" -y --profile minimal --default-toolchain none
fi
args=(--profile minimal)
for t in "$@"; do args+=(--target "$t"); done
rustup toolchain install "$RUST_VERSION" "${args[@]}"
if [ -n "${GITHUB_PATH:-}" ]; then echo "$HOME/.cargo/bin" >> "$GITHUB_PATH"; fi
if [ -n "${GITHUB_ENV:-}" ]; then echo "RUSTUP_TOOLCHAIN=$RUST_VERSION" >> "$GITHUB_ENV"; fi
RUSTUP_TOOLCHAIN="$RUST_VERSION" rustc --version
RUSTUP_TOOLCHAIN="$RUST_VERSION" cargo --version
