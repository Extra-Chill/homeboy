#!/usr/bin/env bash
#
# Rust bench harness end-to-end smoke test.
#
# Runs the fixture at rust/tests/fixtures/bench-noop/ through the bench
# dispatcher and asserts that:
#   - The dispatcher exits 0 with a populated BenchResults JSON envelope.
#   - Both fixture workloads (bench-noop, bench-busy) appear as scenarios.
#   - Every scenario carries the six metric keys homeboy core expects
#     (mean_ms, p50_ms, p95_ms, p99_ms, min_ms, max_ms).
#   - Iteration count matches HOMEBOY_BENCH_ITERATIONS.
#
# Manual integration test, not part of CI. Run after changes to:
#   - bench-runner.sh (the dispatcher)
#   - rust.json (capability registration)
#   - the bench contract (component-side timings_ns/peak_rss_bytes shape)
#
# Usage: bash rust/scripts/bench/rust-bench-smoke.sh
# Exit:  0 = harness round-trips end-to-end, non-zero = regression

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
FIXTURE_DIR="${EXTENSION_PATH}/tests/fixtures/bench-noop"

if [ ! -d "$FIXTURE_DIR" ]; then
    echo "ERROR: fixture not found at $FIXTURE_DIR" >&2
    exit 1
fi

if ! command -v cargo >/dev/null 2>&1; then
    echo "ERROR: cargo not on PATH (install via https://rustup.rs)" >&2
    exit 1
fi

if ! command -v jq >/dev/null 2>&1; then
    echo "ERROR: jq required for smoke assertions" >&2
    exit 1
fi

RESULTS_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/rust-bench-smoke-results.XXXXXX")

# shellcheck disable=SC2064
trap "rm -f '$RESULTS_TMPFILE'" EXIT

echo "============================================"
echo "Rust bench harness smoke test"
echo "============================================"
echo "Fixture:    $FIXTURE_DIR"
echo "Iterations: 5 (per workload)"
echo "Output:     $RESULTS_TMPFILE"
echo

ITERATIONS=5

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_PATH="$FIXTURE_DIR" \
HOMEBOY_COMPONENT_ID="bench-noop-fixture" \
HOMEBOY_BENCH_ITERATIONS="$ITERATIONS" \
HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_TMPFILE" \
    bash "${SCRIPT_DIR}/bench-runner.sh"

echo
echo "── Validating envelope shape ──"

assert() {
    local _label="$1"
    local _actual="$2"
    local _expected="$3"
    if [ "$_actual" = "$_expected" ]; then
        echo "  ✓ $_label: $_actual"
    else
        echo "  ✗ $_label: got '$_actual', expected '$_expected'" >&2
        exit 1
    fi
}

assert "component_id"  "$(jq -r '.component_id' "$RESULTS_TMPFILE")"  "bench-noop-fixture"
assert "iterations"    "$(jq -r '.iterations' "$RESULTS_TMPFILE")"    "$ITERATIONS"

SCENARIO_COUNT="$(jq -r '.scenarios | length' "$RESULTS_TMPFILE")"
if [ "$SCENARIO_COUNT" -lt 2 ]; then
    echo "  ✗ scenarios: expected ≥ 2, got $SCENARIO_COUNT" >&2
    cat "$RESULTS_TMPFILE" >&2
    exit 1
fi
echo "  ✓ scenarios: $SCENARIO_COUNT"

# Every scenario must have the six standard metric keys.
for _required_key in mean_ms p50_ms p95_ms p99_ms min_ms max_ms; do
    MISSING="$(jq -r --arg k "$_required_key" '.scenarios[] | select(.metrics[$k] == null) | .id' "$RESULTS_TMPFILE")"
    if [ -n "$MISSING" ]; then
        echo "  ✗ missing metric '$_required_key' in scenario(s): $MISSING" >&2
        exit 1
    fi
    echo "  ✓ all scenarios have $_required_key"
done

# Every scenario's iteration count must match.
MISMATCH="$(jq -r --argjson n "$ITERATIONS" '.scenarios[] | select(.iterations != $n) | .id' "$RESULTS_TMPFILE")"
if [ -n "$MISMATCH" ]; then
    echo "  ✗ scenario iteration count mismatch: $MISMATCH" >&2
    exit 1
fi
echo "  ✓ all scenarios report iterations=$ITERATIONS"

# All p95 values must be > 0 (sanity check that timing actually happened).
ZERO_TIMINGS="$(jq -r '.scenarios[] | select(.metrics.p95_ms <= 0) | .id' "$RESULTS_TMPFILE")"
if [ -n "$ZERO_TIMINGS" ]; then
    echo "  ✗ scenario(s) with p95_ms <= 0 (no work timed): $ZERO_TIMINGS" >&2
    exit 1
fi
echo "  ✓ all scenarios have p95_ms > 0"

# bench-busy should be slower than bench-noop (sanity check on relative ordering).
NOOP_P95="$(jq -r '.scenarios[] | select(.id == "noop") | .metrics.p95_ms' "$RESULTS_TMPFILE")"
BUSY_P95="$(jq -r '.scenarios[] | select(.id == "busy") | .metrics.p95_ms' "$RESULTS_TMPFILE")"
if [ -n "$NOOP_P95" ] && [ -n "$BUSY_P95" ]; then
    if (( $(awk "BEGIN { print ($BUSY_P95 > $NOOP_P95) }") )); then
        echo "  ✓ relative ordering sane: busy ($BUSY_P95 ms) > noop ($NOOP_P95 ms)"
    else
        echo "  ✗ unexpected ordering: busy ($BUSY_P95 ms) <= noop ($NOOP_P95 ms)" >&2
        exit 1
    fi
fi

echo
echo "============================================"
echo "✓ Smoke test passed"
echo "============================================"
