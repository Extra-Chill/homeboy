#!/usr/bin/env bash
set -euo pipefail

# Test runner router for WordPress Homeboy extension.
#
# Plugin/theme PHPUnit tests run through WP Codebox by default. Core-dev
# checkouts (wordpress-develop) dispatch to WordPress core's native PHPUnit
# runner. Pure host-PHP smoke suites can explicitly use the host-smoke backend.

# Bash 4.0+ required — lint-runner.sh (called during test runs) uses
# associative arrays which are bash 4+ only. Fail early with a clear
# message rather than producing misleading cascading errors.
if ((BASH_VERSINFO[0] < 4)); then
    echo "============================================" >&2
    echo "ERROR: bash 4.0+ required (found ${BASH_VERSION})" >&2
    echo "============================================" >&2
    case "$(uname -s)" in
        Darwin)
            echo "macOS ships bash 3.2. Fix: brew install bash" >&2
            echo "Then restart your terminal (Homebrew bash takes priority on PATH)." >&2
            ;;
        *)
            echo "Update bash via your package manager." >&2
            ;;
    esac
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HOST_SMOKE_RUNNER="${HOMEBOY_RUNTIME_TEST_RUNNER_HOST_SMOKE:-${SCRIPT_DIR}/test-runner-host-smoke.sh}"
WP_CODEBOX_RUNNER="${HOMEBOY_RUNTIME_TEST_RUNNER_WP_CODEBOX:-${SCRIPT_DIR}/test-runner-wp-codebox.sh}"

# Resolve execution context and export env vars that the WordPress test runners expect.
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${SCRIPT_DIR}/../lib/resolve-context.sh}"
# shellcheck source=/dev/null
source "$RESOLVE_CONTEXT_HELPER"
homeboy_resolve_context --component-alias PLUGIN_PATH

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: Extension path: $EXTENSION_PATH"
    echo "DEBUG: Component: ${HOMEBOY_COMPONENT_ID:-none}"
    echo "DEBUG: Component path: ${COMPONENT_PATH:-$(pwd)}"
fi

TEST_BACKEND=""
if [ -n "${HOMEBOY_SETTINGS_JSON:-}" ] && [ "${HOMEBOY_SETTINGS_JSON}" != "{}" ]; then
    TEST_BACKEND=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -r '.test_backend // .testing.backend // empty' 2>/dev/null || true)
fi
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

homeboy_wordpress_is_standalone_smoke() {
    local rel_path="$1"
    case "$rel_path" in
        tests/*-smoke.php|tests/*/*-smoke.php|tests/*/*/*-smoke.php|tests/*/*/*/*-smoke.php)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

if [ -z "$TARGET_FILE" ] && [ -n "${HOMEBOY_CHANGED_TEST_FILES:-}" ]; then
    changed_smoke_files=()
    changed_non_smoke_count=0

    while IFS= read -r changed_file; do
        [ -z "$changed_file" ] && continue

        if ! changed_rel="$(homeboy_wordpress_rel_test_file "$changed_file")"; then
            changed_non_smoke_count=$((changed_non_smoke_count + 1))
            continue
        fi

        if homeboy_wordpress_is_standalone_smoke "$changed_rel"; then
            changed_smoke_files+=("$changed_rel")
        else
            changed_non_smoke_count=$((changed_non_smoke_count + 1))
        fi
    done <<< "$HOMEBOY_CHANGED_TEST_FILES"

    if [ "${#changed_smoke_files[@]}" -gt 0 ] && [ "$changed_non_smoke_count" -eq 0 ]; then
        HOMEBOY_WORDPRESS_HOST_SMOKE_FILES="$(printf '%s\n' "${changed_smoke_files[@]}")" exec bash "$HOST_SMOKE_RUNNER" "${PASSTHROUGH_ARGS[@]}"
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

case "$COMPONENT_SHAPE" in
    core-dev)
        exec bash "${SCRIPT_DIR}/test-runner-core-dev.sh" "${PASSTHROUGH_ARGS[@]}"
        ;;
    *)
        case "$TEST_BACKEND" in
            host|host-smoke)
                exec bash "$HOST_SMOKE_RUNNER" "${PASSTHROUGH_ARGS[@]}"
                ;;
            ""|wp-codebox)
                exec bash "$WP_CODEBOX_RUNNER" "${PASSTHROUGH_ARGS[@]}"
                ;;
            *)
                echo "ERROR: Unsupported WordPress test backend: ${TEST_BACKEND}" >&2
                echo "Supported backends: wp-codebox, host, host-smoke" >&2
                exit 2
                ;;
        esac
        ;;
esac
