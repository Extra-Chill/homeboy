#!/usr/bin/env bash

# Resolve the Homeboy extension runner execution context for shell scripts.
# Scripts source this file, then call homeboy_resolve_context before using the
# exported variables.

homeboy_resolve_context() {
    local project_alias="PROJECT_PATH"
    local component_alias=""

    while [ "$#" -gt 0 ]; do
        case "$1" in
            --project-alias)
                project_alias="${2:-}"
                shift 2
                ;;
            --component-alias)
                component_alias="${2:-}"
                shift 2
                ;;
            *)
                echo "homeboy_resolve_context: unknown argument: $1" >&2
                return 2
                ;;
        esac
    done

    if [ -n "${HOMEBOY_EXTENSION_PATH:-}" ]; then
        EXTENSION_PATH="$HOMEBOY_EXTENSION_PATH"
    else
        if [ -z "${SCRIPT_DIR:-}" ]; then
            echo "homeboy_resolve_context: SCRIPT_DIR is required when HOMEBOY_EXTENSION_PATH is unset" >&2
            return 2
        fi

        local search_dir="$SCRIPT_DIR"
        while [ "$search_dir" != "/" ] && [ -n "$search_dir" ]; do
            if compgen -G "$search_dir/*.json" >/dev/null; then
                EXTENSION_PATH="$search_dir"
                break
            fi
            search_dir="$(dirname "$search_dir")"
        done

        if [ -z "${EXTENSION_PATH:-}" ]; then
            echo "homeboy_resolve_context: could not find extension manifest above SCRIPT_DIR=$SCRIPT_DIR" >&2
            return 2
        fi
    fi

    COMPONENT_PATH="${HOMEBOY_COMPONENT_PATH:-$(pwd)}"
    COMPONENT_ID="${HOMEBOY_COMPONENT_ID:-$(basename "$COMPONENT_PATH")}"
    PROJECT_PATH="${HOMEBOY_PROJECT_PATH:-$COMPONENT_PATH}"

    export EXTENSION_PATH COMPONENT_PATH COMPONENT_ID PROJECT_PATH

    if [ -n "$project_alias" ] && [ "$project_alias" != "PROJECT_PATH" ]; then
        export "$project_alias=$PROJECT_PATH"
    fi

    if [ -n "$component_alias" ]; then
        export "$component_alias=$COMPONENT_PATH"
    fi
}
