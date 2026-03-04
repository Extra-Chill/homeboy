#!/usr/bin/env bash
# Parse cargo test results from output and write JSON to HOMEBOY_TEST_RESULTS_FILE.
#
# Usage: parse-test-results.sh <cargo_test_output_file>
# Env:   HOMEBOY_TEST_RESULTS_FILE — path to write JSON output
#
# Cargo test output patterns (one per test binary):
#   "test result: ok. N passed; N failed; N ignored; N measured; N filtered out"
#   "test result: FAILED. N passed; N failed; N ignored; N measured; N filtered out"
#
# Must aggregate across all test result lines.

set -euo pipefail

CARGO_OUTPUT_FILE="${1:-}"
RESULTS_FILE="${HOMEBOY_TEST_RESULTS_FILE:-}"

if [ -z "$CARGO_OUTPUT_FILE" ] || [ ! -f "$CARGO_OUTPUT_FILE" ]; then
    exit 0
fi

if [ -z "$RESULTS_FILE" ]; then
    exit 0
fi

TOTAL_PASSED=0
TOTAL_FAILED=0
TOTAL_IGNORED=0

# Parse all "test result:" lines and aggregate
while IFS= read -r line; do
    if echo "$line" | grep -qE 'test result:'; then
        passed=$(echo "$line" | grep -oE '([0-9]+) passed' | grep -oE '[0-9]+' || echo "0")
        failed=$(echo "$line" | grep -oE '([0-9]+) failed' | grep -oE '[0-9]+' || echo "0")
        ignored=$(echo "$line" | grep -oE '([0-9]+) ignored' | grep -oE '[0-9]+' || echo "0")
        TOTAL_PASSED=$((TOTAL_PASSED + ${passed:-0}))
        TOTAL_FAILED=$((TOTAL_FAILED + ${failed:-0}))
        TOTAL_IGNORED=$((TOTAL_IGNORED + ${ignored:-0}))
    fi
done < "$CARGO_OUTPUT_FILE"

TOTAL=$((TOTAL_PASSED + TOTAL_FAILED + TOTAL_IGNORED))

cat > "$RESULTS_FILE" << EOF
{"total":${TOTAL},"passed":${TOTAL_PASSED},"failed":${TOTAL_FAILED},"skipped":${TOTAL_IGNORED}}
EOF
