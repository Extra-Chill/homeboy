#!/usr/bin/env bash
# shellcheck disable=SC2034

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
homeboy_node_project_scripts_helper() {
    if [ -n "${HOMEBOY_RUNTIME_PROJECT_SCRIPTS:-}" ]; then
        printf '%s\n' "$HOMEBOY_RUNTIME_PROJECT_SCRIPTS"
        return 0
    fi

    local _candidate
    for _candidate in \
        "${HOMEBOY_EXTENSION_PATH:+$(dirname "$HOMEBOY_EXTENSION_PATH")/scripts/lib/project-scripts.sh}" \
        "${SCRIPT_DIR}/../../../scripts/lib/project-scripts.sh" \
        "${SCRIPT_DIR}/project-scripts.sh"; do
        if [ -n "$_candidate" ] && [ -f "$_candidate" ]; then
            printf '%s\n' "$_candidate"
            return 0
        fi
    done

    echo "Error: Unable to locate Homeboy project script helpers for Node.js extension." >&2
    echo "Expected shared helper near HOMEBOY_EXTENSION_PATH, shared helper at ${SCRIPT_DIR}/../../../scripts/lib/project-scripts.sh, or packaged helper at ${SCRIPT_DIR}/project-scripts.sh." >&2
    return 1
}

PROJECT_SCRIPTS_HELPER="$(homeboy_node_project_scripts_helper)"
# shellcheck source=/dev/null
source "$PROJECT_SCRIPTS_HELPER"

# Verify the resolved component is actually a Node.js project.
# Walks up to find package.json (so monorepo subpackages also pass).
homeboy_require_package_json() {
    homeboy_project_init --ecosystem nodejs --path "${1:-$PROJECT_PATH}"
}

# Detect which package manager the project uses, in priority order:
# pnpm-lock.yaml → pnpm; yarn.lock → yarn; package-lock.json → npm; default npm.
# Sets PKG_MANAGER and PKG_RUN ("pnpm run", "yarn", "npm run", "npx").
homeboy_detect_package_manager() {
    if [ -z "${HOMEBOY_PROJECT_ECOSYSTEM:-}" ]; then
        homeboy_project_init --ecosystem nodejs --path "${1:-$PROJECT_PATH}"
    fi
    PKG_MANAGER="$HOMEBOY_PROJECT_PACKAGE_MANAGER"
    PKG_RUN="$HOMEBOY_PROJECT_RUN_CMD"
    PKG_EXEC="$HOMEBOY_PROJECT_EXEC_CMD"
}

# Prepare dependencies for clean runner snapshots. Lab offload intentionally
# excludes node_modules, so package scripts need a deterministic install step.
homeboy_ensure_node_dependencies() {
    [ -n "${1:-}" ] && homeboy_project_init --ecosystem nodejs --path "$1"
    homeboy_project_ensure_dependencies
}

# Check whether a script name is defined in package.json.
# Returns 0 if defined, 1 otherwise. Uses node so we don't depend on jq.
homeboy_has_npm_script() {
    local _script="$1"
    if [ -n "${2:-}" ]; then
        HOMEBOY_PROJECT_SCRIPT_FILE="$2"
    fi
    homeboy_project_has_script "$_script"
}
