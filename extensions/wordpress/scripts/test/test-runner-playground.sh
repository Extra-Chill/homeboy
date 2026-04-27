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

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="${HOMEBOY_EXTENSION_PATH:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
RUNNER_STEPS_HELPER="${HOMEBOY_RUNTIME_RUNNER_STEPS:-${SCRIPT_DIR}/../lib/runner-steps.sh}"
FAILURE_TRAP_HELPER="${HOMEBOY_RUNTIME_FAILURE_TRAP:-}"
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
# shellcheck source=/dev/null
if [ -n "$FAILURE_TRAP_HELPER" ] && [ -f "$FAILURE_TRAP_HELPER" ]; then
    source "$FAILURE_TRAP_HELPER"
    homeboy_init_failure_trap
else
    FAILED_STEP=""
    FAILURE_OUTPUT=""
    FAILURE_REPLAY_MODE="full"
fi

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

# Extract `wp_config_defines` from the merged settings JSON. The component
# declares its own additional wp-config defines under
# `extensions.wordpress.settings.wp_config_defines`; homeboy core merges
# them into HOMEBOY_SETTINGS_JSON and the runner appends them to
# wp-tests-config.php during pg_run_boot_stage().
WP_CONFIG_DEFINES_JSON="{}"
if [ -n "${HOMEBOY_SETTINGS_JSON:-}" ] && [ "${HOMEBOY_SETTINGS_JSON}" != "{}" ]; then
    extracted=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -c '.wp_config_defines // {}' 2>/dev/null || echo "{}")
    if [ -n "$extracted" ]; then
        WP_CONFIG_DEFINES_JSON="$extracted"
    fi
fi

# Extract `bench_env` from the merged settings JSON. Components declare
# host-shell env vars that should propagate into Playground PHP-WASM under
# `extensions.wordpress.settings.bench_env`. The dispatcher passes them
# to the runner template; the template calls putenv() for each entry
# before fixtures load, so test code's getenv() calls resolve correctly.
# See bench-runner-playground.sh for the full rationale (host shell env
# doesn't cross the wp-playground-cli sandbox boundary by default).
BENCH_ENV_JSON="{}"
if [ -n "${HOMEBOY_SETTINGS_JSON:-}" ] && [ "${HOMEBOY_SETTINGS_JSON}" != "{}" ]; then
    extracted=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -c '.bench_env // {}' 2>/dev/null || echo "{}")
    if [ -n "$extracted" ]; then
        BENCH_ENV_JSON="$extracted"
    fi
fi

# PLUGIN_SLUG is the wp-content/plugins/ path segment Playground uses to
# mount the component-under-test. When homeboy core tells us the canonical
# component id (HOMEBOY_COMPONENT_ID), use it — basename($PLUGIN_PATH)
# breaks for git-worktree checkouts (`<repo>@<branch-slug>`) and any
# workspace where the on-disk directory name diverges from the canonical
# slug. See bench-runner-playground.sh for the full rationale.
if [ -n "${COMPONENT_ID:-}" ]; then
    PLUGIN_SLUG="$COMPONENT_ID"
else
    PLUGIN_SLUG="$(basename "$PLUGIN_PATH")"
fi
MOUNT_ARGS=()

MOUNT_ARGS+=("--mount" "${PLUGIN_PATH}:/wordpress/wp-content/plugins/${PLUGIN_SLUG}")

if [ -n "$DEPENDENCY_PATHS" ]; then
    while IFS= read -r dep_path; do
        [ -z "$dep_path" ] && continue
        dep_slug="$(basename "$dep_path")"
        MOUNT_ARGS+=("--mount" "${dep_path}:/wordpress/wp-content/plugins/${dep_slug}")
    done <<< "$DEPENDENCY_PATHS"
fi

# ---------------------------------------------------------------------------
# db.php drop-in coexistence with Playground's built-in SQLite
#
# Playground ships an internal mu-plugin at
# /internal/shared/mu-plugins/sqlite-database-integration.php that normally
# wires up WordPress to its bundled SQLite implementation. That mu-plugin
# begins with a self-deactivation guard:
#
#     if ( file_exists( '/wordpress/wp-content/db.php' ) ) {
#         return;
#     }
#
# So when we mount a plugin's db.php drop-in at /wordpress/wp-content/db.php,
# the mu-plugin voluntarily steps aside and the drop-in owns $wpdb. No
# --skip-sqlite-setup flag is required. The drop-in can still *reuse*
# Playground's bundled SQLite classes (available at
# /internal/shared/sqlite-database-integration/) — that's what
# markdown-database-integration's db.php does.
#
# See docs/PLAYGROUND_DROPIN.md for the full coexistence model and a minimal
# example drop-in that verifies the plumbing end-to-end.
# ---------------------------------------------------------------------------
PLUGIN_DB_PHP="${PLUGIN_PATH}/db.php"
if [ -f "$PLUGIN_DB_PHP" ]; then
    MOUNT_ARGS+=("--mount" "${PLUGIN_DB_PHP}:/wordpress/wp-content/db.php")
    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: [playground] Plugin db.php drop-in detected at $PLUGIN_DB_PHP"
        echo "DEBUG: [playground] Playground's built-in SQLite mu-plugin will step aside"
    fi
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

WRAPPER_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/pg-runner.XXXXXX")
# ASCII SOH delimiter for the JSON substitution — values may contain `|`
# safely without shell escaping.
WP_CONFIG_DEFINES_DELIM=$(printf '\1')
sed \
    -e "s|{{PLUGIN_SLUG}}|${PLUGIN_SLUG}|g" \
    -e "s|{{PLAYGROUND_DEP_MOUNTS}}|${PLAYGROUND_DEP_MOUNTS}|g" \
    -e "s${WP_CONFIG_DEFINES_DELIM}{{WP_CONFIG_DEFINES_JSON}}${WP_CONFIG_DEFINES_DELIM}${WP_CONFIG_DEFINES_JSON}${WP_CONFIG_DEFINES_DELIM}g" \
    -e "s${WP_CONFIG_DEFINES_DELIM}{{BENCH_ENV_JSON}}${WP_CONFIG_DEFINES_DELIM}${BENCH_ENV_JSON}${WP_CONFIG_DEFINES_DELIM}g" \
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

# ----------------------------------------------------------------------------
# Failure classification
#
# The playground-runner.php template writes a structured log to $RESULT_FILE
# with these patterns (see playground-runner.php docblock for full contract):
#
#   STAGE_BEGIN:<stage>       - entered a bootstrap phase
#   STAGE_OK:<stage>          - phase completed
#   STAGE_FAIL:<stage>:<msg>  - caught Throwable during phase
#   STAGE_FATAL:<stage>:<msg> - uncatchable fatal (shutdown handler)
#   SOME TESTS FAILED         - PHPUnit ran, some assertions failed
#   ALL TESTS PASSED          - PHPUnit ran, all assertions passed
#
# Classification order (first match wins):
#   1. STAGE_FATAL/STAGE_FAIL in result file  -> bootstrap failure, show stage+msg
#   2. SOME TESTS FAILED in result file       -> PHPUnit assertion failures
#   3. Parse/fatal patterns in stdout         -> PHP crashed before writing log
#   4. playground_exit != 0 with no log       -> unknown crash, dump raw output
#   5. NO_TEST_FILES in result file           -> discovery found nothing
#   6. Zero-test PHPUnit run                  -> suite empty
# ----------------------------------------------------------------------------

dump_diagnostics() {
    local label="$1"
    echo ""
    echo "============================================"
    echo "$label"
    echo "============================================"
    if [ -n "$PHPUNIT_OUTPUT" ]; then
        echo ""
        echo "--- Structured log ($RESULT_FILE) ---"
        echo "$PHPUNIT_OUTPUT"
    fi
    if [ -n "$PHPUNIT_STDOUT" ]; then
        echo ""
        echo "--- Playground stdout/stderr ---"
        echo "$PHPUNIT_STDOUT"
    fi
}

# Case 1: bootstrap failure captured in the structured log (STAGE_FAIL/STAGE_FATAL).
if echo "$PHPUNIT_OUTPUT" | grep -qE '^STAGE_(FAIL|FATAL):'; then
    FAILED_STAGE_LINE=$(echo "$PHPUNIT_OUTPUT" | grep -E '^STAGE_(FAIL|FATAL):' | head -1)
    # Extract "<stage>:<msg>" (strip "STAGE_FAIL:" or "STAGE_FATAL:" prefix)
    FAILED_STAGE_DETAIL=$(echo "$FAILED_STAGE_LINE" | sed -E 's/^STAGE_(FAIL|FATAL)://')
    FAILED_STEP="Playground bootstrap (${FAILED_STAGE_DETAIL%%:*} stage)"
    FAILURE_OUTPUT="$FAILED_STAGE_LINE"
    dump_diagnostics "BOOTSTRAP FAILURE: $FAILED_STAGE_DETAIL"
    rm -f "$RESULT_FILE"
    exit ${playground_exit:-1}
fi

# Case 2: PHPUnit ran, some tests failed.
if echo "$PHPUNIT_OUTPUT" | grep -q "SOME TESTS FAILED"; then
    FAILED_STEP="PHPUnit tests (playground backend)"
    FAILURE_REPLAY_MODE="none"
    rm -f "$RESULT_FILE"
    exit ${playground_exit:-1}
fi

# Case 3: PHPUnit emitted FAILURES/ERRORS on stdout (belt-and-suspenders for
# the rare path where the result file was written but SOME TESTS FAILED line
# was somehow dropped).
if [ $playground_exit -ne 0 ] && echo "$PHPUNIT_STDOUT" | grep -qE '^(FAILURES|ERRORS)!'; then
    FAILED_STEP="PHPUnit tests (playground backend)"
    FAILURE_REPLAY_MODE="none"
    rm -f "$RESULT_FILE"
    exit $playground_exit
fi

# Case 4: PHP crashed before our template could write to the result file.
# Parse/fatal errors from Playground go to stderr (captured in $PHPUNIT_STDOUT
# via the tee'd pipe) — surface them clearly instead of failing silently.
if [ $playground_exit -ne 0 ] && echo "$PHPUNIT_STDOUT" | grep -qE '^(PHP Parse error|Parse error:|PHP Fatal error|Fatal error:)'; then
    FAILED_STEP="Playground PHP crash (before runner took control)"
    FAILURE_OUTPUT=$(echo "$PHPUNIT_STDOUT" | grep -E '^(PHP Parse error|Parse error:|PHP Fatal error|Fatal error:)' | head -5)
    dump_diagnostics "PHP CRASH"
    rm -f "$RESULT_FILE"
    exit $playground_exit
fi

# Case 5: playground exited non-zero and we still can't classify it.
# Don't exit 0 here — that's the bug the old code had.
if [ $playground_exit -ne 0 ]; then
    FAILED_STEP="Playground exited with code $playground_exit (unclassified)"
    dump_diagnostics "UNCLASSIFIED PLAYGROUND FAILURE (exit=$playground_exit)"
    rm -f "$RESULT_FILE"
    exit $playground_exit
fi

# Case 6: no output at all (not even a result file). Shouldn't happen on a
# clean exit, but guard anyway.
if [ -z "$PHPUNIT_OUTPUT" ] && [ -z "$PHPUNIT_STDOUT" ]; then
    dump_diagnostics "NO OUTPUT CAPTURED"
    FAILED_STEP="PHPUnit tests (no output, playground)"
    rm -f "$RESULT_FILE"
    exit 1
fi

# Case 7: discovery found zero test files.
if echo "$PHPUNIT_OUTPUT" | grep -q "^NO_TEST_FILES"; then
    dump_diagnostics "NO TEST FILES DISCOVERED"
    FAILED_STEP="PHPUnit tests (no test files, playground)"
    rm -f "$RESULT_FILE"
    exit 1
fi

# Case 8: PHPUnit ran but executed zero tests (class didn't extend TestCase,
# all tests excluded, etc.).
if echo "$PHPUNIT_STDOUT" | grep -qE 'No tests executed|OK \(0 tests'; then
    dump_diagnostics "ZERO TESTS EXECUTED"
    FAILED_STEP="PHPUnit tests (zero tests executed, playground)"
    rm -f "$RESULT_FILE"
    exit 1
fi

# Surface any non-fatal NOTICEs captured during bootstrap even on success —
# warnings inside WP setup have historically masked real problems.
if echo "$PHPUNIT_OUTPUT" | grep -q "^NOTICE:"; then
    echo ""
    echo "--- Bootstrap notices (non-fatal) ---"
    echo "$PHPUNIT_OUTPUT" | grep "^NOTICE:"
fi

rm -f "$RESULT_FILE"

echo ""
echo "Playground test run complete."
