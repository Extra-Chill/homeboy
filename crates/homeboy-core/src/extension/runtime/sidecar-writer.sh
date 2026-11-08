#!/usr/bin/env bash

# Shared JSON sidecar writer for extension runner scripts.
#
# The helper owns the common "sidecar is a JSON array" file mechanics used by
# lint findings, test failures, fix results, and annotation files. Callers still build the
# domain-specific JSON object; Homeboy validates it and writes atomically
# (compact UTF-8, trailing newline) through the running binary exported as
# HOMEBOY_RUNTIME_EXECUTABLE. Typed writes require a target declared in
# structured_sidecars.
#
# Usage:
#   source "${HOMEBOY_RUNTIME_SIDECAR_WRITER}"
#   homeboy_sidecar_emit lint.finding '{"tool":"eslint","message":"...","fingerprint":"..."}'
#   homeboy_sidecar_write test.failures "$failure_json"
#   homeboy_sidecar_merge annotation.phpcs "$tmp_annotations"

homeboy_sidecar_runtime_json() {
    if [ -z "${HOMEBOY_RUNTIME_EXECUTABLE:-}" ]; then
        echo "[sidecar-writer] HOMEBOY_RUNTIME_EXECUTABLE is required" >&2
        return 1
    fi
    "$HOMEBOY_RUNTIME_EXECUTABLE" __homeboy_runtime_json "$@"
}

homeboy_sidecar_write_json_array() {
    [ -n "${1:-}" ] || return 0
    homeboy_sidecar_runtime_json sidecar-write-array "$@"
}

homeboy_sidecar_write_json_object() {
    [ -n "${1:-}" ] || return 0
    homeboy_sidecar_runtime_json sidecar-write-object "$1" "${2:-}"
}

homeboy_sidecar_append_json() {
    [ -n "${1:-}" ] || return 0
    homeboy_sidecar_runtime_json sidecar-append "$1" "${2:-}"
}

homeboy_sidecar_merge_json_array() {
    [ -n "${1:-}" ] && [ -n "${2:-}" ] && [ -s "${2:-}" ] || return 0
    homeboy_sidecar_runtime_json sidecar-merge "$1" "$2"
}

homeboy_sidecar_target_for_type() {
    local type="${1:-}" key target

    case "$type" in
        lint.finding|lint.findings)
            key=lint.findings; target="${HOMEBOY_LINT_FINDINGS_FILE:-}"
            ;;
        test.failure|test.failures)
            key=test.failures; target="${HOMEBOY_TEST_FAILURES_FILE:-}"
            ;;
        test.result|test.results)
            key=test.results; target="${HOMEBOY_TEST_RESULTS_FILE:-}"
            ;;
        fix.result|fix.results)
            key=fix.results; target="${HOMEBOY_FIX_RESULTS_FILE:-}"
            ;;
        annotation.*|annotations.*)
            local name="${type#annotation.}"
            name="${name#annotations.}"
            key=annotations; target="${HOMEBOY_ANNOTATIONS_DIR:-}"
            if [ -n "$target" ]; then
                name="$(printf '%s' "$name" | tr -c 'A-Za-z0-9_.-' '-')"
                target="${target%/}/$name.json"
            fi
            ;;
        *)
            echo "[sidecar-writer] unknown sidecar type: $type" >&2
            return 1
            ;;
    esac
    if [ -z "$target" ]; then
        echo "[sidecar-writer] $key has no target; declare it in structured_sidecars" >&2
        return 1
    fi
    printf '%s\n' "$target"
}

homeboy_sidecar_write() {
    local type="${1:-}"
    shift || true

    local target
    target="$(homeboy_sidecar_target_for_type "$type")" || return 1
    case "$type" in
        test.result|test.results)
            homeboy_sidecar_write_json_object "$target" "${1:-}"
            return $?
            ;;
    esac
    homeboy_sidecar_write_json_array "$target" "$@"
}

homeboy_sidecar_emit() {
    local type="${1:-}"
    local item="${2:-}"

    local target
    target="$(homeboy_sidecar_target_for_type "$type")" || return 1
    homeboy_sidecar_append_json "$target" "$item"
}

homeboy_sidecar_merge() {
    local type="${1:-}"
    local source="${2:-}"

    local target
    target="$(homeboy_sidecar_target_for_type "$type")" || return 1
    homeboy_sidecar_merge_json_array "$target" "$source"
}

homeboy_annotation_file() {
    local source="${1:-}"
    homeboy_sidecar_target_for_type "annotation.$source"
}

homeboy_write_lint_findings() {
    homeboy_sidecar_write lint.findings "$@"
}

homeboy_append_lint_finding() {
    homeboy_sidecar_emit lint.finding "$1"
}

homeboy_merge_lint_findings() {
    homeboy_sidecar_merge lint.findings "$1"
}

homeboy_write_test_failures() {
    homeboy_sidecar_write test.failures "$@"
}

homeboy_write_test_results_json() {
    homeboy_sidecar_write test.results "$1"
}

homeboy_append_test_failure() {
    homeboy_sidecar_emit test.failure "$1"
}

homeboy_merge_test_failures() {
    homeboy_sidecar_merge test.failures "$1"
}

homeboy_write_fix_results() {
    homeboy_sidecar_write fix.results "$@"
}

homeboy_append_fix_result() {
    homeboy_sidecar_emit fix.result "$1"
}

homeboy_merge_fix_results() {
    homeboy_sidecar_merge fix.results "$1"
}

homeboy_write_annotations() {
    local source="${1:-}"
    shift || true

    homeboy_sidecar_write "annotation.$source" "$@"
}

homeboy_append_annotation() {
    local source="${1:-}"
    local item="${2:-}"

    homeboy_sidecar_emit "annotation.$source" "$item"
}

homeboy_merge_annotations() {
    local source="${1:-}"
    local file="${2:-}"

    homeboy_sidecar_merge "annotation.$source" "$file"
}
