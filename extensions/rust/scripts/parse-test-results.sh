#!/usr/bin/env bash
# Parse cargo test output and ask Homeboy's runtime helper to write test results.
#
# Cargo output pattern (one per test binary):
#   test result: ok. 551 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out;
#   test result: FAILED. 540 passed; 11 failed; 2 ignored; 0 measured; 0 filtered out;
#
# Multiple test result lines are aggregated (unit + integration + doc-tests).
#
# Usage: parse-test-results.sh <cargo-output-file>

set -euo pipefail

OUTPUT_FILE="${1:-}"
if [ -z "$OUTPUT_FILE" ] || [ ! -f "$OUTPUT_FILE" ]; then
    exit 0
fi

OUTPUT=$(cat "$OUTPUT_FILE")

WRITE_TEST_RESULTS_HELPER="${HOMEBOY_RUNTIME_WRITE_TEST_RESULTS:-}"
if [ -n "$WRITE_TEST_RESULTS_HELPER" ] && [ -f "$WRITE_TEST_RESULTS_HELPER" ]; then
    # shellcheck source=/dev/null
    source "$WRITE_TEST_RESULTS_HELPER"
fi

# Aggregate all "test result:" lines
TOTAL_PASSED=$( { echo "$OUTPUT" | grep -Eo '[0-9]+ passed' || true; } | awk '{s+=$1} END {print s+0}' )
TOTAL_FAILED=$( { echo "$OUTPUT" | grep -Eo '[0-9]+ failed' || true; } | awk '{s+=$1} END {print s+0}' )
TOTAL_IGNORED=$( { echo "$OUTPUT" | grep -Eo '[0-9]+ ignored' || true; } | awk '{s+=$1} END {print s+0}' )

TOTAL=$((TOTAL_PASSED + TOTAL_FAILED + TOTAL_IGNORED))

# If no test result lines found, exit silently
if [ "$TOTAL" -eq 0 ]; then
    exit 0
fi

# Write JSON to file if requested by core.
if type homeboy_write_test_results >/dev/null 2>&1; then
    homeboy_write_test_results "$TOTAL" "$TOTAL_PASSED" "$TOTAL_FAILED" "$TOTAL_IGNORED"
fi
