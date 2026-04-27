#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOT_DIR/wordpress/wordpress.json"

assert_contains() {
    local file="$1"
    local expected="$2"
    if ! grep -Fq -- "$expected" "$file"; then
        echo "Expected $file to contain: $expected" >&2
        exit 1
    fi
}

php -r 'json_decode(file_get_contents($argv[1]), true, 512, JSON_THROW_ON_ERROR);' "$MANIFEST"

assert_contains "$MANIFEST" '"remote_path_inference"'
assert_contains "$MANIFEST" '"file": "{{dir_name}}.php"'
assert_contains "$MANIFEST" '"file": "{{id}}.php"'
assert_contains "$MANIFEST" '"text": "Plugin Name:"'
assert_contains "$MANIFEST" '"remote_path": "wp-content/plugins/{{dir_name}}"'
assert_contains "$MANIFEST" '"file": "style.css"'
assert_contains "$MANIFEST" '"text": "Theme Name:"'
assert_contains "$MANIFEST" '"remote_path": "wp-content/themes/{{dir_name}}"'

echo "wordpress remote path inference smoke passed"
