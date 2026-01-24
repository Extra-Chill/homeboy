#!/bin/bash
# Validates PSR-4 autoload paths exist in build directory

set -euo pipefail

BUILD_DIR="${1:?Usage: validate-psr4.sh <build-directory>}"
COMPOSER_JSON="$BUILD_DIR/composer.json"

# Skip if no composer.json
[ ! -f "$COMPOSER_JSON" ] && exit 0

# Require jq for JSON parsing
if ! command -v jq &> /dev/null; then
    echo "Warning: jq not installed, skipping PSR-4 validation"
    exit 0
fi

# Extract PSR-4 paths and validate each exists
MISSING=0
while IFS= read -r path; do
    [ -z "$path" ] && continue
    path="${path%/}"  # Remove trailing slash
    if [ ! -d "$BUILD_DIR/$path" ]; then
        echo "ERROR: PSR-4 autoload path missing: $path"
        MISSING=1
    fi
done < <(jq -r '.autoload["psr-4"] // {} | values[]' "$COMPOSER_JSON" 2>/dev/null)

if [ $MISSING -eq 1 ]; then
    echo ""
    echo "Build is missing PSR-4 directories. Check .buildignore."
    exit 1
fi

exit 0
