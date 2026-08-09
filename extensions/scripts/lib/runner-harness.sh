#!/usr/bin/env bash
# Shared shell-runner harness helpers for extension wrappers.
#
# Install custom EXIT traps before the first harness temporary registration.
# The harness captures and composes those traps. A later replacement must be
# followed by another registration to be composed.

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

homeboy_runner_harness_temp_root() {
    local tmpdir="${HOMEBOY_CACHE_DIR:-${TMPDIR:-/tmp}}"
    [ -d "$tmpdir" ] && [ -w "$tmpdir" ] || return 1
    (cd "$tmpdir" && pwd -P)
}

homeboy_runner_harness_append_exit_trap() {
    local declaration="$1"
    [ -n "$declaration" ] || return 0
    case $'\n'"${HOMEBOY_RUNNER_HARNESS_EXIT_TRAPS:-}"$'\n' in
        *$'\n'"$declaration"$'\n'*) return 0 ;;
    esac
    HOMEBOY_RUNNER_HARNESS_EXIT_TRAPS="${HOMEBOY_RUNNER_HARNESS_EXIT_TRAPS:-}${HOMEBOY_RUNNER_HARNESS_EXIT_TRAPS:+$'\n'}${declaration}"
}

homeboy_runner_harness_register_owned_directory() {
    local path="$1" identity marker
    identity="$(homeboy_runner_harness_directory_identity "$path")" || return 1
    marker="$(mktemp "${path}/.homeboy-owned.XXXXXX")" || return 1
    HOMEBOY_RUNNER_HARNESS_OWNED_DIRS="${HOMEBOY_RUNNER_HARNESS_OWNED_DIRS:-}${HOMEBOY_RUNNER_HARNESS_OWNED_DIRS:+$'\n'}${path}"$'\t'"${identity}"$'\t'"${marker}"
}

homeboy_runner_harness_directory_identity() {
    local path="$1"
    stat -f '%d:%i' "$path" 2>/dev/null || stat -c '%d:%i' "$path" 2>/dev/null
}

homeboy_runner_harness_is_owned_directory() {
    local path="$1" root parent base owned identity marker
    [ -d "$path" ] && [ ! -L "$path" ] || return 1
    root="$(homeboy_runner_harness_temp_root)" || return 1
    parent="$(cd "$(dirname "$path")" && pwd -P)" || return 1
    base="$(basename "$path")"
    [ "$parent" = "$root" ] || return 1
    case "$base" in homeboy-runner.*) ;; *) return 1 ;; esac
    while IFS=$'\t' read -r owned identity marker; do
        [ "$owned" = "$path" ] || continue
        [ "$(homeboy_runner_harness_directory_identity "$path")" = "$identity" ] || return 1
        [ -f "$marker" ] && [ ! -L "$marker" ] || return 1
        return 0
    done <<EOF
${HOMEBOY_RUNNER_HARNESS_OWNED_DIRS:-}
EOF
    return 1
}

homeboy_runner_harness_register_cleanup() {
    local path="$1"
    local kind="${2:-file}"
    [ -n "$path" ] || return 0
    case "$kind" in
        file) ;;
        directory)
            if ! homeboy_runner_harness_is_owned_directory "$path"; then
                echo "homeboy_runner_harness_register_cleanup: refusing unowned directory: $path" >&2
                return 2
            fi
            ;;
        *)
            echo "homeboy_runner_harness_register_cleanup: unsupported cleanup kind: $kind" >&2
            return 2
            ;;
    esac
    HOMEBOY_RUNNER_HARNESS_CLEANUP="${HOMEBOY_RUNNER_HARNESS_CLEANUP:-}${HOMEBOY_RUNNER_HARNESS_CLEANUP:+$'\n'}${kind}"$'\t'"${path}"
    local current_trap
    current_trap="$(trap -p EXIT)"
    if [ "$current_trap" != "trap -- 'homeboy_runner_harness_exit' EXIT" ]; then
        homeboy_runner_harness_append_exit_trap "$current_trap"
        trap 'homeboy_runner_harness_exit' EXIT
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

homeboy_runner_harness_temp_dir() {
    local __var_name="$1"
    local template="${2:-homeboy-runner.XXXXXX}"
    local __tmp
    local root
    root="$(homeboy_runner_harness_temp_root)" || return 1
    case "$template" in homeboy-runner.XXXXXX) ;; *) return 2 ;; esac
    __tmp="$(mktemp -d "${root}/${template}")" || return 1
    printf -v "$__var_name" '%s' "$__tmp"
    if ! homeboy_runner_harness_register_owned_directory "$__tmp" || ! homeboy_runner_harness_register_cleanup "$__tmp" directory; then
        rm -rf -- "$__tmp"
        return 1
    fi
}

homeboy_runner_harness_forget_cleanup() {
    local forgotten_kind="$1" forgotten_path="$2" kind path retained=""
    while IFS=$'\t' read -r kind path; do
        [ "$kind" = "$forgotten_kind" ] && [ "$path" = "$forgotten_path" ] && continue
        retained="${retained}${retained:+$'\n'}${kind}"$'\t'"${path}"
    done <<EOF
${HOMEBOY_RUNNER_HARNESS_CLEANUP:-}
EOF
    HOMEBOY_RUNNER_HARNESS_CLEANUP="$retained"
}

homeboy_runner_harness_cleanup_path() {
    local kind="$1" path="$2"
    [ -n "$path" ] || return 0
    case "$kind" in
        directory)
            if [ ! -e "$path" ] && [ ! -L "$path" ]; then
                homeboy_runner_harness_forget_cleanup "$kind" "$path"
            elif homeboy_runner_harness_is_owned_directory "$path"; then
                rm -rf -- "$path"
                homeboy_runner_harness_forget_cleanup "$kind" "$path"
            else
                echo "homeboy_runner_harness_cleanup: refusing changed owned directory: $path" >&2
                return 1
            fi
            ;;
        file)
            rm -f -- "$path"
            homeboy_runner_harness_forget_cleanup "$kind" "$path"
            ;;
        *) return 2 ;;
    esac
}

homeboy_runner_harness_cleanup() {
    local kind path
    while IFS=$'\t' read -r kind path; do
        [ -n "$path" ] || continue
        homeboy_runner_harness_cleanup_path "$kind" "$path" || true
    done <<EOF
${HOMEBOY_RUNNER_HARNESS_CLEANUP:-}
EOF
}

homeboy_runner_harness_exit() {
    local status=$?
    local declaration
    trap - EXIT
    set +e
    homeboy_runner_harness_cleanup
    while IFS= read -r declaration; do
        [ -n "$declaration" ] || continue
        (eval "$declaration"; exit "$status")
    done <<EOF
${HOMEBOY_RUNNER_HARNESS_EXIT_TRAPS:-}
EOF
    exit "$status"
}

homeboy_runner_harness_note_failure() {
    local label="$1"
    local exit_code="$2"
    if [ "$exit_code" -ne 0 ]; then
        FAILED_STEP="${label} (exit ${exit_code})"
    fi
}
