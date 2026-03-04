#!/usr/bin/env bash
# Parse PHPUnit test failures from output and write JSON to HOMEBOY_TEST_FAILURES_FILE.
#
# Usage: parse-test-failures.sh <phpunit_output_file> [component_path]
# Env:   HOMEBOY_TEST_FAILURES_FILE — path to write JSON output

set -euo pipefail

PHPUNIT_OUTPUT_FILE="${1:-}"
COMPONENT_PATH="${2:-}"
FAILURES_FILE="${HOMEBOY_TEST_FAILURES_FILE:-}"

if [ -z "$PHPUNIT_OUTPUT_FILE" ] || [ ! -f "$PHPUNIT_OUTPUT_FILE" ]; then
    exit 0
fi

if [ -z "$FAILURES_FILE" ]; then
    exit 0
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Run PHP parser and write output to failures file
php "${SCRIPT_DIR}/parse-test-failures.php" "$PHPUNIT_OUTPUT_FILE" "$COMPONENT_PATH" > "$FAILURES_FILE" 2>/dev/null

# Validate the output is valid JSON
if [ -f "$FAILURES_FILE" ]; then
    if ! python3 -c "import json; json.load(open('$FAILURES_FILE'))" 2>/dev/null && \
       ! php -r "json_decode(file_get_contents('$FAILURES_FILE'), true); exit(json_last_error() === JSON_ERROR_NONE ? 0 : 1);" 2>/dev/null; then
        # Invalid JSON — remove the file so homeboy core handles gracefully
        rm -f "$FAILURES_FILE"
    fi
fi
