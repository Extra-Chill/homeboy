#!/usr/bin/env bash

# Shared execution context resolution for WordPress extension scripts.
#
# Resolves EXTENSION_PATH, COMPONENT_PATH, PLUGIN_PATH from Homeboy
# environment variables, with a fallback for direct execution.
#
# Usage: source this file after setting SCRIPT_DIR, then call:
#   homeboy_resolve_context
#
# After calling, these variables are set:
#   EXTENSION_PATH  — path to the WordPress extension
#   COMPONENT_PATH  — path to the component being processed
#   PLUGIN_PATH     — alias for COMPONENT_PATH (WordPress convention)
#   COMPONENT_ID    — component identifier (may be empty for project-level)
#
# Requires SCRIPT_DIR to be set by the calling script (usually via):
#   SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

homeboy_resolve_context() {
    if [ -n "${HOMEBOY_EXTENSION_PATH:-}" ]; then
        # Called through Homeboy extension system
        EXTENSION_PATH="${HOMEBOY_EXTENSION_PATH}"
        COMPONENT_PATH="${HOMEBOY_COMPONENT_PATH:-.}"
        PLUGIN_PATH="$COMPONENT_PATH"
        COMPONENT_ID="${HOMEBOY_COMPONENT_ID:-}"
    else
        # Called directly (e.g., from composer scripts or manual invocation).
        # Derive paths from SCRIPT_DIR — walk up to find the extension root.
        # Extension layout: wordpress/scripts/<subdir>/script.sh
        # So extension root is 2 or 3 levels up from SCRIPT_DIR.
        if [ -z "${SCRIPT_DIR:-}" ]; then
            echo "Error: SCRIPT_DIR must be set before calling homeboy_resolve_context" >&2
            return 1
        fi

        # Try to find wordpress.json to locate extension root.
        local _search_dir="$SCRIPT_DIR"
        EXTENSION_PATH=""
        for _i in 1 2 3 4; do
            _search_dir="$(dirname "$_search_dir")"
            if [ -f "${_search_dir}/wordpress.json" ]; then
                EXTENSION_PATH="$_search_dir"
                break
            fi
        done
        if [ -z "$EXTENSION_PATH" ]; then
            # Fallback: assume scripts/<subdir>/script.sh layout
            EXTENSION_PATH="$(dirname "$(dirname "$SCRIPT_DIR")")"
        fi

        COMPONENT_PATH="$(pwd)"
        PLUGIN_PATH="$COMPONENT_PATH"
        COMPONENT_ID="$(basename "$COMPONENT_PATH")"
    fi

    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: Context resolved — extension=$EXTENSION_PATH, component=$PLUGIN_PATH"
    fi
}
