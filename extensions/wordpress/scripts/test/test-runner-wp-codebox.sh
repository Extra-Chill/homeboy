#!/usr/bin/env bash
set -euo pipefail

# WP Codebox test runner for WordPress Homeboy extension.
#
# This backend is opt-in via HOMEBOY_WORDPRESS_TEST_RUNTIME=wp-codebox or
# settings test_runtime=wp-codebox. It intentionally mirrors the Playground
# runner's component/mount argument translation while preserving the top-level
# Homeboy test command contract: stdout/stderr plus the wp-codebox process exit
# status decide pass/fail.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${SCRIPT_DIR}/../lib/resolve-context.sh}"
DEPENDENCY_HELPER="${HOMEBOY_WORDPRESS_DEPENDENCY_HELPER:-${SCRIPT_DIR}/../lib/validation-dependencies.sh}"

# shellcheck source=/dev/null
source "$RESOLVE_CONTEXT_HELPER"
homeboy_resolve_context --component-alias PLUGIN_PATH

# shellcheck source=../lib/validation-dependencies.sh
if [ -f "$DEPENDENCY_HELPER" ]; then
    source "$DEPENDENCY_HELPER"
fi

SETTINGS_JSON="${HOMEBOY_SETTINGS_JSON:-}"
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
            PASSTHROUGH_ARGS+=("$1")
            ;;
    esac
    shift
done

WP_CODEBOX_BIN="${HOMEBOY_WP_CODEBOX_BIN:-}"
if [ -z "$WP_CODEBOX_BIN" ] && [ -n "$SETTINGS_JSON" ] && [ "$SETTINGS_JSON" != "{}" ]; then
    WP_CODEBOX_BIN=$(printf '%s' "$SETTINGS_JSON" | jq -r '.wp_codebox_bin // empty' 2>/dev/null || true)
fi
WP_CODEBOX_BIN="${WP_CODEBOX_BIN:-wp-codebox}"
if [ "$WP_CODEBOX_BIN" = "wp-codebox" ] && ! command -v wp-codebox >/dev/null 2>&1; then
    echo "ERROR: wp-codebox not found; set HOMEBOY_WP_CODEBOX_BIN or settings wp_codebox_bin" >&2
    exit 1
fi

if [ -n "${COMPONENT_ID:-}" ]; then
    PLUGIN_SLUG="$COMPONENT_ID"
else
    PLUGIN_SLUG="$(basename "$PLUGIN_PATH")"
fi

TEST_DIR="${PLUGIN_PATH}/tests"
if [ ! -d "$TEST_DIR" ]; then
    echo ""
    echo "Warning: No tests directory found at ${TEST_DIR}"
    echo "  Skipping PHPUnit tests."
    echo ""
    exit 0
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

PLAYGROUND_WORDPRESS_VERSION="6.9"
if [ -n "$SETTINGS_JSON" ] && [ "$SETTINGS_JSON" != "{}" ]; then
    extracted=$(printf '%s' "$SETTINGS_JSON" | jq -r '.playground_wordpress_version // .wp_codebox_wordpress_version // empty' 2>/dev/null || true)
    if [ -n "$extracted" ] && [ "$extracted" != "null" ]; then
        PLAYGROUND_WORDPRESS_VERSION="$extracted"
    fi
fi

ARTIFACTS_DIR="${HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR:-}"
if [ -z "$ARTIFACTS_DIR" ] && [ -n "$SETTINGS_JSON" ] && [ "$SETTINGS_JSON" != "{}" ]; then
    ARTIFACTS_DIR=$(printf '%s' "$SETTINGS_JSON" | jq -r '.wp_codebox_artifacts_dir // empty' 2>/dev/null || true)
fi
RUNTIME_DIR=""
if [ -z "$ARTIFACTS_DIR" ]; then
    RUNTIME_DIR=$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wp-codebox-test.XXXXXX")
    ARTIFACTS_DIR="$RUNTIME_DIR/artifacts"
fi

if type homeboy_export_validation_dependency_paths >/dev/null 2>&1; then
    homeboy_export_validation_dependency_paths "$PLUGIN_PATH"
fi
DEPENDENCY_PATHS="${HOMEBOY_WORDPRESS_DEPENDENCY_PATHS:-}"

MOUNT_ARGS=("--mount" "${PLUGIN_PATH}:/wordpress/wp-content/plugins/${PLUGIN_SLUG}")
if [ -n "$DEPENDENCY_PATHS" ]; then
    while IFS= read -r dep_path; do
        [ -z "$dep_path" ] && continue
        dep_slug="$(homeboy_get_validation_dependency_slug "$dep_path" || basename "$dep_path")"
        MOUNT_ARGS+=("--mount" "${dep_path}:/wordpress/wp-content/plugins/${dep_slug}")
    done <<< "$DEPENDENCY_PATHS"
fi

PLUGIN_DB_PHP="${PLUGIN_PATH}/db.php"
if [ -f "$PLUGIN_DB_PHP" ]; then
    MOUNT_ARGS+=("--mount" "${PLUGIN_DB_PHP}:/wordpress/wp-content/db.php")
fi

CODE_FILE=$(mktemp "${TMPDIR:-/tmp}/homeboy-wp-codebox-phpunit.XXXXXX.php")
trap 'rm -f "$CODE_FILE"; if [ -n "${RUNTIME_DIR:-}" ]; then rm -rf "$RUNTIME_DIR"; fi' EXIT

php -r '
    $path = $argv[1];
    $pluginSlug = $argv[2];
    $selected = $argv[3];
    $changed = $argv[4];
    $passthrough = array_slice($argv, 5);
    $payload = var_export(array(
        "plugin_slug" => $pluginSlug,
        "plugin_path" => "/wordpress/wp-content/plugins/" . $pluginSlug,
        "selected_test_file" => $selected,
        "changed_test_files" => $changed,
        "passthrough_args" => $passthrough,
    ), true);
    file_put_contents($path, "<?php\n" .
        "\$homeboy_wp_codebox_test_context = " . $payload . ";\n" .
        "echo json_encode(array(\"schema\" => \"homeboy/wp-codebox-test-runner/v1\", \"context\" => \$homeboy_wp_codebox_test_context), JSON_UNESCAPED_SLASHES) . PHP_EOL;\n"
    );
' "$CODE_FILE" "$PLUGIN_SLUG" "$SELECTED_TEST_FILE_REL" "${HOMEBOY_CHANGED_TEST_FILES:-}" "${PASSTHROUGH_ARGS[@]}"

WP_CODEBOX_COMMAND=("$WP_CODEBOX_BIN")
case "$WP_CODEBOX_BIN" in
    *.js)
        WP_CODEBOX_COMMAND=(node "$WP_CODEBOX_BIN")
        ;;
esac

echo "Running PHPUnit tests via WP Codebox..."
echo "  Plugin: ${PLUGIN_SLUG} (${PLUGIN_PATH})"
echo "  Backend: wp-codebox"

"${WP_CODEBOX_COMMAND[@]}" run \
    "${MOUNT_ARGS[@]}" \
    --command wordpress.run-php \
    --arg "code-file=${CODE_FILE}" \
    --wp "$PLAYGROUND_WORDPRESS_VERSION" \
    --artifacts "$ARTIFACTS_DIR" \
    --json
