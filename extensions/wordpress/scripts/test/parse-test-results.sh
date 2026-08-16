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
SHARED_LIB_DIR="${HOMEBOY_SHARED_LIB_DIR:-}"
if [ -z "$SHARED_LIB_DIR" ] && [ -n "${HOMEBOY_EXTENSION_PATH:-}" ] && [ -d "${HOMEBOY_EXTENSION_PATH}/../scripts/lib" ]; then
    SHARED_LIB_DIR="$(cd "${HOMEBOY_EXTENSION_PATH}/../scripts/lib" && pwd)"
fi
SHARED_LIB_DIR="${SHARED_LIB_DIR:-$(cd "${SCRIPT_DIR}/../../../scripts/lib" && pwd)}"

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

ADAPTERS_HELPER="${HOMEBOY_RUNTIME_TEST_RESULT_ADAPTERS:-${SHARED_LIB_DIR}/test-result-adapters.sh}"
# shellcheck source=../../../scripts/lib/test-result-adapters.sh
source "$ADAPTERS_HELPER"
# Shared agent runtimes install beside the extensions directory
# (<homeboy>/agent-runtimes), one level above where a monorepo checkout puts
# them relative to this script. Probe both so an installed extension still
# finds the WP Codebox result adapters instead of silently parsing without
# them (#12585).
WP_CODEBOX_ADAPTERS_HELPER="${HOMEBOY_WP_CODEBOX_TEST_RESULT_ADAPTERS:-}"
if [ -z "$WP_CODEBOX_ADAPTERS_HELPER" ]; then
    for candidate in \
        "${SCRIPT_DIR}/../../../../agent-runtimes/wp-codebox/scripts/lib/test-result-adapters.sh" \
        "${SCRIPT_DIR}/../../../agent-runtimes/wp-codebox/scripts/lib/test-result-adapters.sh"; do
        if [ -f "$candidate" ]; then
            WP_CODEBOX_ADAPTERS_HELPER="$candidate"
            break
        fi
    done
fi
if [ -n "$WP_CODEBOX_ADAPTERS_HELPER" ] && [ -f "$WP_CODEBOX_ADAPTERS_HELPER" ]; then
    # shellcheck source=/dev/null
    source "$WP_CODEBOX_ADAPTERS_HELPER"
fi

homeboy_wordpress_parse_with_adapters() {
    local output_file="$1"
    shift || true
    local generic_adapters=()

    if type homeboy_write_test_results >/dev/null 2>&1 \
        && [ -n "${HOMEBOY_TEST_SHARD_MANIFEST:-}" ] \
        && [ -f "$HOMEBOY_TEST_SHARD_MANIFEST" ] \
        && [ ! -L "$HOMEBOY_TEST_SHARD_MANIFEST" ]; then
        local shard_summary
        shard_summary="$(python3 - "$output_file" "$HOMEBOY_TEST_SHARD_MANIFEST" <<'PY'
import json
import re
import sys

try:
    text = open(sys.argv[1], encoding="utf-8").read()
    manifest = json.load(open(sys.argv[2], encoding="utf-8"))
except (OSError, json.JSONDecodeError):
    sys.exit(0)

matches = re.findall(
    r"^TEST_SHARD_SUMMARY:id=(shard-[1-9][0-9]*) selected=(\d+) routed=(\d+) status=passed$",
    text,
    flags=re.MULTILINE,
)
if matches:
    shard_id, selected_raw, routed_raw = matches[-1]
    selected, routed = int(selected_raw), int(routed_raw)
    expected = len(manifest.get("tests", [])) if isinstance(manifest.get("tests"), list) else 0
    if manifest.get("schema") == "homeboy/test-shard-manifest/v1" and manifest.get("id") == shard_id and selected > 0 and selected == routed == expected:
        print(selected)
PY
)"
        if [ -n "$shard_summary" ]; then
            homeboy_write_test_results "$shard_summary" "$shard_summary" 0 0 "shard-membership"
            return 0
        fi
    fi

    for adapter in "$@"; do
        if [ "$adapter" = "wp-codebox-json" ]; then
            if type homeboy_parse_wp_codebox_test_results >/dev/null 2>&1 && homeboy_parse_wp_codebox_test_results "$output_file"; then
                return 0
            fi
            continue
        fi
        generic_adapters+=("$adapter")
    done

    if [ "${#generic_adapters[@]}" -gt 0 ]; then
        homeboy_parse_test_results_with_adapters "$output_file" "${generic_adapters[@]}"
    fi
}

if [ "$#" -gt 0 ]; then
    homeboy_wordpress_parse_with_adapters "$OUTPUT_FILE" "$@"
else
    homeboy_wordpress_parse_with_adapters "$OUTPUT_FILE" wp-codebox-json host-smoke phpunit phpunit-testdox
fi
