#!/usr/bin/env bash
set -euo pipefail

# Rust formatter for homeboy's post-write formatting.
# Called by engine::format_write after refactor --write applies code.

if [ -n "${HOMEBOY_COMPONENT_PATH:-}" ]; then
    PROJECT_PATH="${HOMEBOY_COMPONENT_PATH}"
else
    PROJECT_PATH="$(pwd)"
fi

if [ ! -f "${PROJECT_PATH}/Cargo.toml" ]; then
    echo "No Cargo.toml found — skipping format"
    exit 0
fi

cargo fmt --manifest-path "${PROJECT_PATH}/Cargo.toml" 2>&1
