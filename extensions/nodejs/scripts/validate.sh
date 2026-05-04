#!/usr/bin/env bash
set -euo pipefail

# Node.js validator for homeboy's post-write validation gate.
# Mirrors the former core TypeScript fallback: run tsc only when tsconfig exists.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${SCRIPT_DIR}/lib/resolve-context.sh}"
# shellcheck source=/dev/null
source "$RESOLVE_CONTEXT_HELPER"
homeboy_resolve_context
# shellcheck source=lib/node-helpers.sh
source "${SCRIPT_DIR}/lib/node-helpers.sh"

if ! homeboy_require_package_json; then
    echo "No package.json found — skipping validation"
    exit 0
fi
homeboy_detect_package_manager

if [ ! -f "${PROJECT_PATH}/tsconfig.json" ]; then
    echo "No tsconfig.json found — skipping validation"
    exit 0
fi

cd "$PROJECT_PATH"
$PKG_EXEC tsc --noEmit 2>&1
