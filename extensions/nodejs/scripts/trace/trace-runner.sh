#!/usr/bin/env bash
set -euo pipefail

# Node.js trace runner for `homeboy trace`.
#
# Dispatches project-owned black-box trace scenarios and writes the trace JSON
# envelope Homeboy core expects.
#
# Standard env vars:
#   HOMEBOY_EXTENSION_PATH        — path to this extension
#   HOMEBOY_COMPONENT_PATH        — path to the Node.js project
#   HOMEBOY_COMPONENT_ID          — component identifier
#   HOMEBOY_TRACE_RESULTS_FILE    — where to write the TraceResults envelope
#   HOMEBOY_TRACE_SCENARIO        — scenario id to run
#   HOMEBOY_TRACE_LIST_ONLY       — when 1, emit scenario inventory only
#   HOMEBOY_TRACE_ARTIFACT_DIR    — artifact directory for scenario output
#   HOMEBOY_TRACE_EXTRA_WORKLOADS — path-delimited rig-owned trace workload files
#   HOMEBOY_RUN_DIR               — run-scoped working directory
#   HOMEBOY_DEBUG                 — verbose output

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BASH_PREFLIGHT_HELPER="${HOMEBOY_RUNTIME_BASH_PREFLIGHT:-${SCRIPT_DIR}/../lib/bash-preflight.sh}"
# shellcheck source=/dev/null
source "$BASH_PREFLIGHT_HELPER"
homeboy_require_bash_version 4

RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${SCRIPT_DIR}/../lib/resolve-context.sh}"
# shellcheck source=/dev/null
source "$RESOLVE_CONTEXT_HELPER"
homeboy_resolve_context
# shellcheck source=../lib/node-helpers.sh
source "${SCRIPT_DIR}/../lib/node-helpers.sh"
homeboy_require_package_json
homeboy_detect_package_manager

RESULTS_FILE="${HOMEBOY_TRACE_RESULTS_FILE:-${PROJECT_PATH}/.node-trace-results.json}"
SCENARIO="${HOMEBOY_TRACE_SCENARIO:-}"
LIST_ONLY="${HOMEBOY_TRACE_LIST_ONLY:-0}"
RUN_DIR="${HOMEBOY_RUN_DIR:-$(dirname "$RESULTS_FILE")}"
ARTIFACT_DIR="${HOMEBOY_TRACE_ARTIFACT_DIR:-${RUN_DIR}/artifacts}"

export HOMEBOY_TRACE_RESULTS_FILE="$RESULTS_FILE"
export HOMEBOY_TRACE_ARTIFACT_DIR="$ARTIFACT_DIR"
export HOMEBOY_RUN_DIR="$RUN_DIR"
export HOMEBOY_COMPONENT_ID="$COMPONENT_ID"
export HOMEBOY_COMPONENT_PATH="$PROJECT_PATH"
export HOMEBOY_NODEJS_INVOCATION_RUNTIME_HELPER="${HOMEBOY_NODEJS_INVOCATION_RUNTIME_HELPER:-${SCRIPT_DIR}/../runtime/invocation-runtime.mjs}"
export HOMEBOY_TRACE_HELPER_DIR="${SCRIPT_DIR}/lib"

mkdir -p "$(dirname "$RESULTS_FILE")" "$RUN_DIR" "$ARTIFACT_DIR"

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: [trace:nodejs] extension=$EXTENSION_PATH" >&2
    echo "DEBUG: [trace:nodejs] project=$PROJECT_PATH" >&2
    echo "DEBUG: [trace:nodejs] component_id=$COMPONENT_ID" >&2
    echo "DEBUG: [trace:nodejs] scenario=${SCENARIO:-<unset>}" >&2
    echo "DEBUG: [trace:nodejs] results=$RESULTS_FILE" >&2
    echo "DEBUG: [trace:nodejs] artifacts=$ARTIFACT_DIR" >&2
    echo "DEBUG: [trace:nodejs] list_only=$LIST_ONLY" >&2
fi

write_trace_envelope() {
    local status="$1"
    local scenario_id="$2"
    local summary="$3"
    local failure="${4:-}"

    node - "$RESULTS_FILE" "$COMPONENT_ID" "$scenario_id" "$status" "$summary" "$failure" <<'NODE'
const fs = require('fs');
const [resultsFile, componentId, scenarioId, status, summary, failure] = process.argv.slice(2);
const envelope = {
  component_id: componentId,
  scenario_id: scenarioId,
  status,
  summary,
  timeline: [],
  assertions: [],
  artifacts: [],
};
if (failure) envelope.failure = failure;
fs.mkdirSync(require('path').dirname(resultsFile), { recursive: true });
fs.writeFileSync(resultsFile, JSON.stringify(envelope, null, 2));
NODE
}

write_list_envelope() {
    local scenario_file="$1"
    node - "$RESULTS_FILE" "$COMPONENT_ID" "$scenario_file" <<'NODE'
const fs = require('fs');
const path = require('path');
const [resultsFile, componentId, scenarioFile] = process.argv.slice(2);
const scenarios = fs.readFileSync(scenarioFile, 'utf8')
  .split(/\r?\n/)
  .filter(Boolean)
  .map((line) => {
    const [id, source] = line.split('\t');
    return { id, source };
  });
fs.mkdirSync(path.dirname(resultsFile), { recursive: true });
fs.writeFileSync(resultsFile, JSON.stringify({
  component_id: componentId,
  scenario_id: '__list__',
  status: 'pass',
  scenarios,
  timeline: [],
  assertions: [],
  artifacts: [],
}, null, 2));
NODE
}

discover_trace_scenarios() {
    local -A seen=()
    local -a workload_files=()
    local file scenario_id source extra_workloads workload

    if [ -d "${PROJECT_PATH}/traces" ]; then
        while IFS= read -r file; do
            scenario_id="$(basename "$file" .trace.mjs)"
            source="traces/$(basename "$file")"
            if [ -z "${seen[$scenario_id]:-}" ]; then
                seen[$scenario_id]=1
                printf '%s\t%s\n' "$scenario_id" "$source"
            fi
        done < <(find "${PROJECT_PATH}/traces" -maxdepth 1 -type f -name '*.trace.mjs' 2>/dev/null | sort)
    fi

    if [ -d "${PROJECT_PATH}/scripts/trace" ]; then
        while IFS= read -r file; do
            scenario_id="$(basename "$file" .mjs)"
            source="scripts/trace/$(basename "$file")"
            if [ -z "${seen[$scenario_id]:-}" ]; then
                seen[$scenario_id]=1
                printf '%s\t%s\n' "$scenario_id" "$source"
            fi
        done < <(find "${PROJECT_PATH}/scripts/trace" -maxdepth 1 -type f -name '*.mjs' 2>/dev/null | sort)
    fi

    extra_workloads="${HOMEBOY_TRACE_EXTRA_WORKLOADS:-}"
    if [ -n "$extra_workloads" ]; then
        IFS=':' read -r -a workload_files <<< "$extra_workloads"
        for workload in "${workload_files[@]}"; do
            [ -n "$workload" ] || continue
            file="$(resolve_extra_trace_file "$workload")"
            [ -f "$file" ] || continue
            if ! scenario_id="$(trace_scenario_id "$file")"; then
                continue
            fi
            source="extra:${file}"
            if [ -z "${seen[$scenario_id]:-}" ]; then
                seen[$scenario_id]=1
                printf '%s\t%s\n' "$scenario_id" "$source"
            fi
        done
    fi
}

trace_scenario_id() {
    local file="$1"
    local name
    name="$(basename "$file")"

    case "$name" in
        *.trace.mjs) printf '%s\n' "${name%.trace.mjs}" ;;
        *.mjs) printf '%s\n' "${name%.mjs}" ;;
        *) return 1 ;;
    esac
}

resolve_extra_trace_file() {
    local file="$1"

    if [[ "$file" = /* ]]; then
        printf '%s\n' "$file"
    else
        printf '%s\n' "${PROJECT_PATH}/${file}"
    fi
}

resolve_trace_scenario() {
    local scenario_id="$1"
    local trace_file="${PROJECT_PATH}/traces/${scenario_id}.trace.mjs"
    local script_file="${PROJECT_PATH}/scripts/trace/${scenario_id}.mjs"

    if [ -f "$trace_file" ]; then
        printf 'node\t%s\t%s\n' "$trace_file" "traces/${scenario_id}.trace.mjs"
        return 0
    fi

    if [ -f "$script_file" ]; then
        printf 'node\t%s\t%s\n' "$script_file" "scripts/trace/${scenario_id}.mjs"
        return 0
    fi

    local extra_workloads="${HOMEBOY_TRACE_EXTRA_WORKLOADS:-}"
    local workload file extra_id
    local -a workload_files=()

    if [ -n "$extra_workloads" ]; then
        IFS=':' read -r -a workload_files <<< "$extra_workloads"
        for workload in "${workload_files[@]}"; do
            [ -n "$workload" ] || continue
            file="$(resolve_extra_trace_file "$workload")"
            [ -f "$file" ] || continue
            if ! extra_id="$(trace_scenario_id "$file")"; then
                continue
            fi
            if [ "$extra_id" = "$scenario_id" ]; then
                printf 'node\t%s\t%s\n' "$file" "extra:${file}"
                return 0
            fi
        done
    fi

    if homeboy_has_npm_script "trace"; then
        printf 'npm\t%s\t%s\n' "trace" "package.json scripts.trace"
        return 0
    fi

    return 1
}

if [ "$LIST_ONLY" = "1" ]; then
    SCENARIO_LIST_FILE="$(mktemp "${TMPDIR:-/tmp}/homeboy-node-trace-scenarios.XXXXXX")"
    trap 'rm -f "$SCENARIO_LIST_FILE"' EXIT
    discover_trace_scenarios > "$SCENARIO_LIST_FILE"
    write_list_envelope "$SCENARIO_LIST_FILE"
    echo "Discovered $(wc -l < "$SCENARIO_LIST_FILE" | tr -d ' ') Node.js trace scenarios."
    exit 0
fi

if [ -z "$SCENARIO" ]; then
    write_trace_envelope "error" "" "No trace scenario specified" "Set HOMEBOY_TRACE_SCENARIO to the scenario id to run."
    echo "ERROR: HOMEBOY_TRACE_SCENARIO is required" >&2
    exit 2
fi

if ! RESOLVED="$(resolve_trace_scenario "$SCENARIO")"; then
    write_trace_envelope "error" "$SCENARIO" "Trace scenario not found" "No project-owned trace scenario matched '${SCENARIO}'. Checked traces/${SCENARIO}.trace.mjs, scripts/trace/${SCENARIO}.mjs, and package.json scripts.trace."
    echo "ERROR: Trace scenario not found: ${SCENARIO}" >&2
    exit 2
fi

IFS=$'\t' read -r RUN_KIND RUN_TARGET RUN_SOURCE <<< "$RESOLVED"

echo "Running Node.js trace scenario..."
echo "  Component: ${COMPONENT_ID} (${PROJECT_PATH})"
echo "  Scenario:  ${SCENARIO}"
echo "  Source:    ${RUN_SOURCE}"
echo "  Artifacts: ${ARTIFACT_DIR}"
echo ""

cd "$PROJECT_PATH"
set +e
case "$RUN_KIND" in
    node)
        node "$RUN_TARGET"
        TRACE_EXIT=$?
        ;;
    npm)
        # shellcheck disable=SC2086 # word-splitting is intentional for package-manager command.
        $PKG_RUN trace -- "$SCENARIO"
        TRACE_EXIT=$?
        ;;
    *)
        echo "ERROR: unknown trace runner kind: $RUN_KIND" >&2
        TRACE_EXIT=2
        ;;
esac
set -e

if [ ! -f "$RESULTS_FILE" ]; then
    if [ $TRACE_EXIT -eq 0 ]; then
        write_trace_envelope "pass" "$SCENARIO" "Trace completed"
    else
        write_trace_envelope "error" "$SCENARIO" "Trace scenario failed" "Scenario '${SCENARIO}' exited with code ${TRACE_EXIT}."
    fi
fi

if [ $TRACE_EXIT -ne 0 ]; then
    echo "ERROR: Trace scenario failed with exit code ${TRACE_EXIT}" >&2
    exit $TRACE_EXIT
fi

echo ""
echo "Node.js trace scenario complete."
