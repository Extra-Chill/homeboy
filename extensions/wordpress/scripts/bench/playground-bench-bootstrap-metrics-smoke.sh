#!/usr/bin/env bash
#
# Bootstrap-stage timings smoke test (homeboy-extensions#255).
#
# Runs the bench-noop fixture and asserts the synthetic `__bootstrap`
# scenario is emitted with the expected shape:
#   - scenarios[0].id == "__bootstrap" (ordering: must be first)
#   - scenarios[0].iterations == 1
#   - scenarios[0].metrics has boot_ms, install_ms, load_deps_ms,
#     load_component_ms, activation_ms (homeboy-extensions#431)
#   - all four metric values are positive floats (> 0)
#   - subsequent fixture scenarios still appear correctly
#
# This locks in the contract that bootstrap stage timings are measured
# once per bench-runner invocation (Playground boots once, runs every
# workload inside the booted process) and surfaced as a single-iteration
# scenario for cross-run / cross-rig comparison via homeboy core's
# baseline + regression-detection machinery.
#
# Manual integration test, not part of CI. Run after changes to:
#   - scripts/lib/playground-bootstrap.php (pg_stage_begin/ok/durations_ms)
#   - scripts/bench/playground-bench-runner.php (the synthetic-scenario emit)
#
# Usage: bash wordpress/scripts/bench/playground-bench-bootstrap-metrics-smoke.sh
# Exit:  0 = bootstrap metrics round-trip, non-zero = regression

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

RESULTS_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/bench-bootstrap-metrics-smoke.XXXXXX")

cleanup() {
    rm -f "$RESULTS_TMPFILE"
}
trap cleanup EXIT

echo "============================================"
echo "Bootstrap stage timings smoke test (#255)"
echo "============================================"
echo "Fixture:    $FIXTURE_DIR"
echo "Iterations: 3 (per workload; bootstrap is always 1)"
echo ""

HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_TMPFILE" \
HOMEBOY_BENCH_ITERATIONS=3 \
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

# --- Assertion 1: scenarios[0].id == "__bootstrap" (ordering matters) ---
first_id=$(jq -r '.scenarios[0].id' "$RESULTS_TMPFILE")
if [ "$first_id" != "__bootstrap" ]; then
    echo "ERROR: expected scenarios[0].id == '__bootstrap', got '$first_id'" >&2
    exit 1
fi
echo "✓ scenarios[0].id == '__bootstrap'"

# --- Assertion 2: scenarios[0].iterations == 1 ---
first_iters=$(jq -r '.scenarios[0].iterations' "$RESULTS_TMPFILE")
if [ "$first_iters" != "1" ]; then
    echo "ERROR: expected scenarios[0].iterations == 1, got '$first_iters'" >&2
    exit 1
fi
echo "✓ scenarios[0].iterations == 1"

# --- Assertion 3: all bootstrap metric keys present + positive floats ---
# activation_ms was added in homeboy-extensions#431 — the post-install activation
# stage runs once per bench process (regardless of plugin count), so its timing
# is always present in the __bootstrap synthetic scenario.
for metric in boot_ms install_ms load_deps_ms load_component_ms activation_ms; do
    val=$(jq -r ".scenarios[0].metrics.${metric} // \"missing\"" "$RESULTS_TMPFILE")
    if [ "$val" = "missing" ] || [ "$val" = "null" ]; then
        echo "ERROR: scenarios[0].metrics.${metric} missing" >&2
        exit 1
    fi
    # jq compare: > 0 against the raw numeric value.
    is_positive=$(jq -r ".scenarios[0].metrics.${metric} > 0" "$RESULTS_TMPFILE")
    if [ "$is_positive" != "true" ]; then
        echo "ERROR: scenarios[0].metrics.${metric} = $val, expected > 0" >&2
        exit 1
    fi
    echo "✓ scenarios[0].metrics.${metric} = ${val} (> 0)"
done

# --- Assertion 4: subsequent fixture scenarios still appear ---
# bench-noop fixture has 2 workloads (noop + array-fill-1k); plus __bootstrap = 3.
scenario_count=$(jq -r '.scenarios | length' "$RESULTS_TMPFILE")
if [ "$scenario_count" -ne 3 ]; then
    echo "ERROR: expected 3 scenarios (__bootstrap + 2 fixtures), got $scenario_count" >&2
    exit 1
fi
echo "✓ scenarios length == 3 (__bootstrap + 2 fixtures)"

# --- Assertion 5: fixture scenarios carry their original metric shape ---
# Spot-check scenario[1] has the standard p50_ms key (not bootstrap-shape).
fixture_p50=$(jq -r '.scenarios[1].metrics.p50_ms // "missing"' "$RESULTS_TMPFILE")
if [ "$fixture_p50" = "missing" ] || [ "$fixture_p50" = "null" ]; then
    echo "ERROR: scenarios[1] missing p50_ms (workload metric shape lost)" >&2
    exit 1
fi
echo "✓ scenarios[1].metrics.p50_ms present (workload shape preserved)"

echo ""
echo "============================================"
echo "✓ Bootstrap metrics smoke test PASSED"
echo "============================================"
