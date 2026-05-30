#!/usr/bin/env bash
set -euo pipefail

# WP Codebox-backed test runner for wordpress-develop / WordPress core source
# checkouts. Unlike plugin/theme tests, core tests mount the checkout's src and
# tests/phpunit directories into a core-shaped Playground runtime. If the core
# vendor autoload is absent, WP Codebox reports that as a structured runtime
# failure instead of this wrapper provisioning host dependencies.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${SCRIPT_DIR}/../lib/resolve-context.sh}"
WP_CODEBOX_PATHS_HELPER="${SCRIPT_DIR}/../lib/wp-codebox-paths.sh"

# shellcheck source=../lib/resolve-context.sh
source "${RESOLVE_CONTEXT_HELPER}"
homeboy_resolve_context --component-alias PLUGIN_PATH
# shellcheck source=../lib/wp-codebox-paths.sh
source "${WP_CODEBOX_PATHS_HELPER}"

CORE_PATH="$PLUGIN_PATH"

fail() {
    echo "Error: $*" >&2
    exit 1
}

is_core_dev_checkout() {
    [ -f "${CORE_PATH}/wp-config-sample.php" ] \
        && [ -f "${CORE_PATH}/src/wp-includes/version.php" ] \
        && [ -d "${CORE_PATH}/tests/phpunit" ]
}

ensure_core_dev_checkout() {
    if ! is_core_dev_checkout; then
        fail "core-dev WP Codebox runner expected wordpress-develop markers: wp-config-sample.php, src/wp-includes/version.php, tests/phpunit/"
    fi
}

WP_CODEBOX_BIN="${HOMEBOY_WP_CODEBOX_BIN:-}"
if [ -z "$WP_CODEBOX_BIN" ] && [ -n "${HOMEBOY_SETTINGS_JSON:-}" ] && [ "${HOMEBOY_SETTINGS_JSON}" != "{}" ]; then
    WP_CODEBOX_BIN=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -r '.wp_codebox_bin // empty' 2>/dev/null || true)
fi
WP_CODEBOX_BIN="${WP_CODEBOX_BIN:-wp-codebox}"
if [ "$WP_CODEBOX_BIN" = "wp-codebox" ] && ! command -v wp-codebox >/dev/null 2>&1; then
    fail "wp-codebox not found; set HOMEBOY_WP_CODEBOX_BIN, settings wp_codebox_bin, or install wp-codebox"
fi

SELECTED_TEST_FILE="${HOMEBOY_WORDPRESS_CORE_PHPUNIT_TEST_FILE:-}"
PASSTHROUGH_ARGS=()
while [ "$#" -gt 0 ]; do
    case "$1" in
        --file)
            shift
            [ "$#" -gt 0 ] && SELECTED_TEST_FILE="$1" || fail "--file requires a path"
            ;;
        --file=*)
            SELECTED_TEST_FILE="${1#--file=}"
            ;;
        *)
            PASSTHROUGH_ARGS+=("$1")
            ;;
    esac
    shift
done

ensure_core_dev_checkout

if [ "${HOMEBOY_CORE_DEV_DRY_RUN:-}" = "1" ]; then
    echo "core-dev WP Codebox test runner selected: ${CORE_PATH}"
    exit 0
fi

CORE_SRC_PATH="$(homeboy_wp_codebox_resolve_mount_path "${CORE_PATH}/src")"
CORE_TESTS_PATH="$(homeboy_wp_codebox_resolve_mount_path "${CORE_PATH}/tests/phpunit")"
CORE_VENDOR_PATH=""
if [ -d "${CORE_PATH}/vendor" ]; then
    CORE_VENDOR_PATH="$(homeboy_wp_codebox_resolve_mount_path "${CORE_PATH}/vendor")"
fi
RESULT_FILE="${CORE_PATH}/src/.pg-test-result.txt"
rm -f "$RESULT_FILE"

if [ -n "$SELECTED_TEST_FILE" ]; then
    if [ "${SELECTED_TEST_FILE#/}" != "$SELECTED_TEST_FILE" ]; then
        selected_abs="$SELECTED_TEST_FILE"
    else
        selected_abs="${CORE_PATH}/${SELECTED_TEST_FILE}"
    fi
    [ -f "$selected_abs" ] || fail "requested core PHPUnit test file not found: ${SELECTED_TEST_FILE}"
    case "$selected_abs" in
        "${CORE_PATH}"/*)
            SELECTED_TEST_FILE="${selected_abs#"${CORE_PATH}/"}"
            ;;
        *)
            fail "requested core PHPUnit test file is outside the component: ${SELECTED_TEST_FILE}"
            ;;
    esac
fi

CHANGED_TEST_FILES_JSON="[]"
if [ -n "${HOMEBOY_CHANGED_TEST_FILES:-}" ]; then
    CHANGED_TEST_FILES_JSON=$(printf '%s' "${HOMEBOY_CHANGED_TEST_FILES}" | php -r '
        $files = array_values(array_filter(array_map("trim", explode("\n", stream_get_contents(STDIN)))));
        echo json_encode($files, JSON_UNESCAPED_SLASHES);
    ' 2>/dev/null || printf '[]')
fi

WP_CONFIG_DEFINES_JSON="{}"
PLAYGROUND_WORDPRESS_VERSION="6.9"
PLAYGROUND_MULTISITE="${HOMEBOY_WORDPRESS_MULTISITE:-}"
if [ -n "${HOMEBOY_SETTINGS_JSON:-}" ] && [ "${HOMEBOY_SETTINGS_JSON}" != "{}" ]; then
    extracted=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -c '.wp_config_defines // {}' 2>/dev/null || echo "{}")
    [ -n "$extracted" ] && WP_CONFIG_DEFINES_JSON="$extracted"

    extracted=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -r '.playground_wordpress_version // .wp_codebox_wordpress_version // empty' 2>/dev/null || true)
    [ -n "$extracted" ] && [ "$extracted" != "null" ] && PLAYGROUND_WORDPRESS_VERSION="$extracted"

    if [ -z "$PLAYGROUND_MULTISITE" ]; then
        extracted=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -r '.playground.multisite // .wp_codebox_multisite // .multisite // empty' 2>/dev/null || true)
        [ -n "$extracted" ] && [ "$extracted" != "null" ] && PLAYGROUND_MULTISITE="$extracted"
    fi
fi
if [ -n "${HOMEBOY_PLAYGROUND_MULTISITE+x}" ]; then
    PLAYGROUND_MULTISITE="$HOMEBOY_PLAYGROUND_MULTISITE"
fi

MOUNTS_JSON=$(jq -nc \
    --arg src "$CORE_SRC_PATH" \
    --arg tests "$CORE_TESTS_PATH" \
    --arg vendor "$CORE_VENDOR_PATH" \
    '[
        {source: $src, target: "/wordpress", mode: "readwrite"},
        {source: $tests, target: "/wordpress/tests/phpunit", mode: "readonly"}
    ] + (if $vendor == "" then [] else [{source: $vendor, target: "/wordpress/vendor", mode: "readonly"}] end)')

ARTIFACTS_DIR="${HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR:-}"
if [ -z "$ARTIFACTS_DIR" ] && [ -n "${HOMEBOY_SETTINGS_JSON:-}" ] && [ "${HOMEBOY_SETTINGS_JSON}" != "{}" ]; then
    ARTIFACTS_DIR=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -r '.wp_codebox_artifacts_dir // empty' 2>/dev/null || true)
fi
if [ -z "$ARTIFACTS_DIR" ]; then
    ARTIFACTS_DIR=$(mktemp -d "${TMPDIR:-/tmp}/homeboy-core-wp-codebox-test-artifacts.XXXXXX")
fi

echo "Running WordPress core PHPUnit tests via WP Codebox..."
echo "  Core: ${CORE_PATH}"
echo "  Backend: wp-codebox (WordPress Playground runtime)"

WP_CODEBOX_TMPFILE=$(mktemp)
PHPUNIT_STDOUT_TMPFILE=$(mktemp)
RECIPE_FILE=$(mktemp "${TMPDIR:-/tmp}/homeboy-core-wp-codebox-test-recipe.XXXXXX")
wp_codebox_command=("$WP_CODEBOX_BIN")
case "$WP_CODEBOX_BIN" in
    *.js)
        wp_codebox_command=(node "$WP_CODEBOX_BIN")
        ;;
esac

jq -n \
    --arg wp "$PLAYGROUND_WORDPRESS_VERSION" \
    --argjson mounts "$MOUNTS_JSON" \
    --arg selectedTestFile "$SELECTED_TEST_FILE" \
    --arg changedTestsJson "$CHANGED_TEST_FILES_JSON" \
    --arg definesJson "$WP_CONFIG_DEFINES_JSON" \
    --arg multisite "$PLAYGROUND_MULTISITE" \
    '{
        schema: "wp-codebox/workspace-recipe/v1",
        runtime: {wp: $wp, blueprint: {steps: []}},
        inputs: {mounts: $mounts},
        workflow: {steps: [{command: "wordpress.core-phpunit", args: [
            "core-root=/wordpress",
            "tests-dir=/wordpress/tests/phpunit",
            "phpunit-xml=/wordpress/tests/phpunit/phpunit.xml.dist",
            "autoload-file=/wordpress/vendor/autoload.php",
            "test-file=" + $selectedTestFile,
            "changed-tests-json=" + $changedTestsJson,
            "wp-config-defines-json=" + $definesJson,
            "multisite=" + (if (($multisite | ascii_downcase) as $v | $v == "1" or $v == "true" or $v == "yes" or $v == "on") then "1" else "0" end)
        ]}]}
    }' > "$RECIPE_FILE"

set +e
"${wp_codebox_command[@]}" recipe-run \
    --recipe "$RECIPE_FILE" \
    --artifacts "$ARTIFACTS_DIR" \
    --json \
    > "$WP_CODEBOX_TMPFILE" 2>&1
wp_codebox_exit=$?
set -e

rm -f "$RECIPE_FILE"

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
    bash "$PARSE_FAILURES" "$PHPUNIT_STDOUT_TMPFILE" "${CORE_PATH:-}" || true
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
    dump_diagnostics "BOOTSTRAP FAILURE: $FAILED_STAGE_LINE"
    rm -f "$RESULT_FILE"
    exit ${wp_codebox_exit:-1}
fi

if echo "$PHPUNIT_OUTPUT" | grep -q "SOME TESTS FAILED"; then
    rm -f "$RESULT_FILE"
    exit ${wp_codebox_exit:-1}
fi

if echo "$PHPUNIT_OUTPUT" | grep -q "^NO_TEST_FILES"; then
    dump_diagnostics "NO CORE PHPUNIT TEST FILES DISCOVERED"
    rm -f "$RESULT_FILE"
    exit 1
fi

if [ $wp_codebox_exit -ne 0 ]; then
    dump_diagnostics "UNCLASSIFIED WP CODEBOX FAILURE (exit=$wp_codebox_exit)"
    rm -f "$RESULT_FILE"
    exit $wp_codebox_exit
fi

if [ -z "$PHPUNIT_OUTPUT" ] && [ -z "$PHPUNIT_STDOUT" ]; then
    dump_diagnostics "NO OUTPUT CAPTURED"
    rm -f "$RESULT_FILE"
    exit 1
fi

rm -f "$RESULT_FILE"

echo ""
echo "WordPress core WP Codebox test run complete."
