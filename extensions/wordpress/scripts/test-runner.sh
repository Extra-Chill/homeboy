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

# Lint PHP files using PHPCS
run_lint() {
    echo "Linting PHP files with PHPCS..."
    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "Linting path: $PLUGIN_PATH"
    fi

    local phpcs="${MODULE_PATH}/vendor/bin/phpcs"
    if [ ! -f "$phpcs" ]; then
        echo "Warning: phpcs not found at $phpcs, skipping linting"
        return 0
    fi

    local phpcs_config="${MODULE_PATH}/phpcs.xml.dist"
    if [ ! -f "$phpcs_config" ]; then
        echo "Warning: phpcs.xml.dist not found at $phpcs_config, skipping linting"
        return 0
    fi

    # Auto-detect text domain from plugin header
    local TEXT_DOMAIN=""
    local MAIN_PLUGIN_FILE
    MAIN_PLUGIN_FILE=$(find "$PLUGIN_PATH" -maxdepth 1 -name "*.php" -exec grep -l "Plugin Name:" {} \; 2>/dev/null | head -1)
    if [ -n "$MAIN_PLUGIN_FILE" ]; then
        TEXT_DOMAIN=$(grep -m1 "Text Domain:" "$MAIN_PLUGIN_FILE" 2>/dev/null | sed 's/.*Text Domain:[[:space:]]*//' | tr -d ' \r')
    fi

    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: Running PHPCS with config: $phpcs_config"
        echo "DEBUG: Target path: $PLUGIN_PATH"
        echo "DEBUG: Main plugin file: ${MAIN_PLUGIN_FILE:-NOT_FOUND}"
        echo "DEBUG: Text domain: ${TEXT_DOMAIN:-NOT_DETECTED}"
    fi

    # Build phpcs command with optional text domain
    local phpcs_args=(--standard="$phpcs_config")
    if [ -n "$TEXT_DOMAIN" ]; then
        phpcs_args+=(--runtime-set text_domain "$TEXT_DOMAIN")
    fi
    phpcs_args+=("$PLUGIN_PATH")

    local lint_exit=0
    "$phpcs" "${phpcs_args[@]}" || lint_exit=$?

    if [ $lint_exit -eq 0 ]; then
        echo "PHPCS linting passed"
    elif [ $lint_exit -le 2 ]; then
        echo ""
        echo "⚠ PHPCS found style issues (see above). Proceeding to tests..."
        echo ""
    else
        echo "PHPCS encountered a fatal error (exit code $lint_exit). Aborting."
        exit 1
    fi
}

# Lint JavaScript files using ESLint
run_eslint() {
    # Check if component has JavaScript files
    local js_files
    js_files=$(find "$PLUGIN_PATH" -type f \( -name "*.js" -o -name "*.jsx" -o -name "*.ts" -o -name "*.tsx" \) \
        -not -path "*/node_modules/*" \
        -not -path "*/vendor/*" \
        -not -path "*/build/*" \
        -not -path "*/dist/*" \
        -not -name "*.min.js" \
        2>/dev/null | head -1)

    if [ -z "$js_files" ]; then
        if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
            echo "DEBUG: No JavaScript files found, skipping ESLint"
        fi
        return 0
    fi

    echo "Linting JavaScript files with ESLint..."

    local eslint="${MODULE_PATH}/node_modules/.bin/eslint"
    if [ ! -f "$eslint" ]; then
        echo "Warning: ESLint not found, skipping JavaScript linting"
        return 0
    fi

    # Auto-detect text domain (same pattern as PHPCS)
    local TEXT_DOMAIN=""
    local MAIN_PLUGIN_FILE
    MAIN_PLUGIN_FILE=$(find "$PLUGIN_PATH" -maxdepth 1 -name "*.php" -exec grep -l "Plugin Name:" {} \; 2>/dev/null | head -1)
    if [ -n "$MAIN_PLUGIN_FILE" ]; then
        TEXT_DOMAIN=$(grep -m1 "Text Domain:" "$MAIN_PLUGIN_FILE" 2>/dev/null | sed 's/.*Text Domain:[[:space:]]*//' | tr -d ' \r')
    fi

    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: Running ESLint with config: ${MODULE_PATH}/.eslintrc.json"
        echo "DEBUG: Target path: $PLUGIN_PATH"
        echo "DEBUG: Text domain: ${TEXT_DOMAIN:-NOT_DETECTED}"
    fi

    # Build ESLint command with text domain rule if detected
    local eslint_args=(--config "${MODULE_PATH}/.eslintrc.json" --ext .js,.jsx,.ts,.tsx)
    if [ -n "$TEXT_DOMAIN" ]; then
        eslint_args+=(--rule "@wordpress/i18n-text-domain: [error, { allowedTextDomain: \"$TEXT_DOMAIN\" }]")
    fi
    eslint_args+=("$PLUGIN_PATH")

    local lint_exit=0
    "$eslint" "${eslint_args[@]}" || lint_exit=$?

    if [ $lint_exit -eq 0 ]; then
        echo "ESLint linting passed"
    elif [ $lint_exit -eq 1 ]; then
        echo ""
        echo "⚠ ESLint found style issues (see above). Proceeding to tests..."
        echo ""
    else
        echo "ESLint encountered a fatal error (exit code $lint_exit). Aborting."
        exit 1
    fi
}

# Export paths for bootstrap
if [ -n "${COMPONENT_ID:-}" ]; then
    export HOMEBOY_COMPONENT_ID="$COMPONENT_ID"
    export HOMEBOY_COMPONENT_PATH="$COMPONENT_PATH"
    export HOMEBOY_PLUGIN_PATH="$PLUGIN_PATH"
    TEST_DIR="${PLUGIN_PATH}/tests"
    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: Using component test directory: $TEST_DIR"
    fi
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

# Run linting before tests (unless skipped)
if [[ "${HOMEBOY_SKIP_LINT:-}" != "1" ]]; then
    run_lint
    run_eslint
else
    echo "Skipping linting (--skip-lint)"
fi

# Validate test directory structure - check for conflicting local infrastructure
LOCAL_BOOTSTRAP="${TEST_DIR}/bootstrap.php"
LOCAL_PHPUNIT_XML="${TEST_DIR}/phpunit.xml"

if [ -f "$LOCAL_BOOTSTRAP" ]; then
    echo "Error: Homeboy WordPress module is not compatible with local bootstrap tests"
    echo ""
    echo "The WordPress module provides complete test infrastructure including:"
    echo "  - WordPress environment setup and bootstrap"
    echo "  - Database configuration (SQLite/MySQL)"
    echo "  - PHPUnit configuration"
    echo "  - Test discovery and execution"
    echo ""
    echo "Local bootstrap file found:"
    echo "  $LOCAL_BOOTSTRAP"
    echo ""
    echo "Component test files (*.php) can remain - only infrastructure files must be removed."
    echo "Please remove: $LOCAL_BOOTSTRAP"
    exit 1
fi

if [ -f "$LOCAL_PHPUNIT_XML" ]; then
    echo "Error: Local phpunit.xml conflicts with module configuration"
    echo ""
    echo "The WordPress module provides complete PHPUnit configuration."
    echo "Local phpunit.xml file found:"
    echo "  $LOCAL_PHPUNIT_XML"
    echo ""
    echo "Please remove: $LOCAL_PHPUNIT_XML"
    exit 1
fi

# Run PHPUnit with module bootstrap
echo "Running PHPUnit tests..."
"${MODULE_PATH}/vendor/bin/phpunit" \
  --bootstrap="${MODULE_PATH}/tests/bootstrap.php" \
  --configuration="${MODULE_PATH}/phpunit.xml.dist" \
  --testdox \
  "${TEST_DIR}"
