#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
RUNNER="${SCRIPT_DIR}/lint-runner.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

COMPONENT_DIR="${TMPDIR}/component"
LINT_OUTPUT="${TMPDIR}/lint-output.txt"
FIX_OUTPUT="${TMPDIR}/fix-output.txt"

mkdir -p "${COMPONENT_DIR}/vendor_prefixed"

cat > "${COMPONENT_DIR}/plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: Vendor Prefixed Exclusion Fixture
 * Text Domain: vendor-prefixed-exclusion-fixture
 */
PHP

cat > "${COMPONENT_DIR}/vendor_prefixed/bad.php" <<'PHP'
<?php
if ( $value == 1 ) {
    echo "vendor";
}
PHP

before_hash=$(shasum "${COMPONENT_DIR}/vendor_prefixed/bad.php" | cut -d ' ' -f 1)

HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
HOMEBOY_COMPONENT_ID="vendor-prefixed-fixture" \
HOMEBOY_STEP=phpcs \
HOMEBOY_SUMMARY_MODE=1 \
    bash "$RUNNER" > "$LINT_OUTPUT" 2>&1

if ! grep -Fq "PHPCS linting passed" "$LINT_OUTPUT"; then
    echo "FAIL: PHPCS should ignore vendor_prefixed/ during lint" >&2
    sed 's/^/  /' "$LINT_OUTPUT" >&2
    exit 1
fi

HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
HOMEBOY_COMPONENT_ID="vendor-prefixed-fixture" \
HOMEBOY_FIX_ONLY=1 \
HOMEBOY_STEP=phpcs \
    bash "$RUNNER" > "$FIX_OUTPUT" 2>&1

after_hash=$(shasum "${COMPONENT_DIR}/vendor_prefixed/bad.php" | cut -d ' ' -f 1)

if [ "$before_hash" != "$after_hash" ]; then
    echo "FAIL: auto-fix modified vendor_prefixed/" >&2
    sed 's/^/  /' "$FIX_OUTPUT" >&2
    exit 1
fi

echo "vendor_prefixed lint exclusion smoke passed"
