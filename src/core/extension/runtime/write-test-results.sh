#!/usr/bin/env bash

# Shared test results writer for extension runner scripts.
#
# Writes the standard test results JSON sidecar and prints a summary to stderr.
# Extensions parse their tool-specific output to extract counts, then call
# this function to write the results in the canonical format.
#
# Usage:
#   source "${HOMEBOY_RUNTIME_WRITE_TEST_RESULTS:-/path/to/fallback}"
#   homeboy_write_test_results <total> <passed> <failed> <skipped> [partial_label]
#
# Arguments:
#   total    — total number of tests
#   passed   — number of passing tests
#   failed   — number of failing tests
#   skipped  — number of skipped/ignored tests
#   partial  — optional label indicating incomplete results (e.g. "testdox-fallback")
#
# Writes to HOMEBOY_TEST_RESULTS_FILE if set. Always prints summary to stderr.

homeboy_write_test_results() {
    local total="${1:-0}"
    local passed="${2:-0}"
    local failed="${3:-0}"
    local skipped="${4:-0}"
    local partial="${5:-}"

    # Write JSON to file if requested
    if [ -n "${HOMEBOY_TEST_RESULTS_FILE:-}" ]; then
        if [ -n "${partial}" ]; then
            cat > "$HOMEBOY_TEST_RESULTS_FILE" << JSONEOF
{
  "total": ${total},
  "passed": ${passed},
  "failed": ${failed},
  "skipped": ${skipped},
  "partial": "${partial}"
}
JSONEOF
        else
            cat > "$HOMEBOY_TEST_RESULTS_FILE" << JSONEOF
{
  "total": ${total},
  "passed": ${passed},
  "failed": ${failed},
  "skipped": ${skipped}
}
JSONEOF
        fi
    fi

    # Print summary to stderr for visibility
    if [ -n "${partial}" ]; then
        echo "[test-results] Total: ${total}, Passed: ${passed}, Failed: ${failed}, Skipped: ${skipped} (${partial})" >&2
    else
        echo "[test-results] Total: ${total}, Passed: ${passed}, Failed: ${failed}, Skipped: ${skipped}" >&2
    fi
}
