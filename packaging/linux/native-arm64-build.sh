#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

# Build the release binaries natively on an arm64 host, inside Ubuntu 22.04 (glibc 2.35, the
# project baseline), then run them in that image to prove the baseline holds.
#   native-arm64-build.sh CARGO_ARGS...      e.g. -p helia-cli -p helia-gui
# Needs docker. RUST_VERSION must be set. The cargo registry and rustup live in
# $CI_HOME (default ~/.cache/ci-home) so repeat builds are quick; target/ is in the workspace.
# If GIT_CONFIG_GLOBAL names a file it is mounted read-only (private git dependencies).
set -euo pipefail
[ "$(uname -m)" = aarch64 ] || { echo "native-arm64-build.sh needs an arm64 host" >&2; exit 2; }
: "${RUST_VERSION:?RUST_VERSION is not set}"
home="${CI_HOME:-$HOME/.cache/ci-home}"
mkdir -p "$home"
img=ci-arm64-build:22.04
docker build -q -t "$img" -f packaging/linux/Dockerfile.arm64-build packaging/linux >/dev/null
extra=()
if [ -n "${GIT_CONFIG_GLOBAL:-}" ] && [ -f "$GIT_CONFIG_GLOBAL" ]; then
  extra+=(-v "$GIT_CONFIG_GLOBAL:/ci-gitconfig:ro" -e GIT_CONFIG_GLOBAL=/ci-gitconfig -e CARGO_NET_GIT_FETCH_WITH_CLI=true)
fi
run() { docker run --rm --user "$(id -u):$(id -g)" -e HOME=/ci-home -v "$home:/ci-home" \
          -v "$PWD:/work" -w /work -e RUST_VERSION -e CARGO_TERM_COLOR=never "${extra[@]}" "$img" "$@"; }
docker run --rm "$img" ldd --version | sed -n 1p
run bash -c 'packaging/setup-rust.sh >/dev/null && export PATH=$HOME/.cargo/bin:$PATH RUSTUP_TOOLCHAIN=$RUST_VERSION && cargo build --release --locked "$@"' _ "$@"
