#!/usr/bin/env bash

# Shared BenchResults helpers for shell extension runners.

# scenario slug helper: turn a workload basename into a stable BenchScenario id.
homeboy_bench_scenario_id() {
    local name
    name="$(basename "${1:-}")"
    name="${name%.*}"
    printf '%s' "$name" \
        | sed -E 's/([a-z0-9])([A-Z])/\1-\2/g; s/[^A-Za-z0-9]+/-/g; s/^-+//; s/-+$//' \
        | tr '[:upper:]' '[:lower:]'
}

homeboy_bench_scenario_selected() {
    local scenario="${1:-}"
    local selected="${2:-${HOMEBOY_BENCH_SCENARIOS:-}}"

    [ -n "$scenario" ] || return 1
    [ -z "$selected" ] && return 0
    case ",${selected}," in
        *",${scenario},"*) return 0 ;;
        *) return 1 ;;
    esac
}

# JSON shaping (artifact refs, responsiveness pings, and the BenchResults
# envelope with its R-7 percentile metrics) runs in the running Homeboy binary,
# exported to runners as HOMEBOY_RUNTIME_EXECUTABLE.
homeboy_bench_runtime_json() {
    if [ -z "${HOMEBOY_RUNTIME_EXECUTABLE:-}" ]; then
        echo "homeboy bench helper: HOMEBOY_RUNTIME_EXECUTABLE is required" >&2
        return 2
    fi
    "$HOMEBOY_RUNTIME_EXECUTABLE" __homeboy_runtime_json "$@"
}

homeboy_bench_artifact_ref_json() {
    homeboy_bench_runtime_json bench-artifact-ref "${1:-}" "${2:-}" "${3:-}"
}

homeboy_bench_responsiveness_ping() {
    local file="${HOMEBOY_BENCH_RESPONSIVENESS_FILE:-}"

    [ -n "$file" ] || return 0
    if [ -z "${__homeboy_bench_responsiveness_started_ms:-}" ]; then
        __homeboy_bench_responsiveness_started_ms="$(homeboy_bench_runtime_json bench-now-ms)" || return 2
    fi
    homeboy_bench_runtime_json bench-responsiveness-ping "$file" \
        "$__homeboy_bench_responsiveness_started_ms" "${1:-}"
}

homeboy_write_empty_bench_results() {
    local component_id="${1:-${HOMEBOY_COMPONENT_ID:-}}"
    local iterations="${2:-0}"
    local results_file="${3:-${HOMEBOY_BENCH_RESULTS_FILE:-}}"

    if [ -z "$component_id" ]; then
        echo "homeboy_write_empty_bench_results: component id is required" >&2
        return 2
    fi
    if [ -z "$results_file" ]; then
        echo "homeboy_write_empty_bench_results: HOMEBOY_BENCH_RESULTS_FILE is required" >&2
        return 2
    fi

    mkdir -p "$(dirname "$results_file")"
    printf '{"component_id":"%s","iterations":%s,"scenarios":[]}\n' "$component_id" "$iterations" > "$results_file"
}

homeboy_write_bench_scenario_inventory() {
    homeboy_write_bench_results_from_payload_files --inventory "$@"
}

# Flags: --inventory, --results-file, --component/--component-id, --iterations,
# --metadata-json, --metric-groups-json, --span-definitions-json,
# --artifacts-json, --timeline-json, --extras-file, then `id=payload=file=source`
# scenario records (after an optional `--`). Flags default to
# HOMEBOY_BENCH_RESULTS_FILE, HOMEBOY_COMPONENT_ID, and HOMEBOY_BENCH_ITERATIONS.
homeboy_write_bench_results_from_payload_files() {
    homeboy_bench_runtime_json bench-results \
        --results-file "${HOMEBOY_BENCH_RESULTS_FILE:-}" \
        --component "${HOMEBOY_COMPONENT_ID:-}" \
        --iterations "${HOMEBOY_BENCH_ITERATIONS:-0}" \
        "$@"
}
