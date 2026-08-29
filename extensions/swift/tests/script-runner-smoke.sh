#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SWIFT_DIR="$ROOT_DIR/swift"
HOMEBOY_CORE_DIR="${HOMEBOY_CORE_DIR:-$(cd "${ROOT_DIR}/.." && pwd)/homeboy}"
# shellcheck source=../../scripts/lib/runtime-helper-resolver.sh
source "${ROOT_DIR}/scripts/lib/runtime-helper-resolver.sh"
RUNNER_PRELUDE_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_RUNNER_PRELUDE runner-prelude.sh)" || exit 1
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

assert_contains() {
    local file="$1"
    local expected="$2"
    if ! grep -Fq "$expected" "$file"; then
        echo "Expected $file to contain: $expected" >&2
        sed 's/^/  /' "$file" >&2
        exit 1
    fi
}

FIXTURE="$TMP_DIR/minimal-swift"
mkdir -p "$FIXTURE/tests"
cat > "$FIXTURE/tests/ContractTests.swift" <<'SWIFT'
import Foundation

let fixturesPath = CommandLine.arguments.dropFirst().first ?? ""
guard fixturesPath.hasSuffix("tests") else {
    fatalError("expected tests directory argument, got: \(fixturesPath)")
}

print("contract ok")
SWIFT

HOMEBOY_COMPONENT_PATH="$FIXTURE" HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" bash "$SWIFT_DIR/scripts/test-runner.sh" > "$TMP_DIR/pass.out"
assert_contains "$TMP_DIR/pass.out" "Running: ContractTests.swift"
assert_contains "$TMP_DIR/pass.out" "contract ok"
assert_contains "$TMP_DIR/pass.out" "Results: 1/1 tests passed"

HOMEBOY_COMPONENT_PATH="$FIXTURE" HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" HOMEBOY_SETTINGS_JSON='{"test_type":"script"}' bash "$SWIFT_DIR/scripts/test-runner.sh" > "$TMP_DIR/settings.out"
assert_contains "$TMP_DIR/settings.out" "Results: 1/1 tests passed"

NO_TESTS="$TMP_DIR/no-tests"
mkdir -p "$NO_TESTS"
if HOMEBOY_COMPONENT_PATH="$NO_TESTS" HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" bash "$SWIFT_DIR/scripts/test-runner.sh" > "$TMP_DIR/missing.out" 2>&1; then
    echo "Expected missing tests directory to fail" >&2
    exit 1
fi
assert_contains "$TMP_DIR/missing.out" "Error: No tests/ directory found"

echo "swift script runner smoke passed"
