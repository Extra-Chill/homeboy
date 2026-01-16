#!/usr/bin/env bash
set -euo pipefail

# Get current package info from Cargo.toml
PACKAGE_NAME=$(cargo metadata --format-version 1 --no-deps 2>/dev/null | jq -r '.packages[0].name // empty')
CURRENT_VERSION=$(cargo metadata --format-version 1 --no-deps 2>/dev/null | jq -r '.packages[0].version // empty')

if [[ -z "$PACKAGE_NAME" || -z "$CURRENT_VERSION" ]]; then
  echo "Failed to read package info from Cargo.toml" >&2
  exit 1
fi

# Check if this version is already published on crates.io
PUBLISHED_VERSION=$(cargo search "$PACKAGE_NAME" --limit 1 2>/dev/null | grep -oE "^$PACKAGE_NAME = \"[^\"]+\"" | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' || echo "")

if [[ "$CURRENT_VERSION" == "$PUBLISHED_VERSION" ]]; then
  echo "Version $CURRENT_VERSION of $PACKAGE_NAME already published to crates.io, skipping..."
  exit 0
fi

echo "Publishing $PACKAGE_NAME v$CURRENT_VERSION to crates.io..."
cargo publish --locked
