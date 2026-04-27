#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUNNER="$ROOT_DIR/wordpress/scripts/lint/lint-runner.sh"
PHPSTAN_RUNNER="$ROOT_DIR/wordpress/scripts/lint/phpstan-runner.sh"
PHPSTAN_CONFIG="$ROOT_DIR/wordpress/phpstan.neon.dist"

assert_contains() {
    local file="$1"
    local expected="$2"
    if ! grep -Fq -- "$expected" "$file"; then
        echo "Expected $file to contain: $expected" >&2
        exit 1
    fi
}

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

COMPONENT_DIR="$TMP_DIR/example-plugin"
FAKE_EXTENSION="$TMP_DIR/fake-wordpress-extension"
mkdir -p "$COMPONENT_DIR" "$FAKE_EXTENSION/vendor/bin" "$FAKE_EXTENSION/HomeboyWordPress"

cat > "$COMPONENT_DIR/example-plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: Example Plugin
 * Text Domain: example-plugin
 */
PHP

touch "$FAKE_EXTENSION/vendor/bin/phpcs" "$FAKE_EXTENSION/vendor/bin/phpcbf" "$FAKE_EXTENSION/phpcs.xml.dist"

HOMEBOY_EXTENSION_PATH="$FAKE_EXTENSION" \
HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
HOMEBOY_COMPONENT_ID="example-plugin" \
HOMEBOY_STEP="none" \
    bash "$RUNNER" > "$TMP_DIR/lint.out" 2>&1

assert_contains "$TMP_DIR/lint.out" "Linting passed"

assert_contains "$RUNNER" 'homeboy_resolve_context --component-alias PLUGIN_PATH'
assert_contains "$RUNNER" "*/vendor_prefixed/*"
assert_contains "$PHPSTAN_RUNNER" "*/vendor_prefixed/*"
assert_contains "$PHPSTAN_CONFIG" "*/vendor_prefixed/*"

echo "wordpress lint context smoke passed"
