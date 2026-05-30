#!/usr/bin/env bash
set -euo pipefail

# Test runner router for WordPress Homeboy extension.
#
# Plugin/theme PHPUnit tests and core-dev checkouts run through WP Codebox.
# Pure host-PHP smoke suites can explicitly use the host-smoke backend.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER_PRELUDE="${HOMEBOY_RUNTIME_RUNNER_PRELUDE:-${SCRIPT_DIR}/../lib/runner-prelude.sh}"

HOST_SMOKE_RUNNER="${HOMEBOY_RUNTIME_TEST_RUNNER_HOST_SMOKE:-${SCRIPT_DIR}/test-runner-host-smoke.sh}"
WP_CODEBOX_RUNNER="${HOMEBOY_RUNTIME_TEST_RUNNER_WP_CODEBOX:-${SCRIPT_DIR}/test-runner-wp-codebox.sh}"
CORE_WP_CODEBOX_RUNNER="${HOMEBOY_RUNTIME_TEST_RUNNER_CORE_WP_CODEBOX:-${SCRIPT_DIR}/test-runner-core-dev-wp-codebox.sh}"

SETTINGS_HELPER="${HOMEBOY_RUNTIME_SETTINGS_HELPER:-${SCRIPT_DIR}/../lib/settings.sh}"
# shellcheck source=/dev/null
source "$RUNNER_PRELUDE"
homeboy_runner_init --bash 4 --component-alias PLUGIN_PATH
# shellcheck source=/dev/null
source "$SETTINGS_HELPER"

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: Extension path: $EXTENSION_PATH"
    echo "DEBUG: Component: ${HOMEBOY_COMPONENT_ID:-none}"
    echo "DEBUG: Component path: ${COMPONENT_PATH:-$(pwd)}"
fi

TEST_BACKEND="$(homeboy_setting test_backend '.test_backend // .testing.backend // empty')"
TEST_BACKEND="${HOMEBOY_WORDPRESS_TEST_BACKEND:-${TEST_BACKEND:-wp-codebox}}"

TARGET_FILE=""
PASSTHROUGH_ARGS=()
while [ "$#" -gt 0 ]; do
    case "$1" in
        --file)
            shift
            if [ "$#" -eq 0 ] || [ -z "${1:-}" ]; then
                echo "ERROR: --file requires a path" >&2
                exit 2
            fi
            TARGET_FILE="$1"
            ;;
        --file=*)
            TARGET_FILE="${1#--file=}"
            ;;
        *)
            PASSTHROUGH_ARGS+=("$1")
            ;;
    esac
    shift
done

COMPONENT_SHAPE="${HOMEBOY_COMPONENT_SHAPE:-}"
if [ -z "$COMPONENT_SHAPE" ]; then
    DETECT_COMPONENT_HELPER="${HOMEBOY_RUNTIME_DETECT_COMPONENT:-${SCRIPT_DIR}/../lib/detect-component.sh}"
    # shellcheck source=../lib/detect-component.sh
    source "${DETECT_COMPONENT_HELPER}"
    if homeboy_detect_component "${COMPONENT_PATH:-$(pwd)}"; then
        COMPONENT_SHAPE="$HOMEBOY_COMPONENT_TYPE"
    fi
fi

if [ "$COMPONENT_SHAPE" = "core-dev" ]; then
    case "$TEST_BACKEND" in
        ""|wp-codebox)
            if [ -n "$TARGET_FILE" ]; then
                HOMEBOY_WORDPRESS_CORE_PHPUNIT_TEST_FILE="$TARGET_FILE" exec bash "$CORE_WP_CODEBOX_RUNNER" "${PASSTHROUGH_ARGS[@]}"
            fi
            exec bash "$CORE_WP_CODEBOX_RUNNER" "${PASSTHROUGH_ARGS[@]}"
            ;;
        *)
            echo "ERROR: Unsupported WordPress core-dev test backend: ${TEST_BACKEND}" >&2
            echo "Supported core-dev backend: wp-codebox" >&2
            exit 2
            ;;
    esac
fi

homeboy_wordpress_rel_test_file() {
    local raw_path="$1"
    local abs_path

    if [ -z "$raw_path" ]; then
        return 1
    fi

    if [ "${raw_path#/}" != "$raw_path" ]; then
        abs_path="$raw_path"
    else
        abs_path="${PLUGIN_PATH}/${raw_path}"
    fi

    if [ ! -f "$abs_path" ]; then
        return 1
    fi

    case "$abs_path" in
        "${PLUGIN_PATH}"/*)
            printf '%s\n' "${abs_path#"${PLUGIN_PATH}/"}"
            ;;
        *)
            return 1
            ;;
    esac
}

if [ -z "$TARGET_FILE" ] && [ "${HOMEBOY_TEST_SCOPE_KIND:-}" = "exclusive_env" ]; then
    if [ "${HOMEBOY_TEST_SCOPE_ENV_NAME:-}" = "HOMEBOY_WORDPRESS_HOST_SMOKE_FILES" ] && [ -n "${HOMEBOY_TEST_SCOPE_ENV_VALUE:-}" ]; then
        HOMEBOY_WORDPRESS_HOST_SMOKE_FILES="$HOMEBOY_TEST_SCOPE_ENV_VALUE" exec bash "$HOST_SMOKE_RUNNER" "${PASSTHROUGH_ARGS[@]}"
    fi
fi

if [ -n "$TARGET_FILE" ]; then
    if [ "${TARGET_FILE#/}" != "$TARGET_FILE" ]; then
        target_abs="$TARGET_FILE"
    else
        target_abs="${PLUGIN_PATH}/${TARGET_FILE}"
    fi

    if [ ! -f "$target_abs" ]; then
        echo "ERROR: requested test file not found: ${TARGET_FILE}" >&2
        exit 2
    fi

    case "$target_abs" in
        "${PLUGIN_PATH}"/*)
            target_rel="${target_abs#"${PLUGIN_PATH}/"}"
            ;;
        *)
            echo "ERROR: requested test file is outside the component: ${TARGET_FILE}" >&2
            exit 2
            ;;
    esac

    target_base="$(basename "$target_rel")"
    case "$target_rel" in
        tests/*.php|tests/*/*.php|tests/*/*/*.php|tests/*/*/*/*.php)
            case "$target_base" in
                *-smoke.php)
                    HOMEBOY_WORDPRESS_HOST_SMOKE_FILE="$target_rel" exec bash "$HOST_SMOKE_RUNNER" "${PASSTHROUGH_ARGS[@]}"
                    ;;
                *Test.php|test-*.php)
                    HOMEBOY_WORDPRESS_PHPUNIT_TEST_FILE="$target_rel" exec bash "$WP_CODEBOX_RUNNER" "${PASSTHROUGH_ARGS[@]}"
                    ;;
                *)
                    echo "ERROR: cannot classify requested WordPress test file: ${target_rel}" >&2
                    echo "  Standalone smoke files must match tests/**/*-smoke.php." >&2
                    echo "  PHPUnit files must match tests/**/*Test.php or tests/**/test-*.php." >&2
                    exit 2
                    ;;
            esac
            ;;
        *)
            echo "ERROR: requested WordPress test file must live under tests/: ${target_rel}" >&2
            exit 2
            ;;
    esac
fi

case "$TEST_BACKEND" in
    host-smoke)
        exec bash "$HOST_SMOKE_RUNNER" "${PASSTHROUGH_ARGS[@]}"
        ;;
    ""|wp-codebox)
        exec bash "$WP_CODEBOX_RUNNER" "${PASSTHROUGH_ARGS[@]}"
        ;;
    *)
        echo "ERROR: Unsupported WordPress test backend: ${TEST_BACKEND}" >&2
        echo "Supported backends: wp-codebox, host-smoke" >&2
        exit 2
        ;;
esac
