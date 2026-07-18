#!/usr/bin/env bash

homeboy_run_step_capture() {
    local output_var="$1"
    local exit_var="$2"
    local step_name="$3"
    shift 3

    if [ "${1:-}" = "--" ]; then
        shift
    fi

    if [ "$#" -eq 0 ]; then
        printf -v "$output_var" '%s' ''
        printf -v "$exit_var" '%s' '127'
        FAILED_STEP="${step_name}"
        FAILURE_OUTPUT="No command provided"
        return 127
    fi

    local output_file
    output_file="$(mktemp "${TMPDIR:-/tmp}/homeboy-command.XXXXXX")"

    set +e
    "$@" 2>&1 | tee "$output_file"
    local command_exit=${PIPESTATUS[0]}
    set -e

    printf -v "$output_var" '%s' "$output_file"
    printf -v "$exit_var" '%s' "$command_exit"

    if [ "$command_exit" -ne 0 ]; then
        FAILED_STEP="${step_name}"
        local tail_lines="${HOMEBOY_FAILURE_TAIL_LINES:-20}"
        if [ "$tail_lines" -gt 0 ] 2>/dev/null; then
            FAILURE_OUTPUT="$(tail -"$tail_lines" "$output_file")"
        else
            FAILURE_OUTPUT=""
        fi
    fi

    return "$command_exit"
}

homeboy_cleanup_step_capture() {
    local output_file="$1"
    [ -z "$output_file" ] || rm -f "$output_file"
}

homeboy_run_step() {
    local output_file=""
    local command_exit="0"

    homeboy_run_step_capture output_file command_exit "$@" || command_exit=$?
    homeboy_cleanup_step_capture "$output_file"
    return "$command_exit"
}
