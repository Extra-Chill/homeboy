#!/usr/bin/env bash
set -euo pipefail

# Playground test runner for WordPress Homeboy extension.
#
# Boots a WordPress Playground instance (PHP-WASM + embedded SQLite),
# mounts the component under test, runs PHPUnit inside it, and emits
# the same JSON result shape as test-runner.sh.
#
# This is the "playground" backend — opt-in, not the default.
# The host backend (test-runner.sh) remains the default.
#
# HOW IT WORKS:
#
# 1. Fills in the static PHP template (playground-runner.php) with the plugin
#    slug and dependency mount paths via sed substitution.
#
#    The template:
#    - Generates wp-tests-config.php inside the wp-phpunit VFS
#    - Runs install.php IN-PROCESS (set $argv, require_once) which boots
#      WordPress AND creates DB tables without needing system()
#    - Loads wp-phpunit test case classes directly (no host bootstrap)
#    - Discovers and runs test files via PHPUnit's programmatic API
#
# 2. Mounts host directories into Playground's VFS:
#    - Plugin under test → /wordpress/wp-content/plugins/<slug>
#    - Dependencies      → /wordpress/wp-content/plugins/<dep>
#    - Extension dir     → /homeboy-extension
#    - Custom db.php     → /wordpress/wp-content/db.php (if present)
#
# 3. Runs the filled template via `wp-playground-cli php`
#
# KNOWN GAPS (Phase 1):
#   - WP version is pinned to match wp-phpunit package (6.9.x).
#   - db.php drop-in support needs per-case testing (Playground's built-in
#     SQLite integration may conflict with custom drop-ins like MDI).
#
# The host bootstrap (tests/bootstrap.php) is NOT used by this backend.
# install.php is included in-process which avoids the system() subprocess.

FAILED_STEP=""
FAILURE_OUTPUT=""
FAILURE_REPLAY_MODE="full"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="${HOMEBOY_EXTENSION_PATH:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
RUNNER_STEPS_HELPER="${HOMEBOY_RUNTIME_RUNNER_STEPS:-${SCRIPT_DIR}/../lib/runner-steps.sh}"
DEPENDENCY_HELPER="${HOMEBOY_WORDPRESS_DEPENDENCY_HELPER:-${SCRIPT_DIR}/../lib/validation-dependencies.sh}"
PHP_PREFLIGHT_HELPER="${SCRIPT_DIR}/../lib/php-preflight.sh"
# shellcheck source=../lib/runner-steps.sh
if [ -f "$RUNNER_STEPS_HELPER" ]; then
    source "$RUNNER_STEPS_HELPER"
fi
# shellcheck source=../lib/validation-dependencies.sh
if [ -f "$DEPENDENCY_HELPER" ]; then
    source "$DEPENDENCY_HELPER"
fi
# shellcheck source=../lib/php-preflight.sh
if [ -f "$PHP_PREFLIGHT_HELPER" ]; then
    source "$PHP_PREFLIGHT_HELPER"
fi

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

SETTINGS_JSON="${HOMEBOY_SETTINGS_JSON:-}"

if [ -n "${HOMEBOY_COMPONENT_PATH:-}" ]; then
    PLUGIN_PATH="${HOMEBOY_COMPONENT_PATH}"
    COMPONENT_ID="${HOMEBOY_COMPONENT_ID:-}"
elif [ -n "${HOMEBOY_PROJECT_PATH:-}" ]; then
    PLUGIN_PATH="${HOMEBOY_PROJECT_PATH}"
    COMPONENT_ID=""
else
    PLUGIN_PATH="$(pwd)"
    COMPONENT_ID="$(basename "$PLUGIN_PATH")"
fi

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: [playground] Extension path: $EXTENSION_PATH"
    echo "DEBUG: [playground] Plugin path: $PLUGIN_PATH"
    echo "DEBUG: [playground] Component ID: ${COMPONENT_ID:-none}"
fi

PLAYGROUND_CLI="${EXTENSION_PATH}/node_modules/.bin/wp-playground-cli"
if [ ! -f "$PLAYGROUND_CLI" ]; then
    echo "Error: @wp-playground/cli not found at $PLAYGROUND_CLI"
    echo ""
    echo "Install it with: cd ${EXTENSION_PATH} && npm install"
    echo "Or switch backends: homeboy component set <id> test_backend host"
    FAILED_STEP="Playground CLI setup"
    exit 1
fi

TEST_DIR="${PLUGIN_PATH}/tests"
if [ ! -d "$TEST_DIR" ]; then
    echo ""
    echo "⚠ Warning: No tests directory found at ${TEST_DIR}"
    echo "  Skipping PHPUnit tests."
    echo ""
    exit 0
fi

if type homeboy_php_preflight &>/dev/null; then
    homeboy_php_preflight "$PLUGIN_PATH"
fi

if [ -n "${COMPONENT_ID:-}" ]; then
    export HOMEBOY_COMPONENT_ID="$COMPONENT_ID"
    export HOMEBOY_COMPONENT_PATH="$PLUGIN_PATH"
    export HOMEBOY_PLUGIN_PATH="$PLUGIN_PATH"
else
    export HOMEBOY_PROJECT_PATH="$PLUGIN_PATH"
    export HOMEBOY_PLUGIN_PATH="$PLUGIN_PATH"
fi

if type homeboy_export_validation_dependency_paths &>/dev/null; then
    homeboy_export_validation_dependency_paths "$PLUGIN_PATH"
fi
DEPENDENCY_PATHS="${HOMEBOY_WORDPRESS_DEPENDENCY_PATHS:-}"

PLUGIN_SLUG="$(basename "$PLUGIN_PATH")"
MOUNT_ARGS=()

MOUNT_ARGS+=("--mount" "${PLUGIN_PATH}:/wordpress/wp-content/plugins/${PLUGIN_SLUG}")

if [ -n "$DEPENDENCY_PATHS" ]; then
    while IFS= read -r dep_path; do
        [ -z "$dep_path" ] && continue
        dep_slug="$(basename "$dep_path")"
        MOUNT_ARGS+=("--mount" "${dep_path}:/wordpress/wp-content/plugins/${dep_slug}")
    done <<< "$DEPENDENCY_PATHS"
fi

PLUGIN_DB_PHP="${PLUGIN_PATH}/db.php"
if [ -f "$PLUGIN_DB_PHP" ]; then
    MOUNT_ARGS+=("--mount" "${PLUGIN_DB_PHP}:/wordpress/wp-content/db.php")
fi

MOUNT_ARGS+=("--mount" "${EXTENSION_PATH}:/homeboy-extension")

PLAYGROUND_DEP_MOUNTS=""
if [ -n "$DEPENDENCY_PATHS" ]; then
    while IFS= read -r dep_path; do
        [ -z "$dep_path" ] && continue
        dep_slug="$(basename "$dep_path")"
        if [ -n "$PLAYGROUND_DEP_MOUNTS" ]; then
            PLAYGROUND_DEP_MOUNTS+="\\n"
        fi
        PLAYGROUND_DEP_MOUNTS+="/wordpress/wp-content/plugins/${dep_slug}"
    done <<< "$DEPENDENCY_PATHS"
fi

RESULT_FILE="${PLUGIN_PATH}/.pg-test-result.txt"
rm -f "$RESULT_FILE"

TEMPLATE="${SCRIPT_DIR}/playground-runner.php"
if [ ! -f "$TEMPLATE" ]; then
    echo "Error: playground-runner.php template not found at $TEMPLATE"
    FAILED_STEP="Playground setup"
    exit 1
fi

WRAPPER_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/pg-runner.XXXXXX.php")
sed \
    -e "s|{{PLUGIN_SLUG}}|${PLUGIN_SLUG}|g" \
    -e "s|{{PLAYGROUND_DEP_MOUNTS}}|${PLAYGROUND_DEP_MOUNTS}|g" \
    "$TEMPLATE" > "$WRAPPER_TMPFILE"

echo "Running PHPUnit tests via WordPress Playground..."
echo "  Plugin: ${PLUGIN_SLUG} (${PLUGIN_PATH})"
echo "  Backend: playground (PHP-WASM + SQLite)"

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "  Wrapper: $WRAPPER_TMPFILE"
    echo "  Mount args: ${MOUNT_ARGS[*]}"
fi

PHPUNIT_TMPFILE=$(mktemp)

set +e
"$PLAYGROUND_CLI" php \
    "${MOUNT_ARGS[@]}" \
    "--mount" "${WRAPPER_TMPFILE}:/runner.php" \
    --wp=6.9 \
    --verbosity=normal \
    -- /runner.php \
    2>&1 | tee "$PHPUNIT_TMPFILE"
playground_exit=${PIPESTATUS[0]}
set -e

rm -f "$WRAPPER_TMPFILE"

PHPUNIT_OUTPUT=""
if [ -f "$RESULT_FILE" ]; then
    PHPUNIT_OUTPUT=$(cat "$RESULT_FILE")
fi

# Also capture PHPUnit stdout from the tee'd output
PHPUNIT_STDOUT=$(cat "$PHPUNIT_TMPFILE")
rm -f "$PHPUNIT_TMPFILE"

# Parse test results for homeboy core (best-effort, non-blocking)
PARSE_RESULTS="${EXTENSION_PATH}/scripts/test/parse-test-results.sh"
PARSE_FAILURES="${EXTENSION_PATH}/scripts/test/parse-test-failures.sh"
if [ -n "${HOMEBOY_TEST_RESULTS_FILE:-}" ] && [ -f "$PARSE_RESULTS" ]; then
    if [ -n "$PHPUNIT_STDOUT" ]; then
        echo "$PHPUNIT_STDOUT" | bash "$PARSE_RESULTS" || true
    elif [ -n "$PHPUNIT_OUTPUT" ]; then
        echo "$PHPUNIT_OUTPUT" | bash "$PARSE_RESULTS" || true
    fi
fi
if [ -n "${HOMEBOY_TEST_FAILURES_FILE:-}" ] && [ -f "$PARSE_FAILURES" ]; then
    if [ -n "$PHPUNIT_STDOUT" ]; then
        echo "$PHPUNIT_STDOUT" | bash "$PARSE_FAILURES" "${PLUGIN_PATH:-}" || true
    fi
fi

if [ $playground_exit -ne 0 ]; then
    if echo "$PHPUNIT_OUTPUT" | grep -q "SOME TESTS FAILED"; then
        FAILED_STEP="PHPUnit tests (playground backend)"
        FAILURE_REPLAY_MODE="none"
        rm -f "$RESULT_FILE"
        exit $playground_exit
    elif echo "$PHPUNIT_STDOUT" | grep -qE 'FAILURES|ERRORS'; then
        FAILED_STEP="PHPUnit tests (playground backend)"
        FAILURE_REPLAY_MODE="none"
        rm -f "$RESULT_FILE"
        exit $playground_exit
    elif echo "$PHPUNIT_OUTPUT" | grep -q "FATAL:"; then
        FAILED_STEP="Playground bootstrap"
        FAILURE_OUTPUT=$(echo "$PHPUNIT_OUTPUT" | grep "FATAL:")
        rm -f "$RESULT_FILE"
        exit $playground_exit
    else
        echo ""
        echo "============================================"
        echo "NOTE: Playground exited with code $playground_exit"
        echo "============================================"
        if [ -n "$PHPUNIT_OUTPUT" ]; then
            echo "Last log: $(echo "$PHPUNIT_OUTPUT" | tail -1)"
        else
            echo "No result file produced. Playground may have crashed during boot."
        fi
    fi
fi

if [ -z "$PHPUNIT_OUTPUT" ] && [ -z "$PHPUNIT_STDOUT" ]; then
    echo ""
    echo "============================================"
    echo "WARNING: No test output captured (playground)"
    echo "============================================"
    echo ""
    FAILED_STEP="PHPUnit tests (no output, playground)"
    rm -f "$RESULT_FILE"
    exit 1
fi

if echo "$PHPUNIT_OUTPUT" | grep -q "NO_TEST_FILES"; then
    echo ""
    echo "============================================"
    echo "WARNING: No test files discovered"
    echo "============================================"
    echo ""
    FAILED_STEP="PHPUnit tests (no test files, playground)"
    rm -f "$RESULT_FILE"
    exit 1
fi

# Detect zero-test runs from PHPUnit stdout
if echo "$PHPUNIT_STDOUT" | grep -qE 'No tests executed|OK \(0 tests'; then
    echo ""
    echo "============================================"
    echo "WARNING: PHPUnit ran 0 tests (playground)"
    echo "============================================"
    echo ""
    FAILED_STEP="PHPUnit tests (zero tests executed, playground)"
    rm -f "$RESULT_FILE"
    exit 1
fi

rm -f "$RESULT_FILE"

echo ""
echo "Playground test run complete."
