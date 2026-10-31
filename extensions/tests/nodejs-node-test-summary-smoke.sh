#!/usr/bin/env bash
# Regression coverage for the nodejs test runner's `node --test` summary
# parser (#2501): both the TAP and the spec reporter (Node's default) must
# yield real counts instead of falling through to `unknown-runner`.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FIXTURES="${ROOT}/nodejs/tests/fixtures/node-test-results"
# shellcheck source=../nodejs/scripts/lib/node-test-summary.sh
source "${ROOT}/nodejs/scripts/lib/node-test-summary.sh"

failures=0

assert_counts() {
    local fixture="$1" expected="$2" actual
    if ! homeboy_node_test_parse_summary "$(cat "${FIXTURES}/${fixture}")"; then
        echo "FAIL ${fixture}: no node --test summary recognized" >&2
        failures=$((failures + 1))
        return
    fi
    actual="total=${NODE_TEST_TOTAL} passed=${NODE_TEST_PASSED} failed=${NODE_TEST_FAILED} skipped=${NODE_TEST_SKIPPED}"
    if [ "$actual" != "$expected" ]; then
        echo "FAIL ${fixture}: expected '${expected}', got '${actual}'" >&2
        failures=$((failures + 1))
        return
    fi
    echo "ok ${fixture}: ${actual}"
}

assert_counts spec-reporter-issue-2501.txt   'total=1 passed=1 failed=0 skipped=0'
assert_counts spec-reporter-mixed.txt        'total=6 passed=3 failed=1 skipped=2'
assert_counts tap-reporter-mixed.txt         'total=6 passed=3 failed=1 skipped=2'
assert_counts spec-reporter-two-processes.txt 'total=10 passed=10 failed=0 skipped=0'

# ANSI-coloured spec output (a TTY run, or FORCE_COLOR) parses the same way.
coloured=$'\e[34mℹ tests 2\e[39m\n\e[34mℹ pass 2\e[39m\n\e[34mℹ fail 0\e[39m'
if homeboy_node_test_parse_summary "$coloured" && [ "${NODE_TEST_TOTAL}/${NODE_TEST_PASSED}" = "2/2" ]; then
    echo "ok ansi-coloured spec summary"
else
    echo "FAIL ansi-coloured spec summary" >&2
    failures=$((failures + 1))
fi

# Indented subtest lines and unrelated output are not a summary.
if homeboy_node_test_parse_summary $'    # tests 3\nTests passed\nall good'; then
    echo "FAIL indented or unrelated output was parsed as a summary" >&2
    failures=$((failures + 1))
else
    echo "ok non-summary output is rejected"
fi

if [ "$failures" -ne 0 ]; then
    echo "node --test summary smoke: ${failures} failure(s)" >&2
    exit 1
fi
echo "node --test summary smoke passed"
