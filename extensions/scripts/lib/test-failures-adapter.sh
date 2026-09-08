#!/usr/bin/env bash
# Shared test-failure sidecar adapter helpers.

homeboy_test_failures_enabled() {
    [ -n "${HOMEBOY_TEST_FAILURES_FILE:-}" ]
}

homeboy_test_failures_require_writer() {
    if type homeboy_merge_test_failures >/dev/null 2>&1; then
        return 0
    fi

    echo "Error: HOMEBOY_RUNTIME_SIDECAR_WRITER is required to write test failures" >&2
    return 1
}

homeboy_test_failures_merge_file() {
    local failures_file="$1"
    homeboy_test_failures_enabled || return 0
    [ -f "$failures_file" ] || return 0
    homeboy_test_failures_require_writer || return 1
    homeboy_merge_test_failures "$failures_file"
}

homeboy_test_failure_record_json() {
    local namespace="$1"
    local test_id="$2"
    local suite="$3"
    local file="$4"
    local line="$5"
    local message="$6"
    local failure_type="$7"
    local stdout_excerpt="${8:-}"
    local stderr_excerpt="${9:-}"

    local adapter_dir input_file status
    adapter_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    input_file="$(mktemp)" || return 1
    printf '%s\0%s' "$stdout_excerpt" "$stderr_excerpt" > "$input_file"
    if node "$adapter_dir/test-failure-record.mjs" "$namespace" "$test_id" "$suite" "$file" "$line" "$message" "$failure_type" "$input_file"; then
        status=0
    else
        status=$?
    fi
    rm -f "$input_file"
    return "$status"
}

homeboy_test_failure_emit_record_json() {
    local failure_json="$1"
    homeboy_test_failures_enabled || return 0
    if ! type homeboy_sidecar_emit >/dev/null 2>&1; then
        echo "Error: HOMEBOY_RUNTIME_SIDECAR_WRITER is required to write test failures" >&2
        return 1
    fi
    homeboy_sidecar_emit test.failure "$failure_json"
}
