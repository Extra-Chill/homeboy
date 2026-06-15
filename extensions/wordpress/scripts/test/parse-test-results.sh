#!/usr/bin/env bash
# Parse PHPUnit/WP Codebox output through shared Homeboy test-result adapters.
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
# Usage: parse-test-results.sh <phpunit-output-file|wp-codebox-artifact-dir|wp-codebox-test-results.json>
#
# Writes JSON to HOMEBOY_TEST_RESULTS_FILE when the runtime helper is provided.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

OUTPUT_FILE="${1:-}"
if [ -z "$OUTPUT_FILE" ]; then
    exit 0
fi
shift || true

if [ -d "$OUTPUT_FILE" ] && [ -f "$OUTPUT_FILE/files/test-results.json" ]; then
    OUTPUT_FILE="$OUTPUT_FILE/files/test-results.json"
fi

if [ ! -f "$OUTPUT_FILE" ]; then
    exit 0
fi

WRITE_TEST_RESULTS_HELPER="${HOMEBOY_RUNTIME_WRITE_TEST_RESULTS:-}"
if [ -n "$WRITE_TEST_RESULTS_HELPER" ] && [ -f "$WRITE_TEST_RESULTS_HELPER" ]; then
    # shellcheck source=/dev/null
    source "$WRITE_TEST_RESULTS_HELPER"
fi

ADAPTERS_HELPER="${HOMEBOY_RUNTIME_TEST_RESULT_ADAPTERS:-${SCRIPT_DIR}/../lib/test-result-adapters.sh}"
# shellcheck source=../lib/test-result-adapters.sh
source "$ADAPTERS_HELPER"
if [ "$#" -gt 0 ]; then
    homeboy_parse_test_results_with_adapters "$OUTPUT_FILE" "$@"
else
    homeboy_parse_test_results_with_adapters "$OUTPUT_FILE" wp-codebox-json host-smoke phpunit phpunit-testdox
fi
