#!/usr/bin/env bash
set -euo pipefail

# Autoload validation for WordPress components (plugins and themes)
# Catches class loading errors before tests run

# Determine extension path
if [ -n "${HOMEBOY_EXTENSION_PATH:-}" ]; then
    EXTENSION_PATH="${HOMEBOY_EXTENSION_PATH}"
else
    SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    EXTENSION_PATH="$(dirname "$SCRIPT_DIR")"
fi

# Determine component path
PLUGIN_PATH="${HOMEBOY_PLUGIN_PATH:-${HOMEBOY_COMPONENT_PATH:-$(pwd)}}"

# Export for PHP script
export HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH"
export HOMEBOY_PLUGIN_PATH="$PLUGIN_PATH"

echo "Checking component can load..."

# Run PHP validation script
php "${EXTENSION_PATH}/scripts/validation/validate-autoload.php" || exit 1
