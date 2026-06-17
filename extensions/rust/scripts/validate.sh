#!/usr/bin/env bash
set -euo pipefail

# Rust validator for homeboy's post-write validation gate.
# Includes tests so generated or edited #[cfg(test)] code is checked before success.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:?HOMEBOY_RUNTIME_RESOLVE_CONTEXT is required}"
# shellcheck source=/dev/null
source "$RESOLVE_CONTEXT_HELPER"
homeboy_resolve_context

if [ ! -f "${PROJECT_PATH}/Cargo.toml" ]; then
    echo "No Cargo.toml found — skipping validation"
    exit 0
fi

cargo check --tests --manifest-path "${PROJECT_PATH}/Cargo.toml" 2>&1
