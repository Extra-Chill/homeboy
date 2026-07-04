#!/usr/bin/env bash
# Shared shell-runner harness helpers for extension wrappers.

homeboy_runner_harness_init() {
    local prelude="${HOMEBOY_RUNTIME_RUNNER_PRELUDE:-}"
    if [ -z "$prelude" ]; then
        local repo_root
        repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
        prelude="${repo_root}/../homeboy/src/core/extension/runtime/runner-prelude.sh"
    fi
    if [ ! -f "$prelude" ]; then
        echo "Error: HOMEBOY_RUNTIME_RUNNER_PRELUDE is required" >&2
        return 1
    fi
    # shellcheck source=/dev/null
    source "$prelude"
    homeboy_runner_init "$@"
}

homeboy_runner_harness_source_command_capture() {
    local helper="${HOMEBOY_RUNTIME_COMMAND_CAPTURE:-}"
    if [ -z "$helper" ]; then
        local repo_root
        repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
        helper="${repo_root}/../homeboy/src/core/extension/runtime/command-capture.sh"
    fi
    if [ ! -f "$helper" ]; then
        echo "Error: HOMEBOY_RUNTIME_COMMAND_CAPTURE is required" >&2
        return 1
    fi
    # shellcheck source=/dev/null
    source "$helper"
}

homeboy_runner_harness_source_if_file() {
    local helper="$1"
    if [ -n "$helper" ] && [ -f "$helper" ]; then
        # shellcheck source=/dev/null
        source "$helper"
    fi
}

homeboy_runner_harness_mktemp() {
    local template="${1:-homeboy-runner.XXXXXX}"
    local tmpdir="${HOMEBOY_CACHE_DIR:-${TMPDIR:-/tmp}}"

    if [ -d "$tmpdir" ] && [ -w "$tmpdir" ]; then
        mktemp "${tmpdir%/}/${template}" 2>/dev/null && return 0
    fi

    mktemp 2>/dev/null
}

homeboy_runner_harness_register_cleanup() {
    local path="$1"
    [ -n "$path" ] || return 0
    HOMEBOY_RUNNER_HARNESS_CLEANUP="${HOMEBOY_RUNNER_HARNESS_CLEANUP:-}${HOMEBOY_RUNNER_HARNESS_CLEANUP:+$'\n'}$path"
    if [ "${HOMEBOY_RUNNER_HARNESS_TRAP_SET:-0}" != "1" ]; then
        trap 'homeboy_runner_harness_cleanup' EXIT
        HOMEBOY_RUNNER_HARNESS_TRAP_SET=1
    fi
}

homeboy_runner_harness_temp() {
    local __var_name="$1"
    local template="${2:-homeboy-runner.XXXXXX}"
    local __tmp
    __tmp="$(homeboy_runner_harness_mktemp "$template")"
    printf -v "$__var_name" '%s' "$__tmp"
    homeboy_runner_harness_register_cleanup "$__tmp"
}

homeboy_runner_harness_cleanup() {
    local path
    while IFS= read -r path; do
        [ -n "$path" ] && rm -f "$path"
    done <<EOF
${HOMEBOY_RUNNER_HARNESS_CLEANUP:-}
EOF
}

homeboy_runner_harness_note_failure() {
    local label="$1"
    local exit_code="$2"
    if [ "$exit_code" -ne 0 ]; then
        FAILED_STEP="${label} (exit ${exit_code})"
    fi
}
