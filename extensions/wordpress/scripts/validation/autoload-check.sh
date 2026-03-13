#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEPENDENCY_HELPER="${HOMEBOY_WORDPRESS_DEPENDENCY_HELPER:-${SCRIPT_DIR}/../lib/validation-dependencies.sh}"
# shellcheck source=../lib/validation-dependencies.sh
source "${DEPENDENCY_HELPER}"

# Autoload validation for WordPress components (plugins and themes)
# Catches class loading errors before tests run

# Resolve execution context (shared helper)
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${SCRIPT_DIR}/../lib/resolve-context.sh}"
# shellcheck source=../lib/resolve-context.sh
source "${RESOLVE_CONTEXT_HELPER}"
homeboy_resolve_context

PLUGIN_PATH="${HOMEBOY_PLUGIN_PATH:-${PLUGIN_PATH}}"
homeboy_export_validation_dependency_paths "$PLUGIN_PATH"

# Export for PHP script
export HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH"
export HOMEBOY_PLUGIN_PATH="$PLUGIN_PATH"

echo "Checking component can load..."

# Run PHP validation script
php "${EXTENSION_PATH}/scripts/validation/validate-autoload.php" || exit 1
