#!/usr/bin/env bash
# Build a Debian .deb for vulthor using cargo-deb.
#
# Usage:
#   packaging/build-deb.sh           # build for current arch
#
# Prereqs:
#   cargo install cargo-deb
#
# Output: target/debian/vulthor_<version>_<arch>.deb
set -euo pipefail

if ! command -v cargo-deb >/dev/null 2>&1; then
    echo "error: cargo-deb is not installed." >&2
    echo "       cargo install cargo-deb" >&2
    exit 1
fi

cd "$(dirname "$0")/.."

cargo deb --no-build
echo
echo "Build .deb output in: target/debian/"
ls -1 target/debian/ || true
