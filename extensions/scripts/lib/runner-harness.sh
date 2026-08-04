#!/usr/bin/env bash
# Shared shell-runner harness helpers for extension wrappers.

# Runtime helpers live in Homeboy's source tree. There is exactly one place that
# knows how to find them: runtime-helper-resolver.sh, which honours an explicit
# override, then HOMEBOY_CORE_DIR, then a sibling checkout. This harness used to
# carry its own copy of the sibling path and ignore HOMEBOY_CORE_DIR, so setting
# it resolved some helpers and not others.
homeboy_runner_harness_resolve_helper() {
    local override_variable="$1"
    local helper_name="$2"
    local resolver_dir repo_root resolved

    resolver_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    repo_root="$(cd "${resolver_dir}/.." && pwd)"

    if ! type homeboy_runtime_helper >/dev/null 2>&1; then
        # shellcheck source=./runtime-helper-resolver.sh
        source "${resolver_dir}/runtime-helper-resolver.sh"
    fi

    if resolved="$(homeboy_runtime_helper "$repo_root" "$override_variable" "$helper_name" 2>/dev/null)"; then
        printf '%s\n' "$resolved"
        return 0
    fi

    echo "Error: ${override_variable} is required" >&2
    return 1
}

homeboy_runner_harness_init() {
    local prelude
    prelude="$(homeboy_runner_harness_resolve_helper HOMEBOY_RUNTIME_RUNNER_PRELUDE runner-prelude.sh)" || return 1
    # shellcheck source=/dev/null
    source "$prelude"
    homeboy_runner_init "$@"
}

homeboy_runner_harness_source_command_capture() {
    local helper
    helper="$(homeboy_runner_harness_resolve_helper HOMEBOY_RUNTIME_COMMAND_CAPTURE command-capture.sh)" || return 1
    # shellcheck source=/dev/null
    source "$helper"
}

homeboy_runner_harness_load_adapter() {
    local adapter_path
    adapter_path="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$1.sh"
    # shellcheck source=/dev/null
    source "$adapter_path"
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
