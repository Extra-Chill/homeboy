#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER="${SCRIPT_DIR}/playground-runner.php"

assert_contains() {
    local file="$1"
    local expected="$2"
    if ! grep -Fq "$expected" "$file"; then
        echo "Expected $file to contain: $expected" >&2
        sed 's/^/  /' "$file" >&2
        exit 1
    fi
}

assert_contains "$RUNNER" "{{CHANGED_TEST_FILES_JSON}}"
assert_contains "$RUNNER" "pg_filter_changed_tests"
assert_contains "$RUNNER" "changed test scope skipped non-PHPUnit file"
assert_contains "${SCRIPT_DIR}/test-runner-playground.sh" "CHANGED_TEST_FILES_JSON"
assert_contains "${SCRIPT_DIR}/test-runner-playground.sh" "{{CHANGED_TEST_FILES_JSON}}"

php -l "$RUNNER" >/dev/null
bash -n "${SCRIPT_DIR}/test-runner-playground.sh"

echo "Playground changed-scope smoke passed"
