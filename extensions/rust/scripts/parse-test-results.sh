#!/usr/bin/env bash
# Parse cargo test output through the shared Homeboy test-result adapter.
#
# Usage: parse-test-results.sh <cargo-output-file>

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

OUTPUT_FILE="${1:-}"
if [ -z "$OUTPUT_FILE" ] || [ ! -f "$OUTPUT_FILE" ]; then
    exit 0
fi

WRITE_TEST_RESULTS_HELPER="${HOMEBOY_RUNTIME_WRITE_TEST_RESULTS:-}"
if [ -n "$WRITE_TEST_RESULTS_HELPER" ] && [ -f "$WRITE_TEST_RESULTS_HELPER" ]; then
    # shellcheck source=/dev/null
    source "$WRITE_TEST_RESULTS_HELPER"
fi

ADAPTERS_HELPER="${HOMEBOY_RUNTIME_TEST_RESULT_ADAPTERS:-${HOMEBOY_EXTENSION_ROOT:-$(cd "${SCRIPT_DIR}/../.." && pwd)}/scripts/lib/test-result-adapters.sh}"
# shellcheck source=../../scripts/lib/test-result-adapters.sh
source "$ADAPTERS_HELPER"
homeboy_parse_test_results_with_adapters "$OUTPUT_FILE" cargo-test
