#!/usr/bin/env bash
set -euo pipefail

# Test runner router for WordPress Homeboy extension.
#
# Plugin/theme PHPUnit tests and core-dev checkouts run through WP Codebox.
# Pure host-PHP smoke suites can explicitly use the host-smoke backend.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER_PRELUDE="${HOMEBOY_RUNTIME_RUNNER_PRELUDE:-${SCRIPT_DIR}/../lib/runner-prelude.sh}"

SMOKE_RUNNER="${HOMEBOY_RUNTIME_TEST_RUNNER_HOST_SMOKE_WP:-${SCRIPT_DIR}/test-runner-host-smoke-wp.sh}"
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

# WordPress tests run through WP Codebox against real WordPress, routed purely by
# file type: tests/**/*Test.php and tests/**/test-*.php run via the PHPUnit
# backend, and tests/**/*-smoke.php standalone scripts run via the real-WordPress
# smoke backend (wordpress.run-php with WordPress booted). There is no
# bare-host/no-WordPress backend and no test_backend toggle; the runtime is
# always WP Codebox.

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
    if [ -n "$TARGET_FILE" ]; then
        HOMEBOY_WORDPRESS_CORE_PHPUNIT_TEST_FILE="$TARGET_FILE" exec bash "$CORE_WP_CODEBOX_RUNNER" "${PASSTHROUGH_ARGS[@]}"
    fi
    exec bash "$CORE_WP_CODEBOX_RUNNER" "${PASSTHROUGH_ARGS[@]}"
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

    if [ ! -f "$abs_path" ] && [[ "$raw_path" == wordpress/* ]]; then
        abs_path="${PLUGIN_PATH}/${raw_path#wordpress/}"
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

homeboy_wordpress_run_js_smoke_files() {
    local smoke_files_raw="$1"
    local node_bin="${HOMEBOY_NODE_BIN:-node}"
    local smoke_files=()
    local smoke_file
    local smoke_abs
    local rel_path
    local passed=0

    while IFS= read -r smoke_file; do
        [ -n "$smoke_file" ] || continue
        if ! smoke_abs="$(homeboy_wordpress_rel_test_file "$smoke_file")"; then
            echo "ERROR: requested JS smoke file not found or outside the component: ${smoke_file}" >&2
            exit 2
        fi
        smoke_files+=("${PLUGIN_PATH}/${smoke_abs}")
    done <<< "$smoke_files_raw"

    if [ "${#smoke_files[@]}" -eq 0 ]; then
        echo "ERROR: no JS smoke files were selected" >&2
        exit 2
    fi

    echo "Running host JS smoke tests..."
    echo "  Component: ${HOMEBOY_COMPONENT_ID:-$(basename "$PLUGIN_PATH")} (${PLUGIN_PATH})"
    echo "  Backend: host-js-smoke"
    echo "  Files: ${#smoke_files[@]}"
    echo ""

    for smoke_file in "${smoke_files[@]}"; do
        rel_path="${smoke_file#"${PLUGIN_PATH}/"}"
        echo "JS_SMOKE_BEGIN:${rel_path}"
        if "$node_bin" "$smoke_file"; then
            echo "JS_SMOKE_OK:${rel_path}"
            passed=$((passed + 1))
        else
            exit_code=$?
            echo "JS_SMOKE_FAIL:${rel_path}:exit=${exit_code}"
            echo ""
            echo "JS smoke test failed: ${rel_path}"
            exit "$exit_code"
        fi
    done

    echo ""
    echo "JS_SMOKE_SUMMARY:passed=${passed} failed=0"
    echo "Host JS smoke test run complete."
}

homeboy_wordpress_is_js_smoke_file() {
    case "$1" in
        tests/*-smoke.js|tests/*/*-smoke.js|tests/*/*/*-smoke.js|tests/*/*/*/*-smoke.js|wordpress/tests/*-smoke.js|wordpress/tests/*/*-smoke.js|wordpress/tests/*/*/*-smoke.js|wordpress/tests/*/*/*/*-smoke.js)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

homeboy_wordpress_run_shell_smoke_files() {
    local smoke_files_raw="$1"
    local smoke_files=()
    local smoke_file smoke_abs rel_path
    local passed=0

    while IFS= read -r smoke_file; do
        [ -n "$smoke_file" ] || continue
        if ! smoke_abs="$(homeboy_wordpress_rel_test_file "$smoke_file")"; then
            echo "ERROR: requested shell smoke file not found or outside the component: ${smoke_file}" >&2
            exit 2
        fi
        smoke_files+=("${PLUGIN_PATH}/${smoke_abs}")
    done <<< "$smoke_files_raw"

    if [ "${#smoke_files[@]}" -eq 0 ]; then
        echo "ERROR: no shell smoke files were selected" >&2
        exit 2
    fi

    echo "Running host shell smoke tests..."
    echo "  Component: ${HOMEBOY_COMPONENT_ID:-$(basename "$PLUGIN_PATH")} (${PLUGIN_PATH})"
    echo "  Backend: host-shell-smoke"
    echo "  Files: ${#smoke_files[@]}"
    echo ""

    for smoke_file in "${smoke_files[@]}"; do
        rel_path="${smoke_file#"${PLUGIN_PATH}/"}"
        echo "SHELL_SMOKE_BEGIN:${rel_path}"
        if bash "$smoke_file"; then
            echo "SHELL_SMOKE_OK:${rel_path}"
            passed=$((passed + 1))
        else
            exit_code=$?
            echo "SHELL_SMOKE_FAIL:${rel_path}:exit=${exit_code}"
            echo ""
            echo "Shell smoke test failed: ${rel_path}"
            exit "$exit_code"
        fi
    done

    echo ""
    echo "SHELL_SMOKE_SUMMARY:passed=${passed} failed=0"
    echo "Host shell smoke test run complete."
}

homeboy_wordpress_is_shell_smoke_file() {
    case "$1" in
        tests/*-smoke.sh|tests/*/*-smoke.sh|tests/*/*/*-smoke.sh|tests/*/*/*/*-smoke.sh|wordpress/tests/*-smoke.sh|wordpress/tests/*/*-smoke.sh|wordpress/tests/*/*/*-smoke.sh|wordpress/tests/*/*/*/*-smoke.sh)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

if [ -z "$TARGET_FILE" ] && [ "${HOMEBOY_TEST_SCOPE_KIND:-}" = "exclusive_env" ]; then
    if [ "${HOMEBOY_TEST_SCOPE_ENV_NAME:-}" = "HOMEBOY_WORDPRESS_HOST_SMOKE_FILES" ] && [ -n "${HOMEBOY_TEST_SCOPE_ENV_VALUE:-}" ]; then
        HOMEBOY_WORDPRESS_HOST_SMOKE_FILES="$HOMEBOY_TEST_SCOPE_ENV_VALUE" exec bash "$SMOKE_RUNNER" "${PASSTHROUGH_ARGS[@]}"
    fi
fi

if [ -z "$TARGET_FILE" ] && [ -n "${HOMEBOY_CHANGED_TEST_FILES:-}" ]; then
    changed_js_smoke_files=""
    changed_shell_smoke_files=""
    changed_non_host_smoke_files=0
    while IFS= read -r changed_test_file; do
        [ -n "$changed_test_file" ] || continue
        if ! changed_test_rel="$(homeboy_wordpress_rel_test_file "$changed_test_file")"; then
            changed_non_host_smoke_files=1
            continue
        fi
        if homeboy_wordpress_is_js_smoke_file "$changed_test_rel"; then
            if [ -n "$changed_js_smoke_files" ]; then
                changed_js_smoke_files+=$'\n'
            fi
            changed_js_smoke_files+="$changed_test_rel"
        elif homeboy_wordpress_is_shell_smoke_file "$changed_test_rel"; then
            if [ -n "$changed_shell_smoke_files" ]; then
                changed_shell_smoke_files+=$'\n'
            fi
            changed_shell_smoke_files+="$changed_test_rel"
        else
            changed_non_host_smoke_files=1
        fi
    done <<< "$HOMEBOY_CHANGED_TEST_FILES"

    if [ -n "$changed_js_smoke_files" ] && [ -z "$changed_shell_smoke_files" ] && [ "$changed_non_host_smoke_files" -eq 0 ]; then
        homeboy_wordpress_run_js_smoke_files "$changed_js_smoke_files"
        exit 0
    fi

    if [ -z "$changed_js_smoke_files" ] && [ -n "$changed_shell_smoke_files" ] && [ "$changed_non_host_smoke_files" -eq 0 ]; then
        homeboy_wordpress_run_shell_smoke_files "$changed_shell_smoke_files"
        exit 0
    fi
fi

if [ -n "$TARGET_FILE" ]; then
    if ! target_rel="$(homeboy_wordpress_rel_test_file "$TARGET_FILE")"; then
        echo "ERROR: requested test file not found: ${TARGET_FILE}" >&2
        exit 2
    fi

    target_base="$(basename "$target_rel")"
    if homeboy_wordpress_is_js_smoke_file "$target_rel"; then
        homeboy_wordpress_run_js_smoke_files "$target_rel"
        exit 0
    fi

    if homeboy_wordpress_is_shell_smoke_file "$target_rel"; then
        homeboy_wordpress_run_shell_smoke_files "$target_rel"
        exit 0
    fi

    case "$target_rel" in
        tests/*.php|tests/*/*.php|tests/*/*/*.php|tests/*/*/*/*.php)
            case "$target_base" in
                *-smoke.php)
                    HOMEBOY_WORDPRESS_HOST_SMOKE_FILE="$target_rel" exec bash "$SMOKE_RUNNER" "${PASSTHROUGH_ARGS[@]}"
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

# Full-suite run (no --file, no changed-file scope): run every test the
# component carries, routed by file type. PHPUnit (tests/**/*Test.php,
# tests/**/test-*.php) runs through the WP Codebox PHPUnit backend; standalone
# tests/**/*-smoke.php scripts run through the real-WordPress smoke backend. Both
# run against real WordPress in WP Codebox. A component may carry one or both;
# each backend skips cleanly when it finds no matching files.
TEST_ROOT="${PLUGIN_PATH}/tests"
has_smoke_files=0
if [ -d "$TEST_ROOT" ] && [ -n "$(find "$TEST_ROOT" -type f -name '*-smoke.php' -print -quit 2>/dev/null)" ]; then
    has_smoke_files=1
fi

# Run the PHPUnit backend (it discovers *Test.php / test-*.php and falls back to
# a composer test script, and is the canonical default path). When the component
# also carries standalone smoke scripts, run the real-WordPress smoke backend
# after it. A non-zero exit from either backend fails the whole run, so neither
# can mask the other's failure.
if [ "$has_smoke_files" -eq 1 ]; then
    bash "$WP_CODEBOX_RUNNER" "${PASSTHROUGH_ARGS[@]}"
    phpunit_status=$?
    if [ "$phpunit_status" -ne 0 ]; then
        exit "$phpunit_status"
    fi
    exec bash "$SMOKE_RUNNER" "${PASSTHROUGH_ARGS[@]}"
fi

exec bash "$WP_CODEBOX_RUNNER" "${PASSTHROUGH_ARGS[@]}"
