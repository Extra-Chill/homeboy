#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOMEBOY_CORE_DIR="${HOMEBOY_CORE_DIR:-$(cd "${ROOT_DIR}/.." && pwd)/homeboy}"
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${HOMEBOY_CORE_DIR}/src/core/extension/runtime/resolve-context.sh}"
BUILD_SCRIPT="$ROOT_DIR/wordpress/scripts/build/build.sh"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

if [ ! -f "$RESOLVE_CONTEXT_HELPER" ]; then
    echo "Missing resolve context helper: $RESOLVE_CONTEXT_HELPER" >&2
    exit 1
fi

PROJECT_DIR="$TMP_DIR/staging-smoke-plugin"
OUTPUT_FILE="$TMP_DIR/build.out"
mkdir -p "$PROJECT_DIR/inc"

cat > "$PROJECT_DIR/staging-smoke-plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: Staging Smoke Plugin
 * Version: 1.2.3
 */
PHP

cat > "$PROJECT_DIR/inc/good.php" <<'PHP'
<?php
function staging_smoke_plugin_good() {
	return true;
}
PHP

cat > "$PROJECT_DIR/.buildignore" <<'EOF'
README.md
EOF

(
    cd "$PROJECT_DIR"
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" bash "$BUILD_SCRIPT" >"$OUTPUT_FILE"
)

ZIP_FILE="$PROJECT_DIR/build/staging-smoke-plugin.zip"
if [ ! -f "$ZIP_FILE" ]; then
    echo "Expected build artifact missing: $ZIP_FILE" >&2
    cat "$OUTPUT_FILE" >&2
    exit 1
fi

if unzip -Z1 "$ZIP_FILE" | grep -q '^staging-smoke-plugin/.homeboy-build/'; then
    echo "Build artifact unexpectedly contains Homeboy staging files" >&2
    unzip -Z1 "$ZIP_FILE" >&2
    exit 1
fi

if ! grep -q 'PHP syntax check passed' "$OUTPUT_FILE"; then
    echo "Build did not run PHP syntax validation" >&2
    cat "$OUTPUT_FILE" >&2
    exit 1
fi

BAD_PROJECT_DIR="$TMP_DIR/staging-bad-plugin"
BAD_OUTPUT_FILE="$TMP_DIR/bad-build.out"
mkdir -p "$BAD_PROJECT_DIR/inc"

cat > "$BAD_PROJECT_DIR/staging-bad-plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: Staging Bad Plugin
 * Version: 1.2.3
 */
PHP

cat > "$BAD_PROJECT_DIR/inc/bad.php" <<'PHP'
<?php
function staging_bad_plugin_broken( {
	return true;
}
PHP

cat > "$BAD_PROJECT_DIR/.buildignore" <<'EOF'
README.md
EOF

if (
    cd "$BAD_PROJECT_DIR"
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" bash "$BUILD_SCRIPT" >"$BAD_OUTPUT_FILE" 2>&1
); then
    echo "Build unexpectedly passed with invalid staged PHP" >&2
    cat "$BAD_OUTPUT_FILE" >&2
    exit 1
fi

if ! grep -q 'PHP syntax errors found' "$BAD_OUTPUT_FILE"; then
    echo "Build did not report PHP syntax validation failure" >&2
    cat "$BAD_OUTPUT_FILE" >&2
    exit 1
fi

echo "wordpress build staging syntax smoke passed"
