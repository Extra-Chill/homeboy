#!/usr/bin/env bash
#
# WP Codebox generic WordPress crawl helper smoke test (homeboy-extensions#1118).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
ROOT_DIR="$(cd "${EXTENSION_PATH}/.." && pwd)"
# shellcheck source=../../../scripts/lib/runtime-helper-resolver.sh
source "${ROOT_DIR}/scripts/lib/runtime-helper-resolver.sh"
BASH_PREFLIGHT_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_BASH_PREFLIGHT bash-preflight.sh)"
FIXTURE_DIR="${EXTENSION_PATH}/tests/fixtures/bench-crawl-helper"

if [ ! -d "$FIXTURE_DIR" ]; then
    echo "ERROR: fixture not found at $FIXTURE_DIR" >&2
    exit 1
fi

if [ -z "${HOMEBOY_WP_CODEBOX_BIN:-}" ] && ! command -v wp-codebox >/dev/null 2>&1; then
    echo "ERROR: wp-codebox not installed." >&2
    echo "Set HOMEBOY_WP_CODEBOX_BIN or run wordpress/scripts/build/setup.sh" >&2
    exit 1
fi

if ! command -v jq >/dev/null 2>&1; then
    echo "ERROR: jq required for JSON assertions in this smoke." >&2
    echo "Install: brew install jq (macOS) or your package manager." >&2
    exit 1
fi

RESULTS_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/bench-crawl-helper-smoke.XXXXXX")

cleanup() {
    rm -f "$RESULTS_TMPFILE"
}
trap cleanup EXIT

echo "============================================"
echo "WP Codebox bench crawl helper smoke test"
echo "============================================"
echo "Fixture:    $FIXTURE_DIR"
echo "Iterations: 1"
echo ""

HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_TMPFILE" \
HOMEBOY_BENCH_ITERATIONS=1 \
HOMEBOY_COMPONENT_ID=bench-crawl-helper \
HOMEBOY_COMPONENT_PATH="$FIXTURE_DIR" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_RUNTIME_BASH_PREFLIGHT="$BASH_PREFLIGHT_HELPER" \
    bash "${SCRIPT_DIR}/bench-runner.sh"

if [ ! -s "$RESULTS_TMPFILE" ]; then
    echo "ERROR: results file empty or missing at $RESULTS_TMPFILE" >&2
    exit 1
fi

scenario='.scenarios[] | select(.id == "crawl")'
scenario_count=$(jq -r '.scenarios | length' "$RESULTS_TMPFILE")
if [ "$scenario_count" -ne 1 ]; then
    echo "ERROR: expected 1 fixture workload scenario, got $scenario_count" >&2
    exit 1
fi
echo "✓ scenarios length == 1 fixture workload"

rows_path="$scenario | .metadata.wordpress_bench_crawl.rows"
rows_count=$(jq -r "$rows_path | length" "$RESULTS_TMPFILE")
if [ "$rows_count" -ne 2 ]; then
    echo "ERROR: expected bounded crawl rows length 2, got $rows_count" >&2
    exit 1
fi
echo "✓ max_requests bounds ordered crawl rows"

schema=$(jq -r "$scenario | .metadata.wordpress_bench_crawl.schema" "$RESULTS_TMPFILE")
if [ "$schema" != "homeboy/wordpress-bench-crawl/v1" ]; then
    echo "ERROR: unexpected crawl schema $schema" >&2
    exit 1
fi
echo "✓ crawl metadata schema emitted"

first_route=$(jq -r "$rows_path | .[0].route" "$RESULTS_TMPFILE")
first_request_index=$(jq -r "$rows_path | .[0].request_index" "$RESULTS_TMPFILE")
first_batch_index=$(jq -r "$rows_path | .[0].batch_index" "$RESULTS_TMPFILE")
if [ "$first_route" != "/" ] || [ "$first_request_index" != "0" ] || [ "$first_batch_index" != "7" ]; then
    echo "ERROR: first row did not preserve route/request/batch indexes" >&2
    exit 1
fi
echo "✓ first row preserves route, request index, and batch index"

first_status=$(jq -r "$rows_path | .[0].status" "$RESULTS_TMPFILE")
first_http_status=$(jq -r "$rows_path | .[0].http_status" "$RESULTS_TMPFILE")
if [ "$first_status" != "ok" ] || [ "$first_http_status" -lt 200 ] || [ "$first_http_status" -ge 400 ]; then
    echo "ERROR: first row expected ok 2xx/3xx status, got status=$first_status http=$first_http_status" >&2
    exit 1
fi
echo "✓ first row captures successful HTTP result"

second_status=$(jq -r "$rows_path | .[1].status" "$RESULTS_TMPFILE")
second_http_status=$(jq -r "$rows_path | .[1].http_status" "$RESULTS_TMPFILE")
second_failure=$(jq -r "$rows_path | .[1].failure_message" "$RESULTS_TMPFILE")
if [ "$second_status" != "http_error" ] || [ "$second_http_status" -lt 400 ] || [ "$second_failure" = "null" ]; then
    echo "ERROR: second row expected HTTP failure evidence, got status=$second_status http=$second_http_status failure=$second_failure" >&2
    exit 1
fi
echo "✓ second row captures HTTP failure evidence"

elapsed_rows=$(jq -r "$rows_path | map(select((.elapsed_ms | type) == \"number\" and .elapsed_ms >= 0)) | length" "$RESULTS_TMPFILE")
byte_rows=$(jq -r "$rows_path | map(select(has(\"response_bytes\") and ((.response_bytes | type) == \"number\" or .response_bytes == null))) | length" "$RESULTS_TMPFILE")
if [ "$elapsed_rows" -ne 2 ] || [ "$byte_rows" -ne 2 ]; then
    echo "ERROR: expected elapsed_ms and optional response_bytes on both rows" >&2
    exit 1
fi
echo "✓ rows include elapsed milliseconds and optional response byte counts"

for metric in crawl_requests crawl_successes crawl_failures crawl_elapsed_ms_total; do
    value=$(jq -r "$scenario | .metrics.${metric}.samples.mean // \"missing\"" "$RESULTS_TMPFILE")
    if [ "$value" = "missing" ] || [ "$value" = "null" ]; then
        echo "ERROR: crawl metric ${metric} missing" >&2
        exit 1
    fi
done
echo "✓ crawl metrics aggregate into bench result metrics"

echo ""
echo "============================================"
echo "✓ WP Codebox bench crawl helper smoke test PASSED"
echo "============================================"
