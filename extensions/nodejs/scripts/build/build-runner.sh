#!/usr/bin/env bash

if [ -z "${BASH_VERSION:-}" ]; then
    exec bash "$0" "$@"
fi

set -euo pipefail

# Node.js build runner for `homeboy build`.
#
# Detection order:
#   1. HOMEBOY_NODE_BUILD_COMMAND env var (full override)
#   2. nx.json present + scripts.build → `nx run-many --target=build` if a
#      multi-package build is implied; otherwise `npm run build`
#   3. turbo.json present → `turbo run build`
#   4. package.json scripts.build → `{npm,pnpm,yarn} run build`
#   5. Fail with a clear "no build defined" message (don't guess `tsc`)
#
# Standard env vars:
#   HOMEBOY_EXTENSION_PATH       — path to this extension
#   HOMEBOY_COMPONENT_PATH       — path to the Node.js project
#   HOMEBOY_NODE_BUILD_COMMAND   — override
#   HOMEBOY_DEBUG                — verbose

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER_PRELUDE="${HOMEBOY_RUNTIME_RUNNER_PRELUDE:?HOMEBOY_RUNTIME_RUNNER_PRELUDE is required}"
COMMAND_CAPTURE_HELPER="${HOMEBOY_RUNTIME_COMMAND_CAPTURE:?HOMEBOY_RUNTIME_COMMAND_CAPTURE is required}"
# shellcheck source=/dev/null
source "$RUNNER_PRELUDE"
homeboy_runner_init --bash 4 --failure-trap
# shellcheck source=/dev/null
source "$COMMAND_CAPTURE_HELPER"
# shellcheck source=../lib/node-helpers.sh
source "${SCRIPT_DIR}/../lib/node-helpers.sh"
homeboy_require_package_json
homeboy_detect_package_manager
homeboy_ensure_node_dependencies

# Resolve the build command.
BUILD_CMD=""
if [ -n "${HOMEBOY_NODE_BUILD_COMMAND:-}" ]; then
    BUILD_CMD="$HOMEBOY_NODE_BUILD_COMMAND"
elif [ -f "${PROJECT_PATH}/nx.json" ]; then
    # Nx monorepo. If `build` is a registered target it should run via nx
    # so the dep graph is honored. Prefer scripts.build if defined (lets
    # the project author choose the exact `nx ...` invocation), else
    # default to `nx run-many`.
    if homeboy_has_npm_script "build"; then
        BUILD_CMD="$(homeboy_project_run_script_command build)"
    else
        BUILD_CMD="$(homeboy_project_exec_command nx run-many --target=build --all)"
    fi
elif [ -f "${PROJECT_PATH}/turbo.json" ]; then
    if homeboy_has_npm_script "build"; then
        BUILD_CMD="$(homeboy_project_run_script_command build)"
    else
        BUILD_CMD="$(homeboy_project_exec_command turbo run build)"
    fi
elif homeboy_has_npm_script "build"; then
    BUILD_CMD="$(homeboy_project_run_script_command build)"
else
    FAILED_STEP="No build defined"
    FAILURE_OUTPUT="No scripts.build in package.json and no nx.json/turbo.json detected. Set HOMEBOY_NODE_BUILD_COMMAND or add a scripts.build entry."
    exit 1
fi

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: build command: $BUILD_CMD" >&2
fi

echo "Running Node.js build..."
echo "  Component: ${COMPONENT_ID} (${PROJECT_PATH})"
echo "  Command:   ${BUILD_CMD}"
echo ""

cd "$PROJECT_PATH"

homeboy_run_step_capture OUTPUT_FILE BUILD_EXIT "Build failed" -- bash -c "$BUILD_CMD \"\$@\"" _ "$@" || true

if [ $BUILD_EXIT -ne 0 ]; then
    FAILED_STEP="Build failed (exit $BUILD_EXIT)"
fi

homeboy_cleanup_step_capture "$OUTPUT_FILE"
exit $BUILD_EXIT
