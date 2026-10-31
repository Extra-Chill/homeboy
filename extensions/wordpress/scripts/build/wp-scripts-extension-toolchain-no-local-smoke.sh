#!/usr/bin/env bash
set -euo pipefail

# Integration proof for homeboy-extensions#2870: a component with no local
# @wordpress/scripts dependency builds, lints, and runs its declared JS unit
# tests through the extension's own pinned copy. Uses the extension's REAL
# installed toolchain (not a stub) so this is a genuine end-to-end proof, not
# just a contract check against a fake binary -- `npm install` in
# wordpress/toolchains/wp-scripts/ must have run first (CI and `homeboy test`
# both do this before invoking package.json scripts).

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
ROOT_DIR="$(cd "${EXTENSION_DIR}/.." && pwd)"
# shellcheck source=../../../scripts/lib/runtime-helper-resolver.sh
source "${ROOT_DIR}/scripts/lib/runtime-helper-resolver.sh"
RESOLVE_CONTEXT_CORE_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_RESOLVE_CONTEXT resolve-context.sh)"

EXTENSION_WP_SCRIPTS="${EXTENSION_DIR}/toolchains/wp-scripts/node_modules/.bin/wp-scripts"
if [ ! -x "$EXTENSION_WP_SCRIPTS" ]; then
    echo "SKIP: extension's own @wordpress/scripts is not installed at ${EXTENSION_WP_SCRIPTS}; run 'npm install' in ${EXTENSION_DIR}/toolchains/wp-scripts first" >&2
    exit 0
fi

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wp-scripts-no-local.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

fail() {
    echo "FAIL: $1" >&2
    exit 1
}

component_dir="${TMP_DIR}/no-local-wp-scripts-plugin"
mkdir -p "${component_dir}/src" "${component_dir}/tests"

cat > "${component_dir}/no-local-wp-scripts-plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: No Local WP Scripts Plugin
 * Version: 1.0.0
 */
PHP

# No @wordpress/scripts anywhere in this file -- the whole point of the
# fixture. build/lint/test all invoke the bare `wp-scripts` CLI.
cat > "${component_dir}/package.json" <<'JSON'
{
  "name": "no-local-wp-scripts-plugin",
  "private": true,
  "scripts": {
    "build": "wp-scripts build",
    "lint:js": "wp-scripts lint-js",
    "test:js": "wp-scripts test-unit-js"
  },
  "dependencies": {
    "@wordpress/element": "^6.0.0"
  }
}
JSON

cat > "${component_dir}/src/index.js" <<'JS'
/**
 * WordPress dependencies
 */
import { createElement } from '@wordpress/element';

export default createElement( 'div', {}, 'hello from the extension toolchain' );
JS

cat > "${component_dir}/src/test.test.js" <<'JS'
/**
 * Runs through wp-scripts test-unit-js (Jest), supplied by the extension.
 */
describe( 'extension-supplied toolchain', () => {
	it( 'runs through wp-scripts test-unit-js without a local @wordpress/scripts', () => {
		expect( 1 + 1 ).toBe( 2 );
	} );
} );
JS

echo "--- Step 1: build.sh builds through the extension's wp-scripts ---"
(
    cd "$component_dir"
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_ID="no-local-wp-scripts-plugin" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
    HOMEBOY_SKIP_TESTS=1 \
        bash "${EXTENSION_DIR}/scripts/build/build.sh"
)

[ -f "${component_dir}/build/index.js" ] || fail "build/index.js was not produced"
[ -f "${component_dir}/build/index.asset.php" ] || fail "build/index.asset.php was not produced"
[ ! -x "${component_dir}/node_modules/.bin/wp-scripts" ] \
    || fail "a local wp-scripts binary was installed even though @wordpress/scripts is not a dependency"

zip_file="${component_dir}/build/no-local-wp-scripts-plugin.zip"
[ -f "$zip_file" ] || fail "production zip was not created"
unzip -l "$zip_file" | grep -q "no-local-wp-scripts-plugin/build/index.js" \
    || fail "production zip does not contain the built index.js"

echo "OK: build produced compiled JS through the extension's wp-scripts, with no local copy installed"

echo "--- Step 2: npm run lint:js resolves wp-scripts lint-js through the extension ---"
(
    cd "$component_dir"
    # shellcheck source=../lib/wp-scripts-toolchain.sh
    source "${EXTENSION_DIR}/scripts/lib/wp-scripts-toolchain.sh"
    homeboy_wordpress_apply_extension_wp_scripts_toolchain "$(pwd)" "$EXTENSION_DIR"
    npm run lint:js
)
echo "OK: lint:js (wp-scripts lint-js) ran clean through the extension"

echo "--- Step 3: npm run test:js resolves wp-scripts test-unit-js through the extension ---"
(
    cd "$component_dir"
    # shellcheck source=../lib/wp-scripts-toolchain.sh
    source "${EXTENSION_DIR}/scripts/lib/wp-scripts-toolchain.sh"
    homeboy_wordpress_apply_extension_wp_scripts_toolchain "$(pwd)" "$EXTENSION_DIR"
    CI=true npm run test:js 2>&1 | tee "${TMP_DIR}/jest-output.log"
)
grep -q "1 passed" "${TMP_DIR}/jest-output.log" \
    || fail "wp-scripts test-unit-js did not report the fixture test as passing"

echo "OK: test:js (wp-scripts test-unit-js / Jest) ran through the extension"

echo "PASS: no-local @wordpress/scripts fixture builds, lints, and tests through the wordpress extension"
