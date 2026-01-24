#!/usr/bin/env bash
set -euo pipefail

FAILED_STEP=""
FAILURE_OUTPUT=""

print_failure_summary() {
    if [ -n "$FAILED_STEP" ]; then
        echo ""
        echo "============================================"
        echo "BUILD FAILED: $FAILED_STEP"
        echo "============================================"
        if [ -n "$FAILURE_OUTPUT" ]; then
            echo ""
            echo "Error details:"
            echo "$FAILURE_OUTPUT"
        fi
    fi
}
trap print_failure_summary EXIT

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
    bash "${MODULE_PATH}/scripts/test/generate-config.sh" "sqlite" "$ABSPATH"
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
    bash "${MODULE_PATH}/scripts/test/generate-config.sh" "mysql" "$ABSPATH" \
        "$MYSQL_HOST" "$MYSQL_DATABASE" "$MYSQL_USER" "$MYSQL_PASSWORD"
fi

# Run linting using external lint-runner.sh with summary mode
run_lint() {
    local lint_runner="${MODULE_PATH}/scripts/lint/lint-runner.sh"
    if [ ! -f "$lint_runner" ]; then
        echo "Warning: lint-runner.sh not found, skipping linting"
        return 0
    fi

    # Run linting in summary mode (lint-runner.sh always exits 0 - warn-only mode)
    HOMEBOY_SUMMARY_MODE=1 bash "$lint_runner"
    echo ""
}

# Run autoload validation (blocking - must pass before tests)
run_autoload_check() {
    local check_script="${MODULE_PATH}/scripts/validation/autoload-check.sh"
    if [ -f "$check_script" ]; then
        local output
        set +e
        output=$(bash "$check_script" 2>&1)
        local exit_code=$?
        set -e
        echo "$output"
        if [ $exit_code -ne 0 ]; then
            FAILED_STEP="Autoload validation"
            FAILURE_OUTPUT="$output"
            exit 1
        fi
        echo ""
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
else
    echo "Skipping linting (--skip-lint)"
fi

# Run autoload validation (catches class loading errors before PHPUnit)
run_autoload_check

# Validate test directory structure - warn about conflicting local infrastructure
LOCAL_BOOTSTRAP="${TEST_DIR}/bootstrap.php"
LOCAL_PHPUNIT_XML="${TEST_DIR}/phpunit.xml"
LOCAL_PHPUNIT_XML_ROOT="${PLUGIN_PATH}/phpunit.xml"
LOCAL_PHPUNIT_XML_DIST_ROOT="${PLUGIN_PATH}/phpunit.xml.dist"

if [ -f "$LOCAL_BOOTSTRAP" ]; then
    echo ""
    echo "⚠ Warning: Local bootstrap.php found and will be IGNORED"
    echo "  Location: $LOCAL_BOOTSTRAP"
    echo "  Homeboy WordPress module provides complete test infrastructure."
    echo "  Consider removing: $LOCAL_BOOTSTRAP"
    echo ""
fi

if [ -f "$LOCAL_PHPUNIT_XML" ]; then
    echo ""
    echo "⚠ Warning: Local phpunit.xml found in tests/ and will be IGNORED"
    echo "  Location: $LOCAL_PHPUNIT_XML"
    echo "  Homeboy WordPress module provides PHPUnit configuration."
    echo "  Consider removing: $LOCAL_PHPUNIT_XML"
    echo ""
fi

if [ -f "$LOCAL_PHPUNIT_XML_ROOT" ]; then
    echo ""
    echo "⚠ Warning: Local phpunit.xml found in root and will be IGNORED"
    echo "  Location: $LOCAL_PHPUNIT_XML_ROOT"
    echo "  Consider removing: $LOCAL_PHPUNIT_XML_ROOT"
    echo ""
fi

if [ -f "$LOCAL_PHPUNIT_XML_DIST_ROOT" ]; then
    echo ""
    echo "⚠ Warning: Local phpunit.xml.dist found in root and will be IGNORED"
    echo "  Location: $LOCAL_PHPUNIT_XML_DIST_ROOT"
    echo "  Consider removing: $LOCAL_PHPUNIT_XML_DIST_ROOT"
    echo ""
fi

# Check if tests directory exists before running PHPUnit
if [ ! -d "${TEST_DIR}" ]; then
    echo ""
    echo "⚠ Warning: No tests directory found at ${TEST_DIR}"
    echo "  Skipping PHPUnit tests."
    echo ""
    exit 0
fi

# Run PHPUnit with module bootstrap
echo "Running PHPUnit tests..."
set +e
phpunit_output=$("${MODULE_PATH}/vendor/bin/phpunit" \
  --bootstrap="${MODULE_PATH}/tests/bootstrap.php" \
  --configuration="${MODULE_PATH}/phpunit.xml.dist" \
  --testdox \
  "${TEST_DIR}" \
  "$@" 2>&1)
phpunit_exit=$?
set -e

echo "$phpunit_output"

if [ $phpunit_exit -ne 0 ]; then
    FAILED_STEP="PHPUnit tests"
    FAILURE_OUTPUT=$(echo "$phpunit_output" | tail -30)
    exit $phpunit_exit
fi
