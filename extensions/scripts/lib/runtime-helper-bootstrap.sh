#!/usr/bin/env bash

# Resolve Homeboy core-owned runtime helpers for direct extension invocation.

homeboy_source_core_runtime_helper() {
    local env_var="$1"
    local helper_name="$2"
    local helper="${!env_var:-}"

    if [ -n "$helper" ] && [ -f "$helper" ]; then
        # shellcheck source=/dev/null
        source "$helper"
        return 0
    fi

    if command -v homeboy >/dev/null 2>&1; then
        helper="$(homeboy runtime helper path --plain "$helper_name" 2>/dev/null || true)"
        if [ -n "$helper" ] && [ -f "$helper" ]; then
            # shellcheck source=/dev/null
            source "$helper"
            return 0
        fi
    fi

    if [ -n "${HOMEBOY_CORE_DIR:-}" ]; then
        helper="${HOMEBOY_CORE_DIR}/src/core/extension/runtime/${helper_name}"
        if [ -f "$helper" ]; then
            # shellcheck source=/dev/null
            source "$helper"
            return 0
        fi
    fi

    echo "Unable to locate Homeboy runtime helper: ${helper_name}" >&2
    echo "Run through homeboy, install a homeboy binary with 'runtime helper path --plain', or set ${env_var}." >&2
    return 2
}
