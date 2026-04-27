#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${SCRIPT_DIR}/lib/resolve-context.sh}"
# shellcheck source=/dev/null
source "$RESOLVE_CONTEXT_HELPER"
homeboy_resolve_context

echo "Running Swift lint for: $(basename "$COMPONENT_PATH")"

if command -v swiftlint >/dev/null 2>&1; then
    swiftlint lint --path "$COMPONENT_PATH"
elif command -v swiftformat >/dev/null 2>&1; then
    swiftformat "$COMPONENT_PATH" --lint
else
    echo "Swift lint skipped: install SwiftLint or SwiftFormat to enable Swift linting."
fi
