#!/usr/bin/env bash
#
# Shared-state bench harness end-to-end smoke test.
#
# Runs the fixture at wordpress/tests/fixtures/bench-shared-state/ through
# the bench dispatcher with HOMEBOY_BENCH_SHARED_STATE set, and asserts:
#   - The dispatcher exits 0 with a populated BenchResults JSON envelope.
#   - The shared-counter workload wrote N lines to <shared>/counter.log
#     (N = iterations + 1 warmup = 5 + 1 = 6 by default).
#   - HOMEBOY_BENCH_INSTANCE_ID and HOMEBOY_BENCH_CONCURRENCY round-trip.
#
# Single-instance only. Multi-instance smoke is exercised by homeboy core
# spawning the dispatcher N times — that's tested at the homeboy CLI level
# via `homeboy bench bench-shared-state --concurrency 2 --shared-state ...`.
#
# Manual integration test, not part of CI. Run after changes to:
#   - bench-runner-playground.sh (shared-state mount + env wiring)
#   - playground-bench-runner.php (constant defines)
#
# Usage: bash wordpress/scripts/bench/playground-bench-shared-state-smoke.sh
# Exit:  0 = harness round-trips end-to-end, non-zero = regression

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
FIXTURE_DIR="${EXTENSION_PATH}/tests/fixtures/bench-shared-state"

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

RESULTS_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/bench-shared-smoke.XXXXXX")
SHARED_STATE_DIR=$(mktemp -d "${TMPDIR:-/tmp}/bench-shared-smoke.XXXXXX")

cleanup() {
    rm -f "$RESULTS_TMPFILE"
    rm -rf "$SHARED_STATE_DIR"
}
trap cleanup EXIT

ITERATIONS=5

echo "============================================"
echo "Playground bench shared-state smoke test"
echo "============================================"
echo "Fixture:       $FIXTURE_DIR"
echo "Shared state:  $SHARED_STATE_DIR"
echo "Iterations:    $ITERATIONS (per workload)"
echo ""

HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_TMPFILE" \
HOMEBOY_BENCH_ITERATIONS="$ITERATIONS" \
HOMEBOY_BENCH_SHARED_STATE="$SHARED_STATE_DIR" \
HOMEBOY_BENCH_INSTANCE_ID=0 \
HOMEBOY_BENCH_CONCURRENCY=1 \
HOMEBOY_COMPONENT_ID=bench-shared-state \
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

# Assert the workload ran end-to-end and wrote to shared state.
COUNTER_LOG="${SHARED_STATE_DIR}/counter.log"
if [ ! -f "$COUNTER_LOG" ]; then
    echo "ERROR: workload did not write counter.log to shared state" >&2
    echo "       (HOMEBOY_BENCH_SHARED_STATE may not be wired into the template)" >&2
    exit 1
fi

LINE_COUNT=$(wc -l < "$COUNTER_LOG" | tr -d ' ')
# iterations + 1 warmup = 6 expected
EXPECTED=$((ITERATIONS + 1))
if [ "$LINE_COUNT" -ne "$EXPECTED" ]; then
    echo "ERROR: expected $EXPECTED lines in counter.log, got $LINE_COUNT" >&2
    cat "$COUNTER_LOG" >&2
    exit 1
fi

# Every line should report instance=0 (this smoke is single-instance) and
# concurrency=1, proving the constants round-trip from the bash env vars
# through sed substitution into the PHP runtime.
if ! grep -q "instance=0 concurrency=1" "$COUNTER_LOG"; then
    echo "ERROR: counter.log lines do not carry expected instance/concurrency tag" >&2
    cat "$COUNTER_LOG" >&2
    exit 1
fi

if ! grep -q '"shared-counter"' "$RESULTS_TMPFILE"; then
    echo "ERROR: shared-counter scenario missing from results envelope" >&2
    exit 1
fi

echo "============================================"
echo "✓ Bench shared-state smoke test PASSED"
echo "  Counter log: $LINE_COUNT lines"
echo "============================================"
