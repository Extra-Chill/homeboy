#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

PATH="$TMP_DIR" /bin/bash "$ROOT_DIR/swift/scripts/setup.sh" > "$TMP_DIR/setup.out"
if ! grep -Fq "Swift unavailable; Swift extension installed but not ready on this runner" "$TMP_DIR/setup.out"; then
    echo "Expected Swift setup to explain unsupported runner capability" >&2
    sed 's/^/  /' "$TMP_DIR/setup.out" >&2
    exit 1
fi

echo "swift setup capability smoke passed"
