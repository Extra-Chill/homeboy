#!/usr/bin/env bash
set -euo pipefail

# WP Codebox-backed WordPress PHPUnit runner.
#
# This translates the component/dependency/drop-in/file-mount/config/env/version
# contract into wp-codebox run arguments for the default WordPress PHPUnit path.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${SCRIPT_DIR}/../lib/resolve-context.sh}"
RUNNER_STEPS_HELPER="${HOMEBOY_RUNTIME_RUNNER_STEPS:-${SCRIPT_DIR}/../lib/runner-steps.sh}"
FAILURE_TRAP_HELPER="${HOMEBOY_RUNTIME_FAILURE_TRAP:-}"
WRITE_TEST_RESULTS_HELPER="${HOMEBOY_RUNTIME_WRITE_TEST_RESULTS:-}"
DEPENDENCY_HELPER="${HOMEBOY_WORDPRESS_DEPENDENCY_HELPER:-${SCRIPT_DIR}/../lib/validation-dependencies.sh}"
PHP_PREFLIGHT_HELPER="${SCRIPT_DIR}/../lib/php-preflight.sh"
PLAYGROUND_PATHS_HELPER="${SCRIPT_DIR}/../lib/playground-paths.sh"

# shellcheck source=/dev/null
source "$RESOLVE_CONTEXT_HELPER"
homeboy_resolve_context --component-alias PLUGIN_PATH
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
# shellcheck source=../lib/playground-paths.sh
source "$PLAYGROUND_PATHS_HELPER"
# shellcheck source=/dev/null
if [ -n "$FAILURE_TRAP_HELPER" ] && [ -f "$FAILURE_TRAP_HELPER" ]; then
    source "$FAILURE_TRAP_HELPER"
    homeboy_init_failure_trap
else
    FAILED_STEP=""
    FAILURE_OUTPUT=""
    FAILURE_REPLAY_MODE="full"
fi
# shellcheck source=/dev/null
if [ -n "$WRITE_TEST_RESULTS_HELPER" ] && [ -f "$WRITE_TEST_RESULTS_HELPER" ]; then
    source "$WRITE_TEST_RESULTS_HELPER"
fi

if [ -n "${COMPONENT_ID:-}" ]; then
    PLUGIN_SLUG="$COMPONENT_ID"
else
    PLUGIN_SLUG="$(basename "$PLUGIN_PATH")"
fi

component_has_composer_test_script() {
    [ -f "${PLUGIN_PATH}/composer.json" ] || return 1

    php -r '
        $composer = json_decode(file_get_contents($argv[1]), true);
        exit(is_array($composer) && isset($composer["scripts"]["test"]) ? 0 : 1);
    ' "${PLUGIN_PATH}/composer.json" 2>/dev/null
}

run_composer_test_script() {
    echo ""
    echo "Running Composer test script..."
    echo "  Plugin: ${PLUGIN_SLUG} (${PLUGIN_PATH})"
    echo "  Backend: composer-script"

    if ! command -v composer >/dev/null 2>&1; then
        echo "ERROR: composer.json declares scripts.test, but composer is not available on PATH." >&2
        FAILED_STEP="Composer test script setup"
        return 1
    fi

    if [ "${#PASSTHROUGH_ARGS[@]}" -gt 0 ]; then
        ( cd "${PLUGIN_PATH}" && composer test -- "${PASSTHROUGH_ARGS[@]}" )
    else
        ( cd "${PLUGIN_PATH}" && composer test )
    fi
}

write_phpunit_discovery_result() {
    local status="$1"
    local partial="$2"
    local message="$3"

    if ! type homeboy_write_test_results >/dev/null 2>&1; then
        return 0
    fi

    if [ "$status" = "failed" ]; then
        homeboy_write_test_results 1 0 1 0 "$partial"
    else
        homeboy_write_test_results 0 0 0 0 "$partial"
    fi

    if [ -n "${HOMEBOY_TEST_RESULTS_FILE:-}" ] && [ -f "$HOMEBOY_TEST_RESULTS_FILE" ]; then
        php -r '
            $path = $argv[1];
            $status = $argv[2];
            $message = $argv[3];
            $data = json_decode(file_get_contents($path), true);
            if (!is_array($data)) {
                $data = [];
            }
            $data["status"] = $status;
            if ($message !== "") {
                $data["message"] = $message;
            }
            file_put_contents($path, json_encode($data, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES) . PHP_EOL);
        ' "$HOMEBOY_TEST_RESULTS_FILE" "$status" "$message"
    fi
}

SELECTED_TEST_FILE="${HOMEBOY_WORDPRESS_PHPUNIT_TEST_FILE:-}"
PASSTHROUGH_ARGS=()
while [ "$#" -gt 0 ]; do
    case "$1" in
        --filter)
            PASSTHROUGH_ARGS+=("$1")
            shift
            if [ "$#" -eq 0 ] || [ -z "${1:-}" ]; then
                echo "ERROR: --filter requires a value" >&2
                exit 2
            fi
            PASSTHROUGH_ARGS+=("$1")
            ;;
        --file)
            shift
            if [ "$#" -eq 0 ] || [ -z "${1:-}" ]; then
                echo "ERROR: --file requires a path" >&2
                exit 2
            fi
            SELECTED_TEST_FILE="$1"
            ;;
        --file=*)
            SELECTED_TEST_FILE="${1#--file=}"
            ;;
        *)
            if [ -z "$SELECTED_TEST_FILE" ]; then
                if [ "${1#/}" != "$1" ]; then
                    candidate_test_file="$1"
                else
                    candidate_test_file="${PLUGIN_PATH}/${1}"
                fi

                if [ -f "$candidate_test_file" ]; then
                    SELECTED_TEST_FILE="$1"
                    shift
                    continue
                fi
            fi

            PASSTHROUGH_ARGS+=("$1")
            ;;
    esac
    shift
done

WP_CODEBOX_BIN="${HOMEBOY_WP_CODEBOX_BIN:-}"
if [ -z "$WP_CODEBOX_BIN" ] && [ -n "${HOMEBOY_SETTINGS_JSON:-}" ] && [ "${HOMEBOY_SETTINGS_JSON}" != "{}" ]; then
    WP_CODEBOX_BIN=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -r '.wp_codebox_bin // empty' 2>/dev/null || true)
fi
WP_CODEBOX_BIN="${WP_CODEBOX_BIN:-wp-codebox}"
if [ "$WP_CODEBOX_BIN" = "wp-codebox" ] && ! command -v wp-codebox >/dev/null 2>&1; then
    echo "Error: wp-codebox not found; set HOMEBOY_WP_CODEBOX_BIN, settings wp_codebox_bin, or install wp-codebox." >&2
    FAILED_STEP="WP Codebox CLI setup"
    exit 1
fi

TEST_DIR="${PLUGIN_PATH}/tests"
if [ ! -d "$TEST_DIR" ]; then
    if component_has_composer_test_script; then
        run_composer_test_script
        exit $?
    fi

    echo ""
    echo "Warning: No tests directory found at ${TEST_DIR}"
    echo "  Skipping PHPUnit tests."
    echo ""
    exit 0
fi

if type homeboy_php_preflight &>/dev/null; then
    homeboy_php_preflight "$PLUGIN_PATH"
fi

WP_TEST_SMELLS="${EXTENSION_PATH}/scripts/audit/wp-test-smells.py"
if [ -f "$WP_TEST_SMELLS" ]; then
    python3 "$WP_TEST_SMELLS" "$PLUGIN_PATH"
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

WP_CONFIG_DEFINES_JSON="{}"
BENCH_ENV_JSON="{}"
PLAYGROUND_FILE_MOUNTS_JSON="[]"
PHPUNIT_NO_TESTS="skipped"
PLAYGROUND_WORDPRESS_VERSION="6.9"
if [ -n "${HOMEBOY_SETTINGS_JSON:-}" ] && [ "${HOMEBOY_SETTINGS_JSON}" != "{}" ]; then
    extracted=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -c '.wp_config_defines // {}' 2>/dev/null || echo "{}")
    [ -n "$extracted" ] && WP_CONFIG_DEFINES_JSON="$extracted"

    extracted=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -c '.bench_env // {}' 2>/dev/null || echo "{}")
    [ -n "$extracted" ] && BENCH_ENV_JSON="$extracted"

    extracted=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -c '.playground_file_mounts // []' 2>/dev/null || echo "[]")
    [ -n "$extracted" ] && [ "$extracted" != "null" ] && PLAYGROUND_FILE_MOUNTS_JSON="$extracted"

    extracted=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -r '.phpunit_no_tests // empty' 2>/dev/null || true)
    [ -n "$extracted" ] && [ "$extracted" != "null" ] && PHPUNIT_NO_TESTS="$extracted"

    extracted=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -r '.playground_wordpress_version // .wp_codebox_wordpress_version // empty' 2>/dev/null || true)
    [ -n "$extracted" ] && [ "$extracted" != "null" ] && PLAYGROUND_WORDPRESS_VERSION="$extracted"
fi

CHANGED_TEST_FILES_JSON="[]"
if [ -n "${HOMEBOY_CHANGED_TEST_FILES:-}" ]; then
    CHANGED_TEST_FILES_JSON=$(printf '%s' "${HOMEBOY_CHANGED_TEST_FILES}" | php -r '
        $files = array_values(array_filter(array_map("trim", explode("\n", stream_get_contents(STDIN)))));
        echo json_encode($files, JSON_UNESCAPED_SLASHES);
    ' 2>/dev/null || printf '[]')
fi

SELECTED_TEST_FILE_REL=""
if [ -n "$SELECTED_TEST_FILE" ]; then
    if [ "${SELECTED_TEST_FILE#/}" != "$SELECTED_TEST_FILE" ]; then
        selected_abs="$SELECTED_TEST_FILE"
    else
        selected_abs="${PLUGIN_PATH}/${SELECTED_TEST_FILE}"
    fi
    if [ ! -f "$selected_abs" ]; then
        echo "ERROR: requested PHPUnit test file not found: ${SELECTED_TEST_FILE}" >&2
        exit 2
    fi
    case "$selected_abs" in
        "${PLUGIN_PATH}"/tests/*.php|"${PLUGIN_PATH}"/tests/*/*.php|"${PLUGIN_PATH}"/tests/*/*/*.php|"${PLUGIN_PATH}"/tests/*/*/*/*.php)
            SELECTED_TEST_FILE_REL="${selected_abs#"${PLUGIN_PATH}/"}"
            ;;
        *)
            echo "ERROR: requested PHPUnit test file must live under tests/: ${SELECTED_TEST_FILE}" >&2
            exit 2
            ;;
    esac
fi
SELECTED_TEST_FILE_B64=$(printf '%s' "$SELECTED_TEST_FILE_REL" | base64 | tr -d '\n')

MOUNTS_JSON="[]"
homeboy_wp_codebox_add_recipe_mount() {
    local source="$1"
    local target="$2"
    local mode="${3:-readwrite}"
    MOUNTS_JSON=$(jq -nc --argjson mounts "$MOUNTS_JSON" --arg source "$source" --arg target "$target" --arg mode "$mode" '$mounts + [{source: $source, target: $target, mode: $mode}]')
}

homeboy_wp_codebox_add_recipe_mount "${PLUGIN_PATH}" "/wordpress/wp-content/plugins/${PLUGIN_SLUG}"

if [ -n "$DEPENDENCY_PATHS" ]; then
    while IFS= read -r dep_path; do
        [ -z "$dep_path" ] && continue
        dep_slug="$(homeboy_get_validation_dependency_slug "$dep_path" || basename "$dep_path")"
        homeboy_wp_codebox_add_recipe_mount "${dep_path}" "/wordpress/wp-content/plugins/${dep_slug}"
    done <<< "$DEPENDENCY_PATHS"
fi

PLUGIN_DB_PHP="${PLUGIN_PATH}/db.php"
if [ -f "$PLUGIN_DB_PHP" ]; then
    homeboy_wp_codebox_add_recipe_mount "${PLUGIN_DB_PHP}" "/wordpress/wp-content/db.php"
fi

if printf '%s' "$PLAYGROUND_FILE_MOUNTS_JSON" | jq -e 'type == "array" and length > 0' >/dev/null 2>&1; then
    while IFS= read -r mount_json; do
        [ -n "$mount_json" ] || continue
        mount_from=$(printf '%s' "$mount_json" | jq -r '.from // empty')
        mount_to=$(printf '%s' "$mount_json" | jq -r '.to // empty')
        mount_dependency=$(printf '%s' "$mount_json" | jq -r '.from_dependency // empty')
        if [ -z "$mount_from" ] || [ -z "$mount_to" ]; then
            echo "Error: playground_file_mounts entries require 'from' and 'to'" >&2
            FAILED_STEP="WP Codebox file mount setup"
            exit 1
        fi
        if [[ "$mount_from" = /* ]] || [[ "$mount_from" == *..* ]]; then
            echo "Error: playground_file_mounts 'from' must be a relative path without '..' (got '$mount_from')" >&2
            FAILED_STEP="WP Codebox file mount setup"
            exit 1
        fi
        if [[ "$mount_to" != /* ]]; then
            echo "Error: playground_file_mounts 'to' must be an absolute Playground path (got '$mount_to')" >&2
            FAILED_STEP="WP Codebox file mount setup"
            exit 1
        fi

        mount_root="$PLUGIN_PATH"
        if [ -n "$mount_dependency" ]; then
            mount_root=""
            if [ -n "$DEPENDENCY_PATHS" ]; then
                while IFS= read -r dep_path; do
                    [ -z "$dep_path" ] && continue
                    dep_slug="$(homeboy_get_validation_dependency_slug "$dep_path" || basename "$dep_path")"
                    if [ "$dep_slug" = "$mount_dependency" ] || [ "$(basename "$dep_path")" = "$mount_dependency" ]; then
                        mount_root="$dep_path"
                        break
                    fi
                done <<< "$DEPENDENCY_PATHS"
            fi
            if [ -z "$mount_root" ]; then
                echo "Error: playground_file_mounts dependency not found: $mount_dependency" >&2
                FAILED_STEP="WP Codebox file mount setup"
                exit 1
            fi
        fi

        mount_host="${mount_root}/${mount_from}"
        if [ ! -f "$mount_host" ]; then
            echo "Error: playground_file_mounts source file not found: $mount_host" >&2
            FAILED_STEP="WP Codebox file mount setup"
            exit 1
        fi
        homeboy_wp_codebox_add_recipe_mount "${mount_host}" "${mount_to}"
    done < <(printf '%s' "$PLAYGROUND_FILE_MOUNTS_JSON" | jq -c '.[]')
fi

EXTENSION_MOUNT_PATH="$(homeboy_playground_resolve_mount_path "$EXTENSION_PATH")"
homeboy_wp_codebox_add_recipe_mount "${EXTENSION_MOUNT_PATH}" "/homeboy-extension" "readonly"

PLAYGROUND_DEP_MOUNTS=""
if [ -n "$DEPENDENCY_PATHS" ]; then
    while IFS= read -r dep_path; do
        [ -z "$dep_path" ] && continue
        dep_slug="$(homeboy_get_validation_dependency_slug "$dep_path" || basename "$dep_path")"
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
    echo "Error: playground-runner.php template not found at $TEMPLATE" >&2
    FAILED_STEP="WP Codebox setup"
    exit 1
fi

WRAPPER_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/wp-codebox-runner.XXXXXX")
WP_CONFIG_DEFINES_DELIM=$(printf '\1')
json_to_base64() {
    printf '%s' "$1" | base64 | tr -d '\n'
}
WP_CONFIG_DEFINES_JSON_B64=$(json_to_base64 "$WP_CONFIG_DEFINES_JSON")
BENCH_ENV_JSON_B64=$(json_to_base64 "$BENCH_ENV_JSON")
CHANGED_TEST_FILES_JSON_B64=$(json_to_base64 "$CHANGED_TEST_FILES_JSON")

sed \
    -e "s|{{PLUGIN_SLUG}}|${PLUGIN_SLUG}|g" \
    -e "s|{{PLAYGROUND_DEP_MOUNTS}}|${PLAYGROUND_DEP_MOUNTS}|g" \
    -e "s|{{PHPUNIT_TEST_FILE_B64}}|${SELECTED_TEST_FILE_B64}|g" \
    -e "s${WP_CONFIG_DEFINES_DELIM}{{WP_CONFIG_DEFINES_JSON_B64}}${WP_CONFIG_DEFINES_DELIM}${WP_CONFIG_DEFINES_JSON_B64}${WP_CONFIG_DEFINES_DELIM}g" \
    -e "s${WP_CONFIG_DEFINES_DELIM}{{BENCH_ENV_JSON_B64}}${WP_CONFIG_DEFINES_DELIM}${BENCH_ENV_JSON_B64}${WP_CONFIG_DEFINES_DELIM}g" \
    -e "s${WP_CONFIG_DEFINES_DELIM}{{CHANGED_TEST_FILES_JSON_B64}}${WP_CONFIG_DEFINES_DELIM}${CHANGED_TEST_FILES_JSON_B64}${WP_CONFIG_DEFINES_DELIM}g" \
    "$TEMPLATE" > "$WRAPPER_TMPFILE"

ARTIFACTS_DIR="${HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR:-}"
if [ -z "$ARTIFACTS_DIR" ] && [ -n "${HOMEBOY_SETTINGS_JSON:-}" ] && [ "${HOMEBOY_SETTINGS_JSON}" != "{}" ]; then
    ARTIFACTS_DIR=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -r '.wp_codebox_artifacts_dir // empty' 2>/dev/null || true)
fi
if [ -z "$ARTIFACTS_DIR" ]; then
    ARTIFACTS_DIR=$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wp-codebox-test-artifacts.XXXXXX")
fi

echo "Running PHPUnit tests via WP Codebox..."
echo "  Plugin: ${PLUGIN_SLUG} (${PLUGIN_PATH})"
echo "  Backend: wp-codebox (WordPress Playground runtime)"

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "  Wrapper: $WRAPPER_TMPFILE"
    echo "  Mounts: ${MOUNTS_JSON}"
    echo "  WordPress version: ${PLAYGROUND_WORDPRESS_VERSION}"
    echo "  Artifacts: ${ARTIFACTS_DIR}"
fi

WP_CODEBOX_TMPFILE=$(mktemp)
PHPUNIT_STDOUT_TMPFILE=$(mktemp)
RECIPE_FILE=$(mktemp "${TMPDIR:-/tmp}/homeboy-wp-codebox-test-recipe.XXXXXX")
wp_codebox_command=("$WP_CODEBOX_BIN")
case "$WP_CODEBOX_BIN" in
    *.js)
        wp_codebox_command=(node "$WP_CODEBOX_BIN")
        ;;
esac

jq -n \
    --arg wp "$PLAYGROUND_WORDPRESS_VERSION" \
    --argjson mounts "$MOUNTS_JSON" \
    --arg codeFile "$WRAPPER_TMPFILE" \
    '{
        schema: "wp-codebox/workspace-recipe/v1",
        runtime: {wp: $wp, blueprint: {steps: []}},
        inputs: {mounts: $mounts},
        workflow: {steps: [{command: "wordpress.run-php", args: ["code-file=" + $codeFile, "bootstrap=none"]}]}
    }' > "$RECIPE_FILE"

set +e
"${wp_codebox_command[@]}" recipe-run \
    --recipe "$RECIPE_FILE" \
    --artifacts "$ARTIFACTS_DIR" \
    --json \
    > "$WP_CODEBOX_TMPFILE" 2>&1
wp_codebox_exit=$?
set -e

rm -f "$WRAPPER_TMPFILE" "$RECIPE_FILE"

WP_CODEBOX_OUTPUT=$(cat "$WP_CODEBOX_TMPFILE")
if [ -n "$WP_CODEBOX_OUTPUT" ]; then
    jq -r '(.executions // [])[-1].stdout // empty' "$WP_CODEBOX_TMPFILE" 2>/dev/null || cat "$WP_CODEBOX_TMPFILE"
fi

PHPUNIT_OUTPUT=""
if [ -f "$RESULT_FILE" ]; then
    PHPUNIT_OUTPUT=$(cat "$RESULT_FILE")
fi
PHPUNIT_STDOUT=$(jq -r '(.executions // [])[-1].stdout // empty' "$WP_CODEBOX_TMPFILE" 2>/dev/null || true)
printf '%s\n' "$PHPUNIT_STDOUT" > "$PHPUNIT_STDOUT_TMPFILE"

PARSE_RESULTS="${EXTENSION_PATH}/scripts/test/parse-test-results.sh"
PARSE_FAILURES="${EXTENSION_PATH}/scripts/test/parse-test-failures.sh"
if [ -n "${HOMEBOY_TEST_RESULTS_FILE:-}" ] && [ -f "$PARSE_RESULTS" ]; then
    if [ -n "$PHPUNIT_STDOUT" ]; then
        bash "$PARSE_RESULTS" "$PHPUNIT_STDOUT_TMPFILE" || true
    elif [ -n "$PHPUNIT_OUTPUT" ]; then
        bash "$PARSE_RESULTS" "$RESULT_FILE" || true
    fi
fi
if [ -n "${HOMEBOY_TEST_FAILURES_FILE:-}" ] && [ -f "$PARSE_FAILURES" ] && [ -n "$PHPUNIT_STDOUT" ]; then
    bash "$PARSE_FAILURES" "$PHPUNIT_STDOUT_TMPFILE" "${PLUGIN_PATH:-}" || true
fi
rm -f "$WP_CODEBOX_TMPFILE" "$PHPUNIT_STDOUT_TMPFILE"

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
        echo "--- WP Codebox stdout ---"
        echo "$PHPUNIT_STDOUT"
    fi
}

if echo "$PHPUNIT_OUTPUT" | grep -qE '^STAGE_(FAIL|FATAL):'; then
    FAILED_STAGE_LINE=$(echo "$PHPUNIT_OUTPUT" | grep -E '^STAGE_(FAIL|FATAL):' | head -1)
    FAILED_STAGE_DETAIL=$(echo "$FAILED_STAGE_LINE" | sed -E 's/^STAGE_(FAIL|FATAL)://')
    FAILED_STEP="WP Codebox bootstrap (${FAILED_STAGE_DETAIL%%:*} stage)"
    FAILURE_OUTPUT="$FAILED_STAGE_LINE"
    dump_diagnostics "BOOTSTRAP FAILURE: $FAILED_STAGE_DETAIL"
    rm -f "$RESULT_FILE"
    exit ${wp_codebox_exit:-1}
fi

if echo "$PHPUNIT_OUTPUT" | grep -q "SOME TESTS FAILED"; then
    FAILED_STEP="PHPUnit tests (wp-codebox backend)"
    FAILURE_REPLAY_MODE="none"
    rm -f "$RESULT_FILE"
    exit ${wp_codebox_exit:-1}
fi

if [ $wp_codebox_exit -ne 0 ] && echo "$PHPUNIT_STDOUT" | grep -qE '^(FAILURES|ERRORS)!'; then
    FAILED_STEP="PHPUnit tests (wp-codebox backend)"
    FAILURE_REPLAY_MODE="none"
    rm -f "$RESULT_FILE"
    exit $wp_codebox_exit
fi

if [ $wp_codebox_exit -ne 0 ] && echo "$PHPUNIT_STDOUT" | grep -qE '^(PHP Parse error|Parse error:|PHP Fatal error|Fatal error:)'; then
    FAILED_STEP="WP Codebox PHP crash (before runner took control)"
    FAILURE_OUTPUT=$(echo "$PHPUNIT_STDOUT" | grep -E '^(PHP Parse error|Parse error:|PHP Fatal error|Fatal error:)' | head -5)
    dump_diagnostics "PHP CRASH"
    rm -f "$RESULT_FILE"
    exit $wp_codebox_exit
fi

if echo "$PHPUNIT_OUTPUT" | grep -q "^NO_TEST_FILES"; then
    dump_diagnostics "NO PHPUNIT TEST FILES DISCOVERED"
    if component_has_composer_test_script; then
        rm -f "$RESULT_FILE"
        run_composer_test_script
        exit $?
    fi

    if [ "$PHPUNIT_NO_TESTS" = "failed" ] || [ "$PHPUNIT_NO_TESTS" = "fail" ] || [ -f "${PLUGIN_PATH}/phpunit.xml" ] || [ -f "${PLUGIN_PATH}/phpunit.xml.dist" ]; then
        echo ""
        if [ "$PHPUNIT_NO_TESTS" = "failed" ] || [ "$PHPUNIT_NO_TESTS" = "fail" ]; then
            echo "PHPUnit no-test discovery is configured as failure, and no files matched the WordPress runner discovery contract."
        else
            echo "PHPUnit config exists, but no files matched the WordPress runner discovery contract."
        fi
        echo "  Check phpunit.xml(.dist), tests/ directory layout, and Test.php/test- naming."
        FAILED_STEP="PHPUnit tests (configured suite discovered no test files, wp-codebox)"
        write_phpunit_discovery_result failed "no-phpunit-tests-configured" "Plugin activation/install passed; PHPUnit discovery found zero tests; no PHPUnit assertions ran."
        rm -f "$RESULT_FILE"
        exit 1
    fi

    echo ""
    echo "Skipping PHPUnit tests: plugin activation/install passed, but no files matched the WordPress runner discovery contract."
    echo "  Contract: files under ${TEST_DIR} ending in Test.php or starting with test-."
    echo "  PHPUnit discovery found zero tests; no PHPUnit assertions ran."
    echo "  Add matching PHPUnit files or a component phpunit.xml(.dist) if this suite should run here."
    write_phpunit_discovery_result skipped "no-phpunit-tests" "Plugin activation/install passed; PHPUnit discovery found zero tests; no PHPUnit assertions ran."
    rm -f "$RESULT_FILE"
    exit 0
fi

if [ $wp_codebox_exit -ne 0 ]; then
    FAILED_STEP="WP Codebox exited with code $wp_codebox_exit (unclassified)"
    dump_diagnostics "UNCLASSIFIED WP CODEBOX FAILURE (exit=$wp_codebox_exit)"
    rm -f "$RESULT_FILE"
    exit $wp_codebox_exit
fi

if [ -z "$PHPUNIT_OUTPUT" ] && [ -z "$PHPUNIT_STDOUT" ]; then
    dump_diagnostics "NO OUTPUT CAPTURED"
    FAILED_STEP="PHPUnit tests (no output, wp-codebox)"
    rm -f "$RESULT_FILE"
    exit 1
fi

if echo "$PHPUNIT_STDOUT" | grep -qE 'No tests executed|OK \(0 tests'; then
    dump_diagnostics "ZERO TESTS EXECUTED"
    FAILED_STEP="PHPUnit tests (zero tests executed, wp-codebox)"
    rm -f "$RESULT_FILE"
    exit 1
fi

if echo "$PHPUNIT_OUTPUT" | grep -q "^NOTICE:"; then
    echo ""
    echo "--- Bootstrap notices (non-fatal) ---"
    echo "$PHPUNIT_OUTPUT" | grep "^NOTICE:"
fi

rm -f "$RESULT_FILE"

echo ""
echo "WP Codebox test run complete."
