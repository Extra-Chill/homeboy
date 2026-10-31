#!/usr/bin/env bash
set -euo pipefail

if [[ ! -f Cargo.lock ]]; then
  echo "No Cargo.lock found; skipping Rust release preparation."
  exit 0
fi

if ! command -v jq &>/dev/null; then
  echo "Error: jq is required for parsing cargo metadata but is not installed." >&2
  exit 1
fi

PACKAGE_NAME=$(cargo metadata --format-version 1 --no-deps 2>/dev/null | jq -r '.packages[0].name // empty')
CURRENT_VERSION=$(cargo metadata --format-version 1 --no-deps 2>/dev/null | jq -r '.packages[0].version // empty')

if [[ -z "$PACKAGE_NAME" || -z "$CURRENT_VERSION" ]]; then
  echo "Failed to read package info from Cargo.toml" >&2
  exit 1
fi

echo "Preparing $PACKAGE_NAME v$CURRENT_VERSION for release..." >&2
cargo update -p "$PACKAGE_NAME" --precise "$CURRENT_VERSION"
