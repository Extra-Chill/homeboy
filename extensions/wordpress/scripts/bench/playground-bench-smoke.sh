#!/usr/bin/env bash
#
# Bench harness end-to-end smoke test.
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
#   - bench-runner-playground.sh (the bash dispatcher)
#   - playground-bench-runner.php (the PHP template)
#   - scripts/lib/playground-bootstrap.php (any boot stage edit affects bench too)
#
# Usage: bash wordpress/scripts/bench/playground-bench-smoke.sh
# Exit:  0 = harness round-trips end-to-end, non-zero = regression

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

RESULTS_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/bench-smoke-results.XXXXXX")

echo "============================================"
echo "Playground bench harness smoke test"
echo "============================================"
echo "Fixture:    $FIXTURE_DIR"
echo "Iterations: 5 (per workload)"
echo ""

HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_TMPFILE" \
HOMEBOY_BENCH_ITERATIONS=5 \
HOMEBOY_COMPONENT_ID=bench-noop \
HOMEBOY_COMPONENT_PATH="$FIXTURE_DIR" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    bash "${SCRIPT_DIR}/bench-runner.sh"

if [ ! -s "$RESULTS_TMPFILE" ]; then
    echo "ERROR: results file empty or missing at $RESULTS_TMPFILE" >&2
    exit 1
fi

echo ""
echo "--- Results envelope ---"
cat "$RESULTS_TMPFILE"
echo ""

# Asserts: the envelope round-trips both fixtures with all six metric keys,
# plus the synthetic `__bootstrap` scenario emitted by the bench runner
# (homeboy-extensions#255).
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
require_field "__bootstrap"
require_field "noop"
require_field "array-fill-1k"
for metric in mean_ms p50_ms p95_ms p99_ms min_ms max_ms; do
    require_field "$metric"
done

# Three scenarios expected: __bootstrap + the two fixture workloads.
# Count `"id":` occurrences.
scenario_count=$(grep -c '"id":' "$RESULTS_TMPFILE" || true)
if [ "$scenario_count" -ne 3 ]; then
    echo "ERROR: expected 3 scenarios (__bootstrap + 2 fixtures), got $scenario_count" >&2
    exit 1
fi

rm -f "$RESULTS_TMPFILE"

echo "============================================"
echo "✓ Bench harness smoke test PASSED"
echo "============================================"
