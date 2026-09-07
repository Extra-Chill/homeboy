#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
DETECTOR="${EXTENSION_DIR}/scripts/env/detect.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

# The detector must emit the `runtimes` shape Homeboy core's
# `component env` parser expects. The flat `{"php": "..."}` shape is rejected
# with "unknown field `php`, expected `runtimes`", which homeboy-action
# swallows, so CI silently skips PHP setup and runs on the runner default.
assert_json_php() {
    local expected="$1"
    local actual="$2"
    python3 - "$expected" "$actual" <<'PY'
import json
import sys

expected = sys.argv[1]
data = json.loads(sys.argv[2])
if expected:
    assert data == {"runtimes": {"php": {"version": expected}}}, data
else:
    assert data == {}, data
PY
}

mkdir -p "${TMPDIR}/plugin"
cat > "${TMPDIR}/plugin/demo.php" <<'PHP'
<?php
/**
 * Plugin Name: Demo Plugin
 * Requires PHP: 8.1
 */
PHP

actual="$(cd "${TMPDIR}/plugin" && "$DETECTOR")"
assert_json_php "8.1" "$actual"

mkdir -p "${TMPDIR}/theme"
cat > "${TMPDIR}/theme/style.css" <<'CSS'
/*
Theme Name: Demo Theme
Requires PHP: 8.2
*/
CSS

actual="$(cd "${TMPDIR}/theme" && "$DETECTOR")"
assert_json_php "8.2" "$actual"

mkdir -p "${TMPDIR}/no-requires"
cat > "${TMPDIR}/no-requires/demo.php" <<'PHP'
<?php
/**
 * Plugin Name: Demo Plugin
 */
PHP

actual="$(cd "${TMPDIR}/no-requires" && "$DETECTOR")"
assert_json_php "" "$actual"

# homeboy.json wordpress_runtime_php_version is the version the WP Codebox
# runtime boots, so it wins over the Requires PHP header.
mkdir -p "${TMPDIR}/runtime-setting"
cat > "${TMPDIR}/runtime-setting/demo.php" <<'PHP'
<?php
/**
 * Plugin Name: Demo Plugin
 * Requires PHP: 8.0
 */
PHP
cat > "${TMPDIR}/runtime-setting/homeboy.json" <<'JSON'
{
  "extensions": {
    "wordpress": {
      "settings": {
        "wordpress_runtime_php_version": "8.4"
      }
    }
  }
}
JSON

actual="$(cd "${TMPDIR}/runtime-setting" && "$DETECTOR")"
assert_json_php "8.4" "$actual"

# A malformed runtime setting falls back to the header instead of emitting
# garbage that setup-php would reject.
mkdir -p "${TMPDIR}/bad-runtime-setting"
cat > "${TMPDIR}/bad-runtime-setting/demo.php" <<'PHP'
<?php
/**
 * Plugin Name: Demo Plugin
 * Requires PHP: 8.1
 */
PHP
cat > "${TMPDIR}/bad-runtime-setting/homeboy.json" <<'JSON'
{
  "extensions": {
    "wordpress": {
      "settings": {
        "wordpress_runtime_php_version": "latest"
      }
    }
  }
}
JSON

actual="$(cd "${TMPDIR}/bad-runtime-setting" && "$DETECTOR")"
assert_json_php "8.1" "$actual"

echo "component env detector smoke passed"
