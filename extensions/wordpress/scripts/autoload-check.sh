#!/usr/bin/env bash
set -euo pipefail

# Autoload validation for WordPress plugins
# Catches class loading errors before tests run

# Determine module path
if [ -n "${HOMEBOY_MODULE_PATH:-}" ]; then
    MODULE_PATH="${HOMEBOY_MODULE_PATH}"
else
    SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    MODULE_PATH="$(dirname "$SCRIPT_DIR")"
fi

# Determine plugin path
PLUGIN_PATH="${HOMEBOY_PLUGIN_PATH:-${HOMEBOY_COMPONENT_PATH:-$(pwd)}}"

# Export for PHP script
export HOMEBOY_MODULE_PATH="$MODULE_PATH"
export HOMEBOY_PLUGIN_PATH="$PLUGIN_PATH"

echo "Checking plugin can load..."

# Run PHP validation script
php "${MODULE_PATH}/scripts/validate-autoload.php" || exit 1
