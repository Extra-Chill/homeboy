#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUNNER="$ROOT_DIR/wordpress/scripts/lint/eslint-runner.sh"

assert_contains() {
    local file="$1"
    local expected="$2"
    if ! grep -Fq -- "$expected" "$file"; then
        echo "Expected $file to contain: $expected" >&2
        echo "Actual contents:" >&2
        cat "$file" >&2
        exit 1
    fi
}

assert_not_contains() {
    local file="$1"
    local unexpected="$2"
    if grep -Fq -- "$unexpected" "$file"; then
        echo "Expected $file to not contain: $unexpected" >&2
        echo "Actual contents:" >&2
        cat "$file" >&2
        exit 1
    fi
}

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

COMPONENT_DIR="$TMP_DIR/example-plugin"
FAKE_EXTENSION="$TMP_DIR/fake-wordpress-extension"
ESLINT_ARGS_FILE="$TMP_DIR/eslint-args.txt"

mkdir -p "$COMPONENT_DIR/inc" "$COMPONENT_DIR/assets" "$COMPONENT_DIR/docs" "$FAKE_EXTENSION/node_modules/.bin"

cat > "$COMPONENT_DIR/example-plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: Example Plugin
 * Text Domain: example-plugin
 */
PHP

cat > "$COMPONENT_DIR/inc/runtime.php" <<'PHP'
<?php
function example_plugin_runtime() {
    return true;
}
PHP

cat > "$COMPONENT_DIR/assets/admin.js" <<'JS'
const examplePlugin = true;
JS

cat > "$COMPONENT_DIR/assets/view.ts" <<'TS'
const examplePluginView: boolean = true;
TS

cat > "$COMPONENT_DIR/docs/example.md" <<'MD'
# Example docs
MD

cat > "$FAKE_EXTENSION/.eslintrc.json" <<'JSON'
{}
JSON

cat > "$FAKE_EXTENSION/node_modules/.bin/eslint" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$@" > "$ESLINT_ARGS_FILE"
for arg in "$@"; do
    case "$arg" in
        */assets/bad.js|assets/bad.js)
            echo "simulated eslint failure" >&2
            exit 1
            ;;
    esac
done
SH
chmod +x "$FAKE_EXTENSION/node_modules/.bin/eslint"

HOMEBOY_EXTENSION_PATH="$FAKE_EXTENSION" \
HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
HOMEBOY_COMPONENT_ID="example-plugin" \
ESLINT_ARGS_FILE="$ESLINT_ARGS_FILE" \
HOMEBOY_LINT_FILE='docs/example.md' \
    bash "$RUNNER" > "$TMP_DIR/single-md.out" 2>&1

if [ -f "$ESLINT_ARGS_FILE" ]; then
    echo "ESLint should not run for single-file Markdown scope" >&2
    cat "$ESLINT_ARGS_FILE" >&2
    exit 1
fi

HOMEBOY_EXTENSION_PATH="$FAKE_EXTENSION" \
HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
HOMEBOY_COMPONENT_ID="example-plugin" \
ESLINT_ARGS_FILE="$ESLINT_ARGS_FILE" \
HOMEBOY_LINT_GLOB='{example-plugin.php,inc/runtime.php}' \
    bash "$RUNNER" > "$TMP_DIR/php-only.out" 2>&1

assert_contains "$TMP_DIR/php-only.out" "No JS/TS files match pattern: {example-plugin.php,inc/runtime.php}"
if [ -f "$ESLINT_ARGS_FILE" ]; then
    echo "ESLint should not run for PHP-only scoped glob" >&2
    cat "$ESLINT_ARGS_FILE" >&2
    exit 1
fi

HOMEBOY_EXTENSION_PATH="$FAKE_EXTENSION" \
HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
HOMEBOY_COMPONENT_ID="example-plugin" \
ESLINT_ARGS_FILE="$ESLINT_ARGS_FILE" \
HOMEBOY_LINT_GLOB='{example-plugin.php,assets/admin.js,inc/runtime.php,assets/view.ts}' \
    bash "$RUNNER" > "$TMP_DIR/mixed.out" 2>&1

assert_contains "$TMP_DIR/mixed.out" "Linting 2 JS/TS files matching: {example-plugin.php,assets/admin.js,inc/runtime.php,assets/view.ts}"
assert_contains "$ESLINT_ARGS_FILE" "assets/admin.js"
assert_contains "$ESLINT_ARGS_FILE" "assets/view.ts"
assert_not_contains "$ESLINT_ARGS_FILE" "example-plugin.php"
assert_not_contains "$ESLINT_ARGS_FILE" "inc/runtime.php"

rm -f "$ESLINT_ARGS_FILE"

HOMEBOY_EXTENSION_PATH="$FAKE_EXTENSION" \
HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
HOMEBOY_COMPONENT_ID="example-plugin" \
ESLINT_ARGS_FILE="$ESLINT_ARGS_FILE" \
HOMEBOY_LINT_FILE='assets/admin.js' \
    bash "$RUNNER" > "$TMP_DIR/js-success.out" 2>&1

assert_contains "$TMP_DIR/js-success.out" "Linting single file: assets/admin.js"
assert_contains "$TMP_DIR/js-success.out" "ESLint linting passed"
assert_contains "$ESLINT_ARGS_FILE" "assets/admin.js"

cat > "$COMPONENT_DIR/assets/bad.js" <<'JS'
const bad = true;
JS

set +e
HOMEBOY_EXTENSION_PATH="$FAKE_EXTENSION" \
HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
HOMEBOY_COMPONENT_ID="example-plugin" \
ESLINT_ARGS_FILE="$ESLINT_ARGS_FILE" \
HOMEBOY_LINT_FILE='assets/bad.js' \
    bash "$RUNNER" > "$TMP_DIR/js-failure.out" 2>&1
failure_status=$?
set -e

if [ "$failure_status" -eq 0 ]; then
    echo "Expected ESLint failure to propagate for JS file scope" >&2
    cat "$TMP_DIR/js-failure.out" >&2
    exit 1
fi

assert_contains "$TMP_DIR/js-failure.out" "Linting single file: assets/bad.js"
assert_contains "$TMP_DIR/js-failure.out" "ESLint linting failed"
assert_contains "$ESLINT_ARGS_FILE" "assets/bad.js"

echo "wordpress eslint scope smoke passed"
