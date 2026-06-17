#!/usr/bin/env bash
set -euo pipefail

# Node.js formatter for homeboy's post-write formatting gate.
# Mirrors the former core fallback: use project-local Prettier when available.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:?HOMEBOY_RUNTIME_RESOLVE_CONTEXT is required}"
# shellcheck source=/dev/null
source "$RESOLVE_CONTEXT_HELPER"
homeboy_resolve_context
# shellcheck source=lib/node-helpers.sh
source "${SCRIPT_DIR}/lib/node-helpers.sh"

if ! homeboy_require_package_json; then
    echo "No package.json found — skipping format"
    exit 0
fi
homeboy_detect_package_manager

if [ ! -x "${PROJECT_PATH}/node_modules/.bin/prettier" ]; then
    echo "No project-local Prettier found — skipping format"
    exit 0
fi

cd "$PROJECT_PATH"
$PKG_EXEC prettier --write . 2>&1
