#!/usr/bin/env bash
#
# WP Codebox workload-provided custom metrics smoke test (homeboy-extensions#265).
#
# Runs a fixture workload that returns:
#   ['metrics' => ['rows' => 10], 'metadata' => ['phase' => 'warm']]
# and asserts the runner folds those values into the scenario's BenchResults
# metrics object without changing the historical duration metric keys.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
ROOT_DIR="$(cd "${EXTENSION_PATH}/.." && pwd)"
# shellcheck source=../../../scripts/lib/runtime-helper-resolver.sh
source "${ROOT_DIR}/scripts/lib/runtime-helper-resolver.sh"
BASH_PREFLIGHT_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_BASH_PREFLIGHT bash-preflight.sh)"
FIXTURE_DIR="${EXTENSION_PATH}/tests/fixtures/bench-custom-metrics"

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

RESULTS_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/bench-custom-metrics-smoke.XXXXXX")

cleanup() {
    rm -f "$RESULTS_TMPFILE"
}
trap cleanup EXIT

echo "============================================"
echo "WP Codebox bench custom metrics smoke test"
echo "============================================"
echo "Fixture:    $FIXTURE_DIR"
echo "Iterations: 3 (per workload)"
echo ""

HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_TMPFILE" \
HOMEBOY_BENCH_ITERATIONS=3 \
HOMEBOY_COMPONENT_ID=bench-custom-metrics \
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

scenario_count=$(jq -r '.scenarios | length' "$RESULTS_TMPFILE")
if [ "$scenario_count" -ne 2 ]; then
    echo "ERROR: expected 2 fixture workload scenarios, got $scenario_count" >&2
    exit 1
fi
echo "✓ scenarios length == 2 fixture workloads"

custom='.scenarios[] | select(.id == "custom-metrics")'

for metric in mean_ms p50_ms p95_ms p99_ms min_ms max_ms; do
    value=$(jq -r "$custom | .metrics.${metric} // \"missing\"" "$RESULTS_TMPFILE")
    if [ "$value" = "missing" ] || [ "$value" = "null" ]; then
        echo "ERROR: custom-metrics missing historical duration metric ${metric}" >&2
        exit 1
    fi
done
echo "✓ historical duration metric keys preserved"

for metric in rows_mean rows_p50 rows_p95 rows_p99 rows_min rows_max; do
    value=$(jq -r "$custom | .metrics.${metric} // \"missing\"" "$RESULTS_TMPFILE")
    if [ "$value" = "missing" ] || [ "$value" = "null" ]; then
        echo "ERROR: custom-metrics missing custom metric ${metric}" >&2
        exit 1
    fi
    if [ "$value" != "10" ]; then
        echo "ERROR: custom-metrics ${metric} expected 10, got ${value}" >&2
        exit 1
    fi
done
echo "✓ custom rows metric aggregated into mean/p50/p95/p99/min/max"

for metric in changed_files_mean changed_files_p50 changed_files_p95 changed_files_p99 changed_files_min changed_files_max; do
    value=$(jq -r "$custom | .metrics.${metric} // \"missing\"" "$RESULTS_TMPFILE")
    if [ "$value" != "3" ]; then
        echo "ERROR: custom-metrics ${metric} expected 3, got ${value}" >&2
        exit 1
    fi
done
echo "✓ custom changed_files metric aggregated into mean/p50/p95/p99/min/max"

ignored=$(jq -r "$custom | .metrics.ignored_label_mean // \"missing\"" "$RESULTS_TMPFILE")
if [ "$ignored" != "missing" ]; then
    echo "ERROR: non-numeric metric ignored_label was emitted as ${ignored}" >&2
    exit 1
fi
echo "✓ non-numeric custom metric values ignored"

phase=$(jq -r "$custom | .metadata.phase // \"missing\"" "$RESULTS_TMPFILE")
if [ "$phase" != "warm" ]; then
    echo "ERROR: expected metadata.phase == warm, got ${phase}" >&2
    exit 1
fi
echo "✓ latest metadata attached to scenario"

legacy_has_metadata=$(jq -r '.scenarios[] | select(.id == "legacy-array") | has("metadata")' "$RESULTS_TMPFILE")
if [ "$legacy_has_metadata" != "false" ]; then
    echo "ERROR: legacy-array scenario unexpectedly emitted metadata" >&2
    exit 1
fi

legacy_metric=$(jq -r '.scenarios[] | select(.id == "legacy-array") | .metrics.kind_mean // "missing"' "$RESULTS_TMPFILE")
if [ "$legacy_metric" != "missing" ]; then
    echo "ERROR: legacy array key was treated as a custom metric" >&2
    exit 1
fi
echo "✓ legacy arrays without metrics key are ignored"

echo ""
echo "============================================"
echo "✓ WP Codebox custom metrics smoke test PASSED"
echo "============================================"
