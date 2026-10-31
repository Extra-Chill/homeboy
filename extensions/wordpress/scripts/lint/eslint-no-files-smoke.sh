#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
RUNNER="${SCRIPT_DIR}/eslint-runner.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

COMPONENT_DIR="${TMPDIR}/component"
LINT_OUTPUT="${TMPDIR}/lint-output.txt"

mkdir -p "$COMPONENT_DIR"

cat > "${COMPONENT_DIR}/plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: ESLint No Files Fixture
 * Text Domain: eslint-no-files-fixture
 */
PHP

HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
HOMEBOY_COMPONENT_ID="eslint-no-files-fixture" \
    bash "$RUNNER" > "$LINT_OUTPUT" 2>&1

if ! grep -Fq "No JavaScript files found, skipping ESLint." "$LINT_OUTPUT"; then
    echo "FAIL: ESLint runner should skip cleanly when no JavaScript files exist" >&2
    sed 's/^/  /' "$LINT_OUTPUT" >&2
    exit 1
fi

echo "eslint no-files smoke passed"
