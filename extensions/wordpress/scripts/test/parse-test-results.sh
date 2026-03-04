#!/usr/bin/env bash
# Parse PHPUnit test results from output and write JSON to HOMEBOY_TEST_RESULTS_FILE.
#
# Usage: parse-test-results.sh <phpunit_output_file>
# Env:   HOMEBOY_TEST_RESULTS_FILE — path to write JSON output
#
# PHPUnit output patterns:
#   Success: "OK (N tests, N assertions)"
#   Failure: "Tests: N, Assertions: N, Errors: N, Failures: N, Skipped: N."
#   No tests: "No tests executed!"

set -euo pipefail

PHPUNIT_OUTPUT_FILE="${1:-}"
RESULTS_FILE="${HOMEBOY_TEST_RESULTS_FILE:-}"

if [ -z "$PHPUNIT_OUTPUT_FILE" ] || [ ! -f "$PHPUNIT_OUTPUT_FILE" ]; then
    exit 0
fi

if [ -z "$RESULTS_FILE" ]; then
    exit 0
fi

OUTPUT=$(cat "$PHPUNIT_OUTPUT_FILE")

TOTAL=0
PASSED=0
FAILED=0
SKIPPED=0
ERRORS=0

# Try success pattern: "OK (N tests, N assertions)"
if echo "$OUTPUT" | grep -qE 'OK \([0-9]+ tests?'; then
    TOTAL=$(echo "$OUTPUT" | grep -oE 'OK \(([0-9]+) tests?' | grep -oE '[0-9]+')
    PASSED=$TOTAL
    FAILED=0
    SKIPPED=0
# Try failure pattern: "Tests: N, Assertions: N, Errors: N, Failures: N, Skipped: N."
elif echo "$OUTPUT" | grep -qE '^Tests: [0-9]+'; then
    SUMMARY_LINE=$(echo "$OUTPUT" | grep -E '^Tests: [0-9]+' | tail -1)
    TOTAL=$(echo "$SUMMARY_LINE" | grep -oE 'Tests: ([0-9]+)' | grep -oE '[0-9]+')
    ERRORS=$(echo "$SUMMARY_LINE" | grep -oE 'Errors: ([0-9]+)' | grep -oE '[0-9]+' || echo "0")
    FAILED_COUNT=$(echo "$SUMMARY_LINE" | grep -oE 'Failures: ([0-9]+)' | grep -oE '[0-9]+' || echo "0")
    SKIPPED=$(echo "$SUMMARY_LINE" | grep -oE 'Skipped: ([0-9]+)' | grep -oE '[0-9]+' || echo "0")
    # Handle missing fields (PHPUnit omits zero fields)
    ERRORS=${ERRORS:-0}
    FAILED_COUNT=${FAILED_COUNT:-0}
    SKIPPED=${SKIPPED:-0}
    FAILED=$((ERRORS + FAILED_COUNT))
    PASSED=$((TOTAL - FAILED - SKIPPED))
    if [ "$PASSED" -lt 0 ]; then PASSED=0; fi
fi

# Write JSON
cat > "$RESULTS_FILE" << EOF
{"total":${TOTAL},"passed":${PASSED},"failed":${FAILED},"skipped":${SKIPPED}}
EOF
