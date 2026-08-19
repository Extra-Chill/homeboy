#!/usr/bin/env bash
set -euo pipefail

# Execute the host-smoke contract through the installed WP Codebox runtime. The
# smoke reads the injected roots and requires a file from the mounted dependency.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/../.." && pwd)"
HOMEBOY_CORE_DIR="${HOMEBOY_CORE_DIR:-$(cd "${EXTENSION_PATH}/../.." && pwd)/homeboy}"
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${HOMEBOY_CORE_DIR}/crates/homeboy-extension/src/runtime/resolve-context.sh}"
WP_CODEBOX_BIN="${HOMEBOY_WP_CODEBOX_BIN:-wp-codebox}"
WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT

if [ ! -f "$RESOLVE_CONTEXT_HELPER" ]; then
    echo "Missing resolve-context helper: ${RESOLVE_CONTEXT_HELPER}" >&2
    exit 1
fi
if ! command -v "$WP_CODEBOX_BIN" >/dev/null 2>&1; then
    echo "Missing WP Codebox executable: ${WP_CODEBOX_BIN}" >&2
    exit 1
fi

component="${WORKDIR}/component"
dependency="${WORKDIR}/dependency roots/runtime-dependency"
mkdir -p "${component}/tests" "$dependency"
cat > "${component}/tests/runtime-contract-smoke.php" <<'PHP'
<?php

$wp_path = getenv( 'WP_PATH' );
$roots   = json_decode( getenv( 'HOMEBOY_WORDPRESS_DEPENDENCY_ROOTS_JSON' ), true );

if ( '/wordpress' !== $wp_path || ! is_dir( $wp_path ) || ! function_exists( 'wp_json_encode' ) ) {

	fwrite( STDERR, "Booted WordPress root is unavailable.\n" );
	exit( 1 );
}
if ( ! is_array( $roots ) || '/wordpress/wp-content/plugins/runtime-dependency' !== ( $roots['runtime-dependency'] ?? null ) ) {

	fwrite( STDERR, "Dependency root mapping is unavailable.\n" );
	exit( 1 );
}
if ( '/wordpress/wp-content/plugins/runtime-dependency' !== getenv( 'HOMEBOY_WORDPRESS_DEPENDENCY_LOWER_R_LOWER_U_LOWER_N_LOWER_T_LOWER_I_LOWER_M_LOWER_E_HYPHEN_LOWER_D_LOWER_E_LOWER_P_LOWER_E_LOWER_N_LOWER_D_LOWER_E_LOWER_N_LOWER_C_LOWER_Y_ROOT' ) ) {

	fwrite( STDERR, "Namespaced dependency root is unavailable.\n" );
	exit( 1 );
}

require $roots['runtime-dependency'] . '/runtime-dependency.php';
if ( ! defined( 'HOMEBOY_RUNTIME_DEPENDENCY_LOADED' ) ) {

	fwrite( STDERR, "Mounted dependency file was not loaded.\n" );
	exit( 1 );
}

echo "runtime host smoke passed\n";
PHP
cat > "${dependency}/runtime-dependency.php" <<'PHP'
<?php
/**
 * Plugin Name: Runtime Dependency
 */

define( 'HOMEBOY_RUNTIME_DEPENDENCY_LOADED', true );
PHP

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" \
HOMEBOY_WP_CODEBOX_BIN="$WP_CODEBOX_BIN" \
HOMEBOY_WORDPRESS_HOST_SMOKE_FILE="tests/runtime-contract-smoke.php" \
HOMEBOY_SETTINGS_JSON="$(jq -nc --arg path "$dependency" '{validation_dependencies: [{path: $path, plugin_slug: "runtime-dependency"}]}')" \
    bash "${SCRIPT_DIR}/test-runner-host-smoke-wp.sh" > "${WORKDIR}/run.out"

if ! grep -Fq "runtime host smoke passed" "${WORKDIR}/run.out"; then
    echo "Expected the WordPress host smoke to read and use runtime mappings." >&2
    sed 's/^/  /' "${WORKDIR}/run.out" >&2
    exit 1
fi

echo "Real WordPress host-smoke runtime contract passed"
