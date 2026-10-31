#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SWIFT_DIR="$ROOT_DIR/swift"
HOMEBOY_CORE_DIR="${HOMEBOY_CORE_DIR:-$(cd "${ROOT_DIR}/.." && pwd)/homeboy}"
# shellcheck source=../../scripts/lib/runtime-helper-resolver.sh
source "${ROOT_DIR}/scripts/lib/runtime-helper-resolver.sh"
RESOLVE_CONTEXT_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_RESOLVE_CONTEXT resolve-context.sh)" || exit 1
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

FIXTURE="$TMP_DIR/minimal-swift"
mkdir -p "$FIXTURE"
cat > "$FIXTURE/App.swift" <<'SWIFT'
struct AppModel {
    let title: String
}
SWIFT

HOMEBOY_COMPONENT_PATH="$FIXTURE" HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" bash "$SWIFT_DIR/scripts/lint-runner.sh" > "$TMP_DIR/lint.out"

if ! command -v swiftlint >/dev/null 2>&1 && ! command -v swiftformat >/dev/null 2>&1; then
    if ! grep -Fq "Swift lint skipped" "$TMP_DIR/lint.out"; then
        echo "Expected skipped message when Swift lint tools are unavailable" >&2
        sed 's/^/  /' "$TMP_DIR/lint.out" >&2
        exit 1
    fi
fi

echo "swift lint runner smoke passed"
