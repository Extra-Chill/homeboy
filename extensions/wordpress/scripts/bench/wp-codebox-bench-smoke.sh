#!/usr/bin/env bash
#
# WP Codebox bench harness end-to-end smoke test.
#
# Runs the fixture at wordpress/tests/fixtures/bench-noop/ through the bench
# dispatcher and asserts that:
#   - The dispatcher exits 0 with a populated BenchResults JSON envelope.
#   - Both fixture workloads (noop.php, array-fill-1k.php) appear as
#     scenarios in the output.
#   - Every scenario carries the six metric keys homeboy core expects
#     (mean_ms, p50_ms, p95_ms, p99_ms, min_ms, max_ms).
#
# Manual integration test, not part of CI. Run after changes to:
#   - bench-runner-wp-codebox.sh
#   - wp-codebox recipe-run / wordpress.bench
#
# Usage: bash wordpress/scripts/bench/wp-codebox-bench-smoke.sh
# Exit:  0 = harness round-trips end-to-end, non-zero = regression

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
ROOT_DIR="$(cd "${EXTENSION_PATH}/.." && pwd)"
# shellcheck source=../../../scripts/lib/runtime-helper-resolver.sh
source "${ROOT_DIR}/scripts/lib/runtime-helper-resolver.sh"
BASH_PREFLIGHT_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_BASH_PREFLIGHT bash-preflight.sh)"
FIXTURE_DIR="${EXTENSION_PATH}/tests/fixtures/bench-noop"

if [ ! -d "$FIXTURE_DIR" ]; then
    echo "ERROR: fixture not found at $FIXTURE_DIR" >&2
    exit 1
fi

if [ -z "${HOMEBOY_WP_CODEBOX_BIN:-}" ] && ! command -v wp-codebox >/dev/null 2>&1; then
    echo "ERROR: wp-codebox not installed." >&2
    echo "Set HOMEBOY_WP_CODEBOX_BIN or run wordpress/scripts/build/setup.sh" >&2
    exit 1
fi

RESULTS_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/bench-smoke-results.XXXXXX")

echo "============================================"
echo "WP Codebox bench harness smoke test"
echo "============================================"
echo "Fixture:    $FIXTURE_DIR"
echo "Iterations: 5 (per workload)"
echo ""

HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_TMPFILE" \
HOMEBOY_BENCH_ITERATIONS=5 \
HOMEBOY_COMPONENT_ID=bench-noop \
HOMEBOY_COMPONENT_PATH="$FIXTURE_DIR" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_RUNTIME_BASH_PREFLIGHT="$BASH_PREFLIGHT_HELPER" \
    bash "${SCRIPT_DIR}/bench-runner.sh"

if [ ! -s "$RESULTS_TMPFILE" ]; then
    echo "ERROR: results file empty or missing at $RESULTS_TMPFILE" >&2
    exit 1
fi

echo ""
echo "--- Results envelope ---"
cat "$RESULTS_TMPFILE"
echo ""

# Asserts: the envelope round-trips both fixtures with all six metric keys.
# Plain bash so the smoke has no extra deps.
require_field() {
    local field="$1"
    if ! grep -q "\"$field\"" "$RESULTS_TMPFILE"; then
        echo "ERROR: missing field '$field' in results envelope" >&2
        exit 1
    fi
}

require_field "component_id"
require_field "iterations"
require_field "scenarios"
require_field "noop"
require_field "array-fill-1k"
for metric in mean_ms p50_ms p95_ms p99_ms min_ms max_ms; do
    require_field "$metric"
done

# Two scenarios expected: the two fixture workloads.
# Count `"id":` occurrences.
scenario_count=$(grep -c '"id":' "$RESULTS_TMPFILE" || true)
if [ "$scenario_count" -ne 2 ]; then
    echo "ERROR: expected 2 scenarios, got $scenario_count" >&2
    exit 1
fi

rm -f "$RESULTS_TMPFILE"

echo "============================================"
echo "✓ WP Codebox bench harness smoke test PASSED"
echo "============================================"
