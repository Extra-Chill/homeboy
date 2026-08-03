#!/usr/bin/env bash
# Resolve Homeboy runtime helpers from supported local checkout layouts.

homeboy_runtime_helper() {
    local extensions_root="$1"
    local override_variable="$2"
    local helper_name="$3"
    local override="${!override_variable:-}"
    local core_dir="${HOMEBOY_CORE_DIR:-${extensions_root%/}/../homeboy}"
    local candidate="${core_dir%/}/crates/homeboy-extension/src/runtime/${helper_name}"

    if [ -n "$override" ]; then
        if [ -f "$override" ]; then
            printf '%s\n' "$override"
            return 0
        fi

        echo "Unable to resolve Homeboy runtime helper '${helper_name}'." >&2
        echo "Explicit override ${override_variable} is not a file: ${override}" >&2
        echo "Set ${override_variable}=/path/to/${helper_name}" >&2
        return 1
    fi

    if [ -f "$candidate" ]; then
        printf '%s/%s\n' "$(cd "$(dirname "$candidate")" && pwd)" "$helper_name"
        return 0
    fi

    echo "Unable to resolve Homeboy runtime helper '${helper_name}'." >&2
    echo "Probed: ${candidate}" >&2
    echo "Set ${override_variable}=/path/to/${helper_name}" >&2
    return 1
}
