#!/usr/bin/env bash
# Shared lint-findings sidecar adapter helpers.

homeboy_lint_findings_enabled() {
    [ -n "${HOMEBOY_LINT_FINDINGS_FILE:-}" ]
}

homeboy_lint_findings_merge_file() {
    local findings_file="$1"
    [ -f "$findings_file" ] || return 0

    if type homeboy_sidecar_merge >/dev/null 2>&1; then
        homeboy_sidecar_merge lint.findings "$findings_file"
        return $?
    fi

    if type homeboy_merge_lint_findings >/dev/null 2>&1; then
        homeboy_merge_lint_findings "$findings_file"
        return $?
    fi

    if type homeboy_sidecar_merge_json_array >/dev/null 2>&1; then
        homeboy_sidecar_merge_json_array "${HOMEBOY_LINT_FINDINGS_FILE:-}" "$findings_file"
        return $?
    fi

    echo "Warning: sidecar writer unavailable (HOMEBOY_RUNTIME_SIDECAR_WRITER unset); skipping lint findings sidecar" >&2
    return 0
}

homeboy_lint_findings_require_writer() {
    if type homeboy_sidecar_merge >/dev/null 2>&1 || type homeboy_merge_lint_findings >/dev/null 2>&1 || type homeboy_sidecar_merge_json_array >/dev/null 2>&1; then
        return 0
    fi

    echo "Error: HOMEBOY_RUNTIME_SIDECAR_WRITER is required to write lint findings" >&2
    return 1
}

homeboy_lint_findings_write_empty() {
    if type homeboy_sidecar_write >/dev/null 2>&1; then
        homeboy_sidecar_write lint.findings
        return $?
    fi

    if [ -n "${HOMEBOY_LINT_FINDINGS_FILE:-}" ]; then
        printf '[]\n' > "$HOMEBOY_LINT_FINDINGS_FILE"
    fi
}
