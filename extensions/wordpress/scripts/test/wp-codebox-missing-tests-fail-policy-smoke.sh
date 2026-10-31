#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER="${SCRIPT_DIR}/test-runner-wp-codebox.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

PLUGIN_DIR="${TMPDIR}/sample-plugin"
FAKE_BIN="${TMPDIR}/wp-codebox"
RESOLVE_CONTEXT_HELPER="${TMPDIR}/resolve-context-helper.sh"
OUTPUT="${TMPDIR}/output.txt"

mkdir -p "$PLUGIN_DIR"
cat > "${PLUGIN_DIR}/sample-plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: Sample Plugin
 */
PHP

cat > "$FAKE_BIN" <<'SH'
#!/usr/bin/env bash
echo "fake wp-codebox should not run when the test directory is missing" >&2
exit 99
SH
chmod +x "$FAKE_BIN"

cat > "$RESOLVE_CONTEXT_HELPER" <<'SH'
#!/usr/bin/env bash
homeboy_resolve_context() {
    PLUGIN_PATH="$HOMEBOY_COMPONENT_PATH"
    COMPONENT_ID="$HOMEBOY_COMPONENT_ID"
    EXTENSION_PATH="$HOMEBOY_EXTENSION_PATH"
}
SH
chmod +x "$RESOLVE_CONTEXT_HELPER"

set +e
HOMEBOY_COMPONENT_ID="sample-plugin" \
HOMEBOY_COMPONENT_PATH="$PLUGIN_DIR" \
HOMEBOY_PROJECT_PATH="$PLUGIN_DIR" \
HOMEBOY_EXTENSION_PATH="${SCRIPT_DIR}/.." \
HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" \
HOMEBOY_RUNTIME_RUNNER_STEPS="${TMPDIR}/missing-runner-steps.sh" \
HOMEBOY_WP_CODEBOX_BIN="$FAKE_BIN" \
HOMEBOY_SETTINGS_JSON='{"phpunit_no_tests":"fail"}' \
bash "$RUNNER" >"$OUTPUT" 2>&1
status=$?
set -e

if [ "$status" -eq 0 ]; then
    echo "Expected missing tests directory to fail when phpunit_no_tests=fail" >&2
    cat "$OUTPUT" >&2
    exit 1
fi

if ! grep -q "NO PHPUNIT TEST DIRECTORY DISCOVERED" "$OUTPUT"; then
    echo "Expected structured missing test directory failure output" >&2
    cat "$OUTPUT" >&2
    exit 1
fi

echo "WP Codebox missing tests fail policy smoke passed"
