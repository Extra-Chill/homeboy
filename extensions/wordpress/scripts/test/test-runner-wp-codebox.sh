#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VALIDATION_DEPENDENCIES="${HOMEBOY_WORDPRESS_VALIDATION_DEPENDENCIES_HELPER:-${SCRIPT_DIR}/../lib/validation-dependencies.sh}"

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

exec node "${SCRIPT_DIR}/wp-codebox-phpunit-adapter.mjs" "$@"
