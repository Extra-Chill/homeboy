#!/usr/bin/env bash

# Shared BenchResults helpers for shell extension runners.

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
