#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${SCRIPT_DIR}/lib/resolve-context.sh}"
# shellcheck source=/dev/null
source "$RESOLVE_CONTEXT_HELPER"
homeboy_resolve_context

echo "Validating Swift project: $(basename "$COMPONENT_PATH")"

if ! command -v swiftc >/dev/null 2>&1; then
    echo "Error: swiftc not found. Install Swift or Xcode Command Line Tools."
    exit 1
fi

SWIFT_FILES=()
while IFS= read -r swift_file; do
    SWIFT_FILES+=("$swift_file")
done < <(
    find "$COMPONENT_PATH" \
        \( -path '*/.build/*' -o -path '*/DerivedData/*' -o -path '*/.swiftpm/*' \) -prune \
        -o -name '*.swift' -type f -print
)

if [ ${#SWIFT_FILES[@]} -eq 0 ]; then
    echo "Warning: No Swift files found in $COMPONENT_PATH"
    exit 0
fi

for swift_file in "${SWIFT_FILES[@]}"; do
    swiftc -parse "$swift_file"
done

if [ -f "$COMPONENT_PATH/project.yml" ] && ! command -v xcodegen >/dev/null 2>&1; then
    echo "Warning: project.yml found, but xcodegen is not installed; skipping project generation check."
fi

WORKSPACE=$(find "$COMPONENT_PATH" -maxdepth 1 -name '*.xcworkspace' -print -quit)
PROJECT=$(find "$COMPONENT_PATH" -maxdepth 1 -name '*.xcodeproj' -print -quit)

if [ -n "$WORKSPACE" ] || [ -n "$PROJECT" ]; then
    DEVELOPER_DIR="$(xcode-select -p 2>/dev/null || true)"
    if [ -z "$DEVELOPER_DIR" ] || [[ "$DEVELOPER_DIR" != *".app/Contents/Developer" ]]; then
        echo "Warning: Xcode project/workspace found, but full Xcode is not active; skipping xcodebuild -list."
        echo "Current developer directory: ${DEVELOPER_DIR:-unavailable}"
    elif [ -n "$WORKSPACE" ]; then
        xcodebuild -list -workspace "$WORKSPACE" >/dev/null
    else
        xcodebuild -list -project "$PROJECT" >/dev/null
    fi
fi

echo "Swift validation passed"
