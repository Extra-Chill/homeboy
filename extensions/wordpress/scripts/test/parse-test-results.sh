#!/usr/bin/env bash
# Parse PHPUnit output and write test results JSON to HOMEBOY_TEST_RESULTS_FILE.
#
# PHPUnit output patterns:
#   OK (481 tests, 1234 assertions)
#   Tests: 533, Assertions: 2100, Failures: 49.
#   Tests: 533, Assertions: 2100, Errors: 10, Failures: 39, Skipped: 3.
#   Tests: 533, Assertions: 2100, Errors: 10, Failures: 39, Warnings: 2, Skipped: 3, Incomplete: 1.
#
# Fallback: when PHPUnit crashes mid-run (e.g., a test calls exit()), the summary
# line is never printed. In --testdox mode, we count ✔/✘ marks as a fallback.
#
# Usage: parse-test-results.sh <phpunit-output-file>
#
# Writes JSON to HOMEBOY_TEST_RESULTS_FILE if set. Always prints summary to stderr.

set -euo pipefail

OUTPUT_FILE="${1:-}"
if [ -z "$OUTPUT_FILE" ] || [ ! -f "$OUTPUT_FILE" ]; then
    exit 0
fi

OUTPUT=$(cat "$OUTPUT_FILE")

TOTAL=0
PASSED=0
FAILED=0
SKIPPED=0
PARTIAL=""

# Pattern 1: "OK (N tests, N assertions)"
if echo "$OUTPUT" | grep -qE 'OK \([0-9]+ test'; then
    TOTAL=$(echo "$OUTPUT" | grep -oP 'OK \(\K[0-9]+' || echo "0")
    PASSED="$TOTAL"
    FAILED=0
    SKIPPED=0

# Pattern 2: "Tests: N, Assertions: N, ..." (failure/mixed output)
elif echo "$OUTPUT" | grep -qE '^Tests: [0-9]+'; then
    SUMMARY_LINE=$(echo "$OUTPUT" | grep -E '^Tests: [0-9]+' | tail -1)

    TOTAL=$(echo "$SUMMARY_LINE" | grep -oP 'Tests: \K[0-9]+' || echo "0")

    ERRORS=$(echo "$SUMMARY_LINE" | grep -oP 'Errors: \K[0-9]+' || echo "0")
    FAILURES=$(echo "$SUMMARY_LINE" | grep -oP 'Failures: \K[0-9]+' || echo "0")
    WARNINGS=$(echo "$SUMMARY_LINE" | grep -oP 'Warnings: \K[0-9]+' || echo "0")
    SKIP_COUNT=$(echo "$SUMMARY_LINE" | grep -oP 'Skipped: \K[0-9]+' || echo "0")
    INCOMPLETE=$(echo "$SUMMARY_LINE" | grep -oP 'Incomplete: \K[0-9]+' || echo "0")
    RISKY=$(echo "$SUMMARY_LINE" | grep -oP 'Risky: \K[0-9]+' || echo "0")

    FAILED=$((ERRORS + FAILURES))
    SKIPPED=$((SKIP_COUNT + INCOMPLETE + RISKY + WARNINGS))
    PASSED=$((TOTAL - FAILED - SKIPPED))

    # Guard against negative passed count
    if [ "$PASSED" -lt 0 ]; then
        PASSED=0
    fi

# Pattern 3 (fallback): count testdox ✔/✘ marks when PHPUnit crashed mid-run.
# PHPUnit prints " ✔ Test name" for passes, " ✘ Test name" for failures in
# --testdox mode. If we see these but no summary line, PHPUnit died before
# printing its summary (e.g., a test called exit()). Count what we have.
elif echo "$OUTPUT" | grep -qE '^ [✔✘]'; then
    PASSED=$(echo "$OUTPUT" | grep -cE '^ ✔' || echo "0")
    FAILED=$(echo "$OUTPUT" | grep -cE '^ ✘' || echo "0")
    TOTAL=$((PASSED + FAILED))
    SKIPPED=0
    PARTIAL="testdox-fallback"
else
    # No recognizable output — exit silently
    exit 0
fi

# Write JSON to file if requested
if [ -n "${HOMEBOY_TEST_RESULTS_FILE:-}" ]; then
    if [ -n "${PARTIAL:-}" ]; then
        cat > "$HOMEBOY_TEST_RESULTS_FILE" << JSONEOF
{
  "total": ${TOTAL},
  "passed": ${PASSED},
  "failed": ${FAILED},
  "skipped": ${SKIPPED},
  "partial": "${PARTIAL}"
}
JSONEOF
    else
        cat > "$HOMEBOY_TEST_RESULTS_FILE" << JSONEOF
{
  "total": ${TOTAL},
  "passed": ${PASSED},
  "failed": ${FAILED},
  "skipped": ${SKIPPED}
}
JSONEOF
    fi
fi

# Print summary to stderr for visibility
if [ -n "${PARTIAL:-}" ]; then
    echo "[test-results] Total: ${TOTAL}, Passed: ${PASSED}, Failed: ${FAILED}, Skipped: ${SKIPPED} (${PARTIAL}: PHPUnit crashed before printing summary)" >&2
else
    echo "[test-results] Total: ${TOTAL}, Passed: ${PASSED}, Failed: ${FAILED}, Skipped: ${SKIPPED}" >&2
fi
