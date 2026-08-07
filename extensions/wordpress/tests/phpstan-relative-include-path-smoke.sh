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

mkdir -p "$COMPONENT_DIR" "$CONFIG_TMPDIR"

cat > "$RESOLVE_CONTEXT_HELPER" <<'SH'
homeboy_resolve_context() {
    EXTENSION_PATH="$HOMEBOY_EXTENSION_PATH"
    COMPONENT_PATH="$HOMEBOY_COMPONENT_PATH"
    PLUGIN_PATH="$HOMEBOY_COMPONENT_PATH"
    COMPONENT_ID="${HOMEBOY_COMPONENT_ID:-phpstan-relative-include-path}"
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
 * Plugin Name: PHPStan Relative Include Fixture
 */
PHP

cat > "${COMPONENT_DIR}/phpstan.neon" <<'NEON'
includes:
    - ./phpstan-baseline.neon
NEON

cat > "${COMPONENT_DIR}/phpstan-baseline.neon" <<'NEON'
parameters:
    ignoreErrors: []
NEON

if [ ! -x "${ROOT_DIR}/vendor/bin/phpstan" ]; then
    echo "FAIL: PHPStan is not installed. Run composer install in ${ROOT_DIR}." >&2
    exit 1
fi

set +e
HOMEBOY_EXTENSION_PATH="$ROOT_DIR" \
HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
HOMEBOY_COMPONENT_ID="phpstan-relative-include-path" \
HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" \
HOMEBOY_RUNTIME_SIDECAR_WRITER="$SIDECAR_WRITER_HELPER" \
HOMEBOY_SUMMARY_MODE=1 \
HOMEBOY_PHPSTAN_THREADS=1 \
TMPDIR="$CONFIG_TMPDIR" \
bash -c 'cd "$1" && "$2"' _ "$CONFIG_TMPDIR" "$PHPSTAN_RUNNER" >"$OUTPUT_FILE" 2>&1
exit_code=$?
set -e

if [ "$exit_code" -ne 0 ]; then
    echo "FAIL: component-relative PHPStan includes should resolve from the component" >&2
    cat "$OUTPUT_FILE" >&2
    exit 1
fi

if grep -F "${CONFIG_TMPDIR}/phpstan-baseline.neon" "$OUTPUT_FILE" >/dev/null; then
    echo "FAIL: PHPStan resolved the baseline from the generated config directory" >&2
    cat "$OUTPUT_FILE" >&2
    exit 1
fi

if grep -F "included multiple times" "$OUTPUT_FILE" >/dev/null; then
    echo "FAIL: component baseline should be included exactly once" >&2
    cat "$OUTPUT_FILE" >&2
    exit 1
fi

echo "PHPStan relative include path smoke passed"
