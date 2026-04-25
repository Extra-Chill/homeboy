#!/usr/bin/env bash

# Shared execution context resolution for Node.js extension scripts.
#
# Resolves EXTENSION_PATH, COMPONENT_PATH, and PROJECT_PATH from Homeboy
# environment variables, with a fallback for direct execution.
#
# Usage: source this file after setting SCRIPT_DIR, then call:
#   homeboy_resolve_context
#
# After calling, these variables are set:
#   EXTENSION_PATH  — path to the Node.js extension
#   COMPONENT_PATH  — path to the component being processed
#   PROJECT_PATH    — alias for COMPONENT_PATH (Node.js convention)
#   COMPONENT_ID    — component identifier (basename if not provided)
#
# Requires SCRIPT_DIR to be set by the calling script:
#   SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

homeboy_resolve_context() {
    if [ -n "${HOMEBOY_EXTENSION_PATH:-}" ]; then
        EXTENSION_PATH="${HOMEBOY_EXTENSION_PATH}"
        COMPONENT_PATH="${HOMEBOY_COMPONENT_PATH:-$(pwd)}"
        PROJECT_PATH="$COMPONENT_PATH"
        COMPONENT_ID="${HOMEBOY_COMPONENT_ID:-$(basename "$COMPONENT_PATH")}"
    else
        # Direct invocation — walk up to find nodejs.json.
        if [ -z "${SCRIPT_DIR:-}" ]; then
            echo "Error: SCRIPT_DIR must be set before calling homeboy_resolve_context" >&2
            return 1
        fi

        local _search_dir="$SCRIPT_DIR"
        EXTENSION_PATH=""
        for _i in 1 2 3 4; do
            _search_dir="$(dirname "$_search_dir")"
            if [ -f "${_search_dir}/nodejs.json" ]; then
                EXTENSION_PATH="$_search_dir"
                break
            fi
        done
        if [ -z "$EXTENSION_PATH" ]; then
            EXTENSION_PATH="$(dirname "$(dirname "$SCRIPT_DIR")")"
        fi

        COMPONENT_PATH="$(pwd)"
        PROJECT_PATH="$COMPONENT_PATH"
        COMPONENT_ID="$(basename "$COMPONENT_PATH")"
    fi

    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: Context resolved — extension=$EXTENSION_PATH, component=$PROJECT_PATH" >&2
    fi
}

# Verify the resolved component is actually a Node.js project.
# Walks up to find package.json (so monorepo subpackages also pass).
homeboy_require_package_json() {
    local _dir="${1:-$PROJECT_PATH}"
    while [ "$_dir" != "/" ] && [ "$_dir" != "." ]; do
        if [ -f "$_dir/package.json" ]; then
            return 0
        fi
        _dir="$(dirname "$_dir")"
    done
    echo "Error: No package.json found at or above ${PROJECT_PATH}" >&2
    echo "Not a Node.js project — cannot run." >&2
    return 1
}

# Detect which package manager the project uses, in priority order:
# pnpm-lock.yaml → pnpm; yarn.lock → yarn; package-lock.json → npm; default npm.
# Sets PKG_MANAGER and PKG_RUN ("pnpm run", "yarn", "npm run", "npx").
homeboy_detect_package_manager() {
    local _dir="${1:-$PROJECT_PATH}"
    if [ -f "$_dir/pnpm-lock.yaml" ]; then
        PKG_MANAGER="pnpm"
        PKG_RUN="pnpm run"
        PKG_EXEC="pnpm exec"
    elif [ -f "$_dir/yarn.lock" ]; then
        PKG_MANAGER="yarn"
        PKG_RUN="yarn"
        PKG_EXEC="yarn"
    else
        PKG_MANAGER="npm"
        PKG_RUN="npm run"
        PKG_EXEC="npx"
    fi

    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: Package manager: $PKG_MANAGER" >&2
    fi
}

# Check whether a script name is defined in package.json.
# Returns 0 if defined, 1 otherwise. Uses node so we don't depend on jq.
homeboy_has_npm_script() {
    local _script="$1"
    local _pkg="${2:-$PROJECT_PATH/package.json}"
    [ -f "$_pkg" ] || return 1
    node -e "
        const pkg = require('$_pkg');
        process.exit(pkg.scripts && pkg.scripts['$_script'] ? 0 : 1);
    " 2>/dev/null
}
