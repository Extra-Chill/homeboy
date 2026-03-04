#!/usr/bin/env bash
# Parse cargo test output and write test results JSON to HOMEBOY_TEST_RESULTS_FILE.
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

# Aggregate all "test result:" lines
TOTAL_PASSED=$(echo "$OUTPUT" | grep -oP '\d+ passed' | awk '{s+=$1} END {print s+0}')
TOTAL_FAILED=$(echo "$OUTPUT" | grep -oP '\d+ failed' | awk '{s+=$1} END {print s+0}')
TOTAL_IGNORED=$(echo "$OUTPUT" | grep -oP '\d+ ignored' | awk '{s+=$1} END {print s+0}')

TOTAL=$((TOTAL_PASSED + TOTAL_FAILED + TOTAL_IGNORED))

# If no test result lines found, exit silently
if [ "$TOTAL" -eq 0 ]; then
    exit 0
fi

# Write JSON to file if requested
if [ -n "${HOMEBOY_TEST_RESULTS_FILE:-}" ]; then
    cat > "$HOMEBOY_TEST_RESULTS_FILE" << JSONEOF
{
  "total": ${TOTAL},
  "passed": ${TOTAL_PASSED},
  "failed": ${TOTAL_FAILED},
  "skipped": ${TOTAL_IGNORED}
}
JSONEOF
fi

# Print summary to stderr for visibility
echo "[test-results] Total: ${TOTAL}, Passed: ${TOTAL_PASSED}, Failed: ${TOTAL_FAILED}, Skipped: ${TOTAL_IGNORED}" >&2
