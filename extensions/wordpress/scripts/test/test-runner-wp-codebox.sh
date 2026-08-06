#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VALIDATION_DEPENDENCIES="${HOMEBOY_WORDPRESS_VALIDATION_DEPENDENCIES_HELPER:-${SCRIPT_DIR}/../lib/validation-dependencies.sh}"
WP_CODEBOX_PATHS="${HOMEBOY_WORDPRESS_WP_CODEBOX_PATHS_HELPER:-${SCRIPT_DIR}/../lib/wp-codebox-paths.sh}"

# Resolve the component declaration before handing the recipe to WP Codebox. The
# adapter consumes the resulting generic path contract; it never knows consumer
# repository names or component registry details.
if [ -f "$VALIDATION_DEPENDENCIES" ]; then
    # shellcheck source=../lib/validation-dependencies.sh
    source "$VALIDATION_DEPENDENCIES"
    if type homeboy_export_validation_dependency_paths >/dev/null 2>&1; then
        homeboy_export_validation_dependency_paths "${HOMEBOY_COMPONENT_PATH:?HOMEBOY_COMPONENT_PATH is required}"
    fi
fi

# Resolve the WP Codebox CLI here, once, and hand the adapter a concrete argv.
# The shell resolver owns candidate precedence, existence checks, the runtime
# probe that rejects a stale PATH wrapper, and `node` prefixing for a `.js`
# entrypoint. Resolving before exec means an unusable CLI fails with a Homeboy
# diagnostic naming the missing artifact instead of a raw Node module stack.
# shellcheck source=../lib/wp-codebox-paths.sh
source "$WP_CODEBOX_PATHS"
homeboy_wp_codebox_export_command "${HOMEBOY_SETTINGS_JSON:-}"

exec node "${SCRIPT_DIR}/wp-codebox-phpunit-adapter.mjs" "$@"
