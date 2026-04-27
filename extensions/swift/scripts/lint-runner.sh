#!/usr/bin/env bash
set -euo pipefail

if [ -n "${HOMEBOY_COMPONENT_PATH:-}" ]; then
    COMPONENT_PATH="$HOMEBOY_COMPONENT_PATH"
else
    COMPONENT_PATH="$(pwd)"
fi

echo "Running Swift lint for: $(basename "$COMPONENT_PATH")"

if command -v swiftlint >/dev/null 2>&1; then
    swiftlint lint --path "$COMPONENT_PATH"
elif command -v swiftformat >/dev/null 2>&1; then
    swiftformat "$COMPONENT_PATH" --lint
else
    echo "Swift lint skipped: install SwiftLint or SwiftFormat to enable Swift linting."
fi
