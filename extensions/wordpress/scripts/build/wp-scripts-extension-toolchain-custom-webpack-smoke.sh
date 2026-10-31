#!/usr/bin/env bash
set -euo pipefail

# Integration proof for homeboy-extensions#2870's core design problem: a
# component with a custom webpack.config.js that extends the default config
# builds through the extension. Covers both the documented consumer contract
# (HOMEBOY_WP_SCRIPTS_WEBPACK_CONFIG, the env var a consumer's config reads
# explicitly) and the NODE_PATH defense-in-depth fallback (an unmodified
# `require( '@wordpress/scripts/config/webpack.config' )` that has not
# adopted the one-line change yet). Uses the extension's REAL installed
# toolchain, not a stub.

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

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wp-scripts-custom-webpack.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

fail() {
    echo "FAIL: $1" >&2
    exit 1
}

build_fixture() {
    local component_dir="$1"
    local webpack_config_body="$2"

    mkdir -p "${component_dir}/src"

    cat > "${component_dir}/fixture-plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: Custom Webpack Fixture Plugin
 * Version: 1.0.0
 */
PHP

    cat > "${component_dir}/package.json" <<'JSON'
{
  "name": "custom-webpack-fixture-plugin",
  "private": true,
  "scripts": {
    "build": "wp-scripts build"
  },
  "dependencies": {
    "@wordpress/element": "^6.0.0"
  }
}
JSON

    cat > "${component_dir}/src/main.js" <<'JS'
import { createElement } from '@wordpress/element';
export default createElement( 'div', {}, 'main entry' );
JS

    cat > "${component_dir}/src/secondary.js" <<'JS'
import { createElement } from '@wordpress/element';
export default createElement( 'span', {}, 'secondary entry' );
JS

    printf '%s' "$webpack_config_body" > "${component_dir}/webpack.config.js"

    (
        cd "$component_dir"
        HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
        HOMEBOY_COMPONENT_ID="custom-webpack-fixture-plugin" \
        HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
        HOMEBOY_SKIP_TESTS=1 \
            bash "${EXTENSION_DIR}/scripts/build/build.sh"
    )

    # Both entries declared by the custom config must land in build/ -- proof
    # the CUSTOM config ran (the default @wordpress/scripts config only knows
    # about src/index.js, which does not exist in this fixture), not that
    # webpack silently fell back to some default.
    [ -f "${component_dir}/build/main.js" ] || fail "custom webpack config's 'main' entry was not built (${component_dir})"
    [ -f "${component_dir}/build/secondary.js" ] || fail "custom webpack config's 'secondary' entry was not built (${component_dir})"
}

echo "--- Case 1: documented contract (HOMEBOY_WP_SCRIPTS_WEBPACK_CONFIG fallback) ---"
documented_dir="${TMP_DIR}/documented-contract"
build_fixture "$documented_dir" '
const defaultConfig = require( process.env.HOMEBOY_WP_SCRIPTS_WEBPACK_CONFIG || "@wordpress/scripts/config/webpack.config" );
const path = require( "path" );

module.exports = {
	...defaultConfig,
	entry: {
		main: "./src/main.js",
		secondary: "./src/secondary.js",
	},
	output: {
		...defaultConfig.output,
		path: path.resolve( __dirname, "build" ),
	},
};
'
echo "OK: custom webpack.config.js using the documented HOMEBOY_WP_SCRIPTS_WEBPACK_CONFIG contract builds through the extension"

echo "--- Case 2: NODE_PATH defense-in-depth fallback (unmodified bare specifier) ---"
unmodified_dir="${TMP_DIR}/unmodified-bare-specifier"
build_fixture "$unmodified_dir" '
const defaultConfig = require( "@wordpress/scripts/config/webpack.config" );
const path = require( "path" );

module.exports = {
	...defaultConfig,
	entry: {
		main: "./src/main.js",
		secondary: "./src/secondary.js",
	},
	output: {
		...defaultConfig.output,
		path: path.resolve( __dirname, "build" ),
	},
};
'
echo "OK: unmodified custom webpack.config.js (bare '@wordpress/scripts/config/webpack.config' specifier) still builds through NODE_PATH"

echo "PASS: custom webpack.config.js fixtures build through the wordpress extension"
