#!/usr/bin/env bash
set -euo pipefail

# Verify cargo-dist is installed before attempting to package.
# Without this check, the command fails silently and the release pipeline
# reports a cryptic "Failed to parse package artifacts" JSON error.
if ! command -v dist &>/dev/null && ! command -v cargo-dist &>/dev/null; then
    echo "Error: cargo-dist is not installed." >&2
    echo "Install with: cargo install cargo-dist" >&2
    echo "Or skip packaging by removing the rust extension's release.package action." >&2
    exit 1
fi

# Verify jq is available for artifact parsing
if ! command -v jq &>/dev/null; then
    echo "Error: jq is required for parsing dist output but is not installed." >&2
    exit 1
fi

dist build --output-format=json > dist-manifest.json

jq -c '[.upload_files[] | {path: ., type: (if endswith(".rb") then "homebrew" else "binary" end), platform: null}]' dist-manifest.json
