#!/usr/bin/env bash
set -euo pipefail

# Node.js bench runner for `homeboy bench`.
#
# Discovers `bench/**/*.bench.{ts,mjs,js}` workloads under the component
# root, runs each via the bench-runner.mjs harness, and writes the
# BenchResults JSON envelope homeboy core expects.
#
# Standard env vars (set by homeboy core):
#   HOMEBOY_EXTENSION_PATH       — path to this extension
#   HOMEBOY_COMPONENT_PATH       — path to the Node.js project
#   HOMEBOY_COMPONENT_ID         — component identifier
#   HOMEBOY_BENCH_ITERATIONS     — iterations per workload (default 10)
#   HOMEBOY_BENCH_RESULTS_FILE   — where core wants the envelope written
#   HOMEBOY_BENCH_LIST_ONLY      — when 1, emit scenario inventory only
#   HOMEBOY_DEBUG                — verbose output

BENCH_SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT_DIR="$BENCH_SCRIPT_DIR"
BASH_PREFLIGHT_HELPER="${HOMEBOY_RUNTIME_BASH_PREFLIGHT:?Homeboy core must provide HOMEBOY_RUNTIME_BASH_PREFLIGHT}"
# shellcheck source=/dev/null
source "$BASH_PREFLIGHT_HELPER"
homeboy_require_bash_version 4

RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:?HOMEBOY_RUNTIME_RESOLVE_CONTEXT is required}"
# shellcheck source=/dev/null
source "$RESOLVE_CONTEXT_HELPER"
homeboy_resolve_context
# shellcheck source=../lib/node-helpers.sh
source "${BENCH_SCRIPT_DIR}/../lib/node-helpers.sh"

homeboy_require_standalone_bench_workloads() {
    local workloads="${HOMEBOY_BENCH_EXTRA_WORKLOADS:-}"
    local workload
    local workload_path
    local -a standalone_workloads

    IFS=':' read -r -a standalone_workloads <<< "$workloads"
    for workload in "${standalone_workloads[@]}"; do
        [ -n "$workload" ] || continue
        workload_path="$workload"
        if [[ "$workload_path" != /* ]]; then
            workload_path="${PROJECT_PATH}/${workload_path}"
        fi
        if [[ "$workload_path" != *.mjs ]] || [ ! -f "$workload_path" ] || [ ! -r "$workload_path" ]; then
            echo "Error: standalone Node.js bench workload must be a readable .mjs file: ${workload_path}" >&2
            return 1
        fi
    done
}

# A rig can provide a standalone .mjs benchmark for a component that is not a
# package, even when an ancestor belongs to an unrelated package. Ordinary
# package-backed runs retain upward package discovery.
if [ -f "${PROJECT_PATH}/package.json" ]; then
    homeboy_require_package_json
elif [ -n "${HOMEBOY_BENCH_EXTRA_WORKLOADS:-}" ]; then
    homeboy_require_standalone_bench_workloads
else
    homeboy_require_package_json
fi

FAILURE_TRAP_HELPER="${HOMEBOY_RUNTIME_FAILURE_TRAP:-}"
BENCH_HELPER_SH="${HOMEBOY_RUNTIME_BENCH_HELPER_SH:?Homeboy core must provide HOMEBOY_RUNTIME_BENCH_HELPER_SH}"
# shellcheck source=/dev/null
if [ -n "$FAILURE_TRAP_HELPER" ] && [ -f "$FAILURE_TRAP_HELPER" ]; then
    source "$FAILURE_TRAP_HELPER"
    homeboy_init_failure_trap
else
    FAILED_STEP=""
    FAILURE_OUTPUT=""
fi
# shellcheck source=/dev/null
if [ -f "$BENCH_HELPER_SH" ]; then
    source "$BENCH_HELPER_SH"
else
    echo "ERROR: Homeboy bench helper not found at ${BENCH_HELPER_SH}" >&2
    exit 2
fi

ITERATIONS="${HOMEBOY_BENCH_ITERATIONS:-10}"
RESULTS_FILE="${HOMEBOY_BENCH_RESULTS_FILE:-${PROJECT_PATH}/.node-bench-results.json}"
BENCH_DIR="${PROJECT_PATH}/bench"

# No workloads → emit an empty-but-valid envelope so core's parser doesn't
# treat the absence as a crash. Rig-declared extra workloads can run without
# an in-tree bench directory, so only skip when both sources are absent.
if [ ! -d "$BENCH_DIR" ] && [ -z "${HOMEBOY_BENCH_EXTRA_WORKLOADS:-}" ]; then
    echo ""
    echo "⚠ No bench/ directory found at ${BENCH_DIR}"
    echo "  Skipping bench run — nothing to measure."
    echo ""
    homeboy_write_empty_bench_results "$COMPONENT_ID" 0 "$RESULTS_FILE"
    exit 0
fi

# Locate a tsx-capable runner. tsx unifies .ts/.mjs/.js workload loading
# so workload authors don't have to think about transpilation. We look for
# tsx in the component's own node_modules first (so it pins to whatever
# version the project uses), then fall back to npx.
TSX_BIN=""
if [ -x "${PROJECT_PATH}/node_modules/.bin/tsx" ]; then
    TSX_BIN="${PROJECT_PATH}/node_modules/.bin/tsx"
elif command -v tsx >/dev/null 2>&1; then
    TSX_BIN="tsx"
elif command -v npx >/dev/null 2>&1; then
    TSX_BIN="npx --yes tsx"
else
    FAILED_STEP="tsx not available"
    FAILURE_OUTPUT="Install tsx: npm i -D tsx (in ${PROJECT_PATH}) or globally."
    exit 1
fi

export HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_FILE"
export HOMEBOY_RUNTIME_BENCH_HELPER_JS="${HOMEBOY_RUNTIME_BENCH_HELPER_JS:?Homeboy core must provide HOMEBOY_RUNTIME_BENCH_HELPER_JS}"
export HOMEBOY_NODEJS_INVOCATION_RUNTIME_HELPER="${HOMEBOY_NODEJS_INVOCATION_RUNTIME_HELPER:-${BENCH_SCRIPT_DIR}/../runtime/invocation-runtime.mjs}"
export HOMEBOY_NODEJS_BROWSER_BENCH_HELPER="${HOMEBOY_NODEJS_BROWSER_BENCH_HELPER:-${BENCH_SCRIPT_DIR}/browser-helper.mjs}"
export HOMEBOY_NODEJS_BENCH_ARTIFACT_CONTEXT="${HOMEBOY_NODEJS_BENCH_ARTIFACT_CONTEXT:-${BENCH_SCRIPT_DIR}/lib/artifact-context.mjs}"
export HOMEBOY_NODEJS_BENCH_REDACTION="${HOMEBOY_NODEJS_BENCH_REDACTION:-${BENCH_SCRIPT_DIR}/lib/redaction.mjs}"
export HOMEBOY_NODEJS_WORKLOAD_UTILS="${HOMEBOY_NODEJS_WORKLOAD_UTILS:-${BENCH_SCRIPT_DIR}/lib/workload-utils.mjs}"
export HOMEBOY_BENCH_ARTIFACTS_DIR="${HOMEBOY_BENCH_ARTIFACTS_DIR:-$(dirname "$RESULTS_FILE")/artifacts}"
export HOMEBOY_COMPONENT_ID="$COMPONENT_ID"
export HOMEBOY_COMPONENT_PATH="$PROJECT_PATH"
export HOMEBOY_BENCH_ITERATIONS="$ITERATIONS"
export HOMEBOY_BENCH_EXTRA_WORKLOADS="${HOMEBOY_BENCH_EXTRA_WORKLOADS:-}"
export HOMEBOY_BENCH_LIST_ONLY="${HOMEBOY_BENCH_LIST_ONLY:-0}"

echo "Running Node.js benchmarks..."
echo "  Component: ${COMPONENT_ID} (${PROJECT_PATH})"
echo "  Iterations: ${ITERATIONS}"

cd "$PROJECT_PATH"

set +e
# shellcheck disable=SC2086 # word-splitting is intentional for npx --yes tsx
$TSX_BIN "${BENCH_SCRIPT_DIR}/bench-runner.mjs"
runner_exit=$?
set -e

if [ $runner_exit -ne 0 ]; then
    FAILED_STEP="bench-runner.mjs exited with code $runner_exit"
    exit $runner_exit
fi

if [ ! -f "$RESULTS_FILE" ]; then
    FAILED_STEP="Bench completed but no results file at $RESULTS_FILE"
    exit 1
fi

echo ""
echo "Node.js bench run complete."
