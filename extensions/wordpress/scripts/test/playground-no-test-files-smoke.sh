#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER="${SCRIPT_DIR}/test-runner-playground.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

EXTENSION_PATH="${TMPDIR}/extension"
PLUGIN_PATH="${TMPDIR}/component"
BIN_PATH="${TMPDIR}/bin"
WRITE_RESULTS_HELPER="${TMPDIR}/write-test-results.sh"
RESULTS_FILE="${TMPDIR}/test-results.json"
mkdir -p "${BIN_PATH}"
export PATH="${BIN_PATH}:${PATH}"
mkdir -p "${EXTENSION_PATH}/node_modules/.bin" "${PLUGIN_PATH}/tests"

RUNNER_SRC="$(cat "${SCRIPT_DIR}/playground-runner.php")"
BOOTSTRAP_SRC="$(cat "${SCRIPT_DIR}/../lib/playground-bootstrap.php")"

cat > "${EXTENSION_PATH}/node_modules/.bin/wp-playground-cli" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
cat > "${HOMEBOY_PLUGIN_PATH}/.pg-test-result.txt" <<'LOG'
STAGE_BEGIN:discover_tests
DISCOVERY: dirs=/wordpress/wp-content/plugins/example/tests suffixes=Test.php prefixes=test- excludes=0 found=0
NO_TEST_FILES
STAGE_OK:discover_tests
LOG
exit 1
SH
chmod +x "${EXTENSION_PATH}/node_modules/.bin/wp-playground-cli"

cat > "$WRITE_RESULTS_HELPER" <<'SH'
function homeboy_write_test_results {
    local total="$1"
    local passed="$2"
    local failed="$3"
    local skipped="$4"
    local partial="${5:-}"

    php -r '
        $payload = array(
            "total" => (int) $argv[2],
            "passed" => (int) $argv[3],
            "failed" => (int) $argv[4],
            "skipped" => (int) $argv[5],
        );
        if ($argv[6] !== "") {
            $payload["partial"] = $argv[6];
        }
        file_put_contents($argv[1], json_encode($payload, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES));
    ' "$HOMEBOY_TEST_RESULTS_FILE" "$total" "$passed" "$failed" "$skipped" "$partial"
}
SH

cat > "${BIN_PATH}/composer" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
echo "composer:$PWD:$*" >> "${HOMEBOY_COMPOSER_CALLS_FILE}"
echo "Composer smoke script ran"
SH
chmod +x "${BIN_PATH}/composer"

assert_contains() {
    local haystack="$1"
    local needle="$2"
    if [[ "$haystack" != *"$needle"* ]]; then
        echo "Expected output to contain: $needle" >&2
        echo "Actual output:" >&2
        echo "$haystack" >&2
        exit 1
    fi
}

assert_not_contains() {
    local haystack="$1"
    local needle="$2"
    if [[ "$haystack" == *"$needle"* ]]; then
        echo "Expected output not to contain: $needle" >&2
        echo "Actual output:" >&2
        echo "$haystack" >&2
        exit 1
    fi
}

rm -f "$RESULTS_FILE"
set +e
skip_output=$(HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" HOMEBOY_COMPONENT_PATH="$PLUGIN_PATH" HOMEBOY_COMPONENT_ID="example" HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$WRITE_RESULTS_HELPER" HOMEBOY_TEST_RESULTS_FILE="$RESULTS_FILE" bash "$RUNNER" 2>&1)
skip_status=$?
set -e

if [ "$skip_status" -ne 0 ]; then
    echo "Expected no-files run without component PHPUnit config to skip with exit 0; got $skip_status" >&2
    echo "$skip_output" >&2
    exit 1
fi
assert_contains "$skip_output" "NO PHPUNIT TEST FILES DISCOVERED"
assert_contains "$skip_output" "Skipping PHPUnit tests: no files matched the WordPress runner discovery contract."
assert_contains "$skip_output" "ending in Test.php or starting with test-."
assert_not_contains "$skip_output" "UNCLASSIFIED PLAYGROUND FAILURE"
assert_contains "$(cat "$RESULTS_FILE")" '"total": 0'
assert_contains "$(cat "$RESULTS_FILE")" '"failed": 0'
assert_contains "$(cat "$RESULTS_FILE")" '"partial": "no-phpunit-tests"'

cat > "${PLUGIN_PATH}/composer.json" <<'JSON'
{
  "scripts": {
    "test": "php tests/smoke.php"
  }
}
JSON
COMPOSER_CALLS_FILE="${TMPDIR}/composer-calls.log"
set +e
composer_output=$(HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" HOMEBOY_COMPONENT_PATH="$PLUGIN_PATH" HOMEBOY_COMPONENT_ID="example" HOMEBOY_COMPOSER_CALLS_FILE="$COMPOSER_CALLS_FILE" bash "$RUNNER" 2>&1)
composer_status=$?
set -e

if [ "$composer_status" -ne 0 ]; then
    echo "Expected no-files run with composer scripts.test to run composer and pass; got $composer_status" >&2
    echo "$composer_output" >&2
    exit 1
fi
assert_contains "$composer_output" "NO PHPUNIT TEST FILES DISCOVERED"
assert_contains "$composer_output" "Running Composer test script..."
assert_contains "$composer_output" "Backend: composer-script"
assert_contains "$composer_output" "Composer smoke script ran"
assert_contains "$(cat "$COMPOSER_CALLS_FILE")" "composer:${PLUGIN_PATH}:test"
rm -f "${PLUGIN_PATH}/composer.json"

touch "${PLUGIN_PATH}/phpunit.xml.dist"
rm -f "$RESULTS_FILE"
set +e
failure_output=$(HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" HOMEBOY_COMPONENT_PATH="$PLUGIN_PATH" HOMEBOY_COMPONENT_ID="example" HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$WRITE_RESULTS_HELPER" HOMEBOY_TEST_RESULTS_FILE="$RESULTS_FILE" bash "$RUNNER" 2>&1)
failure_status=$?
set -e

if [ "$failure_status" -eq 0 ]; then
    echo "Expected no-files run with component PHPUnit config to fail" >&2
    echo "$failure_output" >&2
    exit 1
fi
assert_contains "$failure_output" "NO PHPUNIT TEST FILES DISCOVERED"
assert_contains "$failure_output" "PHPUnit config exists, but no files matched the WordPress runner discovery contract."
assert_not_contains "$failure_output" "UNCLASSIFIED PLAYGROUND FAILURE"
assert_not_contains "$failure_output" "Skipping PHPUnit tests: no files matched"
assert_contains "$(cat "$RESULTS_FILE")" '"total": 1'
assert_contains "$(cat "$RESULTS_FILE")" '"failed": 1'
assert_contains "$(cat "$RESULTS_FILE")" '"partial": "no-phpunit-tests-configured"'

assert_contains "$RUNNER_SRC" "pg_run_install_stage(['config_path' => \$config_path, 'tests_dir' => \$tests_dir]);"
assert_contains "$RUNNER_SRC" "tests_add_filter('muplugins_loaded'"
assert_contains "$RUNNER_SRC" "'activate' => false"
assert_contains "$RUNNER_SRC" "\$loaded_component_file = pg_run_load_component_stage(['plugin_path' => \$plugin_path, 'activate' => false]);"
assert_contains "$RUNNER_SRC" "\$loaded_dep_files = pg_run_load_deps_stage(['dep_mounts' => '{{PLAYGROUND_DEP_MOUNTS}}']);"
assert_contains "$RUNNER_SRC" "\$pre_component_init_callbacks = pg_snapshot_wordpress_hook_callbacks('init');"
assert_contains "$RUNNER_SRC" "tests_add_filter('muplugins_loaded'"
assert_contains "$RUNNER_SRC" "\$deferred_install_init_callbacks = pg_defer_new_wordpress_hook_callbacks('init', \$pre_component_init_callbacks);"
assert_contains "$RUNNER_SRC" "PHP_INT_MAX"
assert_contains "$RUNNER_SRC" "pg_run_deferred_wordpress_hook_callbacks(\$deferred_install_init_callbacks, [], 'init');"
assert_contains "$RUNNER_SRC" "\$pre_component_shutdown_callbacks = pg_snapshot_wordpress_hook_callbacks('shutdown');"
assert_contains "$RUNNER_SRC" "pg_remove_new_wordpress_hook_callbacks('shutdown', \$pre_component_shutdown_callbacks);"
# homeboy-extensions#431: activation must run AFTER install creates wptests_* tables.
# pg_run_load_*_stage now only require_once's plugin entry files; pg_run_activation_stage
# fires the activation hook once per file the upstream stages collected.
assert_contains "$RUNNER_SRC" "pg_run_activation_stage(['plugin_files' => \$activation_files]);"
assert_not_contains "$RUNNER_SRC" "pg_run_load_component_stage(['plugin_path' => \$plugin_path]);"
assert_contains "$BOOTSTRAP_SRC" "function pg_snapshot_wordpress_hook_callbacks"
assert_contains "$BOOTSTRAP_SRC" "function pg_remove_new_wordpress_hook_callbacks"
assert_contains "$BOOTSTRAP_SRC" "function pg_defer_new_wordpress_hook_callbacks"
assert_contains "$BOOTSTRAP_SRC" "function pg_run_deferred_wordpress_hook_callbacks"
assert_contains "$BOOTSTRAP_SRC" "function pg_run_activation_stage"
assert_not_contains "$RUNNER_SRC" "pg_snapshot_hook_callback_ids"
assert_not_contains "$RUNNER_SRC" "pg_replay_new_hook_callbacks"
assert_contains "$RUNNER_SRC" "pg_reopen_wordpress_action('wp_abilities_api_categories_init')"
assert_contains "$RUNNER_SRC" "pg_fire_reopened_wordpress_action('wp_abilities_api_init'"
assert_contains "$BOOTSTRAP_SRC" "\$cfg['activate'] ?? true"

echo "Playground no-test-files smoke passed (43 assertions)"
