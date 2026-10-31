#!/bin/bash
set -euo pipefail

# Pre-build validation script for WordPress components
# Checks PHP syntax errors (fatal) before build proceeds

PLUGIN_PATH="${HOMEBOY_PLUGIN_PATH:-${HOMEBOY_COMPONENT_PATH:-.}}"

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: Validating build for: $PLUGIN_PATH"
fi

echo "Checking PHP syntax..."
ERRORS_FOUND=0

while IFS= read -r file; do
    if ! php -l "$file" > /dev/null 2>&1; then
        php -l "$file" 2>&1
        ERRORS_FOUND=1
    fi
done < <(find "$PLUGIN_PATH" -name "*.php" -not -path "*/vendor/*" -not -path "*/node_modules/*" -not -path "*/build/*")

if [ "$ERRORS_FOUND" -eq 1 ]; then
    echo ""
    echo "PHP syntax errors found. Fix before building."
    exit 1
fi

echo "PHP syntax check passed."
exit 0
