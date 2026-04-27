#!/usr/bin/env bash
#
# bench_workloads filter smoke test.
#
# Runs the bench-noop fixture through the Playground bench dispatcher and
# asserts the optional workload filter:
#   - leaves default discovery unchanged when absent;
#   - accepts a comma-separated string setting;
#   - accepts a JSON-array setting;
#   - accepts HOMEBOY_BENCH_WORKLOADS for direct runner invocations;
#   - fails loudly when the filter matches no discovered workload.
#
# Manual integration test, not part of CI. Run after changes to:
#   - bench-runner-playground.sh (settings extraction + template fill)
#   - playground-bench-runner.php (workload discovery/filtering)
#
# Usage: bash wordpress/scripts/bench/playground-bench-workloads-smoke.sh
# Exit:  0 = workload filtering round-trips, non-zero = regression

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
FIXTURE_DIR="${EXTENSION_PATH}/tests/fixtures/bench-noop"

if [ ! -d "$FIXTURE_DIR" ]; then
    echo "ERROR: fixture not found at $FIXTURE_DIR" >&2
    exit 1
fi

if [ ! -f "${EXTENSION_PATH}/node_modules/.bin/wp-playground-cli" ]; then
    echo "ERROR: @wp-playground/cli not installed." >&2
    echo "Run: cd ${EXTENSION_PATH} && npm install" >&2
    exit 1
fi

if [ ! -d "${EXTENSION_PATH}/vendor/wp-phpunit" ]; then
    echo "ERROR: wp-phpunit not installed." >&2
    echo "Run: cd ${EXTENSION_PATH} && composer install" >&2
    exit 1
fi

if ! command -v jq >/dev/null 2>&1; then
    echo "ERROR: jq required for JSON assertions in this smoke." >&2
    echo "Install: brew install jq (macOS) or your package manager." >&2
    exit 1
fi

RESULTS_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/bench-workloads-smoke-results.XXXXXX")
FAILURE_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/bench-workloads-smoke-failure.XXXXXX")

cleanup() {
    rm -f "$RESULTS_TMPFILE" "$FAILURE_TMPFILE"
}
trap cleanup EXIT

run_bench_case() {
    local label="$1"
    local settings_json="$2"
    local env_workloads="$3"
    local expected_ids="$4"

    rm -f "$RESULTS_TMPFILE"

    echo "--- Case: $label ---"
    HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_TMPFILE" \
    HOMEBOY_BENCH_ITERATIONS=1 \
    HOMEBOY_COMPONENT_ID=bench-noop \
    HOMEBOY_COMPONENT_PATH="$FIXTURE_DIR" \
    HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_SETTINGS_JSON="$settings_json" \
    HOMEBOY_BENCH_WORKLOADS="$env_workloads" \
        bash "${SCRIPT_DIR}/bench-runner.sh"

    if [ ! -s "$RESULTS_TMPFILE" ]; then
        echo "ERROR: results file empty or missing for case '$label'" >&2
        exit 1
    fi

    local actual_ids
    actual_ids=$(jq -r '[.scenarios[] | select(.id != "__bootstrap") | .id] | join(",")' "$RESULTS_TMPFILE")
    if [ "$actual_ids" != "$expected_ids" ]; then
        echo "ERROR: case '$label' expected workload IDs '$expected_ids', got '$actual_ids'" >&2
        cat "$RESULTS_TMPFILE" >&2
        exit 1
    fi
    echo "✓ workload IDs: $actual_ids"
}

echo "============================================"
echo "Playground bench workload filter smoke test"
echo "============================================"
echo "Fixture:    $FIXTURE_DIR"
echo "Iterations: 1 (per selected workload)"
echo ""

run_bench_case \
    "no filter runs all workloads" \
    "{}" \
    "" \
    "array-fill-1k,noop"

run_bench_case \
    "comma-separated setting runs one workload" \
    '{"bench_workloads":"noop"}' \
    "" \
    "noop"

run_bench_case \
    "JSON-array setting runs selected workloads in discovery order" \
    '{"bench_workloads":["noop","array-fill-1k"]}' \
    "" \
    "array-fill-1k,noop"

run_bench_case \
    "HOMEBOY_BENCH_WORKLOADS fallback runs one workload" \
    "{}" \
    "array-fill-1k" \
    "array-fill-1k"

echo "--- Case: unknown filter fails clearly ---"
rm -f "$RESULTS_TMPFILE" "$FAILURE_TMPFILE"
set +e
HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_TMPFILE" \
HOMEBOY_BENCH_ITERATIONS=1 \
HOMEBOY_COMPONENT_ID=bench-noop \
HOMEBOY_COMPONENT_PATH="$FIXTURE_DIR" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_SETTINGS_JSON='{"bench_workloads":["missing-workload"]}' \
    bash "${SCRIPT_DIR}/bench-runner.sh" >"$FAILURE_TMPFILE" 2>&1
status=$?
set -e

if [ "$status" -eq 0 ]; then
    echo "ERROR: unknown workload filter unexpectedly succeeded" >&2
    cat "$FAILURE_TMPFILE" >&2
    exit 1
fi
if ! grep -q "bench_workloads matched no workloads" "$FAILURE_TMPFILE"; then
    echo "ERROR: unknown workload failure did not explain the filter miss" >&2
    cat "$FAILURE_TMPFILE" >&2
    exit 1
fi
if ! grep -q "missing-workload" "$FAILURE_TMPFILE" || ! grep -q "array-fill-1k" "$FAILURE_TMPFILE"; then
    echo "ERROR: unknown workload failure did not include requested and available IDs" >&2
    cat "$FAILURE_TMPFILE" >&2
    exit 1
fi
echo "✓ unknown workload fails clearly"

echo ""
echo "============================================"
echo "✓ bench_workloads smoke test PASSED"
echo "============================================"
