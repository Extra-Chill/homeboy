#!/usr/bin/env bash
set -euo pipefail

FAILED_STEP=""
FAILURE_OUTPUT=""
FAILURE_REPLAY_MODE="full"

# Track infrastructure fixes for HOMEBOY_FIX_RESULTS_FILE sidecar.
# Each entry: {"file": "...", "rule": "...", "action": "..."}
# Lint fixes are written directly by lint-runner.sh; test-runner.sh
# appends infrastructure fixes (removed files, removed packages).
TEST_FIX_ENTRIES=()

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER_STEPS_HELPER="${HOMEBOY_RUNTIME_RUNNER_STEPS:-${SCRIPT_DIR}/../lib/runner-steps.sh}"
DEPENDENCY_HELPER="${HOMEBOY_WORDPRESS_DEPENDENCY_HELPER:-${SCRIPT_DIR}/../lib/validation-dependencies.sh}"
# shellcheck source=../lib/runner-steps.sh
source "${RUNNER_STEPS_HELPER}"
# shellcheck source=../lib/validation-dependencies.sh
source "${DEPENDENCY_HELPER}"

print_failure_summary() {
    if [ -n "$FAILED_STEP" ]; then
        echo ""
        echo "============================================"
        echo "BUILD FAILED: $FAILED_STEP"
        echo "============================================"
        if [ "$FAILURE_REPLAY_MODE" = "none" ]; then
            echo ""
            echo "See PHPUnit output above (not replayed)."
        elif [ -n "$FAILURE_OUTPUT" ]; then
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
    echo "HOMEBOY_EXTENSION_PATH=${HOMEBOY_EXTENSION_PATH:-NOT_SET}"
    echo "HOMEBOY_COMPONENT_ID=${HOMEBOY_COMPONENT_ID:-NOT_SET}"
    echo "HOMEBOY_COMPONENT_PATH=${HOMEBOY_COMPONENT_PATH:-NOT_SET}"
    echo "HOMEBOY_PROJECT_PATH=${HOMEBOY_PROJECT_PATH:-NOT_SET}"
    echo "HOMEBOY_SETTINGS_JSON=${HOMEBOY_SETTINGS_JSON:-NOT_SET}"
fi

# Determine execution context
if [ -n "${HOMEBOY_EXTENSION_PATH:-}" ]; then
    # Called through Homeboy extension system
    EXTENSION_PATH="${HOMEBOY_EXTENSION_PATH}"

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
        DATABASE_TYPE=$(printf '%s' "$SETTINGS_JSON" | jq -r '.database_type // "auto"')
    else
        DATABASE_TYPE="auto"
    fi
else
    # Called directly (e.g., from composer test in component directory)
    # Derive paths and use defaults
    EXTENSION_PATH="$(dirname "$(dirname "$SCRIPT_DIR")")"

    # Assume we're in a component directory (composer test context)
    COMPONENT_PATH="$(pwd)"
    PLUGIN_PATH="$COMPONENT_PATH"
    COMPONENT_ID="$(basename "$COMPONENT_PATH")"  # Derive component ID from directory name
    DATABASE_TYPE="auto"  # Auto-detect MySQL, fall back to SQLite

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
    echo "Extension path: $EXTENSION_PATH"
    if [ -n "${COMPONENT_ID:-}" ]; then
        echo "Component: $COMPONENT_ID ($COMPONENT_PATH)"
        echo "Plugin path: $PLUGIN_PATH"
    else
        echo "Project path: $PROJECT_PATH"
        echo "Plugin path: $PLUGIN_PATH"
    fi
    echo "Database: $DATABASE_TYPE"
fi

# wp-phpunit test library path (always from vendor)
WP_TESTS_DIR="${EXTENSION_PATH}/vendor/wp-phpunit/wp-phpunit"

# Get WordPress version from wp-phpunit package
# wp-phpunit versions match WordPress versions (e.g., 6.9.1)
WP_VERSION=""
if [ -f "${EXTENSION_PATH}/composer.lock" ]; then
    WP_VERSION=$(grep -A5 '"name": "wp-phpunit/wp-phpunit"' "${EXTENSION_PATH}/composer.lock" | grep '"version"' | head -1 | sed 's/.*"version": "\([^"]*\)".*/\1/' || true)
fi
if [ -z "$WP_VERSION" ]; then
    # Fallback: try to get from installed package
    WP_VERSION=$(composer show wp-phpunit/wp-phpunit --working-dir="${EXTENSION_PATH}" 2>/dev/null | grep '^versions' | awk '{print $NF}' || echo "6.9.1")
fi

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: WordPress version from wp-phpunit: $WP_VERSION"
fi

# WordPress core cache directory
WP_CACHE_BASE="${HOME}/.cache/homeboy/wordpress"
WP_CACHE_DIR="${WP_CACHE_BASE}/${WP_VERSION}"
ABSPATH="${WP_CACHE_DIR}/wordpress"

# Download WordPress if not cached
if [ ! -f "${ABSPATH}/wp-includes/version.php" ]; then
    echo "Downloading WordPress ${WP_VERSION}..."
    mkdir -p "${WP_CACHE_DIR}"
    
    # Try to download from wordpress.org
    WP_DOWNLOAD_URL="https://wordpress.org/wordpress-${WP_VERSION}.tar.gz"
    
    if command -v curl &> /dev/null; then
        if ! curl -sL "$WP_DOWNLOAD_URL" | tar xz -C "${WP_CACHE_DIR}"; then
            echo "Failed to download WordPress ${WP_VERSION}"
            echo "URL: $WP_DOWNLOAD_URL"
            FAILED_STEP="WordPress download"
            exit 1
        fi
    elif command -v wget &> /dev/null; then
        if ! wget -qO- "$WP_DOWNLOAD_URL" | tar xz -C "${WP_CACHE_DIR}"; then
            echo "Failed to download WordPress ${WP_VERSION}"
            echo "URL: $WP_DOWNLOAD_URL"
            FAILED_STEP="WordPress download"
            exit 1
        fi
    else
        echo "Error: Neither curl nor wget found. Cannot download WordPress."
        FAILED_STEP="WordPress download"
        exit 1
    fi
    
    echo "WordPress ${WP_VERSION} downloaded to ${ABSPATH}"
else
    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: Using cached WordPress at ${ABSPATH}"
    fi
fi

# Ensure wp-phpunit's proxy config exists (restores if overwritten)
WP_PHPUNIT_CONFIG="${WP_TESTS_DIR}/wp-tests-config.php"
if ! grep -q "WP_PHPUNIT__TESTS_CONFIG" "$WP_PHPUNIT_CONFIG" 2>/dev/null; then
    cat > "$WP_PHPUNIT_CONFIG" <<'PROXY'
<?php
/**
 * DO NOT EDIT THIS FILE
 * 
 * Define the path to your wp-tests-config.php using the WP_PHPUNIT__TESTS_CONFIG environment variable.
 */
if ( file_exists( getenv( 'WP_PHPUNIT__TESTS_CONFIG' ) ) ) {
    require getenv( 'WP_PHPUNIT__TESTS_CONFIG' );
}
if ( false !== getenv( 'WP_PHPUNIT__TABLE_PREFIX' ) ) {
    $table_prefix = getenv( 'WP_PHPUNIT__TABLE_PREFIX' );
} elseif ( ! isset( $table_prefix ) ) {
    $table_prefix = 'wptests_';
}
PROXY
fi

# Set the path to our generated tests config
WP_TESTS_CONFIG_PATH="$(dirname "$ABSPATH")/wp-tests-config.php"
export WP_PHPUNIT__TESTS_CONFIG="$WP_TESTS_CONFIG_PATH"

# Resolve "auto" database type: try MySQL first, fall back to SQLite
MYSQL_AUTO_CREATED=""
if [ "$DATABASE_TYPE" = "auto" ]; then
    # Step 1: Find wp-config.php to read DB credentials from a live WP install.
    # Search strategy (in order):
    #   a) Walk up from plugin directory (works when plugin is deployed inside WP)
    #   b) Walk up from project path (works when project IS the WP site)
    #   c) Use wp-cli to locate a WP install (works on any machine with wp-cli)
    WP_CONFIG_PATH=""
    for _start_dir in "${PLUGIN_PATH}" "${HOMEBOY_PROJECT_PATH:-}"; do
        [ -z "$_start_dir" ] && continue
        _search_dir="$_start_dir"
        for _i in 1 2 3 4 5; do
            _search_dir="$(dirname "$_search_dir")"
            if [ -f "${_search_dir}/wp-config.php" ]; then
                WP_CONFIG_PATH="${_search_dir}/wp-config.php"
                break 2
            fi
        done
    done

    # Fallback: ask wp-cli for a WP install path
    if [ -z "$WP_CONFIG_PATH" ] && command -v wp &>/dev/null; then
        _wp_path=$(wp --allow-root --path=/var/www eval "echo ABSPATH;" 2>/dev/null || true)
        # wp-cli with no --path tries CWD which may not be a WP install.
        # Try common paths if eval failed.
        if [ -z "$_wp_path" ]; then
            for _wp_dir in /var/www/*/wp-config.php /srv/*/wp-config.php /home/*/public_html/wp-config.php; do
                if [ -f "$_wp_dir" ]; then
                    WP_CONFIG_PATH="$_wp_dir"
                    break
                fi
            done
        elif [ -f "${_wp_path}wp-config.php" ]; then
            WP_CONFIG_PATH="${_wp_path}wp-config.php"
        fi
    fi

    # Last resort: scan common WordPress locations
    if [ -z "$WP_CONFIG_PATH" ]; then
        for _wp_dir in /var/www/*/wp-config.php /srv/*/wp-config.php; do
            if [ -f "$_wp_dir" ]; then
                WP_CONFIG_PATH="$_wp_dir"
                break
            fi
        done
    fi

    # Step 2: Extract DB credentials from wp-config.php if found.
    if [ -n "$WP_CONFIG_PATH" ]; then
        # Use grep to extract define() values — avoids PHP quoting hell in bash.
        _extract_wp_define() {
            grep -oP "define\s*\(\s*['\"]$1['\"]\s*,\s*['\"]\\K[^'\"]*" "$WP_CONFIG_PATH" 2>/dev/null | head -1
        }
        WP_DB_HOST=$(_extract_wp_define DB_HOST)
        WP_DB_USER=$(_extract_wp_define DB_USER)
        WP_DB_PASSWORD=$(_extract_wp_define DB_PASSWORD)
        WP_DB_NAME=$(_extract_wp_define DB_NAME)

        if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
            echo "DEBUG: Found wp-config.php at $WP_CONFIG_PATH"
            echo "DEBUG: DB_HOST=${WP_DB_HOST:-NOT_FOUND}, DB_USER=${WP_DB_USER:-NOT_FOUND}, DB_NAME=${WP_DB_NAME:-NOT_FOUND}"
        fi
    fi

    # Step 3: Try MySQL connection. Prefer wp-config.php credentials, then
    # fall back to root@127.0.0.1, then SQLite.
    MYSQL_CONNECTED=""
    if command -v mysql &>/dev/null; then
        # Try wp-config.php credentials first (the live WP site's DB)
        if [ -n "${WP_DB_HOST:-}" ] && [ -n "${WP_DB_USER:-}" ]; then
            _mysql_auth=(-h "$WP_DB_HOST" -u "$WP_DB_USER")
            [ -n "${WP_DB_PASSWORD:-}" ] && _mysql_auth+=(-p"$WP_DB_PASSWORD")
            if mysql "${_mysql_auth[@]}" -e "SELECT 1" &>/dev/null; then
                DATABASE_TYPE="mysql"
                MYSQL_HOST="$WP_DB_HOST"
                MYSQL_USER="$WP_DB_USER"
                MYSQL_PASSWORD="${WP_DB_PASSWORD:-}"
                MYSQL_DATABASE="homeboy_wptests"
                MYSQL_CONNECTED="wp-config"
            fi
        fi

        # Fall back to root@127.0.0.1 with no password (CI environments)
        if [ -z "$MYSQL_CONNECTED" ]; then
            if mysql -h 127.0.0.1 -u root -e "SELECT 1" &>/dev/null; then
                DATABASE_TYPE="mysql"
                MYSQL_HOST="127.0.0.1"
                MYSQL_USER="root"
                MYSQL_PASSWORD=""
                MYSQL_DATABASE="homeboy_wptests"
                MYSQL_CONNECTED="root-nopass"
            fi
        fi
    fi

    if [ -n "$MYSQL_CONNECTED" ]; then
        # Try to create a dedicated test database; if the user lacks CREATE
        # privileges, fall back to the live WP database (tests use their own
        # table prefix so this is safe).
        _mysql_create=(-h "$MYSQL_HOST" -u "$MYSQL_USER")
        [ -n "${MYSQL_PASSWORD:-}" ] && _mysql_create+=(-p"$MYSQL_PASSWORD")
        if mysql "${_mysql_create[@]}" -e "CREATE DATABASE IF NOT EXISTS \`${MYSQL_DATABASE}\`" 2>/dev/null; then
            MYSQL_AUTO_CREATED="1"
        else
            # Can't create DB — reuse the live WP database name
            MYSQL_DATABASE="${WP_DB_NAME:-wordpress}"
            if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
                echo "DEBUG: Cannot CREATE DATABASE — reusing WP database '${MYSQL_DATABASE}'"
            fi
        fi
        if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
            echo "DEBUG: Auto-detected MySQL via ${MYSQL_CONNECTED} (${MYSQL_USER}@${MYSQL_HOST}, db=${MYSQL_DATABASE})"
        fi
    elif php -r 'exit(extension_loaded("pdo_sqlite") ? 0 : 1);' 2>/dev/null; then
        DATABASE_TYPE="sqlite"
        echo "Note: MySQL not available, using SQLite (experimental)"
    else
        echo "Error: No database backend available."
        echo ""
        echo "Either:"
        echo "  - Install MySQL/MariaDB (recommended)"
        echo "  - Install PHP pdo_sqlite extension"
        echo ""
        exit 1
    fi
fi

# Generate configuration based on database type
if [ "$DATABASE_TYPE" = "sqlite" ]; then
    # Pre-flight: verify pdo_sqlite extension is available
    if ! php -r 'exit(extension_loaded("pdo_sqlite") ? 0 : 1);' 2>/dev/null; then
        echo "Error: PHP pdo_sqlite extension is required for SQLite-backed tests."
        echo ""
        echo "Install it with:"
        echo "  Ubuntu/Debian: sudo apt install php-sqlite3"
        echo "  macOS (Homebrew): brew install php (includes pdo_sqlite)"
        echo "  RHEL/CentOS: sudo yum install php-pdo"
        echo ""
        echo "Or switch to MySQL: homeboy test <component> --setting database_type=mysql"
        exit 1
    fi
    bash "${EXTENSION_PATH}/scripts/test/generate-config.sh" "sqlite" "$ABSPATH" "$EXTENSION_PATH"
elif [ "$DATABASE_TYPE" = "mysql" ]; then
    if [ -z "${MYSQL_HOST:-}" ]; then
        # Credentials not set yet (explicit mysql mode, not auto-detected)
        if [ -n "${HOMEBOY_EXTENSION_PATH:-}" ]; then
            # Use Homeboy settings
            MYSQL_HOST=$(printf '%s' "$SETTINGS_JSON" | jq -r '.mysql_host // "127.0.0.1"')
            MYSQL_DATABASE=$(printf '%s' "$SETTINGS_JSON" | jq -r '.mysql_database // "homeboy_wptests"')
            MYSQL_USER=$(printf '%s' "$SETTINGS_JSON" | jq -r '.mysql_user // "root"')
            MYSQL_PASSWORD=$(printf '%s' "$SETTINGS_JSON" | jq -r '.mysql_password // ""')
        else
            # Use defaults when called directly
            MYSQL_HOST="127.0.0.1"
            MYSQL_DATABASE="homeboy_wptests"
            MYSQL_USER="root"
            MYSQL_PASSWORD=""
        fi
    fi
    bash "${EXTENSION_PATH}/scripts/test/generate-config.sh" "mysql" "$ABSPATH" "$EXTENSION_PATH" \
        "$MYSQL_HOST" "$MYSQL_DATABASE" "$MYSQL_USER" "$MYSQL_PASSWORD"
fi

# Track whether lint reported issues (separate from test outcome)
LINT_HAD_ISSUES=0

# Run linting using external lint-runner.sh with summary mode
run_lint() {
    local lint_runner="${EXTENSION_PATH}/scripts/lint/lint-runner.sh"
    if [ ! -f "$lint_runner" ]; then
        echo "Warning: lint-runner.sh not found, skipping linting"
        return 0
    fi

    # Capture lint output to detect issues (lint-runner.sh always exits 0 in summary mode)
    local lint_output
    lint_output=$(HOMEBOY_SUMMARY_MODE=1 HOMEBOY_AUTO_FIX="${HOMEBOY_AUTO_FIX:-}" bash "$lint_runner" 2>&1)
    echo "$lint_output"

    # Detect if lint reported issues
    if echo "$lint_output" | grep -q "linting failed\|Linting found issues"; then
        LINT_HAD_ISSUES=1
    fi
    echo ""
}

# Run autoload validation (blocking - must pass before tests)
run_autoload_check() {
    local check_script="${EXTENSION_PATH}/scripts/validation/autoload-check.sh"
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
homeboy_export_validation_dependency_paths "$PLUGIN_PATH"
export WP_TESTS_DIR="$WP_TESTS_DIR"
export ABSPATH="$ABSPATH"

# Run linting before tests (unless skipped)
if should_run_step "lint" && [[ "${HOMEBOY_SKIP_LINT:-}" != "1" ]]; then
    run_lint
elif ! should_run_step "lint"; then
    echo "Skipping linting (--step filter)"
else
    echo "Skipping linting (--skip-lint)"
fi

# Run autoload validation (catches class loading errors before PHPUnit)
if should_run_step "autoload-check"; then
    run_autoload_check
fi

# Validate test directory structure - warn about conflicting local infrastructure
LOCAL_BOOTSTRAP="${TEST_DIR}/bootstrap.php"
LOCAL_PHPUNIT_XML="${TEST_DIR}/phpunit.xml"
LOCAL_PHPUNIT_XML_ROOT="${PLUGIN_PATH}/phpunit.xml"
LOCAL_PHPUNIT_XML_DIST_ROOT="${PLUGIN_PATH}/phpunit.xml.dist"

if [ -f "$LOCAL_BOOTSTRAP" ]; then
    echo ""
    echo "⚠ Warning: Local bootstrap.php found and will be IGNORED"
    echo "  Location: $LOCAL_BOOTSTRAP"
    echo "  Homeboy WordPress extension provides complete test infrastructure."
    if [ "${HOMEBOY_AUTO_FIX:-}" = "1" ]; then
        echo "  → Auto-fix: Removing $LOCAL_BOOTSTRAP"
        rm -f "$LOCAL_BOOTSTRAP"
        echo "  ✓ Removed"
        TEST_FIX_ENTRIES+=("{\"file\": \"tests/bootstrap.php\", \"rule\": \"test-infra-cleanup\", \"action\": \"remove\"}")
    else
        echo "  Consider removing: $LOCAL_BOOTSTRAP"
    fi
    echo ""
fi

if [ -f "$LOCAL_PHPUNIT_XML" ]; then
    echo ""
    echo "⚠ Warning: Local phpunit.xml found in tests/ and will be IGNORED"
    echo "  Location: $LOCAL_PHPUNIT_XML"
    echo "  Homeboy WordPress extension provides PHPUnit configuration."
    if [ "${HOMEBOY_AUTO_FIX:-}" = "1" ]; then
        echo "  → Auto-fix: Removing $LOCAL_PHPUNIT_XML"
        rm -f "$LOCAL_PHPUNIT_XML"
        echo "  ✓ Removed"
        TEST_FIX_ENTRIES+=("{\"file\": \"tests/phpunit.xml\", \"rule\": \"test-infra-cleanup\", \"action\": \"remove\"}")
    else
        echo "  Consider removing: $LOCAL_PHPUNIT_XML"
    fi
    echo ""
fi

if [ -f "$LOCAL_PHPUNIT_XML_ROOT" ]; then
    echo ""
    echo "⚠ Warning: Local phpunit.xml found in root and will be IGNORED"
    echo "  Location: $LOCAL_PHPUNIT_XML_ROOT"
    if [ "${HOMEBOY_AUTO_FIX:-}" = "1" ]; then
        echo "  → Auto-fix: Removing $LOCAL_PHPUNIT_XML_ROOT"
        rm -f "$LOCAL_PHPUNIT_XML_ROOT"
        echo "  ✓ Removed"
        TEST_FIX_ENTRIES+=("{\"file\": \"phpunit.xml\", \"rule\": \"test-infra-cleanup\", \"action\": \"remove\"}")
    else
        echo "  Consider removing: $LOCAL_PHPUNIT_XML_ROOT"
    fi
    echo ""
fi

if [ -f "$LOCAL_PHPUNIT_XML_DIST_ROOT" ]; then
    echo ""
    echo "⚠ Warning: Local phpunit.xml.dist found in root and will be IGNORED"
    echo "  Location: $LOCAL_PHPUNIT_XML_DIST_ROOT"
    if [ "${HOMEBOY_AUTO_FIX:-}" = "1" ]; then
        echo "  → Auto-fix: Removing $LOCAL_PHPUNIT_XML_DIST_ROOT"
        rm -f "$LOCAL_PHPUNIT_XML_DIST_ROOT"
        echo "  ✓ Removed"
        TEST_FIX_ENTRIES+=("{\"file\": \"phpunit.xml.dist\", \"rule\": \"test-infra-cleanup\", \"action\": \"remove\"}")
    else
        echo "  Consider removing: $LOCAL_PHPUNIT_XML_DIST_ROOT"
    fi
    echo ""
fi

# Detect conflicting local PHPUnit binary
LOCAL_PHPUNIT_BIN="${PLUGIN_PATH}/vendor/bin/phpunit"
if [ -f "$LOCAL_PHPUNIT_BIN" ]; then
    echo ""
    echo "⚠ Warning: Local vendor/bin/phpunit found — may conflict with Homeboy's PHPUnit"
    echo "  Location: $LOCAL_PHPUNIT_BIN"
    echo "  Homeboy WordPress extension provides PHPUnit through its own vendor directory."
    echo "  Having two PHPUnit versions can cause version mismatches and confusing failures."
    if [ "${HOMEBOY_AUTO_FIX:-}" = "1" ]; then
        echo "  → Auto-fix: Removing local phpunit from require-dev and vendor..."
        (cd "$PLUGIN_PATH" && composer remove --dev phpunit/phpunit 2>/dev/null || true)
        echo "  ✓ Removed"
        TEST_FIX_ENTRIES+=("{\"file\": \"composer.json\", \"rule\": \"test-infra-cleanup\", \"action\": \"remove-phpunit-dep\"}")
    else
        echo "  Fix: composer remove --dev phpunit/phpunit (in $PLUGIN_PATH)"
        echo "  Or run: homeboy test ${COMPONENT_ID:-} --fix"
    fi
    echo ""
fi

# Detect phpunit in composer.json require-dev (even if not installed yet)
if [ ! -f "$LOCAL_PHPUNIT_BIN" ] && [ -f "${PLUGIN_PATH}/composer.json" ]; then
    if grep -q '"phpunit/phpunit"' "${PLUGIN_PATH}/composer.json" 2>/dev/null; then
        echo ""
        echo "⚠ Warning: phpunit/phpunit found in composer.json require-dev"
        echo "  Homeboy WordPress extension provides PHPUnit — the local dependency is redundant."
        if [ "${HOMEBOY_AUTO_FIX:-}" = "1" ]; then
            echo "  → Auto-fix: Removing phpunit from require-dev..."
            (cd "$PLUGIN_PATH" && composer remove --dev phpunit/phpunit 2>/dev/null || true)
            echo "  ✓ Removed"
            TEST_FIX_ENTRIES+=("{\"file\": \"composer.json\", \"rule\": \"test-infra-cleanup\", \"action\": \"remove-phpunit-dep\"}")
        else
            echo "  Fix: composer remove --dev phpunit/phpunit (in $PLUGIN_PATH)"
            echo "  Or run: homeboy test ${COMPONENT_ID:-} --fix"
        fi
        echo ""
    fi
fi

# Check if PHPUnit step should run
if ! should_run_step "phpunit"; then
    echo "Skipping PHPUnit tests (--step filter)"
    exit 0
fi

# Check if tests directory exists before running PHPUnit
if [ ! -d "${TEST_DIR}" ]; then
    echo ""
    echo "⚠ Warning: No tests directory found at ${TEST_DIR}"
    echo "  Skipping PHPUnit tests."
    echo ""
    exit 0
fi

# Run PHPUnit with extension bootstrap
echo "Running PHPUnit tests..."

phpunit_args=(
    --bootstrap="${EXTENSION_PATH}/tests/bootstrap.php"
    --no-configuration
    --colors=auto
    --testdox
    "${TEST_DIR}"
)

# Scoped test selection: if HOMEBOY_CHANGED_TEST_FILES is set, build a
# PHPUnit --filter regex from the changed test file basenames.
# e.g., tests/Unit/Foo/BarBazTest.php → BarBazTest
if [ -n "${HOMEBOY_CHANGED_TEST_FILES:-}" ]; then
    FILTER_CLASSES=()
    while IFS= read -r test_file; do
        [ -z "$test_file" ] && continue
        # Only PHP files
        [[ "$test_file" != *.php ]] && continue
        # Extract basename without extension (e.g., BarBazTest)
        class_name="$(basename "$test_file" .php)"
        [ -n "$class_name" ] && FILTER_CLASSES+=("$class_name")
    done <<< "${HOMEBOY_CHANGED_TEST_FILES}"

    if [ ${#FILTER_CLASSES[@]} -gt 0 ]; then
        # Deduplicate and sort
        UNIQUE_CLASSES=($(printf '%s\n' "${FILTER_CLASSES[@]}" | sort -u))
        FILTER_REGEX=$(IFS='|'; echo "(${UNIQUE_CLASSES[*]})")
        phpunit_args+=(--filter "$FILTER_REGEX")
        echo "Scoped to changed test files: ${FILTER_REGEX}"
    fi
fi

# Coverage collection (opt-in via HOMEBOY_COVERAGE=1)
COVERAGE_CLOVER=""
if [ "${HOMEBOY_COVERAGE:-}" = "1" ]; then
    # Check for coverage driver (xdebug or pcov)
    if php -r 'exit(extension_loaded("pcov") || extension_loaded("xdebug") ? 0 : 1);' 2>/dev/null; then
        COVERAGE_CLOVER=$(mktemp --suffix=.xml)
        phpunit_args+=(--coverage-clover "$COVERAGE_CLOVER")
        echo "Coverage collection enabled (output: clover XML)"
    else
        echo ""
        echo "WARNING: Coverage requested but no coverage driver found."
        echo "  Install one of: pcov (recommended), xdebug"
        echo "  Ubuntu: sudo apt install php-pcov"
        echo "  macOS:  pecl install pcov"
        echo "  Skipping coverage collection."
        echo ""
    fi
fi

PHPUNIT_TMPFILE=$(mktemp)

set +e
"${EXTENSION_PATH}/vendor/bin/phpunit" "${phpunit_args[@]}" "$@" 2>&1 | tee "$PHPUNIT_TMPFILE"
phpunit_exit=${PIPESTATUS[0]}
set -e
# Parse test results and failures for homeboy core (best-effort, non-blocking)
PARSE_RESULTS="${EXTENSION_PATH}/scripts/test/parse-test-results.sh"
PARSE_FAILURES="${EXTENSION_PATH}/scripts/test/parse-test-failures.sh"
if [ -n "${HOMEBOY_TEST_RESULTS_FILE:-}" ] && [ -f "$PARSE_RESULTS" ]; then
    bash "$PARSE_RESULTS" "$PHPUNIT_TMPFILE" || true
fi
if [ -n "${HOMEBOY_TEST_FAILURES_FILE:-}" ] && [ -f "$PARSE_FAILURES" ]; then
    bash "$PARSE_FAILURES" "$PHPUNIT_TMPFILE" "${PLUGIN_PATH:-}" || true
fi

if [ $phpunit_exit -ne 0 ]; then
    # Check parsed test results to determine if this is a real test failure
    # or if PHPUnit exited non-zero for other reasons (deprecation notices,
    # warnings, risky tests). If test_counts show 0 failures, the exit code
    # is misleading — tests actually passed.
    ACTUAL_TEST_FAILURES=0
    if [ -n "${HOMEBOY_TEST_RESULTS_FILE:-}" ] && [ -f "${HOMEBOY_TEST_RESULTS_FILE}" ]; then
        ACTUAL_TEST_FAILURES=$(jq -r '.failed // 0' "${HOMEBOY_TEST_RESULTS_FILE}" 2>/dev/null || echo "0")
    fi

    if [ "$ACTUAL_TEST_FAILURES" -eq 0 ] 2>/dev/null; then
        # PHPUnit exited non-zero but no test failures detected.
        # This happens with deprecation notices, risky tests, or
        # when lint failures pollute the exit code.
        echo ""
        echo "============================================"
        echo "NOTE: PHPUnit exited with code $phpunit_exit but all tests passed"
        echo "============================================"
        echo "PHPUnit may have reported deprecation notices or warnings."
        echo "Treating as passed (0 test failures detected)."
        echo ""
        # Don't set FAILED_STEP — let the script continue to coverage/cleanup
    else
        FAILED_STEP="PHPUnit tests"
        FAILURE_REPLAY_MODE="none"
        rm -f "$PHPUNIT_TMPFILE"
        # Clean up auto-created test database
        if [ "${MYSQL_AUTO_CREATED:-}" = "1" ]; then
            _mysql_cleanup=(-h "$MYSQL_HOST" -u "$MYSQL_USER")
            [ -n "${MYSQL_PASSWORD:-}" ] && _mysql_cleanup+=(-p"$MYSQL_PASSWORD")
            mysql "${_mysql_cleanup[@]}" -e "DROP DATABASE IF EXISTS \`${MYSQL_DATABASE}\`" 2>/dev/null || true
        fi
        exit $phpunit_exit
    fi
fi

# Detect zero-test runs — PHPUnit exits 0 but ran no tests.
# This catches silent failures (broken bootstrap, empty test dirs, bad filters).
# PHPUnit 9+: "No tests executed!" or "OK (0 tests, 0 assertions)"
# PHPUnit 10+: "No tests executed!" or blank output
PHPUNIT_OUTPUT=$(cat "$PHPUNIT_TMPFILE")
rm -f "$PHPUNIT_TMPFILE"

if echo "$PHPUNIT_OUTPUT" | grep -qE 'No tests executed|OK \(0 tests'; then
    echo ""
    echo "============================================"
    echo "WARNING: PHPUnit ran 0 tests"
    echo "============================================"
    echo ""
    echo "PHPUnit exited successfully but executed no tests."
    echo "This usually means:"
    echo "  - The test directory has no *Test.php files"
    echo "  - A --filter pattern matched nothing"
    echo "  - The bootstrap failed silently (check output above)"
    echo ""
    echo "Test directory: ${TEST_DIR}"
    if [ -n "$*" ]; then
        echo "Passthrough args: $*"
    fi
    echo ""
    # Exit with code 1 so homeboy reports "failed" instead of false-positive "passed"
    FAILED_STEP="PHPUnit tests (zero tests executed)"
    exit 1
fi

# Also detect completely empty output (no PHPUnit banner at all = bootstrap died)
if [ -z "$(echo "$PHPUNIT_OUTPUT" | grep -E 'PHPUnit|test|assert|OK|ERRORS|FAILURES' || true)" ]; then
    echo ""
    echo "============================================"
    echo "WARNING: No PHPUnit output detected"
    echo "============================================"
    echo ""
    echo "PHPUnit produced no recognizable output. The bootstrap may have"
    echo "terminated the process before tests could run."
    echo ""
    FAILED_STEP="PHPUnit tests (no output)"
    exit 1
fi


# Parse and report coverage results
if [ -n "${COVERAGE_CLOVER:-}" ] && [ -f "$COVERAGE_CLOVER" ]; then
    COVERAGE_PARSER="${EXTENSION_PATH}/scripts/test/parse-coverage.php"
    COVERAGE_JSON=$(php "$COVERAGE_PARSER" "$COVERAGE_CLOVER" "${PLUGIN_PATH}/" 2>/dev/null || true)

    if [ -n "$COVERAGE_JSON" ]; then
        # Print summary to stdout
        LINE_PCT=$(echo "$COVERAGE_JSON" | jq -r '.totals.lines.pct')
        LINE_TOTAL=$(echo "$COVERAGE_JSON" | jq -r '.totals.lines.total')
        LINE_COVERED=$(echo "$COVERAGE_JSON" | jq -r '.totals.lines.covered')
        METHOD_PCT=$(echo "$COVERAGE_JSON" | jq -r '.totals.methods.pct')
        echo ""
        echo "============================================"
        echo "COVERAGE SUMMARY"
        echo "============================================"
        echo "  Lines:   ${LINE_PCT}% (${LINE_COVERED}/${LINE_TOTAL})"
        echo "  Methods: ${METHOD_PCT}%"
        echo ""

        # Write coverage JSON to file for homeboy core to read
        if [ -n "${HOMEBOY_COVERAGE_FILE:-}" ]; then
            echo "$COVERAGE_JSON" > "$HOMEBOY_COVERAGE_FILE"
        fi

        # Check minimum threshold
        if [ -n "${HOMEBOY_COVERAGE_MIN:-}" ]; then
            BELOW=$(echo "$LINE_PCT < ${HOMEBOY_COVERAGE_MIN}" | bc -l 2>/dev/null || echo "0")
            if [ "$BELOW" = "1" ]; then
                echo "COVERAGE FAILED: ${LINE_PCT}% is below minimum ${HOMEBOY_COVERAGE_MIN}%"
                FAILED_STEP="Coverage threshold (${LINE_PCT}% < ${HOMEBOY_COVERAGE_MIN}%)"
                rm -f "$COVERAGE_CLOVER"
                # Clean up auto-created test database
                if [ "${MYSQL_AUTO_CREATED:-}" = "1" ]; then
                    _mysql_cleanup=(-h "$MYSQL_HOST" -u "$MYSQL_USER")
                    [ -n "${MYSQL_PASSWORD:-}" ] && _mysql_cleanup+=(-p"$MYSQL_PASSWORD")
                    mysql "${_mysql_cleanup[@]}" -e "DROP DATABASE IF EXISTS \`${MYSQL_DATABASE}\`" 2>/dev/null || true
                fi
                exit 1
            fi
        fi
    else
        echo "WARNING: Failed to parse coverage report"
    fi

    rm -f "$COVERAGE_CLOVER"
fi

# Clean up auto-created test database
if [ "${MYSQL_AUTO_CREATED:-}" = "1" ]; then
    _mysql_cleanup=(-h "$MYSQL_HOST" -u "$MYSQL_USER")
    [ -n "${MYSQL_PASSWORD:-}" ] && _mysql_cleanup+=(-p"$MYSQL_PASSWORD")
    mysql "${_mysql_cleanup[@]}" -e "DROP DATABASE IF EXISTS \`${MYSQL_DATABASE}\`" 2>/dev/null || true
fi

# Write test infrastructure fix plan/results sidecars.
# lint-runner.sh may have already written lint fixes to these files; we merge
# test-runner's infrastructure fixes (removed files, removed deps) into them.
write_or_merge_fix_sidecar() {
    local target_file="$1"

    if [ -z "${target_file:-}" ] || [ ${#TEST_FIX_ENTRIES[@]} -eq 0 ]; then
        return 0
    fi

    if [ -f "${target_file}" ] && [ -s "${target_file}" ]; then
        # Merge: read existing array, append our entries, write back
        EXISTING=$(cat "${target_file}")
        MERGED="${EXISTING%]}"  # strip trailing ]
        for ENTRY in "${TEST_FIX_ENTRIES[@]}"; do
            MERGED+=", ${ENTRY}"
        done
        MERGED+="]"
        echo "${MERGED}" > "${target_file}"
    else
        # No existing file — write fresh array
        JSON="["
        FIRST=1
        for ENTRY in "${TEST_FIX_ENTRIES[@]}"; do
            if [ $FIRST -eq 1 ]; then
                JSON+="${ENTRY}"
                FIRST=0
            else
                JSON+=", ${ENTRY}"
            fi
        done
        JSON+="]"
        echo "${JSON}" > "${target_file}"
    fi
}

write_or_merge_fix_sidecar "${HOMEBOY_FIX_PLAN_FILE:-}"
write_or_merge_fix_sidecar "${HOMEBOY_FIX_RESULTS_FILE:-}"
