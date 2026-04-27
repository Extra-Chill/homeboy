#!/usr/bin/env bash
# Parse PHPUnit output and ask Homeboy's runtime helper to write test results.
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
# Writes JSON to HOMEBOY_TEST_RESULTS_FILE when the runtime helper is provided.

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

TOTAL=0
PASSED=0
FAILED=0
SKIPPED=0
PARTIAL=""

# Portable helper: extract the number after a label like "Tests: 42"
# Usage: extract_count "label" "text"
# Uses sed instead of grep -P (Perl regex) for macOS compatibility.
extract_count() {
    echo "$2" | sed -n "s/.*$1 *\([0-9][0-9]*\).*/\1/p" | head -1
}

# Pattern 1: "OK (N tests, N assertions)"
if echo "$OUTPUT" | grep -qE 'OK \([0-9]+ test'; then
    TOTAL=$(echo "$OUTPUT" | sed -n 's/.*OK (\([0-9][0-9]*\) test.*/\1/p' | head -1)
    TOTAL="${TOTAL:-0}"
    PASSED="$TOTAL"
    FAILED=0
    SKIPPED=0

# Pattern 2: "Tests: N, Assertions: N, ..." (failure/mixed output)
elif echo "$OUTPUT" | grep -qE '^Tests: [0-9]+'; then
    SUMMARY_LINE=$(echo "$OUTPUT" | grep -E '^Tests: [0-9]+' | tail -1)

    TOTAL=$(extract_count "Tests:" "$SUMMARY_LINE")
    TOTAL="${TOTAL:-0}"

    ERRORS=$(extract_count "Errors:" "$SUMMARY_LINE")
    ERRORS="${ERRORS:-0}"
    FAILURES=$(extract_count "Failures:" "$SUMMARY_LINE")
    FAILURES="${FAILURES:-0}"
    WARNINGS=$(extract_count "Warnings:" "$SUMMARY_LINE")
    WARNINGS="${WARNINGS:-0}"
    SKIP_COUNT=$(extract_count "Skipped:" "$SUMMARY_LINE")
    SKIP_COUNT="${SKIP_COUNT:-0}"
    INCOMPLETE=$(extract_count "Incomplete:" "$SUMMARY_LINE")
    INCOMPLETE="${INCOMPLETE:-0}"
    RISKY=$(extract_count "Risky:" "$SUMMARY_LINE")
    RISKY="${RISKY:-0}"

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

# Write JSON to file if requested by core.
if type homeboy_write_test_results >/dev/null 2>&1; then
    homeboy_write_test_results "$TOTAL" "$PASSED" "$FAILED" "$SKIPPED" "$PARTIAL"
fi
