#!/usr/bin/env bash
set -euo pipefail

# Host PHP smoke runner for WordPress components that carry standalone smoke
# scripts instead of PHPUnit suites. This backend intentionally does not boot
# WordPress, MySQL, or Playground.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${SCRIPT_DIR}/../lib/resolve-context.sh}"

# shellcheck source=/dev/null
source "$RESOLVE_CONTEXT_HELPER"
homeboy_resolve_context --component-alias PLUGIN_PATH

PHP_BIN="${HOMEBOY_PHP_BIN:-php}"
TEST_DIR="${PLUGIN_PATH}/tests"
TARGET_SMOKE_FILE="${HOMEBOY_WORDPRESS_HOST_SMOKE_FILE:-}"

echo "Running host PHP smoke tests..."
echo "  Component: ${COMPONENT_ID:-$(basename "$PLUGIN_PATH")} (${PLUGIN_PATH})"
echo "  Backend: host-smoke"

if [ ! -d "$TEST_DIR" ]; then
    echo ""
    echo "Skipping host smoke tests: no tests directory found at ${TEST_DIR}"
    exit 0
fi

if [ -n "$TARGET_SMOKE_FILE" ]; then
    if [ "${TARGET_SMOKE_FILE#/}" != "$TARGET_SMOKE_FILE" ]; then
        target_abs="$TARGET_SMOKE_FILE"
    else
        target_abs="${PLUGIN_PATH}/${TARGET_SMOKE_FILE}"
    fi
    if [ ! -f "$target_abs" ]; then
        echo "ERROR: requested host smoke file not found: ${TARGET_SMOKE_FILE}" >&2
        exit 2
    fi
    case "$target_abs" in
        "${PLUGIN_PATH}"/tests/*-smoke.php)
            smoke_files=("$target_abs")
            ;;
        *)
            echo "ERROR: requested host smoke file must match tests/**/*-smoke.php: ${TARGET_SMOKE_FILE}" >&2
            exit 2
            ;;
    esac
else
    mapfile -t smoke_files < <(find "$TEST_DIR" -type f -name '*-smoke.php' | sort)
fi

if [ "${#smoke_files[@]}" -eq 0 ]; then
    echo ""
    echo "Skipping host smoke tests: no files matched ${TEST_DIR}/**/*-smoke.php"
    exit 0
fi

echo "  Files: ${#smoke_files[@]}"
echo ""

passed=0
for smoke_file in "${smoke_files[@]}"; do
    rel_path="${smoke_file#"${PLUGIN_PATH}/"}"
    echo "HOST_SMOKE_BEGIN:${rel_path}"
    if "$PHP_BIN" "$smoke_file"; then
        echo "HOST_SMOKE_OK:${rel_path}"
        passed=$((passed + 1))
    else
        exit_code=$?
        echo "HOST_SMOKE_FAIL:${rel_path}:exit=${exit_code}"
        echo ""
        echo "Host smoke test failed: ${rel_path}"
        exit "$exit_code"
    fi
done

echo ""
echo "HOST_SMOKE_SUMMARY:passed=${passed} failed=0"
echo "Host smoke test run complete."
