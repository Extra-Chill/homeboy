#!/usr/bin/env bash
set -euo pipefail

# Rust formatter for homeboy's post-write formatting.
# Called by engine::format_write after refactor --write applies code.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${SCRIPT_DIR}/lib/resolve-context.sh}"
# shellcheck source=/dev/null
source "$RESOLVE_CONTEXT_HELPER"
homeboy_resolve_context

if [ ! -f "${PROJECT_PATH}/Cargo.toml" ]; then
    echo "No Cargo.toml found — skipping format"
    exit 0
fi

cargo fmt --manifest-path "${PROJECT_PATH}/Cargo.toml" 2>&1
