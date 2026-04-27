#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOMEBOY_CORE_DIR="${HOMEBOY_CORE_DIR:-$(cd "${ROOT_DIR}/.." && pwd)/homeboy}"
FAILURE_TRAP_HELPER="${HOMEBOY_RUNTIME_FAILURE_TRAP:-${HOMEBOY_CORE_DIR}/src/core/extension/runtime/failure-trap.sh}"
WRITE_TEST_RESULTS_HELPER="${HOMEBOY_RUNTIME_WRITE_TEST_RESULTS:-${HOMEBOY_CORE_DIR}/src/core/extension/runtime/write-test-results.sh}"

assert_file() {
    local path="$1"
    if [ ! -f "$path" ]; then
        echo "Missing required file: $path" >&2
        exit 1
    fi
}

assert_contains() {
    local file="$1"
    local expected="$2"
    if ! grep -Fq "$expected" "$file"; then
        echo "Expected $file to contain: $expected" >&2
        echo "Actual contents:" >&2
        sed 's/^/  /' "$file" >&2
        exit 1
    fi
}

assert_not_contains() {
    local file="$1"
    local unexpected="$2"
    if grep -Fq "$unexpected" "$file"; then
        echo "Expected $file not to contain: $unexpected" >&2
        echo "Actual contents:" >&2
        sed 's/^/  /' "$file" >&2
        exit 1
    fi
}

assert_file "$FAILURE_TRAP_HELPER"
assert_file "$WRITE_TEST_RESULTS_HELPER"

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

WORDPRESS_OUTPUT="$TMP_DIR/phpunit.txt"
WORDPRESS_RESULTS="$TMP_DIR/wordpress-results.json"
cat > "$WORDPRESS_OUTPUT" <<'EOF'
Tests: 12, Assertions: 30, Errors: 1, Failures: 2, Warnings: 1, Skipped: 3, Incomplete: 1, Risky: 1.
EOF

HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$WRITE_TEST_RESULTS_HELPER" \
HOMEBOY_TEST_RESULTS_FILE="$WORDPRESS_RESULTS" \
    bash "$ROOT_DIR/wordpress/scripts/test/parse-test-results.sh" "$WORDPRESS_OUTPUT" >/dev/null
assert_contains "$WORDPRESS_RESULTS" '"total": 12'
assert_contains "$WORDPRESS_RESULTS" '"passed": 3'
assert_contains "$WORDPRESS_RESULTS" '"failed": 3'
assert_contains "$WORDPRESS_RESULTS" '"skipped": 6'
assert_not_contains "$WORDPRESS_RESULTS" '"partial"'

WORDPRESS_PARTIAL_OUTPUT="$TMP_DIR/phpunit-partial.txt"
WORDPRESS_PARTIAL_RESULTS="$TMP_DIR/wordpress-partial-results.json"
cat > "$WORDPRESS_PARTIAL_OUTPUT" <<'EOF'
 ✔ First test
 ✔ Second test
 ✘ Third test
EOF

HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$WRITE_TEST_RESULTS_HELPER" \
HOMEBOY_TEST_RESULTS_FILE="$WORDPRESS_PARTIAL_RESULTS" \
    bash "$ROOT_DIR/wordpress/scripts/test/parse-test-results.sh" "$WORDPRESS_PARTIAL_OUTPUT" >/dev/null
assert_contains "$WORDPRESS_PARTIAL_RESULTS" '"total": 3'
assert_contains "$WORDPRESS_PARTIAL_RESULTS" '"passed": 2'
assert_contains "$WORDPRESS_PARTIAL_RESULTS" '"failed": 1'
assert_contains "$WORDPRESS_PARTIAL_RESULTS" '"partial": "testdox-fallback"'

RUST_OUTPUT="$TMP_DIR/cargo-test.txt"
RUST_RESULTS="$TMP_DIR/rust-results.json"
cat > "$RUST_OUTPUT" <<'EOF'
test result: ok. 10 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out;
test result: FAILED. 4 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out;
EOF

HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$WRITE_TEST_RESULTS_HELPER" \
HOMEBOY_TEST_RESULTS_FILE="$RUST_RESULTS" \
    bash "$ROOT_DIR/rust/scripts/parse-test-results.sh" "$RUST_OUTPUT" >/dev/null
assert_contains "$RUST_RESULTS" '"total": 17'
assert_contains "$RUST_RESULTS" '"passed": 14'
assert_contains "$RUST_RESULTS" '"failed": 1'
assert_contains "$RUST_RESULTS" '"skipped": 2'

RUST_EMPTY_OUTPUT="$TMP_DIR/cargo-test-empty.txt"
RUST_EMPTY_RESULTS="$TMP_DIR/rust-empty-results.json"
printf 'compiler output without a cargo summary\n' > "$RUST_EMPTY_OUTPUT"
HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$WRITE_TEST_RESULTS_HELPER" \
HOMEBOY_TEST_RESULTS_FILE="$RUST_EMPTY_RESULTS" \
    bash "$ROOT_DIR/rust/scripts/parse-test-results.sh" "$RUST_EMPTY_OUTPUT" >/dev/null
if [ -e "$RUST_EMPTY_RESULTS" ]; then
    echo "Rust parser should not write a sidecar when no test summary exists" >&2
    exit 1
fi

NODE_PROJECT="$TMP_DIR/node-project"
mkdir -p "$NODE_PROJECT"
cat > "$NODE_PROJECT/package.json" <<'EOF'
{"name":"runtime-helper-smoke","scripts":{}}
EOF

set +e
HOMEBOY_RUNTIME_FAILURE_TRAP="$FAILURE_TRAP_HELPER" \
HOMEBOY_EXTENSION_PATH="$ROOT_DIR/nodejs" \
HOMEBOY_COMPONENT_PATH="$NODE_PROJECT" \
HOMEBOY_COMPONENT_ID="runtime-helper-smoke" \
    bash "$ROOT_DIR/nodejs/scripts/build/build-runner.sh" >"$TMP_DIR/node-build.out" 2>&1
NODE_EXIT=$?
set -e
if [ "$NODE_EXIT" -eq 0 ]; then
    echo "Expected node build runner to fail without scripts.build" >&2
    exit 1
fi
assert_contains "$TMP_DIR/node-build.out" 'BUILD FAILED: No build defined'
assert_contains "$TMP_DIR/node-build.out" 'Error details:'

if grep -R "print_failure_summary()" \
    "$ROOT_DIR/nodejs/scripts" \
    "$ROOT_DIR/rust/scripts" \
    "$ROOT_DIR/wordpress/scripts/test" \
    "$ROOT_DIR/wordpress/scripts/bench" >/dev/null; then
    echo "Runner scripts should not define local print_failure_summary functions" >&2
    exit 1
fi

if grep -R "homeboy_write_test_results()" \
    "$ROOT_DIR/nodejs/scripts" \
    "$ROOT_DIR/rust/scripts" \
    "$ROOT_DIR/wordpress/scripts/test" >/dev/null; then
    echo "Extension scripts should not define local homeboy_write_test_results functions" >&2
    exit 1
fi

echo "runtime helper smoke passed"
