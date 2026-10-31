#!/usr/bin/env bash
#
# WP Codebox WordPress DB query profiler helper smoke test (homeboy-extensions#1213).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
ROOT_DIR="$(cd "${EXTENSION_PATH}/.." && pwd)"
# shellcheck source=../../../scripts/lib/runtime-helper-resolver.sh
source "${ROOT_DIR}/scripts/lib/runtime-helper-resolver.sh"
BASH_PREFLIGHT_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_BASH_PREFLIGHT bash-preflight.sh)"
FIXTURE_DIR="${EXTENSION_PATH}/tests/fixtures/bench-query-profiler"

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
    exit 1
fi

RESULTS_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/bench-query-profiler-smoke.XXXXXX")

cleanup() {
    rm -f "$RESULTS_TMPFILE"
}
trap cleanup EXIT

echo "============================================"
echo "WP Codebox bench query profiler smoke test"
echo "============================================"
echo "Fixture:    $FIXTURE_DIR"
echo "Iterations: 2 (per workload)"
echo ""

HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_TMPFILE" \
HOMEBOY_BENCH_ITERATIONS=2 \
HOMEBOY_COMPONENT_ID=bench-query-profiler \
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

query_profiler='.scenarios[] | select(.id == "query-profiler")'

query_count=$(jq -r "$query_profiler | .metrics.query_count_mean // 0" "$RESULTS_TMPFILE")
if ! jq -e --argjson value "$query_count" '$value > 0' >/dev/null; then
    echo "ERROR: expected query_count_mean > 0, got ${query_count}" >&2
    exit 1
fi
echo "✓ query_count_mean > 0"

postmeta_queries=$(jq -r "$query_profiler | .metrics.postmeta_queries_mean // 0" "$RESULTS_TMPFILE")
if ! jq -e --argjson value "$postmeta_queries" '$value > 0' >/dev/null; then
    echo "ERROR: expected postmeta_queries_mean > 0, got ${postmeta_queries}" >&2
    exit 1
fi
echo "✓ postmeta table queries captured"

invariant_failures=$(jq -r "$query_profiler | .metrics.invariant_failure_count_mean // -1" "$RESULTS_TMPFILE")
if [ "$invariant_failures" != "0" ]; then
    echo "ERROR: expected invariant_failure_count_mean == 0, got ${invariant_failures}" >&2
    exit 1
fi
echo "✓ profiler invariants passed"

profile_label=$(jq -r "$query_profiler | .metadata.profile.label // \"missing\"" "$RESULTS_TMPFILE")
if [ "$profile_label" != "wp-core-write-read" ]; then
    echo "ERROR: expected metadata.profile.label == wp-core-write-read, got ${profile_label}" >&2
    exit 1
fi
echo "✓ profile metadata attached"

echo ""
echo "============================================"
echo "✓ WP Codebox query profiler smoke test PASSED"
echo "============================================"
