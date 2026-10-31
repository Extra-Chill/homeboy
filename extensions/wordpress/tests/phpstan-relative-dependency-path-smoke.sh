#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PHPSTAN_RUNNER="${ROOT_DIR}/scripts/lint/phpstan-runner.sh"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

COMPONENT_DIR="${TMP_ROOT}/component"
CONFIG_TMPDIR="${TMP_ROOT}/generated-config"
OUTPUT_FILE="${TMP_ROOT}/phpstan-output.txt"
RESOLVE_CONTEXT_HELPER="${TMP_ROOT}/resolve-context.sh"
SIDECAR_WRITER_HELPER="${TMP_ROOT}/sidecar-writer.sh"

mkdir -p "${COMPONENT_DIR}/bbpress" "$CONFIG_TMPDIR"

cat > "$RESOLVE_CONTEXT_HELPER" <<'SH'
homeboy_resolve_context() {
    EXTENSION_PATH="$HOMEBOY_EXTENSION_PATH"
    COMPONENT_PATH="$HOMEBOY_COMPONENT_PATH"
    PLUGIN_PATH="$HOMEBOY_COMPONENT_PATH"
    COMPONENT_ID="${HOMEBOY_COMPONENT_ID:-phpstan-relative-dependency-path}"
}
SH

cat > "$SIDECAR_WRITER_HELPER" <<'SH'
homeboy_sidecar_merge_json_array() {
    return 0
}
SH

cat > "${COMPONENT_DIR}/plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: PHPStan Relative Dependency Fixture
 */
PHP

cat > "${COMPONENT_DIR}/bbpress/bbpress.php" <<'PHP'
<?php

class PHPStan_Relative_Dependency_Fixture {}
PHP

if [ ! -x "${ROOT_DIR}/vendor/bin/phpstan" ]; then
    echo "FAIL: PHPStan is not installed. Run composer install in ${ROOT_DIR}." >&2
    exit 1
fi

set +e
HOMEBOY_EXTENSION_PATH="$ROOT_DIR" \
HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
HOMEBOY_COMPONENT_ID="phpstan-relative-dependency-path" \
HOMEBOY_SETTINGS_JSON='{"validation_dependencies":"bbpress"}' \
HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" \
HOMEBOY_RUNTIME_SIDECAR_WRITER="$SIDECAR_WRITER_HELPER" \
HOMEBOY_SUMMARY_MODE=1 \
HOMEBOY_PHPSTAN_THREADS=1 \
COLUMNS=240 \
TMPDIR="$CONFIG_TMPDIR" \
"$PHPSTAN_RUNNER" >"$OUTPUT_FILE" 2>&1
exit_code=$?
set -e

if [ "$exit_code" -ne 0 ]; then
    echo "FAIL: component-relative dependency path should be resolved from the workspace" >&2
    cat "$OUTPUT_FILE" >&2
    exit 1
fi

if grep -F "${CONFIG_TMPDIR}/bbpress" "$OUTPUT_FILE" >/dev/null; then
    echo "FAIL: generated PHPStan config resolved bbpress from its temp directory" >&2
    cat "$OUTPUT_FILE" >&2
    exit 1
fi

set +e
HOMEBOY_EXTENSION_PATH="$ROOT_DIR" \
HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
HOMEBOY_COMPONENT_ID="phpstan-relative-dependency-path" \
HOMEBOY_SETTINGS_JSON='{"validation_dependencies":"./missing-bbpress"}' \
HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" \
HOMEBOY_RUNTIME_SIDECAR_WRITER="$SIDECAR_WRITER_HELPER" \
HOMEBOY_SUMMARY_MODE=1 \
HOMEBOY_PHPSTAN_THREADS=1 \
COLUMNS=240 \
TMPDIR="$CONFIG_TMPDIR" \
"$PHPSTAN_RUNNER" >"$OUTPUT_FILE" 2>&1
exit_code=$?
set -e

if [ "$exit_code" -eq 0 ]; then
    echo "FAIL: explicitly missing component-relative dependency should fail PHPStan" >&2
    cat "$OUTPUT_FILE" >&2
    exit 1
fi

if ! grep -F "${COMPONENT_DIR}/missing-bbpress" "$OUTPUT_FILE" >/dev/null; then
    echo "FAIL: missing dependency error should name the component-rooted path" >&2
    cat "$OUTPUT_FILE" >&2
    exit 1
fi

if grep -F "${CONFIG_TMPDIR}/missing-bbpress" "$OUTPUT_FILE" >/dev/null; then
    echo "FAIL: missing dependency error should not name the generated config directory" >&2
    cat "$OUTPUT_FILE" >&2
    exit 1
fi

echo "PHPStan relative dependency path smoke passed"
