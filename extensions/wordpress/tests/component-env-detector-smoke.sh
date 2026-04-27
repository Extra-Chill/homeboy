#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
DETECTOR="${EXTENSION_DIR}/scripts/env/detect.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

assert_json_php() {
    local expected="$1"
    local actual="$2"
    python3 - "$expected" "$actual" <<'PY'
import json
import sys

expected = sys.argv[1]
data = json.loads(sys.argv[2])
if expected:
    assert data == {"php": expected}, data
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

echo "component env detector smoke passed"
