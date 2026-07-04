#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SWIFT_DIR="$ROOT_DIR/swift"
HOMEBOY_CORE_DIR="${HOMEBOY_CORE_DIR:-$(cd "${ROOT_DIR}/.." && pwd)/homeboy}"
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${HOMEBOY_CORE_DIR}/src/core/extension/runtime/resolve-context.sh}"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

if [ ! -f "$RESOLVE_CONTEXT_HELPER" ]; then
    echo "Missing resolve context helper: $RESOLVE_CONTEXT_HELPER" >&2
    exit 1
fi

assert_contains() {
    local file="$1"
    local expected="$2"
    if ! grep -Fq "$expected" "$file"; then
        echo "Expected $file to contain: $expected" >&2
        sed 's/^/  /' "$file" >&2
        exit 1
    fi
}

MINIMAL="$TMP_DIR/minimal"
mkdir -p "$MINIMAL"
cat > "$MINIMAL/App.swift" <<'SWIFT'
struct AppModel {
    let title: String
}
SWIFT

HOMEBOY_COMPONENT_PATH="$MINIMAL" HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" bash "$SWIFT_DIR/scripts/validate.sh" > "$TMP_DIR/minimal.out"
assert_contains "$TMP_DIR/minimal.out" "Swift validation passed"

XCODEGEN="$TMP_DIR/xcodegen"
mkdir -p "$XCODEGEN/Homeboy"
cat > "$XCODEGEN/project.yml" <<'YAML'
name: Homeboy
targets:
  Homeboy:
    type: application
    platform: macOS
    sources: Homeboy
YAML
cat > "$XCODEGEN/Homeboy/App.swift" <<'SWIFT'
struct AppModel {
    let title: String
}
SWIFT

HOMEBOY_COMPONENT_PATH="$XCODEGEN" HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" bash "$SWIFT_DIR/scripts/validate.sh" > "$TMP_DIR/xcodegen.out"
assert_contains "$TMP_DIR/xcodegen.out" "Swift validation passed"
if ! command -v xcodegen >/dev/null 2>&1; then
    assert_contains "$TMP_DIR/xcodegen.out" "project.yml found, but xcodegen is not installed"
fi

BAD="$TMP_DIR/bad"
mkdir -p "$BAD"
cat > "$BAD/Broken.swift" <<'SWIFT'
struct Broken {
    let value: String =
}
SWIFT

if HOMEBOY_COMPONENT_PATH="$BAD" HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" bash "$SWIFT_DIR/scripts/validate.sh" > "$TMP_DIR/bad.out" 2>&1; then
    echo "Expected invalid Swift syntax to fail" >&2
    exit 1
fi
assert_contains "$TMP_DIR/bad.out" "error:"

echo "swift validate runner smoke passed"
