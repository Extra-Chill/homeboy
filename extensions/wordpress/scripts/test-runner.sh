#!/bin/bash
set -euo pipefail

# Debug environment variables (only shown when HOMEBOY_DEBUG=1)
if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: Environment variables:"
    echo "HOMEBOY_MODULE_PATH=${HOMEBOY_MODULE_PATH:-NOT_SET}"
    echo "HOMEBOY_COMPONENT_ID=${HOMEBOY_COMPONENT_ID:-NOT_SET}"
    echo "HOMEBOY_COMPONENT_PATH=${HOMEBOY_COMPONENT_PATH:-NOT_SET}"
    echo "HOMEBOY_PROJECT_PATH=${HOMEBOY_PROJECT_PATH:-NOT_SET}"
    echo "HOMEBOY_SETTINGS_JSON=${HOMEBOY_SETTINGS_JSON:-NOT_SET}"
fi

# Determine execution context
if [ -n "${HOMEBOY_MODULE_PATH:-}" ]; then
    # Called through Homeboy module system
    MODULE_PATH="${HOMEBOY_MODULE_PATH}"

    # Check if this is component-level or project-level testing
    if [ -n "${HOMEBOY_COMPONENT_ID:-}" ]; then
        # Component-level testing
        COMPONENT_ID="${HOMEBOY_COMPONENT_ID}"
        COMPONENT_PATH="${HOMEBOY_COMPONENT_PATH:-.}"
        PLUGIN_PATH="$COMPONENT_PATH"
        SETTINGS_JSON="${HOMEBOY_SETTINGS_JSON:-}"
        if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
            echo "DEBUG: Component context detected"
        fi
    else
        # Project-level testing
        PROJECT_PATH="${HOMEBOY_PROJECT_PATH:-.}"
        PLUGIN_PATH="$PROJECT_PATH"
        SETTINGS_JSON="${HOMEBOY_SETTINGS_JSON:-}"
        if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
            echo "DEBUG: Project context detected"
        fi
    fi

    # Parse settings from JSON using jq
    if [ -n "$SETTINGS_JSON" ] && [ "$SETTINGS_JSON" != "{}" ]; then
        DATABASE_TYPE=$(printf '%s' "$SETTINGS_JSON" | jq -r '.database_type // "sqlite"')
    else
        DATABASE_TYPE="sqlite"
    fi
else
    # Called directly (e.g., from composer test in component directory)
    # Derive paths and use defaults
    SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    MODULE_PATH="$(dirname "$SCRIPT_DIR")"

    # Assume we're in a component directory (composer test context)
    COMPONENT_PATH="$(pwd)"
    PLUGIN_PATH="$COMPONENT_PATH"
    COMPONENT_ID="$(basename "$COMPONENT_PATH")"  # Derive component ID from directory name
    DATABASE_TYPE="sqlite"  # Default to SQLite

    # Set component environment variables for bootstrap
    export HOMEBOY_COMPONENT_ID="$COMPONENT_ID"
    export HOMEBOY_COMPONENT_PATH="$COMPONENT_PATH"

    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: Direct execution context (component: $COMPONENT_ID)"
    fi
fi

echo "Running WordPress tests..."
if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "Current dir: $(pwd)"
    echo "Module path: $MODULE_PATH"
    if [ -n "${COMPONENT_ID:-}" ]; then
        echo "Component: $COMPONENT_ID ($COMPONENT_PATH)"
        echo "Plugin path: $PLUGIN_PATH"
    else
        echo "Project path: $PROJECT_PATH"
        echo "Plugin path: $PLUGIN_PATH"
    fi
    echo "Database: $DATABASE_TYPE"
fi

# Derive WordPress paths from module path
WP_TESTS_DIR="${MODULE_PATH}/vendor/wp-phpunit/wp-phpunit"
ABSPATH="${MODULE_PATH}/vendor/wp-phpunit/wp-phpunit/wordpress"

# Generate configuration based on database type
if [ "$DATABASE_TYPE" = "sqlite" ]; then
    bash "${MODULE_PATH}/scripts/generate-config.sh" "sqlite" "$ABSPATH"
elif [ "$DATABASE_TYPE" = "mysql" ]; then
    if [ -n "${HOMEBOY_MODULE_PATH:-}" ]; then
        # Use Homeboy settings
        MYSQL_HOST=$(printf '%s' "$SETTINGS_JSON" | jq -r '.mysql_host // "localhost"')
        MYSQL_DATABASE=$(printf '%s' "$SETTINGS_JSON" | jq -r '.mysql_database // "wordpress_test"')
        MYSQL_USER=$(printf '%s' "$SETTINGS_JSON" | jq -r '.mysql_user // "root"')
        MYSQL_PASSWORD=$(printf '%s' "$SETTINGS_JSON" | jq -r '.mysql_password // ""')
    else
        # Use defaults when called directly
        MYSQL_HOST="localhost"
        MYSQL_DATABASE="wordpress_test"
        MYSQL_USER="root"
        MYSQL_PASSWORD=""
    fi
    bash "${MODULE_PATH}/scripts/generate-config.sh" "mysql" "$ABSPATH" \
        "$MYSQL_HOST" "$MYSQL_DATABASE" "$MYSQL_USER" "$MYSQL_PASSWORD"
fi

# Export paths for bootstrap
if [ -n "${COMPONENT_ID:-}" ]; then
    export HOMEBOY_COMPONENT_ID="$COMPONENT_ID"
    export HOMEBOY_COMPONENT_PATH="$COMPONENT_PATH"
    export HOMEBOY_PLUGIN_PATH="$PLUGIN_PATH"
    TEST_DIR="${PLUGIN_PATH}/tests"
    echo "DEBUG: Using component test directory: $TEST_DIR"
else
    export HOMEBOY_PROJECT_PATH="$PROJECT_PATH"
    export HOMEBOY_PLUGIN_PATH="$PLUGIN_PATH"
    TEST_DIR="${PROJECT_PATH}/tests"
    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: Using project test directory: $TEST_DIR"
    fi
fi
export WP_TESTS_DIR="$WP_TESTS_DIR"
export ABSPATH="$ABSPATH"

# Run PHPUnit with module bootstrap
"${MODULE_PATH}/vendor/bin/phpunit" \
  --bootstrap="${MODULE_PATH}/tests/bootstrap.php" \
  --testdox \
  "${TEST_DIR}"
