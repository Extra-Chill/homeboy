#!/usr/bin/env bash

set -euo pipefail

HOMEBOY_TRACE_SPAWNED_PIDS=()

trace_launch() {
    local command="$1"
    local stdout_file="${2:-${HOMEBOY_TRACE_ARTIFACT_DIR:-.}/process.stdout.txt}"
    local stderr_file="${3:-${HOMEBOY_TRACE_ARTIFACT_DIR:-.}/process.stderr.txt}"

    mkdir -p "$(dirname "$stdout_file")" "$(dirname "$stderr_file")"
    bash -c "$command" >"$stdout_file" 2>"$stderr_file" &
    local pid=$!
    HOMEBOY_TRACE_SPAWNED_PIDS+=("$pid")
    printf '%s\n' "$pid"
}

trace_process_tree() {
    local pid="$1"
    if command -v ps >/dev/null 2>&1; then
        ps -axo pid=,ppid=,stat=,comm=,args= | awk -v root="$pid" '
            $1 == root { found = 1 }
            found { print }
        '
    else
        printf 'process tree skipped: ps unavailable\n'
    fi
}

trace_tail_log() {
    local source="$1"
    local destination="$2"
    mkdir -p "$(dirname "$destination")"
    if [ -f "$source" ]; then
        cp "$source" "$destination"
    else
        printf 'log unavailable: %s\n' "$source" >"$destination"
    fi
}

trace_cleanup_processes() {
    local pid
    for pid in "${HOMEBOY_TRACE_SPAWNED_PIDS[@]:-}"; do
        if kill -0 "$pid" >/dev/null 2>&1; then
            kill "$pid" >/dev/null 2>&1 || true
        fi
    done
}

trap trace_cleanup_processes EXIT INT TERM
